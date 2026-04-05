use crate::fd_table::{
    FdResult, FileDescription, ProcessFdTable, SharedFileDescription, FILETYPE_PIPE, O_RDONLY,
    O_WRONLY,
};
use crate::poll::{PollEvents, PollNotifier, POLLERR, POLLHUP, POLLIN, POLLOUT};
use std::collections::{BTreeMap, VecDeque};
use std::error::Error;
use std::fmt;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

pub const MAX_PIPE_BUFFER_BYTES: usize = 65_536;
pub const PIPE_BUF_BYTES: usize = 4_096;

pub type PipeResult<T> = Result<T, PipeError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipeError {
    code: &'static str,
    message: String,
}

impl PipeError {
    pub fn code(&self) -> &'static str {
        self.code
    }

    fn bad_file_descriptor(message: impl Into<String>) -> Self {
        Self {
            code: "EBADF",
            message: message.into(),
        }
    }

    fn broken_pipe(message: impl Into<String>) -> Self {
        Self {
            code: "EPIPE",
            message: message.into(),
        }
    }

    fn would_block(message: impl Into<String>) -> Self {
        Self {
            code: "EAGAIN",
            message: message.into(),
        }
    }
}

impl fmt::Display for PipeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl Error for PipeError {}

#[derive(Debug, Clone)]
pub struct PipeEnd {
    pub description: SharedFileDescription,
    pub filetype: u8,
}

#[derive(Debug, Clone)]
pub struct PipePair {
    pub read: PipeEnd,
    pub write: PipeEnd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PipeRef {
    pipe_id: u64,
    end: PipeSide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PipeSide {
    Read,
    Write,
}

#[derive(Debug, Default)]
struct PendingRead {
    result: Option<Option<Vec<u8>>>,
}

#[derive(Debug, Default)]
struct PipeState {
    buffer: VecDeque<Vec<u8>>,
    closed_read: bool,
    closed_write: bool,
    waiting_reads: VecDeque<u64>,
}

#[derive(Debug)]
struct PipeManagerState {
    pipes: BTreeMap<u64, PipeState>,
    desc_to_pipe: BTreeMap<u64, PipeRef>,
    waiters: BTreeMap<u64, PendingRead>,
    next_pipe_id: u64,
    next_desc_id: u64,
    next_waiter_id: u64,
}

impl Default for PipeManagerState {
    fn default() -> Self {
        Self {
            pipes: BTreeMap::new(),
            desc_to_pipe: BTreeMap::new(),
            waiters: BTreeMap::new(),
            next_pipe_id: 1,
            next_desc_id: 100_000,
            next_waiter_id: 1,
        }
    }
}

#[derive(Debug)]
struct PipeManagerInner {
    state: Mutex<PipeManagerState>,
    waiters: Condvar,
}

#[derive(Debug, Clone)]
pub struct PipeManager {
    inner: Arc<PipeManagerInner>,
    notifier: Option<PollNotifier>,
}

impl Default for PipeManager {
    fn default() -> Self {
        Self {
            inner: Arc::new(PipeManagerInner {
                state: Mutex::new(PipeManagerState::default()),
                waiters: Condvar::new(),
            }),
            notifier: None,
        }
    }
}

impl PipeManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn with_notifier(notifier: PollNotifier) -> Self {
        Self {
            notifier: Some(notifier),
            ..Self::default()
        }
    }

    pub fn create_pipe(&self) -> PipePair {
        let mut state = lock_or_recover(&self.inner.state);
        let pipe_id = state.next_pipe_id;
        state.next_pipe_id += 1;

        let read_id = state.next_desc_id;
        state.next_desc_id += 1;
        let write_id = state.next_desc_id;
        state.next_desc_id += 1;

        state.pipes.insert(pipe_id, PipeState::default());
        state.desc_to_pipe.insert(
            read_id,
            PipeRef {
                pipe_id,
                end: PipeSide::Read,
            },
        );
        state.desc_to_pipe.insert(
            write_id,
            PipeRef {
                pipe_id,
                end: PipeSide::Write,
            },
        );
        drop(state);

        PipePair {
            read: PipeEnd {
                description: Arc::new(FileDescription::with_ref_count(
                    read_id,
                    format!("pipe:{pipe_id}:read"),
                    O_RDONLY,
                    0,
                )),
                filetype: FILETYPE_PIPE,
            },
            write: PipeEnd {
                description: Arc::new(FileDescription::with_ref_count(
                    write_id,
                    format!("pipe:{pipe_id}:write"),
                    O_WRONLY,
                    0,
                )),
                filetype: FILETYPE_PIPE,
            },
        }
    }

    pub fn poll(&self, description_id: u64, requested: PollEvents) -> PipeResult<PollEvents> {
        let state = lock_or_recover(&self.inner.state);
        let pipe_ref = state
            .desc_to_pipe
            .get(&description_id)
            .copied()
            .ok_or_else(|| PipeError::bad_file_descriptor("not a pipe end"))?;
        let pipe = state
            .pipes
            .get(&pipe_ref.pipe_id)
            .ok_or_else(|| PipeError::bad_file_descriptor("pipe not found"))?;

        let mut events = PollEvents::empty();
        match pipe_ref.end {
            PipeSide::Read => {
                if requested.intersects(POLLIN) && !pipe.buffer.is_empty() {
                    events |= POLLIN;
                }
                if pipe.closed_write {
                    events |= POLLHUP;
                }
            }
            PipeSide::Write => {
                if pipe.closed_read {
                    events |= POLLERR;
                } else if requested.intersects(POLLOUT)
                    && (available_capacity(pipe) > 0 || !pipe.waiting_reads.is_empty())
                {
                    events |= POLLOUT;
                }
            }
        }

        Ok(events)
    }

    pub fn write(&self, description_id: u64, data: impl AsRef<[u8]>) -> PipeResult<usize> {
        self.write_with_mode(description_id, data, true)
    }

    pub fn write_blocking(&self, description_id: u64, data: impl AsRef<[u8]>) -> PipeResult<usize> {
        self.write_with_mode(description_id, data, false)
    }

    pub fn write_with_mode(
        &self,
        description_id: u64,
        data: impl AsRef<[u8]>,
        nonblocking: bool,
    ) -> PipeResult<usize> {
        let payload = data.as_ref();
        let mut state = lock_or_recover(&self.inner.state);
        let pipe_ref = state
            .desc_to_pipe
            .get(&description_id)
            .copied()
            .ok_or_else(|| PipeError::bad_file_descriptor("not a pipe write end"))?;
        if pipe_ref.end != PipeSide::Write {
            return Err(PipeError::bad_file_descriptor("not a pipe write end"));
        }

        loop {
            let waiter_id = {
                let pipe = state
                    .pipes
                    .get_mut(&pipe_ref.pipe_id)
                    .ok_or_else(|| PipeError::bad_file_descriptor("pipe not found"))?;
                if pipe.closed_write {
                    return Err(PipeError::broken_pipe("write end closed"));
                }
                if pipe.closed_read {
                    return Err(PipeError::broken_pipe("read end closed"));
                }
                pipe.waiting_reads.pop_front()
            };

            if let Some(waiter_id) = waiter_id {
                if let Some(waiter) = state.waiters.get_mut(&waiter_id) {
                    waiter.result = Some(Some(payload.to_vec()));
                    self.notify_waiters_and_pollers();
                    return Ok(payload.len());
                }
                continue;
            }

            let current_buffer_size = {
                let pipe = state
                    .pipes
                    .get(&pipe_ref.pipe_id)
                    .ok_or_else(|| PipeError::bad_file_descriptor("pipe not found"))?;
                buffer_size(&pipe.buffer)
            };
            let available = MAX_PIPE_BUFFER_BYTES.saturating_sub(current_buffer_size);

            if payload.len() <= PIPE_BUF_BYTES {
                if available >= payload.len() {
                    let pipe = state
                        .pipes
                        .get_mut(&pipe_ref.pipe_id)
                        .ok_or_else(|| PipeError::bad_file_descriptor("pipe not found"))?;
                    pipe.buffer.push_back(payload.to_vec());
                    self.notify_waiters_and_pollers();
                    return Ok(payload.len());
                }
            } else if available > 0 {
                let chunk_len = available.min(payload.len());
                let pipe = state
                    .pipes
                    .get_mut(&pipe_ref.pipe_id)
                    .ok_or_else(|| PipeError::bad_file_descriptor("pipe not found"))?;
                pipe.buffer.push_back(payload[..chunk_len].to_vec());
                self.notify_waiters_and_pollers();
                return Ok(chunk_len);
            }

            if nonblocking {
                return Err(PipeError::would_block("pipe buffer full"));
            }

            state = wait_or_recover(&self.inner.waiters, state);
        }
    }

    pub fn read(&self, description_id: u64, length: usize) -> PipeResult<Option<Vec<u8>>> {
        self.read_with_timeout(description_id, length, None)
    }

    pub fn read_with_timeout(
        &self,
        description_id: u64,
        length: usize,
        timeout: Option<Duration>,
    ) -> PipeResult<Option<Vec<u8>>> {
        let mut state = lock_or_recover(&self.inner.state);
        let pipe_ref = state
            .desc_to_pipe
            .get(&description_id)
            .copied()
            .ok_or_else(|| PipeError::bad_file_descriptor("not a pipe read end"))?;
        if pipe_ref.end != PipeSide::Read {
            return Err(PipeError::bad_file_descriptor("not a pipe read end"));
        }

        let mut waiter_id = None;
        let deadline = timeout.map(|duration| Instant::now() + duration);

        loop {
            if let Some(id) = waiter_id {
                if let Some(waiter) = state.waiters.get_mut(&id) {
                    if let Some(result) = waiter.result.take() {
                        state.waiters.remove(&id);
                        return Ok(result);
                    }
                }
            }

            {
                let pipe = state
                    .pipes
                    .get_mut(&pipe_ref.pipe_id)
                    .ok_or_else(|| PipeError::bad_file_descriptor("pipe not found"))?;

                if !pipe.buffer.is_empty() {
                    let result = drain_buffer(&mut pipe.buffer, length);
                    self.notify_waiters_and_pollers();
                    return Ok(Some(result));
                }

                if pipe.closed_write {
                    if let Some(id) = waiter_id {
                        state.waiters.remove(&id);
                    }
                    return Ok(None);
                }
            }

            let id = if let Some(id) = waiter_id {
                id
            } else {
                let next = state.next_waiter_id;
                state.next_waiter_id += 1;
                state.waiters.insert(next, PendingRead::default());
                let Some(pipe) = state.pipes.get_mut(&pipe_ref.pipe_id) else {
                    state.waiters.remove(&next);
                    return Err(PipeError::bad_file_descriptor("pipe not found"));
                };
                pipe.waiting_reads.push_back(next);
                self.notify_waiters_and_pollers();
                waiter_id = Some(next);
                next
            };

            let Some(deadline) = deadline else {
                state = wait_or_recover(&self.inner.waiters, state);
                if !state.waiters.contains_key(&id) {
                    waiter_id = None;
                }
                continue;
            };

            let now = Instant::now();
            if now >= deadline {
                if let Some(id) = waiter_id.take() {
                    state.waiters.remove(&id);
                    if let Some(pipe) = state.pipes.get_mut(&pipe_ref.pipe_id) {
                        pipe.waiting_reads.retain(|queued| *queued != id);
                    }
                    self.notify_waiters_and_pollers();
                }
                return Err(PipeError::would_block("pipe read timed out"));
            }

            let remaining = deadline.saturating_duration_since(now);
            let (next_state, wait_result) =
                wait_timeout_or_recover(&self.inner.waiters, state, remaining);
            state = next_state;
            if !state.waiters.contains_key(&id) {
                waiter_id = None;
            }
            if wait_result.timed_out() {
                if let Some(id) = waiter_id.take() {
                    state.waiters.remove(&id);
                    if let Some(pipe) = state.pipes.get_mut(&pipe_ref.pipe_id) {
                        pipe.waiting_reads.retain(|queued| *queued != id);
                    }
                    self.notify_waiters_and_pollers();
                }
                return Err(PipeError::would_block("pipe read timed out"));
            }
        }
    }

    pub fn close(&self, description_id: u64) {
        let mut state = lock_or_recover(&self.inner.state);
        let Some(pipe_ref) = state.desc_to_pipe.remove(&description_id) else {
            return;
        };

        let (waiter_ids, remove_pipe, should_notify) =
            if let Some(pipe) = state.pipes.get_mut(&pipe_ref.pipe_id) {
                match pipe_ref.end {
                    PipeSide::Read => {
                        pipe.closed_read = true;
                        (Vec::new(), pipe.closed_read && pipe.closed_write, true)
                    }
                    PipeSide::Write => {
                        pipe.closed_write = true;
                        let waiter_ids = pipe.waiting_reads.drain(..).collect::<Vec<_>>();
                        (waiter_ids, pipe.closed_read && pipe.closed_write, true)
                    }
                }
            } else {
                (Vec::new(), false, false)
            };

        for waiter_id in waiter_ids {
            if let Some(waiter) = state.waiters.get_mut(&waiter_id) {
                waiter.result = Some(None);
            }
        }

        if remove_pipe {
            state.pipes.remove(&pipe_ref.pipe_id);
        }
        if should_notify {
            self.notify_waiters_and_pollers();
        }
    }

    pub fn is_pipe(&self, description_id: u64) -> bool {
        lock_or_recover(&self.inner.state)
            .desc_to_pipe
            .contains_key(&description_id)
    }

    pub fn pipe_id_for(&self, description_id: u64) -> Option<u64> {
        lock_or_recover(&self.inner.state)
            .desc_to_pipe
            .get(&description_id)
            .map(|pipe_ref| pipe_ref.pipe_id)
    }

    pub fn pipe_count(&self) -> usize {
        lock_or_recover(&self.inner.state).pipes.len()
    }

    pub fn buffered_bytes(&self) -> usize {
        lock_or_recover(&self.inner.state)
            .pipes
            .values()
            .map(|pipe| buffer_size(&pipe.buffer))
            .sum()
    }

    pub fn create_pipe_fds(&self, fd_table: &mut ProcessFdTable) -> FdResult<(u32, u32)> {
        let pipe = self.create_pipe();
        let read_fd =
            fd_table.open_with(Arc::clone(&pipe.read.description), FILETYPE_PIPE, None)?;
        match fd_table.open_with(Arc::clone(&pipe.write.description), FILETYPE_PIPE, None) {
            Ok(write_fd) => Ok((read_fd, write_fd)),
            Err(error) => {
                fd_table.close(read_fd);
                self.close(pipe.read.description.id());
                self.close(pipe.write.description.id());
                Err(error)
            }
        }
    }

    fn notify_waiters_and_pollers(&self) {
        self.inner.waiters.notify_all();
        if let Some(notifier) = &self.notifier {
            notifier.notify();
        }
    }
}

fn buffer_size(buffer: &VecDeque<Vec<u8>>) -> usize {
    buffer.iter().map(Vec::len).sum()
}

fn available_capacity(pipe: &PipeState) -> usize {
    MAX_PIPE_BUFFER_BYTES.saturating_sub(buffer_size(&pipe.buffer))
}

fn drain_buffer(buffer: &mut VecDeque<Vec<u8>>, length: usize) -> Vec<u8> {
    let mut chunks = Vec::new();
    let mut remaining = length;

    while remaining > 0 {
        let Some(chunk) = buffer.pop_front() else {
            break;
        };
        if chunk.len() <= remaining {
            remaining -= chunk.len();
            chunks.push(chunk);
        } else {
            let (head, tail) = chunk.split_at(remaining);
            chunks.push(head.to_vec());
            buffer.push_front(tail.to_vec());
            remaining = 0;
        }
    }

    if chunks.len() == 1 {
        return chunks.pop().expect("single chunk should exist");
    }

    let total = chunks.iter().map(Vec::len).sum();
    let mut result = Vec::with_capacity(total);
    for chunk in chunks {
        result.extend_from_slice(&chunk);
    }
    result
}

fn lock_or_recover<'a, T>(mutex: &'a Mutex<T>) -> MutexGuard<'a, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn wait_or_recover<'a, T>(condvar: &Condvar, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
    match condvar.wait(guard) {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn wait_timeout_or_recover<'a, T>(
    condvar: &Condvar,
    guard: MutexGuard<'a, T>,
    timeout: Duration,
) -> (MutexGuard<'a, T>, std::sync::WaitTimeoutResult) {
    match condvar.wait_timeout(guard, timeout) {
        Ok(result) => result,
        Err(poisoned) => poisoned.into_inner(),
    }
}
