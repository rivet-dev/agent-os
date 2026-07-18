use crate::fd_table::{
    allocate_file_description_id, FdResult, FileDescription, ProcessFdTable, SharedFileDescription,
    FILETYPE_PIPE, O_NONBLOCK, O_RDONLY, O_RDWR, O_WRONLY,
};
use crate::poll::{PollEvents, PollNotifier, POLLERR, POLLHUP, POLLIN, POLLOUT};
use crate::resource_accounting::BlockingReadDeadline;
use std::collections::{BTreeMap, VecDeque};
use std::error::Error;
use std::fmt;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;
use web_time::Instant;

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

    fn no_reader(message: impl Into<String>) -> Self {
        Self {
            code: "ENXIO",
            message: message.into(),
        }
    }

    fn io(message: impl Into<String>) -> Self {
        Self {
            code: "EIO",
            message: message.into(),
        }
    }

    fn overflow(message: impl Into<String>) -> Self {
        Self {
            code: "EOVERFLOW",
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
    ReadWrite,
}

#[derive(Debug, Default)]
struct PendingRead {
    length: usize,
    result: Option<Option<Vec<u8>>>,
}

#[derive(Debug)]
struct PipeState {
    buffer: VecDeque<Vec<u8>>,
    readers: usize,
    writers: usize,
    waiting_reads: VecDeque<u64>,
    mode: u32,
    uid: u32,
    gid: u32,
    named_key: Option<(u64, u64)>,
}

impl Default for PipeState {
    fn default() -> Self {
        Self {
            buffer: VecDeque::new(),
            readers: 0,
            writers: 0,
            waiting_reads: VecDeque::new(),
            mode: 0o600,
            uid: 0,
            gid: 0,
            named_key: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PipeMetadata {
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
}

#[derive(Debug)]
struct PipeManagerState {
    pipes: BTreeMap<u64, PipeState>,
    desc_to_pipe: BTreeMap<u64, PipeRef>,
    named_pipes: BTreeMap<(u64, u64), u64>,
    waiters: BTreeMap<u64, PendingRead>,
    next_pipe_id: u64,
    next_waiter_id: u64,
}

impl Default for PipeManagerState {
    fn default() -> Self {
        Self {
            pipes: BTreeMap::new(),
            desc_to_pipe: BTreeMap::new(),
            named_pipes: BTreeMap::new(),
            waiters: BTreeMap::new(),
            next_pipe_id: 1,
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
    pub fn is_write_to_read_pair(
        &self,
        write_description_id: u64,
        read_description_id: u64,
    ) -> bool {
        let state = lock_or_recover(&self.inner.state);
        match (
            state.desc_to_pipe.get(&write_description_id),
            state.desc_to_pipe.get(&read_description_id),
        ) {
            (Some(write), Some(read)) => {
                write.pipe_id == read.pipe_id
                    && write.end == PipeSide::Write
                    && read.end == PipeSide::Read
            }
            _ => false,
        }
    }

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

        let read_id = allocate_file_description_id();
        let write_id = allocate_file_description_id();

        state.pipes.insert(
            pipe_id,
            PipeState {
                readers: 1,
                writers: 1,
                ..PipeState::default()
            },
        );
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

    pub fn open_named_pipe(
        &self,
        key: (u64, u64),
        path: &str,
        flags: u32,
        timeout: Option<Duration>,
    ) -> PipeResult<PipeEnd> {
        self.open_named_pipe_with_deadline(key, path, flags, timeout, None)
    }

    pub(crate) fn open_named_pipe_with_deadline(
        &self,
        key: (u64, u64),
        path: &str,
        flags: u32,
        timeout: Option<Duration>,
        mut blocking_deadline: Option<BlockingReadDeadline>,
    ) -> PipeResult<PipeEnd> {
        let timeout_deadline = timeout
            .map(|timeout| {
                Instant::now().checked_add(timeout).ok_or_else(|| {
                    PipeError::overflow("FIFO open timeout exceeds the supported deadline range")
                })
            })
            .transpose()?;
        let access_mode = flags & 0b11;
        if !matches!(access_mode, O_RDONLY | O_WRONLY | O_RDWR) {
            return Err(PipeError::bad_file_descriptor("invalid FIFO access mode"));
        }

        let mut state = lock_or_recover(&self.inner.state);
        let pipe_id = match state.named_pipes.get(&key).copied() {
            Some(pipe_id) => pipe_id,
            None => {
                let pipe_id = state.next_pipe_id;
                state.next_pipe_id += 1;
                state.named_pipes.insert(key, pipe_id);
                state.pipes.insert(
                    pipe_id,
                    PipeState {
                        named_key: Some(key),
                        ..PipeState::default()
                    },
                );
                pipe_id
            }
        };

        if access_mode == O_WRONLY
            && flags & O_NONBLOCK != 0
            && state
                .pipes
                .get(&pipe_id)
                .is_none_or(|pipe| pipe.readers == 0)
        {
            return Err(PipeError::no_reader(format!("FIFO has no reader: {path}")));
        }

        let description_id = allocate_file_description_id();
        let side = match access_mode {
            O_RDONLY => PipeSide::Read,
            O_WRONLY => PipeSide::Write,
            O_RDWR => PipeSide::ReadWrite,
            _ => unreachable!(),
        };
        state
            .desc_to_pipe
            .insert(description_id, PipeRef { pipe_id, end: side });
        let pipe = state
            .pipes
            .get_mut(&pipe_id)
            .expect("named pipe must exist after allocation");
        match side {
            PipeSide::Read => pipe.readers += 1,
            PipeSide::Write => pipe.writers += 1,
            PipeSide::ReadWrite => {
                pipe.readers += 1;
                pipe.writers += 1;
            }
        }
        self.notify_waiters_and_pollers();

        let should_wait = flags & O_NONBLOCK == 0 && access_mode != O_RDWR;
        if should_wait {
            let ready = |state: &PipeManagerState| {
                state.pipes.get(&pipe_id).is_some_and(|pipe| match side {
                    PipeSide::Read => pipe.writers > 0,
                    PipeSide::Write => pipe.readers > 0,
                    PipeSide::ReadWrite => true,
                })
            };
            if let Some(deadline) = timeout_deadline {
                while !ready(&state) {
                    let remaining = blocking_deadline
                        .as_mut()
                        .and_then(BlockingReadDeadline::wait_slice)
                        .unwrap_or_else(|| deadline.saturating_duration_since(Instant::now()));
                    let (next, result) = self
                        .inner
                        .waiters
                        .wait_timeout_while(state, remaining, |state| !ready(state))
                        .map_err(|_| {
                            PipeError::io("FIFO wait state was poisoned by a prior panic")
                        })?;
                    state = next;
                    if ready(&state) {
                        break;
                    }
                    if result.timed_out()
                        && blocking_deadline
                            .as_mut()
                            .is_some_and(|deadline| !deadline.expired())
                    {
                        continue;
                    }
                    if result.timed_out() || Instant::now() >= deadline {
                        drop(state);
                        self.close(description_id);
                        return Err(PipeError::would_block(format!(
                            "FIFO open timed out: {path}"
                        )));
                    }
                }
            } else {
                state = self
                    .inner
                    .waiters
                    .wait_while(state, |state| !ready(state))
                    .map_err(|_| PipeError::io("FIFO wait state was poisoned by a prior panic"))?;
            }
        }
        drop(state);

        Ok(PipeEnd {
            description: Arc::new(FileDescription::with_ref_count(
                description_id,
                path,
                flags,
                0,
            )),
            filetype: FILETYPE_PIPE,
        })
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
                if pipe.writers == 0 {
                    events |= POLLHUP;
                }
            }
            PipeSide::Write => {
                if pipe.readers == 0 {
                    events |= POLLERR;
                } else if requested.intersects(POLLOUT)
                    && (available_capacity(pipe) > 0 || !pipe.waiting_reads.is_empty())
                {
                    events |= POLLOUT;
                }
            }
            PipeSide::ReadWrite => {
                if requested.intersects(POLLIN) && !pipe.buffer.is_empty() {
                    events |= POLLIN;
                }
                if requested.intersects(POLLOUT)
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
        if !matches!(pipe_ref.end, PipeSide::Write | PipeSide::ReadWrite) {
            return Err(PipeError::bad_file_descriptor("not a pipe write end"));
        }

        loop {
            let waiter_id = {
                let pipe = state
                    .pipes
                    .get_mut(&pipe_ref.pipe_id)
                    .ok_or_else(|| PipeError::bad_file_descriptor("pipe not found"))?;
                if pipe.readers == 0 {
                    return Err(PipeError::broken_pipe("read end closed"));
                }
                pipe.waiting_reads.pop_front()
            };

            if let Some(waiter_id) = waiter_id {
                let waiter_length = match state.waiters.get(&waiter_id) {
                    Some(waiter) => waiter.length,
                    None => continue,
                };
                let delivered_len = waiter_length.min(payload.len());
                let delivered = payload[..delivered_len].to_vec();
                let remainder = &payload[delivered_len..];

                if !remainder.is_empty() {
                    let pipe = state
                        .pipes
                        .get_mut(&pipe_ref.pipe_id)
                        .ok_or_else(|| PipeError::bad_file_descriptor("pipe not found"))?;
                    pipe.buffer.push_back(remainder.to_vec());
                }

                if let Some(waiter) = state.waiters.get_mut(&waiter_id) {
                    waiter.result = Some(Some(delivered));
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
        self.read_with_timeout_and_deadline(description_id, length, timeout, None)
    }

    pub(crate) fn read_with_timeout_and_deadline(
        &self,
        description_id: u64,
        length: usize,
        timeout: Option<Duration>,
        mut blocking_deadline: Option<BlockingReadDeadline>,
    ) -> PipeResult<Option<Vec<u8>>> {
        let mut state = lock_or_recover(&self.inner.state);
        let pipe_ref = state
            .desc_to_pipe
            .get(&description_id)
            .copied()
            .ok_or_else(|| PipeError::bad_file_descriptor("not a pipe read end"))?;
        if !matches!(pipe_ref.end, PipeSide::Read | PipeSide::ReadWrite) {
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

                if pipe.writers == 0 {
                    if let Some(id) = waiter_id {
                        state.waiters.remove(&id);
                    }
                    return Ok(None);
                }
            }

            // A zero/expired timeout is a nonblocking readiness probe. Do not
            // register and immediately remove a waiter: both transitions wake
            // the process-wide poll notifier and can make a deferred probe
            // wake itself forever even though no pipe state changed.
            if waiter_id.is_none() && deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(PipeError::would_block("pipe read timed out"));
            }

            let id = if let Some(id) = waiter_id {
                id
            } else {
                let next = state.next_waiter_id;
                state.next_waiter_id += 1;
                state.waiters.insert(
                    next,
                    PendingRead {
                        length,
                        result: None,
                    },
                );
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
                if let Some(blocking_deadline) = &mut blocking_deadline {
                    blocking_deadline.expired();
                }
                if let Some(id) = waiter_id.take() {
                    state.waiters.remove(&id);
                    if let Some(pipe) = state.pipes.get_mut(&pipe_ref.pipe_id) {
                        pipe.waiting_reads.retain(|queued| *queued != id);
                    }
                    self.notify_waiters_and_pollers();
                }
                return Err(PipeError::would_block("pipe read timed out"));
            }

            let remaining = blocking_deadline
                .as_mut()
                .and_then(BlockingReadDeadline::wait_slice)
                .unwrap_or_else(|| deadline.saturating_duration_since(now));
            let (next_state, wait_result) =
                wait_timeout_or_recover(&self.inner.waiters, state, remaining);
            state = next_state;
            if !state.waiters.contains_key(&id) {
                waiter_id = None;
            }
            if wait_result.timed_out() {
                if blocking_deadline
                    .as_mut()
                    .is_some_and(|deadline| !deadline.expired())
                {
                    continue;
                }
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
                        pipe.readers = pipe.readers.saturating_sub(1);
                        (Vec::new(), pipe.readers == 0 && pipe.writers == 0, true)
                    }
                    PipeSide::Write => {
                        pipe.writers = pipe.writers.saturating_sub(1);
                        let waiter_ids = if pipe.writers == 0 {
                            pipe.waiting_reads.drain(..).collect::<Vec<_>>()
                        } else {
                            Vec::new()
                        };
                        (waiter_ids, pipe.readers == 0 && pipe.writers == 0, true)
                    }
                    PipeSide::ReadWrite => {
                        pipe.readers = pipe.readers.saturating_sub(1);
                        pipe.writers = pipe.writers.saturating_sub(1);
                        let waiter_ids = if pipe.writers == 0 {
                            pipe.waiting_reads.drain(..).collect::<Vec<_>>()
                        } else {
                            Vec::new()
                        };
                        (waiter_ids, pipe.readers == 0 && pipe.writers == 0, true)
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
            if let Some(pipe) = state.pipes.remove(&pipe_ref.pipe_id) {
                if let Some(key) = pipe.named_key {
                    state.named_pipes.remove(&key);
                }
            }
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

    pub fn metadata(&self, description_id: u64) -> Option<PipeMetadata> {
        let state = lock_or_recover(&self.inner.state);
        let pipe_id = state.desc_to_pipe.get(&description_id)?.pipe_id;
        let pipe = state.pipes.get(&pipe_id)?;
        Some(PipeMetadata {
            mode: pipe.mode,
            uid: pipe.uid,
            gid: pipe.gid,
        })
    }

    pub fn set_owner(&self, description_id: u64, uid: u32, gid: u32) -> PipeResult<()> {
        let mut state = lock_or_recover(&self.inner.state);
        let pipe_id = state
            .desc_to_pipe
            .get(&description_id)
            .ok_or_else(|| PipeError::bad_file_descriptor("not a pipe end"))?
            .pipe_id;
        let pipe = state
            .pipes
            .get_mut(&pipe_id)
            .ok_or_else(|| PipeError::bad_file_descriptor("pipe not found"))?;
        pipe.uid = uid;
        pipe.gid = gid;
        Ok(())
    }

    pub fn chmod(&self, description_id: u64, mode: u32) -> PipeResult<()> {
        let mut state = lock_or_recover(&self.inner.state);
        let pipe_id = state
            .desc_to_pipe
            .get(&description_id)
            .ok_or_else(|| PipeError::bad_file_descriptor("not a pipe end"))?
            .pipe_id;
        let pipe = state
            .pipes
            .get_mut(&pipe_id)
            .ok_or_else(|| PipeError::bad_file_descriptor("pipe not found"))?;
        pipe.mode = mode & 0o7777;
        Ok(())
    }

    pub fn pipe_count(&self) -> usize {
        lock_or_recover(&self.inner.state).pipes.len()
    }

    pub fn has_named_pipe(&self, key: (u64, u64)) -> bool {
        lock_or_recover(&self.inner.state)
            .named_pipes
            .contains_key(&key)
    }

    pub fn named_pipe_peer_ready(&self, description_id: u64) -> PipeResult<Option<bool>> {
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
        if pipe.named_key.is_none() {
            return Ok(None);
        }
        Ok(Some(match pipe_ref.end {
            PipeSide::Read => pipe.writers > 0,
            PipeSide::Write => pipe.readers > 0,
            PipeSide::ReadWrite => true,
        }))
    }

    pub fn buffered_bytes(&self) -> usize {
        lock_or_recover(&self.inner.state)
            .pipes
            .values()
            .map(|pipe| buffer_size(&pipe.buffer))
            .sum()
    }

    pub fn waiting_reader_count(&self, description_id: u64) -> PipeResult<usize> {
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
        Ok(pipe.waiting_reads.len())
    }

    pub fn pending_read_waiter_count(&self) -> usize {
        lock_or_recover(&self.inner.state).waiters.len()
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
        Err(poisoned) => {
            eprintln!("ERR_AGENTOS_PIPE_STATE_POISONED: recovering pipe state after a prior panic");
            poisoned.into_inner()
        }
    }
}

fn wait_or_recover<'a, T>(condvar: &Condvar, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
    match condvar.wait(guard) {
        Ok(guard) => guard,
        Err(poisoned) => {
            eprintln!(
                "ERR_AGENTOS_PIPE_WAIT_POISONED: recovering pipe wait state after a prior panic"
            );
            poisoned.into_inner()
        }
    }
}

fn wait_timeout_or_recover<'a, T>(
    condvar: &Condvar,
    guard: MutexGuard<'a, T>,
    timeout: Duration,
) -> (MutexGuard<'a, T>, std::sync::WaitTimeoutResult) {
    match condvar.wait_timeout(guard, timeout) {
        Ok(result) => result,
        Err(poisoned) => {
            eprintln!(
                "ERR_AGENTOS_PIPE_WAIT_POISONED: recovering timed pipe wait state after a prior panic"
            );
            poisoned.into_inner()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_named_pipe_timeout_fails_before_allocating_state() {
        let manager = PipeManager::new();

        let error = manager
            .open_named_pipe((1, 2), "/fifo", O_RDONLY, Some(Duration::MAX))
            .expect_err("an unrepresentable deadline must fail");

        assert_eq!(error.code(), "EOVERFLOW");
        assert_eq!(manager.pipe_count(), 0);
        assert!(lock_or_recover(&manager.inner.state)
            .desc_to_pipe
            .is_empty());
    }

    #[test]
    fn zero_timeout_empty_read_does_not_publish_false_readiness() {
        let notifier = PollNotifier::default();
        let manager = PipeManager::with_notifier(notifier.clone());
        let pipe = manager.create_pipe();
        let observed = notifier.snapshot();

        let error = manager
            .read_with_timeout(pipe.read.description.id(), 1, Some(Duration::ZERO))
            .expect_err("empty nonblocking read must return EAGAIN");

        assert_eq!(error.code(), "EAGAIN");
        assert_eq!(notifier.snapshot(), observed);
        let state = lock_or_recover(&manager.inner.state);
        assert!(state.waiters.is_empty());
        assert_eq!(state.next_waiter_id, 1);
    }
}
