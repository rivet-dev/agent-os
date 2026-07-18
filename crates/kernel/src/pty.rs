use crate::fd_table::{
    allocate_file_description_id, FdResult, FileDescription, ProcessFdTable, SharedFileDescription,
    FILETYPE_CHARACTER_DEVICE, O_RDWR,
};
use crate::poll::{PollEvents, PollNotifier, POLLHUP, POLLIN, POLLOUT};
use crate::resource_accounting::BlockingReadDeadline;
use std::collections::{BTreeMap, VecDeque};
use std::error::Error;
use std::fmt;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;
use web_time::Instant;

pub const MAX_PTY_BUFFER_BYTES: usize = 65_536;
pub const MAX_CANON: usize = 4_096;
pub const MAX_PTY_READ_BYTES: usize = MAX_PTY_BUFFER_BYTES;
pub const MAX_PTY_WRITE_BYTES: usize = MAX_PTY_BUFFER_BYTES;
pub const MAX_PTY_READ_WAITERS: usize = 1_024;
pub const MAX_PTY_RETAINED_READ_BYTES: usize = 1024 * 1024;
pub const SIGINT: i32 = 2;
pub const SIGQUIT: i32 = 3;
pub const SIGTSTP: i32 = 20;
const DEFAULT_PTY_COLUMNS: u16 = 80;
const DEFAULT_PTY_ROWS: u16 = 24;

pub type PtyResult<T> = Result<T, PtyError>;
pub type SignalHandler = Arc<dyn Fn(u32, i32) + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PtyError {
    code: &'static str,
    message: String,
}

impl PtyError {
    pub fn code(&self) -> &'static str {
        self.code
    }

    fn not_a_tty(message: impl Into<String>) -> Self {
        Self {
            code: "ENOTTY",
            message: message.into(),
        }
    }

    fn dangling_pty(message: impl Into<String>) -> Self {
        Self {
            code: "EIO",
            message: message.into(),
        }
    }

    fn io(message: impl Into<String>) -> Self {
        Self {
            code: "EIO",
            message: message.into(),
        }
    }

    fn would_block(message: impl Into<String>) -> Self {
        Self {
            code: "EAGAIN",
            message: message.into(),
        }
    }

    fn too_big(message: impl Into<String>) -> Self {
        Self {
            code: "E2BIG",
            message: message.into(),
        }
    }
}

impl fmt::Display for PtyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl Error for PtyError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LineDisciplineConfig {
    pub icrnl: Option<bool>,
    pub canonical: Option<bool>,
    pub echo: Option<bool>,
    pub isig: Option<bool>,
    pub opost: Option<bool>,
    pub onlcr: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Termios {
    pub icrnl: bool,
    pub opost: bool,
    pub onlcr: bool,
    pub icanon: bool,
    pub echo: bool,
    pub isig: bool,
    pub cc: TermiosControlChars,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PartialTermios {
    pub icrnl: Option<bool>,
    pub opost: Option<bool>,
    pub onlcr: Option<bool>,
    pub icanon: Option<bool>,
    pub echo: Option<bool>,
    pub isig: Option<bool>,
    pub cc: Option<PartialTermiosControlChars>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermiosControlChars {
    pub vintr: u8,
    pub vquit: u8,
    pub vsusp: u8,
    pub veof: u8,
    pub verase: u8,
    pub vkill: u8,
    pub vwerase: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PartialTermiosControlChars {
    pub vintr: Option<u8>,
    pub vquit: Option<u8>,
    pub vsusp: Option<u8>,
    pub veof: Option<u8>,
    pub verase: Option<u8>,
    pub vkill: Option<u8>,
    pub vwerase: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PtyWindowSize {
    pub cols: u16,
    pub rows: u16,
}

impl Default for PtyWindowSize {
    fn default() -> Self {
        Self {
            cols: DEFAULT_PTY_COLUMNS,
            rows: DEFAULT_PTY_ROWS,
        }
    }
}

impl Default for Termios {
    fn default() -> Self {
        Self {
            icrnl: true,
            opost: true,
            onlcr: true,
            icanon: true,
            echo: true,
            isig: true,
            cc: TermiosControlChars {
                vintr: 0x03,
                vquit: 0x1c,
                vsusp: 0x1a,
                veof: 0x04,
                verase: 0x7f,
                vkill: 0x15,
                vwerase: 0x17,
            },
        }
    }
}

impl Termios {
    fn merge(&mut self, update: PartialTermios) {
        if let Some(icrnl) = update.icrnl {
            self.icrnl = icrnl;
        }
        if let Some(opost) = update.opost {
            self.opost = opost;
        }
        if let Some(onlcr) = update.onlcr {
            self.onlcr = onlcr;
        }
        if let Some(icanon) = update.icanon {
            self.icanon = icanon;
        }
        if let Some(echo) = update.echo {
            self.echo = echo;
        }
        if let Some(isig) = update.isig {
            self.isig = isig;
        }
        if let Some(cc) = update.cc {
            self.cc.merge(cc);
        }
    }
}

impl TermiosControlChars {
    fn merge(&mut self, update: PartialTermiosControlChars) {
        if let Some(vintr) = update.vintr {
            self.vintr = vintr;
        }
        if let Some(vquit) = update.vquit {
            self.vquit = vquit;
        }
        if let Some(vsusp) = update.vsusp {
            self.vsusp = vsusp;
        }
        if let Some(veof) = update.veof {
            self.veof = veof;
        }
        if let Some(verase) = update.verase {
            self.verase = verase;
        }
        if let Some(vkill) = update.vkill {
            self.vkill = vkill;
        }
        if let Some(vwerase) = update.vwerase {
            self.vwerase = vwerase;
        }
    }
}

#[derive(Debug, Clone)]
pub struct PtyEnd {
    pub description: SharedFileDescription,
    pub filetype: u8,
}

#[derive(Debug, Clone)]
pub struct PtyPair {
    pub master: PtyEnd,
    pub slave: PtyEnd,
    pub path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PtyRef {
    pty_id: u64,
    end: PtyEndKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PtyEndKind {
    Master,
    Slave,
}

#[derive(Debug, Clone)]
struct PendingRead {
    length: usize,
    end: PtyEndKind,
    result: Option<Option<Vec<u8>>>,
}

#[derive(Debug, Clone)]
struct RawModeLease {
    owner_pid: u32,
    generation: u64,
    applied_termios_generation: u64,
    restore_termios: Termios,
}

#[derive(Debug, Clone, Default)]
struct PtyState {
    path: String,
    input_buffer: VecDeque<Vec<u8>>,
    output_buffer: VecDeque<Vec<u8>>,
    input_eof_pending: bool,
    closed_master: bool,
    closed_slave: bool,
    waiting_input_reads: VecDeque<u64>,
    waiting_output_reads: VecDeque<u64>,
    termios: Termios,
    termios_generation: u64,
    next_raw_mode_generation: u64,
    raw_mode_leases: Vec<RawModeLease>,
    line_buffer: Vec<u8>,
    foreground_pgid: u32,
    window_size: PtyWindowSize,
}

#[derive(Debug)]
struct PtyManagerState {
    ptys: BTreeMap<u64, PtyState>,
    desc_to_pty: BTreeMap<u64, PtyRef>,
    waiters: BTreeMap<u64, PendingRead>,
    next_pty_id: u64,
    next_waiter_id: u64,
    warned_waiter_limit: bool,
    warned_retained_read_limit: bool,
}

impl Default for PtyManagerState {
    fn default() -> Self {
        Self {
            ptys: BTreeMap::new(),
            desc_to_pty: BTreeMap::new(),
            waiters: BTreeMap::new(),
            next_pty_id: 0,
            next_waiter_id: 1,
            warned_waiter_limit: false,
            warned_retained_read_limit: false,
        }
    }
}

#[derive(Debug)]
struct PtyManagerInner {
    state: Mutex<PtyManagerState>,
    waiters: Condvar,
}

#[derive(Clone)]
pub struct PtyManager {
    inner: Arc<PtyManagerInner>,
    on_signal: Option<SignalHandler>,
    notifier: Option<PollNotifier>,
}

impl Default for PtyManager {
    fn default() -> Self {
        Self {
            inner: Arc::new(PtyManagerInner {
                state: Mutex::new(PtyManagerState::default()),
                waiters: Condvar::new(),
            }),
            on_signal: None,
            notifier: None,
        }
    }
}

impl PtyManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_signal_handler(on_signal: SignalHandler) -> Self {
        let mut manager = Self::new();
        manager.on_signal = Some(on_signal);
        manager
    }

    pub(crate) fn with_signal_handler_and_notifier(
        on_signal: SignalHandler,
        notifier: PollNotifier,
    ) -> Self {
        let mut manager = Self::with_notifier(notifier);
        manager.on_signal = Some(on_signal);
        manager
    }

    pub(crate) fn with_notifier(notifier: PollNotifier) -> Self {
        Self {
            notifier: Some(notifier),
            ..Self::default()
        }
    }

    pub fn create_pty(&self) -> PtyPair {
        let mut state = lock_or_recover(&self.inner.state);
        let pty_id = state.next_pty_id;
        state.next_pty_id += 1;

        let master_id = allocate_file_description_id();
        let slave_id = allocate_file_description_id();

        let path = format!("/dev/pts/{pty_id}");
        state.ptys.insert(
            pty_id,
            PtyState {
                path: path.clone(),
                termios: Termios::default(),
                window_size: PtyWindowSize::default(),
                ..PtyState::default()
            },
        );
        state.desc_to_pty.insert(
            master_id,
            PtyRef {
                pty_id,
                end: PtyEndKind::Master,
            },
        );
        state.desc_to_pty.insert(
            slave_id,
            PtyRef {
                pty_id,
                end: PtyEndKind::Slave,
            },
        );
        drop(state);

        PtyPair {
            master: PtyEnd {
                description: Arc::new(FileDescription::with_ref_count(
                    master_id,
                    format!("pty:{pty_id}:master"),
                    O_RDWR,
                    0,
                )),
                filetype: FILETYPE_CHARACTER_DEVICE,
            },
            slave: PtyEnd {
                description: Arc::new(FileDescription::with_ref_count(
                    slave_id,
                    path.clone(),
                    O_RDWR,
                    0,
                )),
                filetype: FILETYPE_CHARACTER_DEVICE,
            },
            path,
        }
    }

    pub fn create_pty_fds(&self, fd_table: &mut ProcessFdTable) -> FdResult<(u32, u32, String)> {
        let pty = self.create_pty();
        let master_fd = fd_table.open_with(
            Arc::clone(&pty.master.description),
            FILETYPE_CHARACTER_DEVICE,
            None,
        )?;
        match fd_table.open_with(
            Arc::clone(&pty.slave.description),
            FILETYPE_CHARACTER_DEVICE,
            None,
        ) {
            Ok(slave_fd) => Ok((master_fd, slave_fd, pty.path)),
            Err(error) => {
                fd_table.close(master_fd);
                self.close(pty.master.description.id());
                self.close(pty.slave.description.id());
                Err(error)
            }
        }
    }

    pub fn poll(&self, description_id: u64, requested: PollEvents) -> PtyResult<PollEvents> {
        let state = lock_or_recover(&self.inner.state);
        let pty_ref = state
            .desc_to_pty
            .get(&description_id)
            .copied()
            .ok_or_else(|| PtyError::not_a_tty("not a PTY end"))?;
        let pty = state
            .ptys
            .get(&pty_ref.pty_id)
            .ok_or_else(|| PtyError::dangling_pty("PTY not found"))?;
        let retained_read_capacity_available =
            retained_read_bytes(&state.waiters) < MAX_PTY_RETAINED_READ_BYTES;

        let mut events = PollEvents::empty();
        match pty_ref.end {
            PtyEndKind::Master => {
                if requested.intersects(POLLIN) && !pty.output_buffer.is_empty() {
                    events |= POLLIN;
                }
                if pty.closed_slave {
                    events |= POLLHUP;
                } else if requested.intersects(POLLOUT)
                    && if pty.waiting_input_reads.is_empty() {
                        available_capacity(&pty.input_buffer) > 0
                    } else {
                        retained_read_capacity_available
                    }
                {
                    events |= POLLOUT;
                }
            }
            PtyEndKind::Slave => {
                if requested.intersects(POLLIN)
                    && (pty.input_eof_pending || !pty.input_buffer.is_empty())
                {
                    events |= POLLIN;
                }
                if pty.closed_master {
                    events |= POLLHUP;
                } else if requested.intersects(POLLOUT)
                    && if pty.waiting_output_reads.is_empty() {
                        available_capacity(&pty.output_buffer) > 0
                    } else {
                        retained_read_capacity_available
                    }
                {
                    events |= POLLOUT;
                }
            }
        }

        Ok(events)
    }

    pub fn write(&self, description_id: u64, data: impl AsRef<[u8]>) -> PtyResult<usize> {
        let payload = data.as_ref();
        if payload.len() > MAX_PTY_WRITE_BYTES {
            return Err(PtyError::too_big(format!(
                "maxPtyWriteBytes is {MAX_PTY_WRITE_BYTES}, requested {} bytes; split the terminal write into bounded chunks",
                payload.len()
            )));
        }
        let mut signals = Vec::new();

        {
            let mut state = lock_or_recover(&self.inner.state);
            let pty_ref = state
                .desc_to_pty
                .get(&description_id)
                .copied()
                .ok_or_else(|| PtyError::not_a_tty("not a PTY end"))?;
            let PtyManagerState {
                ptys,
                waiters,
                warned_retained_read_limit,
                ..
            } = &mut *state;
            let pty = ptys
                .get_mut(&pty_ref.pty_id)
                .ok_or_else(|| PtyError::dangling_pty("PTY not found"))?;

            match pty_ref.end {
                PtyEndKind::Master => {
                    if pty.closed_master {
                        return Err(PtyError::io("master closed"));
                    }
                    if pty.closed_slave {
                        return Err(PtyError::io("slave closed"));
                    }
                    process_input(
                        pty,
                        waiters,
                        payload,
                        &mut signals,
                        warned_retained_read_limit,
                    )?;
                }
                PtyEndKind::Slave => {
                    if pty.closed_slave {
                        return Err(PtyError::io("slave closed"));
                    }
                    if pty.closed_master {
                        return Err(PtyError::io("master closed"));
                    }

                    let processed = process_output(&pty.termios, payload)?;
                    deliver_output(pty, waiters, &processed, false, warned_retained_read_limit)?;
                    // Terminal emulation: answer a Device Status Report cursor-position
                    // query (ESC[6n) with a cursor report (ESC[row;colR) on the slave's
                    // input. A real terminal emulator on the master side does this; the
                    // converged PTY may have no such emulator, so crossterm/reedline guests
                    // that probe the cursor at startup would otherwise stall and abort.
                    if contains_dsr_cursor_query(payload) {
                        deliver_input(pty, waiters, b"\x1b[1;1R", warned_retained_read_limit)?;
                    }
                }
            }
        }

        self.notify_waiters_and_pollers();
        if let Some(on_signal) = &self.on_signal {
            for (pgid, signal) in signals {
                if pgid > 0 {
                    on_signal(pgid, signal);
                }
            }
        }

        Ok(payload.len())
    }

    pub fn read(&self, description_id: u64, length: usize) -> PtyResult<Option<Vec<u8>>> {
        self.read_with_timeout(description_id, length, None)
    }

    pub fn read_with_timeout(
        &self,
        description_id: u64,
        length: usize,
        timeout: Option<Duration>,
    ) -> PtyResult<Option<Vec<u8>>> {
        self.read_with_timeout_and_deadline(description_id, length, timeout, None)
    }

    pub(crate) fn read_with_timeout_and_deadline(
        &self,
        description_id: u64,
        length: usize,
        timeout: Option<Duration>,
        mut blocking_deadline: Option<BlockingReadDeadline>,
    ) -> PtyResult<Option<Vec<u8>>> {
        if length > MAX_PTY_READ_BYTES {
            return Err(PtyError::too_big(format!(
                "maxPtyReadBytes is {MAX_PTY_READ_BYTES}, requested {length} bytes; lower the terminal read size"
            )));
        }
        let mut state = lock_or_recover(&self.inner.state);
        let pty_ref = state
            .desc_to_pty
            .get(&description_id)
            .copied()
            .ok_or_else(|| PtyError::not_a_tty("not a PTY end"))?;
        if length == 0 {
            return Ok(Some(Vec::new()));
        }
        let mut waiter_id = None;
        let deadline = timeout.map(|duration| Instant::now() + duration);

        loop {
            if let Some(id) = waiter_id {
                if let Some(waiter) = state.waiters.get_mut(&id) {
                    if let Some(result) = waiter.result.take() {
                        state.waiters.remove(&id);
                        // Releasing a retained result can make a PTY writable
                        // again even when its ordinary byte buffer is full.
                        self.notify_waiters_and_pollers();
                        return Ok(result);
                    }
                }
            }

            {
                let pty = state
                    .ptys
                    .get_mut(&pty_ref.pty_id)
                    .ok_or_else(|| PtyError::dangling_pty("PTY not found"))?;

                match pty_ref.end {
                    PtyEndKind::Master => {
                        if pty.closed_master {
                            if let Some(id) = waiter_id {
                                state.waiters.remove(&id);
                            }
                            return Err(PtyError::io("master closed"));
                        }

                        if !pty.output_buffer.is_empty() {
                            let result = drain_buffer(&mut pty.output_buffer, length);
                            // This reader consumed buffered data directly, so its queued waiter
                            // entry must be removed or a later delivery will assign data to an
                            // orphan.
                            if let Some(id) = waiter_id.take() {
                                pty.waiting_input_reads.retain(|queued| *queued != id);
                                pty.waiting_output_reads.retain(|queued| *queued != id);
                                state.waiters.remove(&id);
                            }
                            self.notify_waiters_and_pollers();
                            return Ok(Some(result));
                        }

                        if pty.closed_slave {
                            if let Some(id) = waiter_id {
                                state.waiters.remove(&id);
                            }
                            return Ok(None);
                        }
                    }
                    PtyEndKind::Slave => {
                        if pty.closed_slave {
                            if let Some(id) = waiter_id {
                                state.waiters.remove(&id);
                            }
                            return Err(PtyError::io("slave closed"));
                        }

                        if !pty.input_buffer.is_empty() {
                            let result = drain_buffer(&mut pty.input_buffer, length);
                            // This reader consumed buffered data directly, so its queued waiter
                            // entry must be removed or a later delivery will assign data to an
                            // orphan.
                            if let Some(id) = waiter_id.take() {
                                pty.waiting_input_reads.retain(|queued| *queued != id);
                                pty.waiting_output_reads.retain(|queued| *queued != id);
                                state.waiters.remove(&id);
                            }
                            self.notify_waiters_and_pollers();
                            return Ok(Some(result));
                        }

                        if pty.input_eof_pending {
                            pty.input_eof_pending = false;
                            if let Some(id) = waiter_id {
                                state.waiters.remove(&id);
                            }
                            self.notify_waiters_and_pollers();
                            return Ok(None);
                        }

                        if pty.closed_master {
                            if let Some(id) = waiter_id {
                                state.waiters.remove(&id);
                            }
                            return Ok(None);
                        }
                    }
                }
            }

            // A zero/expired timeout is a nonblocking readiness probe. Do not
            // register and immediately remove a waiter: both transitions wake
            // the process-wide poll notifier and can make a deferred probe
            // wake itself forever even though no PTY state changed.
            if waiter_id.is_none() && deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(PtyError::would_block("PTY read timed out"));
            }

            let id = if let Some(id) = waiter_id {
                id
            } else {
                if state.waiters.len() >= MAX_PTY_READ_WAITERS {
                    return Err(PtyError::would_block(format!(
                        "maxPtyReadWaiters limit is {MAX_PTY_READ_WAITERS}; retry after another terminal read completes"
                    )));
                }
                let waiter_count = state.waiters.len().saturating_add(1);
                warn_near_pty_limit(
                    &mut state.warned_waiter_limit,
                    "maxPtyReadWaiters",
                    waiter_count,
                    MAX_PTY_READ_WAITERS,
                );
                let next = state.next_waiter_id;
                state.next_waiter_id = state
                    .next_waiter_id
                    .checked_add(1)
                    .ok_or_else(|| PtyError::io("PTY read waiter identifier space exhausted"))?;
                state.waiters.insert(
                    next,
                    PendingRead {
                        length,
                        end: pty_ref.end,
                        result: None,
                    },
                );
                let Some(pty) = state.ptys.get_mut(&pty_ref.pty_id) else {
                    state.waiters.remove(&next);
                    return Err(PtyError::dangling_pty("PTY not found"));
                };
                match pty_ref.end {
                    PtyEndKind::Master => pty.waiting_output_reads.push_back(next),
                    PtyEndKind::Slave => pty.waiting_input_reads.push_back(next),
                }
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
                    if let Some(pty) = state.ptys.get_mut(&pty_ref.pty_id) {
                        pty.waiting_input_reads.retain(|queued| *queued != id);
                        pty.waiting_output_reads.retain(|queued| *queued != id);
                    }
                    self.notify_waiters_and_pollers();
                }
                return Err(PtyError::would_block("PTY read timed out"));
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
                    if let Some(pty) = state.ptys.get_mut(&pty_ref.pty_id) {
                        pty.waiting_input_reads.retain(|queued| *queued != id);
                        pty.waiting_output_reads.retain(|queued| *queued != id);
                    }
                    self.notify_waiters_and_pollers();
                }
                return Err(PtyError::would_block("PTY read timed out"));
            }
        }
    }

    pub fn close(&self, description_id: u64) {
        let mut state = lock_or_recover(&self.inner.state);
        let Some(pty_ref) = state.desc_to_pty.remove(&description_id) else {
            return;
        };

        let (waiter_ids, remove_pty) = if let Some(pty) = state.ptys.get_mut(&pty_ref.pty_id) {
            match pty_ref.end {
                PtyEndKind::Master => {
                    pty.closed_master = true;
                    let mut waiters = pty.waiting_input_reads.drain(..).collect::<Vec<_>>();
                    waiters.extend(pty.waiting_output_reads.drain(..));
                    (waiters, pty.closed_master && pty.closed_slave)
                }
                PtyEndKind::Slave => {
                    pty.closed_slave = true;
                    let mut waiters = pty.waiting_output_reads.drain(..).collect::<Vec<_>>();
                    waiters.extend(pty.waiting_input_reads.drain(..));
                    (waiters, pty.closed_master && pty.closed_slave)
                }
            }
        } else {
            (Vec::new(), false)
        };

        for waiter_id in waiter_ids {
            if let Some(waiter) = state.waiters.get_mut(&waiter_id) {
                waiter.result = Some(None);
            }
        }

        if remove_pty {
            state.ptys.remove(&pty_ref.pty_id);
        }
        self.notify_waiters_and_pollers();
    }

    pub fn is_pty(&self, description_id: u64) -> bool {
        lock_or_recover(&self.inner.state)
            .desc_to_pty
            .contains_key(&description_id)
    }

    pub fn is_slave(&self, description_id: u64) -> bool {
        lock_or_recover(&self.inner.state)
            .desc_to_pty
            .get(&description_id)
            .map(|pty_ref| pty_ref.end == PtyEndKind::Slave)
            .unwrap_or(false)
    }

    pub fn set_discipline(
        &self,
        description_id: u64,
        config: LineDisciplineConfig,
    ) -> PtyResult<()> {
        let mut state = lock_or_recover(&self.inner.state);
        let pty_ref = state
            .desc_to_pty
            .get(&description_id)
            .copied()
            .ok_or_else(|| PtyError::not_a_tty("not a PTY end"))?;
        let pty = state
            .ptys
            .get_mut(&pty_ref.pty_id)
            .ok_or_else(|| PtyError::dangling_pty("PTY not found"))?;
        pty.termios_generation = pty
            .termios_generation
            .checked_add(1)
            .ok_or_else(|| PtyError::io("PTY terminal-attribute generation counter exhausted"))?;
        if let Some(canonical) = config.canonical {
            pty.termios.icanon = canonical;
        }
        if let Some(icrnl) = config.icrnl {
            pty.termios.icrnl = icrnl;
        }
        if let Some(echo) = config.echo {
            pty.termios.echo = echo;
        }
        if let Some(isig) = config.isig {
            pty.termios.isig = isig;
        }
        if let Some(opost) = config.opost {
            pty.termios.opost = opost;
        }
        if let Some(onlcr) = config.onlcr {
            pty.termios.onlcr = onlcr;
        }
        Ok(())
    }

    /// Apply or release raw mode for a process. A foreground owner receives a
    /// generation-scoped lease so teardown can recover the exact attributes it
    /// inherited without letting an unrelated child restore a stale snapshot.
    ///
    /// `lease_owner_pid = None` applies the requested mode but deliberately
    /// does not register teardown recovery (used for a background process).
    pub fn set_raw_mode(
        &self,
        description_id: u64,
        lease_owner_pid: Option<u32>,
        enabled: bool,
    ) -> PtyResult<Option<u64>> {
        let mut state = lock_or_recover(&self.inner.state);
        let pty_ref = state
            .desc_to_pty
            .get(&description_id)
            .copied()
            .ok_or_else(|| PtyError::not_a_tty("not a PTY end"))?;
        let pty = state
            .ptys
            .get_mut(&pty_ref.pty_id)
            .ok_or_else(|| PtyError::dangling_pty("PTY not found"))?;

        if !enabled {
            if let Some(owner_pid) = lease_owner_pid {
                if release_raw_mode_lease(pty, owner_pid, None)? {
                    return Ok(None);
                }
            }
            advance_termios_generation(pty)?;
            apply_raw_mode(&mut pty.termios, false);
            return Ok(None);
        }

        let Some(owner_pid) = lease_owner_pid else {
            advance_termios_generation(pty)?;
            apply_raw_mode(&mut pty.termios, true);
            return Ok(None);
        };

        // Repeated setRawMode(true) by one process keeps its original restore
        // point. If another owner acquired raw mode in between, remove this
        // owner's older frame first and re-acquire at the top of the stack.
        if let Some(index) = pty
            .raw_mode_leases
            .iter()
            .position(|lease| lease.owner_pid == owner_pid)
        {
            if index + 1 == pty.raw_mode_leases.len() {
                advance_termios_generation(pty)?;
                apply_raw_mode(&mut pty.termios, true);
                let lease = pty
                    .raw_mode_leases
                    .last_mut()
                    .expect("raw-mode lease index was just validated");
                lease.applied_termios_generation = pty.termios_generation;
                return Ok(Some(lease.generation));
            }
            let generation = pty.raw_mode_leases[index].generation;
            release_raw_mode_lease(pty, owner_pid, Some(generation))?;
        }

        let generation = pty
            .next_raw_mode_generation
            .checked_add(1)
            .ok_or_else(|| PtyError::io("PTY raw-mode generation counter exhausted"))?;
        pty.next_raw_mode_generation = generation;
        let restore_termios = pty.termios.clone();
        advance_termios_generation(pty)?;
        apply_raw_mode(&mut pty.termios, true);
        pty.raw_mode_leases.push(RawModeLease {
            owner_pid,
            generation,
            applied_termios_generation: pty.termios_generation,
            restore_termios,
        });
        Ok(Some(generation))
    }

    /// Release a particular foreground raw-mode lease during process cleanup.
    /// Returns whether the lease still existed. A stale/non-top release never
    /// changes live terminal attributes; its restore point is transferred to
    /// the next owner so out-of-order child exits still unwind correctly.
    pub fn release_raw_mode(
        &self,
        description_id: u64,
        owner_pid: u32,
        generation: u64,
    ) -> PtyResult<bool> {
        let mut state = lock_or_recover(&self.inner.state);
        let pty_ref = state
            .desc_to_pty
            .get(&description_id)
            .copied()
            .ok_or_else(|| PtyError::not_a_tty("not a PTY end"))?;
        let pty = state
            .ptys
            .get_mut(&pty_ref.pty_id)
            .ok_or_else(|| PtyError::dangling_pty("PTY not found"))?;
        release_raw_mode_lease(pty, owner_pid, Some(generation))
    }

    pub fn get_termios(&self, description_id: u64) -> PtyResult<Termios> {
        let state = lock_or_recover(&self.inner.state);
        let pty_ref = state
            .desc_to_pty
            .get(&description_id)
            .copied()
            .ok_or_else(|| PtyError::not_a_tty("not a PTY end"))?;
        state
            .ptys
            .get(&pty_ref.pty_id)
            .cloned()
            .map(|pty| pty.termios)
            .ok_or_else(|| PtyError::dangling_pty("PTY not found"))
    }

    pub fn set_termios(&self, description_id: u64, termios: PartialTermios) -> PtyResult<()> {
        let mut state = lock_or_recover(&self.inner.state);
        let pty_ref = state
            .desc_to_pty
            .get(&description_id)
            .copied()
            .ok_or_else(|| PtyError::not_a_tty("not a PTY end"))?;
        let pty = state
            .ptys
            .get_mut(&pty_ref.pty_id)
            .ok_or_else(|| PtyError::dangling_pty("PTY not found"))?;
        advance_termios_generation(pty)?;
        pty.termios.merge(termios);
        Ok(())
    }

    pub fn set_foreground_pgid(&self, description_id: u64, pgid: u32) -> PtyResult<()> {
        let mut state = lock_or_recover(&self.inner.state);
        let pty_ref = state
            .desc_to_pty
            .get(&description_id)
            .copied()
            .ok_or_else(|| PtyError::not_a_tty("not a PTY end"))?;
        let pty = state
            .ptys
            .get_mut(&pty_ref.pty_id)
            .ok_or_else(|| PtyError::dangling_pty("PTY not found"))?;
        pty.foreground_pgid = pgid;
        Ok(())
    }

    pub fn get_foreground_pgid(&self, description_id: u64) -> PtyResult<u32> {
        let state = lock_or_recover(&self.inner.state);
        let pty_ref = state
            .desc_to_pty
            .get(&description_id)
            .copied()
            .ok_or_else(|| PtyError::not_a_tty("not a PTY end"))?;
        state
            .ptys
            .get(&pty_ref.pty_id)
            .map(|pty| pty.foreground_pgid)
            .ok_or_else(|| PtyError::dangling_pty("PTY not found"))
    }

    pub fn window_size(&self, description_id: u64) -> PtyResult<PtyWindowSize> {
        let state = lock_or_recover(&self.inner.state);
        let pty_ref = state
            .desc_to_pty
            .get(&description_id)
            .copied()
            .ok_or_else(|| PtyError::not_a_tty("not a PTY end"))?;
        state
            .ptys
            .get(&pty_ref.pty_id)
            .map(|pty| pty.window_size)
            .ok_or_else(|| PtyError::dangling_pty("PTY not found"))
    }

    pub fn resize(&self, description_id: u64, cols: u16, rows: u16) -> PtyResult<Option<u32>> {
        let mut state = lock_or_recover(&self.inner.state);
        let pty_ref = state
            .desc_to_pty
            .get(&description_id)
            .copied()
            .ok_or_else(|| PtyError::not_a_tty("not a PTY end"))?;
        let pty = state
            .ptys
            .get_mut(&pty_ref.pty_id)
            .ok_or_else(|| PtyError::dangling_pty("PTY not found"))?;
        let next_size = PtyWindowSize { cols, rows };
        if pty.window_size == next_size {
            return Ok(None);
        }
        pty.window_size = next_size;
        Ok((pty.foreground_pgid > 0).then_some(pty.foreground_pgid))
    }

    pub fn pty_count(&self) -> usize {
        lock_or_recover(&self.inner.state).ptys.len()
    }

    pub fn buffered_input_bytes(&self) -> usize {
        let state = lock_or_recover(&self.inner.state);
        let buffered: usize = state
            .ptys
            .values()
            .map(|pty| buffer_size(&pty.input_buffer))
            .sum();
        buffered.saturating_add(retained_read_bytes_for_end(
            &state.waiters,
            PtyEndKind::Slave,
        ))
    }

    pub fn buffered_output_bytes(&self) -> usize {
        let state = lock_or_recover(&self.inner.state);
        let buffered: usize = state
            .ptys
            .values()
            .map(|pty| buffer_size(&pty.output_buffer))
            .sum();
        buffered.saturating_add(retained_read_bytes_for_end(
            &state.waiters,
            PtyEndKind::Master,
        ))
    }

    pub fn pending_read_waiter_count(&self) -> usize {
        lock_or_recover(&self.inner.state).waiters.len()
    }

    pub fn queued_read_waiter_count(&self) -> usize {
        lock_or_recover(&self.inner.state)
            .ptys
            .values()
            .map(|pty| pty.waiting_input_reads.len() + pty.waiting_output_reads.len())
            .sum()
    }

    pub fn path_for(&self, description_id: u64) -> Option<String> {
        let state = lock_or_recover(&self.inner.state);
        let pty_ref = state.desc_to_pty.get(&description_id)?;
        state.ptys.get(&pty_ref.pty_id).map(|pty| pty.path.clone())
    }

    fn notify_waiters_and_pollers(&self) {
        self.inner.waiters.notify_all();
        if let Some(notifier) = &self.notifier {
            notifier.notify();
        }
    }
}

/// True if `data` contains a Device Status Report cursor-position query
/// (`ESC [ 6 n`). Used to drive the converged PTY's terminal-style auto-reply.
fn contains_dsr_cursor_query(data: &[u8]) -> bool {
    const QUERY: &[u8] = b"\x1b[6n";
    data.windows(QUERY.len()).any(|window| window == QUERY)
}

fn process_output(termios: &Termios, data: &[u8]) -> PtyResult<Vec<u8>> {
    if !termios.opost || !termios.onlcr || !data.contains(&b'\n') {
        return Ok(data.to_vec());
    }

    let extra_crs = data
        .iter()
        .enumerate()
        .filter(|(index, byte)| **byte == b'\n' && (*index == 0 || data[*index - 1] != b'\r'))
        .count();
    if extra_crs == 0 {
        return Ok(data.to_vec());
    }

    let transformed_len = data.len().checked_add(extra_crs).ok_or_else(|| {
        PtyError::too_big("maxPtyTransformedOutputBytes length calculation overflowed")
    })?;
    if transformed_len > MAX_PTY_BUFFER_BYTES {
        return Err(PtyError::too_big(format!(
            "maxPtyTransformedOutputBytes is {MAX_PTY_BUFFER_BYTES}, terminal output processing would produce {transformed_len} bytes; split the write into smaller chunks"
        )));
    }
    let mut result = Vec::with_capacity(transformed_len);
    for (index, byte) in data.iter().enumerate() {
        if *byte == b'\n' && (index == 0 || data[index - 1] != b'\r') {
            result.push(b'\r');
        }
        result.push(*byte);
    }
    Ok(result)
}

fn process_input(
    pty: &mut PtyState,
    waiters: &mut BTreeMap<u64, PendingRead>,
    data: &[u8],
    signals: &mut Vec<(u32, i32)>,
    warned_retained_read_limit: &mut bool,
) -> PtyResult<()> {
    if !pty.termios.icanon && !pty.termios.echo && !pty.termios.isig {
        let translated = translate_input(&pty.termios, data);
        deliver_input(pty, waiters, &translated, warned_retained_read_limit)?;
        return Ok(());
    }

    for mut byte in data.iter().copied() {
        if pty.termios.icrnl && byte == b'\r' {
            byte = b'\n';
        }

        if pty.termios.isig {
            if let Some(signal) = signal_for_byte(&pty.termios, byte) {
                if pty.termios.icanon {
                    pty.line_buffer.clear();
                }
                let has_foreground_process_group = pty.foreground_pgid > 0;
                // Only echo the signal-generating control char (e.g. "^C") as a
                // line-editor fallback when there is NO foreground process group
                // to receive the signal. With a foreground process group the
                // signal is delivered to it and the char is not echoed, matching
                // the integration suite's VINTR/VSUSP/VQUIT expectations.
                if pty.termios.echo && !has_foreground_process_group {
                    deliver_output(
                        pty,
                        waiters,
                        &echo_control_byte(byte),
                        true,
                        warned_retained_read_limit,
                    )?;
                    if pty.termios.icanon {
                        deliver_output(pty, waiters, b"\r\n", true, warned_retained_read_limit)?;
                    }
                }
                if has_foreground_process_group {
                    signals.push((pty.foreground_pgid, signal));
                } else if pty.termios.icanon {
                    deliver_input(pty, waiters, b"\n", warned_retained_read_limit)?;
                }
                continue;
            }
        }

        if pty.termios.icanon {
            if byte == pty.termios.cc.veof {
                if pty.line_buffer.is_empty() {
                    deliver_input_eof(pty, waiters);
                } else {
                    let line = pty.line_buffer.clone();
                    deliver_input(pty, waiters, &line, warned_retained_read_limit)?;
                    pty.line_buffer.clear();
                }
                continue;
            }

            if byte == pty.termios.cc.verase || byte == 0x08 {
                if let Some(&erased) = pty.line_buffer.last() {
                    if pty.termios.echo {
                        deliver_output(
                            pty,
                            waiters,
                            &erase_sequence(erased),
                            true,
                            warned_retained_read_limit,
                        )?;
                    }
                    pty.line_buffer.pop();
                }
                continue;
            }

            if byte == pty.termios.cc.vkill {
                if !pty.line_buffer.is_empty() {
                    if pty.termios.echo {
                        let erase: Vec<u8> = pty
                            .line_buffer
                            .iter()
                            .flat_map(|b| erase_sequence(*b))
                            .collect();
                        deliver_output(pty, waiters, &erase, true, warned_retained_read_limit)?;
                    }
                    pty.line_buffer.clear();
                }
                continue;
            }

            if byte == pty.termios.cc.vwerase {
                let mut erased: Vec<u8> = Vec::new();
                while matches!(pty.line_buffer.last(), Some(b' ') | Some(b'\t')) {
                    if let Some(b) = pty.line_buffer.pop() {
                        erased.push(b);
                    }
                }
                while let Some(&b) = pty.line_buffer.last() {
                    if b == b' ' || b == b'\t' {
                        break;
                    }
                    pty.line_buffer.pop();
                    erased.push(b);
                }
                if pty.termios.echo && !erased.is_empty() {
                    let sequence: Vec<u8> =
                        erased.iter().flat_map(|b| erase_sequence(*b)).collect();
                    deliver_output(pty, waiters, &sequence, true, warned_retained_read_limit)?;
                }
                continue;
            }

            if byte == b'\n' {
                let mut line = pty.line_buffer.clone();
                line.push(b'\n');
                if pty.termios.echo {
                    deliver_output(pty, waiters, b"\r\n", true, warned_retained_read_limit)?;
                }
                deliver_input(pty, waiters, &line, warned_retained_read_limit)?;
                pty.line_buffer.clear();
                continue;
            }

            if pty.line_buffer.len() >= MAX_CANON {
                continue;
            }
            if pty.termios.echo {
                // ECHOCTL: echo control chars in caret form (e.g. 0x01 -> "^A")
                // so they are visible; printable bytes echo verbatim.
                deliver_output(
                    pty,
                    waiters,
                    &echo_control_byte(byte),
                    true,
                    warned_retained_read_limit,
                )?;
            }
            pty.line_buffer.push(byte);
        } else {
            if pty.termios.echo {
                deliver_output(pty, waiters, &[byte], true, warned_retained_read_limit)?;
            }
            deliver_input(pty, waiters, &[byte], warned_retained_read_limit)?;
        }
    }

    Ok(())
}

fn translate_input(termios: &Termios, data: &[u8]) -> Vec<u8> {
    if !termios.icrnl || !data.contains(&b'\r') {
        return data.to_vec();
    }

    data.iter()
        .map(|byte| if *byte == b'\r' { b'\n' } else { *byte })
        .collect()
}

fn deliver_input(
    pty: &mut PtyState,
    waiters: &mut BTreeMap<u64, PendingRead>,
    data: &[u8],
    warned_retained_read_limit: &mut bool,
) -> PtyResult<()> {
    if let Some(waiter_id) = pty.waiting_input_reads.front().copied() {
        if let Some(waiter) = waiters.get(&waiter_id) {
            let retained_len = data.len().min(waiter.length);
            check_retained_read_capacity(waiters, retained_len, warned_retained_read_limit)?;
            let tail_len = data.len().saturating_sub(waiter.length);
            if tail_len > available_capacity(&pty.input_buffer) {
                return Err(PtyError::would_block("PTY input buffer full"));
            }
            let split_at = retained_len;
            let (head, tail) = data.split_at(split_at);
            pty.waiting_input_reads.pop_front();
            waiters
                .get_mut(&waiter_id)
                .expect("validated PTY input waiter must remain registered")
                .result = Some(Some(head.to_vec()));
            if !tail.is_empty() {
                pty.input_buffer.push_front(tail.to_vec());
            }
            return Ok(());
        }
        pty.waiting_input_reads.pop_front();
    }

    if buffer_size(&pty.input_buffer).saturating_add(data.len()) > MAX_PTY_BUFFER_BYTES {
        return Err(PtyError::would_block("PTY input buffer full"));
    }

    pty.input_buffer.push_back(data.to_vec());
    Ok(())
}

fn deliver_input_eof(pty: &mut PtyState, waiters: &mut BTreeMap<u64, PendingRead>) {
    if let Some(waiter_id) = pty.waiting_input_reads.pop_front() {
        if let Some(waiter) = waiters.get_mut(&waiter_id) {
            waiter.result = Some(None);
            return;
        }
    }

    pty.input_eof_pending = true;
}

fn deliver_output(
    pty: &mut PtyState,
    waiters: &mut BTreeMap<u64, PendingRead>,
    data: &[u8],
    echo: bool,
    warned_retained_read_limit: &mut bool,
) -> PtyResult<()> {
    if let Some(waiter_id) = pty.waiting_output_reads.front().copied() {
        if let Some(waiter) = waiters.get(&waiter_id) {
            let retained_len = data.len().min(waiter.length);
            check_retained_read_capacity(waiters, retained_len, warned_retained_read_limit)?;
            let tail_len = data.len().saturating_sub(waiter.length);
            if tail_len > available_capacity(&pty.output_buffer) {
                let message = if echo {
                    "PTY output buffer full (echo backpressure)"
                } else {
                    "PTY output buffer full"
                };
                return Err(PtyError::would_block(message));
            }
            let (head, tail) = data.split_at(retained_len);
            pty.waiting_output_reads.pop_front();
            waiters
                .get_mut(&waiter_id)
                .expect("validated PTY output waiter must remain registered")
                .result = Some(Some(head.to_vec()));
            if !tail.is_empty() {
                pty.output_buffer.push_front(tail.to_vec());
            }
            return Ok(());
        }
        pty.waiting_output_reads.pop_front();
    }

    if buffer_size(&pty.output_buffer).saturating_add(data.len()) > MAX_PTY_BUFFER_BYTES {
        let message = if echo {
            "PTY output buffer full (echo backpressure)"
        } else {
            "PTY output buffer full"
        };
        return Err(PtyError::would_block(message));
    }

    pty.output_buffer.push_back(data.to_vec());
    Ok(())
}

fn check_retained_read_capacity(
    waiters: &BTreeMap<u64, PendingRead>,
    additional: usize,
    warned_near_limit: &mut bool,
) -> PtyResult<()> {
    let current = retained_read_bytes(waiters);
    let observed = current
        .checked_add(additional)
        .ok_or_else(|| PtyError::would_block("maxPtyRetainedReadBytes accounting overflowed"))?;
    if observed > MAX_PTY_RETAINED_READ_BYTES {
        return Err(PtyError::would_block(format!(
            "maxPtyRetainedReadBytes limit is {MAX_PTY_RETAINED_READ_BYTES}, retaining this result would use {observed} bytes; retry after another terminal reader consumes its result"
        )));
    }
    warn_near_pty_limit(
        warned_near_limit,
        "maxPtyRetainedReadBytes",
        observed,
        MAX_PTY_RETAINED_READ_BYTES,
    );
    Ok(())
}

fn retained_read_bytes(waiters: &BTreeMap<u64, PendingRead>) -> usize {
    waiters
        .values()
        .filter_map(|waiter| waiter.result.as_ref()?.as_ref())
        .map(Vec::len)
        .fold(0usize, usize::saturating_add)
}

fn retained_read_bytes_for_end(waiters: &BTreeMap<u64, PendingRead>, end: PtyEndKind) -> usize {
    waiters
        .values()
        .filter(|waiter| waiter.end == end)
        .filter_map(|waiter| waiter.result.as_ref()?.as_ref())
        .map(Vec::len)
        .fold(0usize, usize::saturating_add)
}

fn warn_near_pty_limit(warned: &mut bool, limit_name: &str, observed: usize, limit: usize) {
    if *warned || observed < limit.saturating_sub(limit / 10) {
        return;
    }
    *warned = true;
    eprintln!(
        "WARN_AGENTOS_RESOURCE_NEAR_LIMIT: resource={limit_name} used={observed} limit={limit}"
    );
}

fn advance_termios_generation(pty: &mut PtyState) -> PtyResult<()> {
    pty.termios_generation = pty
        .termios_generation
        .checked_add(1)
        .ok_or_else(|| PtyError::io("PTY terminal-attribute generation counter exhausted"))?;
    Ok(())
}

fn apply_raw_mode(termios: &mut Termios, enabled: bool) {
    termios.icrnl = !enabled;
    termios.icanon = !enabled;
    termios.echo = !enabled;
    termios.isig = !enabled;
    termios.opost = !enabled;
    termios.onlcr = !enabled;
}

fn release_raw_mode_lease(
    pty: &mut PtyState,
    owner_pid: u32,
    expected_generation: Option<u64>,
) -> PtyResult<bool> {
    let Some(index) = pty.raw_mode_leases.iter().position(|lease| {
        lease.owner_pid == owner_pid
            && expected_generation.is_none_or(|generation| lease.generation == generation)
    }) else {
        return Ok(false);
    };

    let was_top = index + 1 == pty.raw_mode_leases.len();
    let lease = pty.raw_mode_leases.remove(index);
    if !was_top {
        // The newer owner inherited this owner's effective state. If the older
        // owner exits first, splice its restore point into that newer frame so
        // the eventual top-level release still reaches the pre-stack state.
        pty.raw_mode_leases[index].restore_termios = lease.restore_termios;
        return Ok(true);
    }

    // A direct tcsetattr/set-discipline after this lease was applied supersedes
    // it. Do not clobber that newer terminal state during delayed process reap.
    if pty.termios_generation != lease.applied_termios_generation {
        return Ok(true);
    }

    advance_termios_generation(pty)?;
    pty.termios = lease.restore_termios;
    let restored_generation = pty.termios_generation;
    if let Some(previous) = pty.raw_mode_leases.last_mut() {
        // The previous owner is active again after unwinding the top frame.
        // Point its compare-and-restore token at the state just restored.
        previous.applied_termios_generation = restored_generation;
    }
    Ok(true)
}

fn signal_for_byte(termios: &Termios, byte: u8) -> Option<i32> {
    if byte == termios.cc.vintr {
        return Some(SIGINT);
    }
    if byte == termios.cc.vquit {
        return Some(SIGQUIT);
    }
    if byte == termios.cc.vsusp {
        return Some(SIGTSTP);
    }
    None
}

fn echo_control_byte(byte: u8) -> Vec<u8> {
    if byte < 0x20 {
        vec![b'^', byte + 0x40]
    } else if byte == 0x7f {
        b"^?".to_vec()
    } else {
        vec![byte]
    }
}

/// Backspace-erase sequence for a single buffered input byte, accounting for how
/// wide it was echoed: a control char echoed in caret form (`^X`, ECHOCTL)
/// occupies two columns and needs two `BS SP BS` triples, while a printable byte
/// occupies one. Used by VERASE / VKILL / VWERASE erase echo so the erased echo
/// width matches the displayed width.
fn erase_sequence(byte: u8) -> Vec<u8> {
    let columns = echo_control_byte(byte).len();
    (0..columns).flat_map(|_| [0x08, 0x20, 0x08]).collect()
}

fn buffer_size(buffer: &VecDeque<Vec<u8>>) -> usize {
    buffer.iter().map(Vec::len).sum()
}

fn available_capacity(buffer: &VecDeque<Vec<u8>>) -> usize {
    MAX_PTY_BUFFER_BYTES.saturating_sub(buffer_size(buffer))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_read_is_rejected_before_waiter_registration() {
        let manager = PtyManager::new();
        let pty = manager.create_pty();

        let error = manager
            .read_with_timeout(
                pty.slave.description.id(),
                MAX_PTY_READ_BYTES + 1,
                Some(Duration::from_secs(1)),
            )
            .expect_err("oversized PTY read must fail before waiting");

        assert_eq!(error.code(), "E2BIG");
        assert!(error.to_string().contains("maxPtyReadBytes"));
        assert_eq!(manager.pending_read_waiter_count(), 0);
        assert_eq!(manager.queued_read_waiter_count(), 0);
        assert_eq!(manager.buffered_input_bytes(), 0);
        assert_eq!(manager.buffered_output_bytes(), 0);
    }

    #[test]
    fn oversized_write_is_rejected_before_line_discipline_side_effects() {
        let manager = PtyManager::new();
        let pty = manager.create_pty();

        let error = manager
            .write(
                pty.master.description.id(),
                vec![b'x'; MAX_PTY_WRITE_BYTES + 1],
            )
            .expect_err("oversized PTY write must fail before processing input");

        assert_eq!(error.code(), "E2BIG");
        assert!(error.to_string().contains("maxPtyWriteBytes"));
        let state = lock_or_recover(&manager.inner.state);
        let pty_ref = state
            .desc_to_pty
            .get(&pty.master.description.id())
            .expect("master must remain registered");
        let pty_state = state.ptys.get(&pty_ref.pty_id).expect("PTY must exist");
        assert!(pty_state.line_buffer.is_empty());
        assert!(pty_state.input_buffer.is_empty());
        assert!(pty_state.output_buffer.is_empty());
        assert!(state.waiters.is_empty());
    }

    #[test]
    fn output_transform_limit_rejects_before_buffering() {
        let manager = PtyManager::new();
        let pty = manager.create_pty();
        let newlines = vec![b'\n'; MAX_PTY_BUFFER_BYTES / 2 + 1];

        let error = manager
            .write(pty.slave.description.id(), newlines)
            .expect_err("ONLCR expansion beyond the PTY cap must fail");

        assert_eq!(error.code(), "E2BIG");
        assert!(error.to_string().contains("maxPtyTransformedOutputBytes"));
        assert_eq!(manager.buffered_output_bytes(), 0);
        let read_error = manager
            .read_with_timeout(pty.master.description.id(), 1, Some(Duration::ZERO))
            .expect_err("failed transformed write must publish no output");
        assert_eq!(read_error.code(), "EAGAIN");
    }

    #[test]
    fn read_waiter_limit_fails_with_named_typed_error() {
        let manager = PtyManager::new();
        let pty = manager.create_pty();
        {
            let mut state = lock_or_recover(&manager.inner.state);
            for id in 1..=MAX_PTY_READ_WAITERS as u64 {
                state.waiters.insert(
                    id,
                    PendingRead {
                        length: 1,
                        end: PtyEndKind::Slave,
                        result: None,
                    },
                );
            }
            state.next_waiter_id = MAX_PTY_READ_WAITERS as u64 + 1;
        }

        let error = manager
            .read_with_timeout(pty.slave.description.id(), 1, Some(Duration::from_secs(1)))
            .expect_err("waiter saturation must fail without registering another waiter");

        assert_eq!(error.code(), "EAGAIN");
        assert!(error.to_string().contains("maxPtyReadWaiters"));
        assert_eq!(manager.pending_read_waiter_count(), MAX_PTY_READ_WAITERS);
        assert_eq!(manager.queued_read_waiter_count(), 0);
    }

    #[test]
    fn retained_read_limit_is_accounted_and_fails_before_delivery() {
        const RETAINED_CHUNK_BYTES: usize = 64 * 1024;
        const RETAINED_CHUNKS: usize = MAX_PTY_RETAINED_READ_BYTES / RETAINED_CHUNK_BYTES;

        let manager = PtyManager::new();
        let pty = manager.create_pty();
        let pending_id = RETAINED_CHUNKS as u64 + 1;
        {
            let mut state = lock_or_recover(&manager.inner.state);
            let pty_id = state
                .desc_to_pty
                .get(&pty.master.description.id())
                .expect("master must be registered")
                .pty_id;
            for id in 1..=RETAINED_CHUNKS as u64 {
                state.waiters.insert(
                    id,
                    PendingRead {
                        length: RETAINED_CHUNK_BYTES,
                        end: PtyEndKind::Master,
                        result: Some(Some(vec![0; RETAINED_CHUNK_BYTES])),
                    },
                );
            }
            state.waiters.insert(
                pending_id,
                PendingRead {
                    length: 1,
                    end: PtyEndKind::Master,
                    result: None,
                },
            );
            state
                .ptys
                .get_mut(&pty_id)
                .expect("PTY must exist")
                .waiting_output_reads
                .push_back(pending_id);
        }

        assert_eq!(manager.buffered_output_bytes(), MAX_PTY_RETAINED_READ_BYTES);
        let error = manager
            .write(pty.slave.description.id(), b"x")
            .expect_err("retained result saturation must block another delivery");

        assert_eq!(error.code(), "EAGAIN");
        assert!(error.to_string().contains("maxPtyRetainedReadBytes"));
        let state = lock_or_recover(&manager.inner.state);
        let pending = state
            .waiters
            .get(&pending_id)
            .expect("blocked waiter must remain registered");
        assert!(pending.result.is_none());
        let pty_ref = state
            .desc_to_pty
            .get(&pty.master.description.id())
            .expect("master must remain registered");
        let pty_state = state.ptys.get(&pty_ref.pty_id).expect("PTY must exist");
        assert_eq!(pty_state.waiting_output_reads.front(), Some(&pending_id));
        assert!(pty_state.output_buffer.is_empty());
    }

    #[test]
    fn zero_timeout_empty_read_does_not_publish_false_readiness() {
        let notifier = PollNotifier::default();
        let manager = PtyManager::with_notifier(notifier.clone());
        let pty = manager.create_pty();
        let observed = notifier.snapshot();

        let error = manager
            .read_with_timeout(pty.slave.description.id(), 1, Some(Duration::ZERO))
            .expect_err("empty nonblocking read must return EAGAIN");

        assert_eq!(error.code(), "EAGAIN");
        assert_eq!(notifier.snapshot(), observed);
        let state = lock_or_recover(&manager.inner.state);
        assert!(state.waiters.is_empty());
        assert_eq!(state.next_waiter_id, 1);
    }
}
