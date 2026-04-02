use crate::fd_table::{
    FdResult, FileDescription, ProcessFdTable, SharedFileDescription, FILETYPE_CHARACTER_DEVICE,
    O_RDWR,
};
use std::collections::{BTreeMap, VecDeque};
use std::error::Error;
use std::fmt;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

pub const MAX_PTY_BUFFER_BYTES: usize = 65_536;
pub const MAX_CANON: usize = 4_096;
pub const SIGINT: i32 = 2;
pub const SIGQUIT: i32 = 3;
pub const SIGTSTP: i32 = 20;

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

    fn bad_file_descriptor(message: impl Into<String>) -> Self {
        Self {
            code: "EBADF",
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
}

impl fmt::Display for PtyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl Error for PtyError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LineDisciplineConfig {
    pub canonical: Option<bool>,
    pub echo: Option<bool>,
    pub isig: Option<bool>,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PartialTermiosControlChars {
    pub vintr: Option<u8>,
    pub vquit: Option<u8>,
    pub vsusp: Option<u8>,
    pub veof: Option<u8>,
    pub verase: Option<u8>,
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

#[derive(Debug, Default)]
struct PendingRead {
    result: Option<Option<Vec<u8>>>,
}

#[derive(Debug, Clone, Default)]
struct PtyState {
    path: String,
    input_buffer: VecDeque<Vec<u8>>,
    output_buffer: VecDeque<Vec<u8>>,
    closed_master: bool,
    closed_slave: bool,
    waiting_input_reads: VecDeque<u64>,
    waiting_output_reads: VecDeque<u64>,
    termios: Termios,
    line_buffer: Vec<u8>,
    foreground_pgid: u32,
}

#[derive(Debug)]
struct PtyManagerState {
    ptys: BTreeMap<u64, PtyState>,
    desc_to_pty: BTreeMap<u64, PtyRef>,
    waiters: BTreeMap<u64, PendingRead>,
    next_pty_id: u64,
    next_desc_id: u64,
    next_waiter_id: u64,
}

impl Default for PtyManagerState {
    fn default() -> Self {
        Self {
            ptys: BTreeMap::new(),
            desc_to_pty: BTreeMap::new(),
            waiters: BTreeMap::new(),
            next_pty_id: 0,
            next_desc_id: 200_000,
            next_waiter_id: 1,
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
}

impl Default for PtyManager {
    fn default() -> Self {
        Self {
            inner: Arc::new(PtyManagerInner {
                state: Mutex::new(PtyManagerState::default()),
                waiters: Condvar::new(),
            }),
            on_signal: None,
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

    pub fn create_pty(&self) -> PtyPair {
        let mut state = lock_or_recover(&self.inner.state);
        let pty_id = state.next_pty_id;
        state.next_pty_id += 1;

        let master_id = state.next_desc_id;
        state.next_desc_id += 1;
        let slave_id = state.next_desc_id;
        state.next_desc_id += 1;

        let path = format!("/dev/pts/{pty_id}");
        state.ptys.insert(
            pty_id,
            PtyState {
                path: path.clone(),
                termios: Termios::default(),
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

    pub fn write(&self, description_id: u64, data: impl AsRef<[u8]>) -> PtyResult<usize> {
        let payload = data.as_ref();
        let mut signals = Vec::new();

        {
            let mut state = lock_or_recover(&self.inner.state);
            let pty_ref = state
                .desc_to_pty
                .get(&description_id)
                .copied()
                .ok_or_else(|| PtyError::bad_file_descriptor("not a PTY end"))?;
            let PtyManagerState { ptys, waiters, .. } = &mut *state;
            let pty = ptys
                .get_mut(&pty_ref.pty_id)
                .ok_or_else(|| PtyError::bad_file_descriptor("PTY not found"))?;

            match pty_ref.end {
                PtyEndKind::Master => {
                    if pty.closed_master {
                        return Err(PtyError::io("master closed"));
                    }
                    if pty.closed_slave {
                        return Err(PtyError::io("slave closed"));
                    }
                    process_input(pty, waiters, payload, &mut signals)?;
                }
                PtyEndKind::Slave => {
                    if pty.closed_slave {
                        return Err(PtyError::io("slave closed"));
                    }
                    if pty.closed_master {
                        return Err(PtyError::io("master closed"));
                    }

                    let processed = process_output(&pty.termios, payload);
                    deliver_output(pty, waiters, &processed, false)?;
                }
            }
        }

        self.inner.waiters.notify_all();
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
        let mut state = lock_or_recover(&self.inner.state);
        let pty_ref = state
            .desc_to_pty
            .get(&description_id)
            .copied()
            .ok_or_else(|| PtyError::bad_file_descriptor("not a PTY end"))?;
        let mut waiter_id = None;

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
                let pty = state
                    .ptys
                    .get_mut(&pty_ref.pty_id)
                    .ok_or_else(|| PtyError::bad_file_descriptor("PTY not found"))?;

                match pty_ref.end {
                    PtyEndKind::Master => {
                        if pty.closed_master {
                            if let Some(id) = waiter_id {
                                state.waiters.remove(&id);
                            }
                            return Err(PtyError::io("master closed"));
                        }

                        if !pty.output_buffer.is_empty() {
                            return Ok(Some(drain_buffer(&mut pty.output_buffer, length)));
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
                            return Ok(Some(drain_buffer(&mut pty.input_buffer, length)));
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

            let id = if let Some(id) = waiter_id {
                id
            } else {
                let next = state.next_waiter_id;
                state.next_waiter_id += 1;
                state.waiters.insert(next, PendingRead::default());
                let Some(pty) = state.ptys.get_mut(&pty_ref.pty_id) else {
                    state.waiters.remove(&next);
                    return Err(PtyError::bad_file_descriptor("PTY not found"));
                };
                match pty_ref.end {
                    PtyEndKind::Master => pty.waiting_output_reads.push_back(next),
                    PtyEndKind::Slave => pty.waiting_input_reads.push_back(next),
                }
                waiter_id = Some(next);
                next
            };

            state = wait_or_recover(&self.inner.waiters, state);

            if !state.waiters.contains_key(&id) {
                waiter_id = None;
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
        self.inner.waiters.notify_all();
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
            .ok_or_else(|| PtyError::bad_file_descriptor("not a PTY end"))?;
        let pty = state
            .ptys
            .get_mut(&pty_ref.pty_id)
            .ok_or_else(|| PtyError::bad_file_descriptor("PTY not found"))?;
        if let Some(canonical) = config.canonical {
            pty.termios.icanon = canonical;
        }
        if let Some(echo) = config.echo {
            pty.termios.echo = echo;
        }
        if let Some(isig) = config.isig {
            pty.termios.isig = isig;
        }
        Ok(())
    }

    pub fn get_termios(&self, description_id: u64) -> PtyResult<Termios> {
        let state = lock_or_recover(&self.inner.state);
        let pty_ref = state
            .desc_to_pty
            .get(&description_id)
            .copied()
            .ok_or_else(|| PtyError::bad_file_descriptor("not a PTY end"))?;
        state
            .ptys
            .get(&pty_ref.pty_id)
            .cloned()
            .map(|pty| pty.termios)
            .ok_or_else(|| PtyError::bad_file_descriptor("PTY not found"))
    }

    pub fn set_termios(&self, description_id: u64, termios: PartialTermios) -> PtyResult<()> {
        let mut state = lock_or_recover(&self.inner.state);
        let pty_ref = state
            .desc_to_pty
            .get(&description_id)
            .copied()
            .ok_or_else(|| PtyError::bad_file_descriptor("not a PTY end"))?;
        let pty = state
            .ptys
            .get_mut(&pty_ref.pty_id)
            .ok_or_else(|| PtyError::bad_file_descriptor("PTY not found"))?;
        pty.termios.merge(termios);
        Ok(())
    }

    pub fn set_foreground_pgid(&self, description_id: u64, pgid: u32) -> PtyResult<()> {
        let mut state = lock_or_recover(&self.inner.state);
        let pty_ref = state
            .desc_to_pty
            .get(&description_id)
            .copied()
            .ok_or_else(|| PtyError::bad_file_descriptor("not a PTY end"))?;
        let pty = state
            .ptys
            .get_mut(&pty_ref.pty_id)
            .ok_or_else(|| PtyError::bad_file_descriptor("PTY not found"))?;
        pty.foreground_pgid = pgid;
        Ok(())
    }

    pub fn get_foreground_pgid(&self, description_id: u64) -> PtyResult<u32> {
        let state = lock_or_recover(&self.inner.state);
        let pty_ref = state
            .desc_to_pty
            .get(&description_id)
            .copied()
            .ok_or_else(|| PtyError::bad_file_descriptor("not a PTY end"))?;
        state
            .ptys
            .get(&pty_ref.pty_id)
            .map(|pty| pty.foreground_pgid)
            .ok_or_else(|| PtyError::bad_file_descriptor("PTY not found"))
    }

    pub fn pty_count(&self) -> usize {
        lock_or_recover(&self.inner.state).ptys.len()
    }

    pub fn buffered_input_bytes(&self) -> usize {
        lock_or_recover(&self.inner.state)
            .ptys
            .values()
            .map(|pty| buffer_size(&pty.input_buffer))
            .sum()
    }

    pub fn buffered_output_bytes(&self) -> usize {
        lock_or_recover(&self.inner.state)
            .ptys
            .values()
            .map(|pty| buffer_size(&pty.output_buffer))
            .sum()
    }

    pub fn path_for(&self, description_id: u64) -> Option<String> {
        let state = lock_or_recover(&self.inner.state);
        let pty_ref = state.desc_to_pty.get(&description_id)?;
        state.ptys.get(&pty_ref.pty_id).map(|pty| pty.path.clone())
    }
}

fn process_output(termios: &Termios, data: &[u8]) -> Vec<u8> {
    if !termios.opost || !termios.onlcr || !data.contains(&b'\n') {
        return data.to_vec();
    }

    let extra_crs = data
        .iter()
        .enumerate()
        .filter(|(index, byte)| **byte == b'\n' && (*index == 0 || data[*index - 1] != b'\r'))
        .count();
    if extra_crs == 0 {
        return data.to_vec();
    }

    let mut result = Vec::with_capacity(data.len() + extra_crs);
    for (index, byte) in data.iter().enumerate() {
        if *byte == b'\n' && (index == 0 || data[index - 1] != b'\r') {
            result.push(b'\r');
        }
        result.push(*byte);
    }
    result
}

fn process_input(
    pty: &mut PtyState,
    waiters: &mut BTreeMap<u64, PendingRead>,
    data: &[u8],
    signals: &mut Vec<(u32, i32)>,
) -> PtyResult<()> {
    if !pty.termios.icanon && !pty.termios.echo && !pty.termios.isig {
        let translated = translate_input(&pty.termios, data);
        deliver_input(pty, waiters, &translated)?;
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
                if pty.foreground_pgid > 0 {
                    signals.push((pty.foreground_pgid, signal));
                }
                continue;
            }
        }

        if pty.termios.icanon {
            if byte == pty.termios.cc.veof {
                if pty.line_buffer.is_empty() {
                    deliver_input(pty, waiters, &[])?;
                } else {
                    let line = pty.line_buffer.clone();
                    deliver_input(pty, waiters, &line)?;
                    pty.line_buffer.clear();
                }
                continue;
            }

            if byte == pty.termios.cc.verase || byte == 0x08 {
                if !pty.line_buffer.is_empty() {
                    pty.line_buffer.pop();
                    if pty.termios.echo {
                        deliver_output(pty, waiters, &[0x08, 0x20, 0x08], true)?;
                    }
                }
                continue;
            }

            if byte == b'\n' {
                pty.line_buffer.push(b'\n');
                if pty.termios.echo {
                    deliver_output(pty, waiters, &[b'\r', b'\n'], true)?;
                }
                let line = pty.line_buffer.clone();
                deliver_input(pty, waiters, &line)?;
                pty.line_buffer.clear();
                continue;
            }

            if pty.line_buffer.len() >= MAX_CANON {
                continue;
            }
            pty.line_buffer.push(byte);
            if pty.termios.echo {
                deliver_output(pty, waiters, &[byte], true)?;
            }
        } else {
            if pty.termios.echo {
                deliver_output(pty, waiters, &[byte], true)?;
            }
            deliver_input(pty, waiters, &[byte])?;
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
) -> PtyResult<()> {
    if let Some(waiter_id) = pty.waiting_input_reads.pop_front() {
        if let Some(waiter) = waiters.get_mut(&waiter_id) {
            waiter.result = Some(Some(data.to_vec()));
            return Ok(());
        }
    }

    if buffer_size(&pty.input_buffer).saturating_add(data.len()) > MAX_PTY_BUFFER_BYTES {
        return Err(PtyError::would_block("PTY input buffer full"));
    }

    pty.input_buffer.push_back(data.to_vec());
    Ok(())
}

fn deliver_output(
    pty: &mut PtyState,
    waiters: &mut BTreeMap<u64, PendingRead>,
    data: &[u8],
    echo: bool,
) -> PtyResult<()> {
    if let Some(waiter_id) = pty.waiting_output_reads.pop_front() {
        if let Some(waiter) = waiters.get_mut(&waiter_id) {
            waiter.result = Some(Some(data.to_vec()));
            return Ok(());
        }
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

fn buffer_size(buffer: &VecDeque<Vec<u8>>) -> usize {
    buffer.iter().map(Vec::len).sum()
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
