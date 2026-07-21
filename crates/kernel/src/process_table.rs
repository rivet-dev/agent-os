use crate::process_runtime::{
    ProcessControlAckSink, ProcessControlRequest, ProcessExit, ProcessExitSink,
    ProcessRuntimeEndpoint, ProcessRuntimeEndpointError, ProcessRuntimeFault,
    ProcessRuntimeIdentity, ProcessTermination,
};
use crate::user::ProcessIdentity;
use event_listener::Event;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::error::Error;
use std::fmt;
use std::ops::{BitOr, BitOrAssign};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;
use web_time::{Instant, SystemTime, UNIX_EPOCH};

const ZOMBIE_TTL: Duration = Duration::from_secs(60);
const INIT_PID: u32 = 1;
const MAX_ALLOCATED_PID: u32 = i32::MAX as u32;
pub const DEFAULT_PROCESS_UMASK: u32 = 0o022;
pub const SIGHUP: i32 = 1;
pub const SIGCHLD: i32 = 17;
pub const SIGCONT: i32 = 18;
pub const SIGSTOP: i32 = 19;
pub const SIGTSTP: i32 = 20;
pub const SIGTERM: i32 = 15;
pub const SIGKILL: i32 = 9;
pub const SIGPIPE: i32 = 13;
pub const SIGWINCH: i32 = 28;
const MAX_SIGNAL: i32 = 64;
const MAX_SIGNAL_HANDLER_DEPTH: usize = 64;
const MAX_SIGNAL_THREADS_PER_PROCESS: usize = 1024;
pub const MAIN_SIGNAL_THREAD_ID: u32 = 0;
const SIGTTIN: i32 = 21;
const SIGTTOU: i32 = 22;
const SIGURG: i32 = 23;

pub const SA_RESTART: u32 = 0x1000_0000;
pub const SA_NODEFER: u32 = 0x4000_0000;
pub const SA_RESETHAND: u32 = 0x8000_0000;

pub type ProcessResult<T> = Result<T, ProcessTableError>;

/// Runtime-neutral capability tier attached to one kernel process.
///
/// Protocol and executor-specific tier enums are converted to this type once,
/// before registration. Host operations read this kernel-owned, monotonically
/// restricted state; a guest request can never select or raise its authority.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProcessPermissionTier {
    Isolated,
    ReadOnly,
    ReadWrite,
    #[default]
    Full,
}

impl ProcessPermissionTier {
    /// Apply a requested child-process ceiling without allowing an inherited
    /// process to regain authority.
    pub fn restrict(self, requested: Self) -> Self {
        self.min(requested)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessTableError {
    code: &'static str,
    message: String,
}

impl ProcessTableError {
    pub fn code(&self) -> &'static str {
        self.code
    }

    fn invalid_signal(signal: i32) -> Self {
        Self {
            code: "EINVAL",
            message: format!("invalid signal {signal}"),
        }
    }

    fn no_such_process(pid: u32) -> Self {
        Self {
            code: "ESRCH",
            message: format!("no such process {pid}"),
        }
    }

    fn no_such_process_group(pgid: u32) -> Self {
        Self {
            code: "ESRCH",
            message: format!("no such process group {pgid}"),
        }
    }

    fn no_matching_child(waiter_pid: u32, pid: i32) -> Self {
        Self {
            code: "ECHILD",
            message: format!("process {waiter_pid} has no matching child for waitpid({pid})"),
        }
    }

    fn pid_space_exhausted() -> Self {
        Self {
            code: "EAGAIN",
            message: String::from("process id space exhausted"),
        }
    }

    fn permission_denied(message: impl Into<String>) -> Self {
        Self {
            code: "EPERM",
            message: message.into(),
        }
    }

    fn invalid_argument(message: impl Into<String>) -> Self {
        Self {
            code: "EINVAL",
            message: message.into(),
        }
    }

    fn interrupted(message: impl Into<String>) -> Self {
        Self {
            code: "EINTR",
            message: message.into(),
        }
    }

    fn signal_delivery_depth_exceeded(pid: u32) -> Self {
        Self {
            code: "EAGAIN",
            message: format!(
                "process {pid} exceeded {MAX_SIGNAL_HANDLER_DEPTH} nested signal handlers"
            ),
        }
    }

    fn invalid_signal_delivery_token(pid: u32, token: u64) -> Self {
        Self {
            code: "EINVAL",
            message: format!("invalid signal delivery token {token} for process {pid}"),
        }
    }

    fn stale_runtime_identity(expected: ProcessRuntimeIdentity) -> Self {
        Self {
            code: "ESTALE",
            message: format!(
                "runtime reporter for VM generation {} pid {} no longer owns that process",
                expected.generation, expected.pid
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProcessResourceLimitKind {
    AddressSpace,
    Core,
    Cpu,
    Data,
    FileSize,
    LockedMemory,
    OpenFiles,
    Processes,
    ResidentSet,
    Stack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProcessResourceLimit {
    /// `None` is Linux `RLIM_INFINITY`.
    pub soft: Option<u64>,
    /// `None` is Linux `RLIM_INFINITY`.
    pub hard: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProcessResourceLimits {
    values: BTreeMap<ProcessResourceLimitKind, ProcessResourceLimit>,
}

impl ProcessResourceLimits {
    pub fn with_open_files(limit: u64) -> Self {
        let mut limits = Self::default();
        limits.values.insert(
            ProcessResourceLimitKind::OpenFiles,
            ProcessResourceLimit {
                soft: Some(limit),
                hard: Some(limit),
            },
        );
        limits
    }

    pub fn get(&self, kind: ProcessResourceLimitKind) -> ProcessResourceLimit {
        self.values.get(&kind).copied().unwrap_or_default()
    }

    fn set(&mut self, kind: ProcessResourceLimitKind, value: ProcessResourceLimit) {
        if value == ProcessResourceLimit::default() {
            self.values.remove(&kind);
        } else {
            self.values.insert(kind, value);
        }
    }
}

impl fmt::Display for ProcessTableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl Error for ProcessTableError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessStatus {
    Running,
    Stopped,
    Exited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SignalSet {
    bits: u64,
}

impl SignalSet {
    pub const fn empty() -> Self {
        Self { bits: 0 }
    }

    pub const fn is_empty(self) -> bool {
        self.bits == 0
    }

    pub fn from_signal(signal: i32) -> ProcessResult<Self> {
        Ok(Self {
            bits: signal_bit(signal)?,
        })
    }

    pub fn from_signals(signals: impl IntoIterator<Item = i32>) -> ProcessResult<Self> {
        let mut set = Self::empty();
        for signal in signals {
            set.insert(signal)?;
        }
        Ok(set)
    }

    pub fn contains(self, signal: i32) -> bool {
        signal_bit(signal)
            .map(|bit| self.bits & bit != 0)
            .unwrap_or(false)
    }

    pub fn insert(&mut self, signal: i32) -> ProcessResult<()> {
        self.bits |= signal_bit(signal)?;
        Ok(())
    }

    pub fn remove(&mut self, signal: i32) -> ProcessResult<()> {
        self.bits &= !signal_bit(signal)?;
        Ok(())
    }

    pub fn union(self, other: Self) -> Self {
        Self {
            bits: self.bits | other.bits,
        }
    }

    pub fn difference(self, other: Self) -> Self {
        Self {
            bits: self.bits & !other.bits,
        }
    }

    pub fn signals(self) -> Vec<i32> {
        let mut signals = Vec::new();
        for signal in 1..=MAX_SIGNAL {
            if self.contains(signal) {
                signals.push(signal);
            }
        }
        signals
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigmaskHow {
    Block,
    Unblock,
    SetMask,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SignalDisposition {
    #[default]
    Default,
    Ignore,
    User,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SignalAction {
    pub disposition: SignalDisposition,
    pub mask: SignalSet,
    pub flags: u32,
}

impl SignalAction {
    pub const DEFAULT: Self = Self {
        disposition: SignalDisposition::Default,
        mask: SignalSet::empty(),
        flags: 0,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignalDelivery {
    pub token: u64,
    pub signal: i32,
    pub action: SignalAction,
    /// Kernel signal-thread record selected for this process-directed signal.
    pub thread_id: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InProgressSignalDelivery {
    token: u64,
    previous_mask: SignalSet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TemporarySignalMask {
    token: u64,
    previous_mask: SignalSet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitPidFlags {
    bits: u32,
}

impl WaitPidFlags {
    pub const WNOHANG: Self = Self { bits: 1 << 0 };
    pub const WUNTRACED: Self = Self { bits: 1 << 1 };
    pub const WCONTINUED: Self = Self { bits: 1 << 2 };

    pub const fn empty() -> Self {
        Self { bits: 0 }
    }

    pub const fn contains(self, other: Self) -> bool {
        (self.bits & other.bits) == other.bits
    }
}

impl Default for WaitPidFlags {
    fn default() -> Self {
        Self::empty()
    }
}

impl BitOr for WaitPidFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self {
            bits: self.bits | rhs.bits,
        }
    }
}

impl BitOrAssign for WaitPidFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.bits |= rhs.bits;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessWaitEvent {
    Exited,
    Stopped,
    Continued,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessWaitResult {
    pub pid: u32,
    pub status: i32,
    pub event: ProcessWaitEvent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessWaitTransition {
    pub result: ProcessWaitResult,
    pub termination: Option<ProcessExit>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessFileDescriptors {
    pub stdin: u32,
    pub stdout: u32,
    pub stderr: u32,
}

impl Default for ProcessFileDescriptors {
    fn default() -> Self {
        Self {
            stdin: 0,
            stdout: 1,
            stderr: 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessContext {
    pub pid: u32,
    pub ppid: u32,
    pub env: BTreeMap<String, String>,
    pub cwd: String,
    pub umask: u32,
    pub fds: ProcessFileDescriptors,
    pub identity: ProcessIdentity,
    pub blocked_signals: SignalSet,
    pub pending_signals: SignalSet,
    pub resource_limits: ProcessResourceLimits,
    pub permission_tier: ProcessPermissionTier,
}

impl Default for ProcessContext {
    fn default() -> Self {
        Self {
            pid: 0,
            ppid: 0,
            env: BTreeMap::new(),
            cwd: String::from("/"),
            umask: DEFAULT_PROCESS_UMASK,
            fds: ProcessFileDescriptors::default(),
            identity: ProcessIdentity::default(),
            blocked_signals: SignalSet::empty(),
            pending_signals: SignalSet::empty(),
            resource_limits: ProcessResourceLimits::default(),
            permission_tier: ProcessPermissionTier::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessEntry {
    pub pid: u32,
    pub ppid: u32,
    pub pgid: u32,
    pub sid: u32,
    pub driver: String,
    pub command: String,
    pub args: Vec<String>,
    pub status: ProcessStatus,
    pub exit_code: Option<i32>,
    pub pending_termination: Option<ProcessTermination>,
    pub termination: Option<ProcessExit>,
    pub runtime_fault: Option<ProcessRuntimeFault>,
    pub exit_time_ms: Option<u64>,
    pub env: BTreeMap<String, String>,
    pub cwd: String,
    pub umask: u32,
    pub identity: ProcessIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInfo {
    pub pid: u32,
    pub ppid: u32,
    pub pgid: u32,
    pub sid: u32,
    pub driver: String,
    pub command: String,
    pub status: ProcessStatus,
    pub exit_code: Option<i32>,
    pub pending_termination: Option<ProcessTermination>,
    pub termination: Option<ProcessExit>,
    pub runtime_fault: Option<ProcessRuntimeFault>,
    pub identity: ProcessIdentity,
}

#[derive(Clone)]
pub struct ProcessTable {
    inner: Arc<ProcessTableInner>,
}

struct ProcessTableInner {
    state: Mutex<ProcessTableState>,
    waiters: Condvar,
    wait_generation: Mutex<u64>,
    async_waiters: Event,
    reaper: Arc<ZombieReaper>,
}

/// Cloneable async notification capability for process-table wait state.
///
/// Callers snapshot before probing `waitpid(..., WNOHANG)`, then await a
/// generation change off the kernel-owning thread. The process table remains
/// the source of truth; a wake only authorizes another nonblocking probe.
#[derive(Clone)]
pub struct ProcessWaitHandle {
    inner: Arc<ProcessTableInner>,
}

impl fmt::Debug for ProcessWaitHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProcessWaitHandle").finish_non_exhaustive()
    }
}

impl ProcessWaitHandle {
    pub fn snapshot(&self) -> u64 {
        *lock_or_recover(&self.inner.wait_generation)
    }

    pub async fn wait_for_change_async(&self, observed: u64) {
        loop {
            let listener = self.inner.async_waiters.listen();
            if self.snapshot() != observed {
                return;
            }
            listener.await;
            if self.snapshot() != observed {
                return;
            }
        }
    }
}

struct ProcessRecord {
    entry: ProcessEntry,
    runtime_endpoint: Arc<dyn ProcessRuntimeEndpoint>,
    pending_wait_events: VecDeque<PendingWaitEvent>,
    pending_signals: SignalSet,
    signal_actions: [SignalAction; MAX_SIGNAL as usize],
    signal_threads: BTreeMap<u32, ProcessThreadSignalState>,
    next_signal_delivery_token: u64,
    next_signal_mask_token: u64,
    resource_limits: ProcessResourceLimits,
    permission_tier: ProcessPermissionTier,
}

#[derive(Debug, Clone)]
struct ProcessThreadSignalState {
    blocked_signals: SignalSet,
    signal_deliveries: Vec<InProgressSignalDelivery>,
    temporary_signal_masks: Vec<TemporarySignalMask>,
}

impl ProcessThreadSignalState {
    fn new(blocked_signals: SignalSet) -> Self {
        Self {
            blocked_signals,
            signal_deliveries: Vec::new(),
            temporary_signal_masks: Vec::new(),
        }
    }
}

struct ScheduledSignalDelivery {
    pid: u32,
    signal: i32,
    runtime_endpoint: Arc<dyn ProcessRuntimeEndpoint>,
    controls: Vec<ProcessControlRequest>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingWaitEvent {
    status: i32,
    event: ProcessWaitEvent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WaitSelector {
    AnyChild,
    ChildPid(u32),
    ProcessGroup(u32),
}

struct ZombieReaper {
    state: Mutex<ZombieReaperState>,
}

#[derive(Default)]
struct ZombieReaperState {
    deadlines: BTreeMap<u32, Instant>,
}

struct ProcessTableState {
    entries: BTreeMap<u32, ProcessRecord>,
    next_pid: u32,
    zombie_ttl: Duration,
    on_process_exit: Option<Arc<dyn Fn(u32) + Send + Sync + 'static>>,
    terminating_all: bool,
}

impl Default for ProcessTableState {
    fn default() -> Self {
        Self {
            entries: BTreeMap::new(),
            next_pid: 1,
            zombie_ttl: ZOMBIE_TTL,
            on_process_exit: None,
            terminating_all: false,
        }
    }
}

impl Default for ProcessTable {
    fn default() -> Self {
        let reaper = Arc::new(ZombieReaper::default());
        Self {
            inner: Arc::new(ProcessTableInner {
                state: Mutex::new(ProcessTableState::default()),
                waiters: Condvar::new(),
                wait_generation: Mutex::new(0),
                async_waiters: Event::new(),
                reaper,
            }),
        }
    }
}

impl ProcessTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_zombie_ttl(zombie_ttl: Duration) -> Self {
        let table = Self::new();
        table.inner.lock_state().zombie_ttl = zombie_ttl;
        table
    }

    pub fn wait_handle(&self) -> ProcessWaitHandle {
        ProcessWaitHandle {
            inner: Arc::clone(&self.inner),
        }
    }

    pub fn allocate_pid(&self) -> ProcessResult<u32> {
        let mut state = self.inner.lock_state();
        let start = normalize_next_pid(state.next_pid);
        let mut pid = start;

        loop {
            if !state.entries.contains_key(&pid) {
                state.next_pid = next_allocated_pid_after(pid);
                return Ok(pid);
            }

            pid = next_allocated_pid_after(pid);
            if pid == start {
                return Err(ProcessTableError::pid_space_exhausted());
            }
        }
    }

    pub fn set_on_process_exit(&self, callback: Option<Arc<dyn Fn(u32) + Send + Sync + 'static>>) {
        self.inner.lock_state().on_process_exit = callback;
    }

    pub fn register(
        &self,
        pid: u32,
        driver: impl Into<String>,
        command: impl Into<String>,
        args: Vec<String>,
        ctx: ProcessContext,
        runtime_endpoint: Arc<dyn ProcessRuntimeEndpoint>,
    ) -> ProcessEntry {
        self.register_with_process_group(pid, driver, command, args, ctx, runtime_endpoint, None)
            .expect("inheriting a process group cannot fail")
    }

    // Registration keeps the process image, context, driver, and requested
    // group explicit so ownership validation happens at one boundary.
    #[allow(clippy::too_many_arguments)]
    pub fn register_with_process_group(
        &self,
        pid: u32,
        driver: impl Into<String>,
        command: impl Into<String>,
        args: Vec<String>,
        ctx: ProcessContext,
        runtime_endpoint: Arc<dyn ProcessRuntimeEndpoint>,
        requested_pgid: Option<u32>,
    ) -> ProcessResult<ProcessEntry> {
        let driver = driver.into();
        let command = command.into();
        let mut state = self.inner.lock_state();
        let (inherited_pgid, sid, inherited_signal_actions) = match state.entries.get(&ctx.ppid) {
            Some(parent) => {
                let mut actions = [SignalAction::DEFAULT; MAX_SIGNAL as usize];
                for (target, source) in actions.iter_mut().zip(parent.signal_actions) {
                    if source.disposition == SignalDisposition::Ignore {
                        *target = source;
                    }
                }
                (parent.entry.pgid, parent.entry.sid, actions)
            }
            None => (pid, pid, [SignalAction::DEFAULT; MAX_SIGNAL as usize]),
        };
        let pgid = requested_pgid.map_or(inherited_pgid, |pgid| if pgid == 0 { pid } else { pgid });
        if requested_pgid.is_some() && pgid != pid {
            let mut group_exists = false;
            for record in state.entries.values() {
                if record.entry.pgid != pgid || record.entry.status == ProcessStatus::Exited {
                    continue;
                }
                if record.entry.sid != sid {
                    return Err(ProcessTableError::permission_denied(
                        "cannot join process group in different session",
                    ));
                }
                group_exists = true;
                break;
            }
            if !group_exists {
                return Err(ProcessTableError::permission_denied(format!(
                    "no such process group {pgid}"
                )));
            }
        }

        let entry = ProcessEntry {
            pid,
            ppid: ctx.ppid,
            pgid,
            sid,
            driver,
            command,
            args,
            status: ProcessStatus::Running,
            exit_code: None,
            pending_termination: None,
            termination: None,
            runtime_fault: None,
            exit_time_ms: None,
            env: ctx.env,
            cwd: ctx.cwd,
            umask: ctx.umask & 0o777,
            identity: ctx.identity,
        };

        state.next_pid = next_pid_after_registered(state.next_pid, pid);
        state.entries.insert(
            pid,
            ProcessRecord {
                entry: entry.clone(),
                runtime_endpoint,
                pending_wait_events: VecDeque::new(),
                pending_signals: ctx.pending_signals,
                signal_actions: inherited_signal_actions,
                signal_threads: BTreeMap::from([(
                    MAIN_SIGNAL_THREAD_ID,
                    ProcessThreadSignalState::new(ctx.blocked_signals),
                )]),
                next_signal_delivery_token: 1,
                next_signal_mask_token: 1,
                resource_limits: ctx.resource_limits,
                permission_tier: ctx.permission_tier,
            },
        );
        Ok(entry)
    }

    pub fn get(&self, pid: u32) -> Option<ProcessEntry> {
        self.reap_due_zombies();
        self.inner
            .lock_state()
            .entries
            .get(&pid)
            .map(|record| record.entry.clone())
    }

    pub fn set_identity(&self, pid: u32, identity: ProcessIdentity) -> ProcessResult<()> {
        let mut state = self.inner.lock_state();
        let record = state
            .entries
            .get_mut(&pid)
            .ok_or_else(|| ProcessTableError::no_such_process(pid))?;
        record.entry.identity = identity;
        Ok(())
    }

    pub fn inherited_context(&self, parent_pid: u32) -> ProcessResult<ProcessContext> {
        let state = self.inner.lock_state();
        let parent = state
            .entries
            .get(&parent_pid)
            .ok_or_else(|| ProcessTableError::no_such_process(parent_pid))?;
        Ok(ProcessContext {
            pid: 0,
            ppid: parent_pid,
            env: parent.entry.env.clone(),
            cwd: parent.entry.cwd.clone(),
            umask: parent.entry.umask,
            fds: ProcessFileDescriptors::default(),
            identity: parent.entry.identity.clone(),
            blocked_signals: parent
                .signal_threads
                .get(&MAIN_SIGNAL_THREAD_ID)
                .map(|thread| thread.blocked_signals)
                .unwrap_or_else(SignalSet::empty),
            pending_signals: SignalSet::empty(),
            resource_limits: parent.resource_limits.clone(),
            permission_tier: parent.permission_tier,
        })
    }

    pub fn permission_tier(&self, pid: u32) -> ProcessResult<ProcessPermissionTier> {
        let state = self.inner.lock_state();
        let record = state
            .entries
            .get(&pid)
            .ok_or_else(|| ProcessTableError::no_such_process(pid))?;
        Ok(record.permission_tier)
    }

    pub fn get_resource_limit(
        &self,
        pid: u32,
        kind: ProcessResourceLimitKind,
    ) -> ProcessResult<ProcessResourceLimit> {
        let state = self.inner.lock_state();
        let record = state
            .entries
            .get(&pid)
            .ok_or_else(|| ProcessTableError::no_such_process(pid))?;
        Ok(record.resource_limits.get(kind))
    }

    pub fn set_resource_limit(
        &self,
        pid: u32,
        kind: ProcessResourceLimitKind,
        value: ProcessResourceLimit,
    ) -> ProcessResult<()> {
        if matches!((value.soft, value.hard), (Some(soft), Some(hard)) if soft > hard) {
            return Err(ProcessTableError::invalid_argument(
                "resource-limit soft value exceeds hard value",
            ));
        }

        let mut state = self.inner.lock_state();
        let record = state
            .entries
            .get_mut(&pid)
            .ok_or_else(|| ProcessTableError::no_such_process(pid))?;
        let current_hard = record.resource_limits.get(kind).hard;
        let raises_hard = match (current_hard, value.hard) {
            (Some(_), None) => true,
            (Some(current), Some(requested)) => requested > current,
            (None, _) => false,
        };
        if raises_hard {
            return Err(ProcessTableError::permission_denied(
                "resource-limit hard value cannot be raised",
            ));
        }
        record.resource_limits.set(kind, value);
        Ok(())
    }

    /// Replace the userspace image metadata while retaining Linux process
    /// identity (PID/PPID/PGID/SID), wait relationships, signal mask, pending
    /// signals, and the runtime endpoint used to report the eventual exit.
    /// Caught dispositions reset to default while ignored dispositions survive,
    /// matching execve(2).
    #[allow(clippy::too_many_arguments)]
    pub fn exec(
        &self,
        pid: u32,
        driver: impl Into<String>,
        command: impl Into<String>,
        args: Vec<String>,
        env: BTreeMap<String, String>,
        cwd: String,
        requested_permission_tier: Option<ProcessPermissionTier>,
    ) -> ProcessResult<()> {
        let mut state = self.inner.lock_state();
        let record = state
            .entries
            .get_mut(&pid)
            .ok_or_else(|| ProcessTableError::no_such_process(pid))?;
        if record.entry.status == ProcessStatus::Exited {
            return Err(ProcessTableError::no_such_process(pid));
        }
        if record.entry.pending_termination.is_some() {
            return Err(ProcessTableError::interrupted(format!(
                "process {pid} cannot replace its image after termination was requested"
            )));
        }
        record.entry.driver = driver.into();
        record.entry.command = command.into();
        record.entry.args = args;
        record.entry.env = env;
        record.entry.cwd = cwd;
        record.entry.status = ProcessStatus::Running;
        record.entry.exit_code = None;
        record.entry.termination = None;
        record.entry.exit_time_ms = None;
        if let Some(requested_tier) = requested_permission_tier {
            record.permission_tier = record.permission_tier.restrict(requested_tier);
        }
        for action in &mut record.signal_actions {
            if action.disposition == SignalDisposition::User {
                *action = SignalAction::DEFAULT;
            }
        }
        reset_signal_threads_for_exec(record);
        self.inner.notify_waiters();
        Ok(())
    }

    pub fn zombie_timer_count(&self) -> usize {
        self.reap_due_zombies();
        self.inner.reaper.scheduled_count()
    }

    /// Earliest cooperative zombie-reap deadline. Runtime adapters compare the
    /// exact instant when deciding whether their one process-level timer must
    /// be replaced; deriving a fresh duration on every pump would make the same
    /// deadline appear to move and cause cancellation churn.
    pub fn next_zombie_reap_deadline(&self) -> Option<Instant> {
        self.inner.reaper.next_deadline()
    }

    /// Cooperatively reap any zombies whose TTL deadline has elapsed.
    ///
    /// The kernel owns deadlines but no scheduler or worker. Runtime adapters
    /// call this from their bounded timer/event turn.
    pub fn reap_due_zombies(&self) {
        while let Some(pid) = self.inner.reaper.take_due_pid_now() {
            reap_due_pid(&self.inner, &self.inner.reaper, pid);
        }
    }

    pub fn running_count(&self) -> usize {
        self.reap_due_zombies();
        self.inner
            .lock_state()
            .entries
            .values()
            .filter(|record| record.entry.status == ProcessStatus::Running)
            .count()
    }

    pub fn mark_exited(&self, pid: u32, exit_code: i32) -> ProcessResult<()> {
        mark_exited_inner(&self.inner, pid, None, ProcessExit::Exited(exit_code), None)
    }

    /// Reports one exact terminal result. The first report wins; duplicate or
    /// late adapter reports cannot rewrite wait status.
    pub fn report_exit(&self, pid: u32, termination: ProcessExit) -> ProcessResult<()> {
        mark_exited_inner(&self.inner, pid, None, termination, None)
    }

    pub fn mark_stopped(&self, pid: u32, signal: i32) -> ProcessResult<()> {
        mark_wait_event_inner(
            &self.inner,
            pid,
            None,
            ProcessStatus::Stopped,
            PendingWaitEvent {
                status: signal,
                event: ProcessWaitEvent::Stopped,
            },
        )
    }

    pub fn mark_continued(&self, pid: u32) -> ProcessResult<()> {
        mark_wait_event_inner(
            &self.inner,
            pid,
            None,
            ProcessStatus::Running,
            PendingWaitEvent {
                status: SIGCONT,
                event: ProcessWaitEvent::Continued,
            },
        )
    }

    pub fn waitpid(&self, pid: u32) -> ProcessResult<(u32, i32)> {
        let mut state = self.inner.lock_state();
        loop {
            let Some(record) = state.entries.get(&pid) else {
                return Err(ProcessTableError::no_such_process(pid));
            };

            if record.entry.status == ProcessStatus::Exited {
                let status = record.entry.exit_code.unwrap_or_default();
                state.entries.remove(&pid);
                drop(state);
                self.inner.reaper.cancel(pid);
                self.inner.notify_waiters();
                return Ok((pid, status));
            }

            state = self.inner.wait_for_state(state);
        }
    }

    /// Wait for terminal state without consuming the zombie record.
    pub fn wait_for_exit(&self, pid: u32, timeout: Duration) -> ProcessResult<Option<ProcessExit>> {
        let deadline = Instant::now().checked_add(timeout).ok_or_else(|| {
            ProcessTableError::invalid_argument(
                "process wait timeout exceeds the supported deadline range",
            )
        })?;
        let mut state = self.inner.lock_state();
        loop {
            let Some(record) = state.entries.get(&pid) else {
                return Err(ProcessTableError::no_such_process(pid));
            };
            if record.entry.status == ProcessStatus::Exited {
                return Ok(record.entry.termination);
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            state = wait_timeout_or_recover(&self.inner.waiters, state, deadline - now);
        }
    }

    pub fn waitpid_for(
        &self,
        waiter_pid: u32,
        pid: i32,
        flags: WaitPidFlags,
    ) -> ProcessResult<Option<ProcessWaitResult>> {
        Ok(self
            .waitpid_for_detailed(waiter_pid, pid, flags)?
            .map(|transition| transition.result))
    }

    pub fn waitpid_for_detailed(
        &self,
        waiter_pid: u32,
        pid: i32,
        flags: WaitPidFlags,
    ) -> ProcessResult<Option<ProcessWaitTransition>> {
        let mut state = self.inner.lock_state();
        loop {
            let selector = resolve_wait_selector(&state, waiter_pid, pid)?;
            let matching_children = matching_child_pids(&state, waiter_pid, selector);
            if matching_children.is_empty() {
                return Err(ProcessTableError::no_matching_child(waiter_pid, pid));
            }

            if let Some(transition) =
                take_waitable_transition(&mut state, &matching_children, flags)
            {
                let should_reap = transition.result.event == ProcessWaitEvent::Exited;
                drop(state);
                if should_reap {
                    self.inner.reaper.cancel(transition.result.pid);
                    self.inner.notify_waiters();
                }
                return Ok(Some(transition));
            }

            if flags.contains(WaitPidFlags::WNOHANG) {
                return Ok(None);
            }

            state = self.inner.wait_for_state(state);
        }
    }

    /// Consume one waitable stopped/continued transition without observing or
    /// reaping terminal state. Sidecar child-process bridges use this while
    /// terminal reaping remains coupled to stdout/stderr EOF delivery.
    pub fn take_nonterminal_wait_event_for(
        &self,
        waiter_pid: u32,
        pid: i32,
        flags: WaitPidFlags,
    ) -> ProcessResult<Option<ProcessWaitResult>> {
        let mut state = self.inner.lock_state();
        let selector = resolve_wait_selector(&state, waiter_pid, pid)?;
        let matching_children = matching_child_pids(&state, waiter_pid, selector);
        if matching_children.is_empty() {
            return Err(ProcessTableError::no_matching_child(waiter_pid, pid));
        }

        for child_pid in matching_children {
            let Some(record) = state.entries.get_mut(&child_pid) else {
                continue;
            };
            let Some(index) = record.pending_wait_events.iter().position(|event| {
                event.event != ProcessWaitEvent::Exited && is_waitable_event(event.event, flags)
            }) else {
                continue;
            };
            let event = record
                .pending_wait_events
                .remove(index)
                .expect("pending nonterminal wait event should exist");
            return Ok(Some(ProcessWaitResult {
                pid: child_pid,
                status: event.status,
                event: event.event,
            }));
        }

        Ok(None)
    }

    pub fn kill(&self, pid: i32, signal: i32) -> ProcessResult<()> {
        if !(0..=MAX_SIGNAL).contains(&signal) {
            return Err(ProcessTableError::invalid_signal(signal));
        }

        let deliveries = {
            let mut state = self.inner.lock_state();
            if pid < 0 {
                let pgid = pid.unsigned_abs();
                let grouped = state
                    .entries
                    .values()
                    .filter(|record| {
                        record.entry.pgid == pgid && record.entry.status != ProcessStatus::Exited
                    })
                    .map(|record| record.entry.pid)
                    .collect::<Vec<_>>();
                if grouped.is_empty() {
                    return Err(ProcessTableError::no_such_process_group(pgid));
                }
                if signal == 0 {
                    return Ok(());
                }
                collect_signal_deliveries(&mut state, &grouped, signal)?
            } else {
                let pid = pid as u32;
                let Some(record) = state.entries.get(&pid) else {
                    return Err(ProcessTableError::no_such_process(pid));
                };
                if record.entry.status == ProcessStatus::Exited || signal == 0 {
                    return Ok(());
                }
                collect_signal_deliveries(&mut state, &[pid], signal)?
            }
        };

        if signal == 0 {
            return Ok(());
        }

        deliver_signals(&self.inner, deliveries);
        self.inner.notify_waiters();
        Ok(())
    }

    pub fn setpgid(&self, pid: u32, pgid: u32) -> ProcessResult<()> {
        let mut state = self.inner.lock_state();
        let (current_sid, target_pgid) = {
            let Some(record) = state.entries.get(&pid) else {
                return Err(ProcessTableError::no_such_process(pid));
            };
            (record.entry.sid, if pgid == 0 { pid } else { pgid })
        };

        if target_pgid != pid {
            let mut group_exists = false;
            for record in state.entries.values() {
                if record.entry.pgid != target_pgid || record.entry.status == ProcessStatus::Exited
                {
                    continue;
                }
                if record.entry.sid != current_sid {
                    return Err(ProcessTableError::permission_denied(
                        "cannot join process group in different session",
                    ));
                }
                group_exists = true;
                break;
            }
            if !group_exists {
                return Err(ProcessTableError::permission_denied(format!(
                    "no such process group {target_pgid}"
                )));
            }
        }

        if let Some(record) = state.entries.get_mut(&pid) {
            record.entry.pgid = target_pgid;
        }
        Ok(())
    }

    pub fn getpgid(&self, pid: u32) -> ProcessResult<u32> {
        self.get(pid)
            .map(|entry| entry.pgid)
            .ok_or_else(|| ProcessTableError::no_such_process(pid))
    }

    pub fn setsid(&self, pid: u32) -> ProcessResult<u32> {
        let mut state = self.inner.lock_state();
        let Some(record) = state.entries.get_mut(&pid) else {
            return Err(ProcessTableError::no_such_process(pid));
        };

        if record.entry.pgid == pid {
            return Err(ProcessTableError::permission_denied(format!(
                "process {pid} is already a process group leader"
            )));
        }

        record.entry.sid = pid;
        record.entry.pgid = pid;
        Ok(pid)
    }

    pub fn getsid(&self, pid: u32) -> ProcessResult<u32> {
        self.get(pid)
            .map(|entry| entry.sid)
            .ok_or_else(|| ProcessTableError::no_such_process(pid))
    }

    pub fn getppid(&self, pid: u32) -> ProcessResult<u32> {
        self.get(pid)
            .map(|entry| entry.ppid)
            .ok_or_else(|| ProcessTableError::no_such_process(pid))
    }

    pub fn get_umask(&self, pid: u32) -> ProcessResult<u32> {
        self.get(pid)
            .map(|entry| entry.umask)
            .ok_or_else(|| ProcessTableError::no_such_process(pid))
    }

    pub fn set_umask(&self, pid: u32, umask: u32) -> ProcessResult<u32> {
        let mut state = self.inner.lock_state();
        let record = state
            .entries
            .get_mut(&pid)
            .ok_or_else(|| ProcessTableError::no_such_process(pid))?;
        let previous = record.entry.umask;
        record.entry.umask = umask & 0o777;
        Ok(previous)
    }

    pub fn has_process_group(&self, pgid: u32) -> bool {
        self.inner
            .lock_state()
            .entries
            .values()
            .any(|record| record.entry.pgid == pgid && record.entry.status != ProcessStatus::Exited)
    }

    pub fn list_processes(&self) -> BTreeMap<u32, ProcessInfo> {
        self.reap_due_zombies();
        self.inner
            .lock_state()
            .entries
            .values()
            .map(|record| (record.entry.pid, to_process_info(&record.entry)))
            .collect()
    }

    pub fn terminate_all(&self) {
        let graceful_termination = ProcessTermination::Signal {
            signal: SIGTERM,
            force: false,
        };
        let running = {
            let mut state = self.inner.lock_state();
            state.terminating_all = true;
            self.inner.reaper.clear();
            state
                .entries
                .values_mut()
                .filter(|record| record.entry.status != ProcessStatus::Exited)
                .map(|record| {
                    record.entry.pending_termination = Some(graceful_termination);
                    (record.entry.pid, Arc::clone(&record.runtime_endpoint))
                })
                .collect::<Vec<_>>()
        };

        for (pid, endpoint) in &running {
            if let Err(error) =
                endpoint.request_control(ProcessControlRequest::Terminate(graceful_termination))
            {
                eprintln!(
                    "ERR_AGENTOS_PROCESS_CONTROL: pid={pid} control=terminate-graceful code={} error={}",
                    error.code(),
                    error.message()
                );
            }
        }
        self.wait_for_terminal_state(
            &running
                .iter()
                .filter(|(_, endpoint)| endpoint.has_control_consumer())
                .map(|(pid, _)| *pid)
                .collect::<Vec<_>>(),
            Duration::from_secs(1),
        );

        let survivors = {
            let state = self.inner.lock_state();
            running
                .iter()
                .filter(|(pid, _)| {
                    state
                        .entries
                        .get(pid)
                        .map(|record| record.entry.status != ProcessStatus::Exited)
                        .unwrap_or(false)
                })
                .cloned()
                .collect::<Vec<_>>()
        };

        let forced_termination = ProcessTermination::Signal {
            signal: SIGKILL,
            force: true,
        };
        {
            let mut state = self.inner.lock_state();
            for (pid, _) in &survivors {
                if let Some(record) = state.entries.get_mut(pid) {
                    record.entry.pending_termination = Some(forced_termination);
                }
            }
        }

        for (pid, endpoint) in &survivors {
            if let Err(error) =
                endpoint.request_control(ProcessControlRequest::Terminate(forced_termination))
            {
                eprintln!(
                    "ERR_AGENTOS_PROCESS_CONTROL: pid={pid} control=terminate-forced code={} error={}",
                    error.code(),
                    error.message()
                );
            }
        }
        self.wait_for_terminal_state(
            &survivors
                .iter()
                .filter(|(_, endpoint)| endpoint.has_control_consumer())
                .map(|(pid, _)| *pid)
                .collect::<Vec<_>>(),
            Duration::from_millis(500),
        );
        for (pid, _) in &survivors {
            let still_running = self
                .get(*pid)
                .is_some_and(|entry| entry.status != ProcessStatus::Exited);
            if still_running {
                if let Err(error) = self.report_exit(
                    *pid,
                    ProcessExit::Signaled {
                        signal: SIGKILL,
                        core_dumped: false,
                    },
                ) {
                    eprintln!(
                        "ERR_AGENTOS_PROCESS_EXIT: pid={pid} control=terminate-forced code={} error={}",
                        error.code(),
                        error
                    );
                }
            }
        }

        self.inner.lock_state().terminating_all = false;
    }

    fn wait_for_terminal_state(&self, pids: &[u32], timeout: Duration) {
        let deadline = Instant::now() + timeout;
        let mut state = self.inner.lock_state();
        loop {
            let all_terminal = pids.iter().all(|pid| {
                state
                    .entries
                    .get(pid)
                    .map(|record| record.entry.status == ProcessStatus::Exited)
                    .unwrap_or(true)
            });
            if all_terminal {
                return;
            }
            let now = Instant::now();
            if now >= deadline {
                return;
            }
            state = wait_timeout_or_recover(&self.inner.waiters, state, deadline - now);
        }
    }

    pub fn signal_action(
        &self,
        pid: u32,
        signal: i32,
        action: Option<SignalAction>,
    ) -> ProcessResult<SignalAction> {
        if !(1..=MAX_SIGNAL).contains(&signal) {
            return Err(ProcessTableError::invalid_signal(signal));
        }
        if action.is_some() && matches!(signal, SIGKILL | SIGSTOP) {
            return Err(ProcessTableError::invalid_signal(signal));
        }
        let mut state = self.inner.lock_state();
        let record = state
            .entries
            .get_mut(&pid)
            .ok_or_else(|| ProcessTableError::no_such_process(pid))?;
        let slot = &mut record.signal_actions[(signal - 1) as usize];
        let previous = *slot;
        if let Some(action) = action {
            *slot = action;
            if action.disposition == SignalDisposition::Ignore {
                record.pending_signals.remove(signal)?;
            }
        }
        Ok(previous)
    }

    pub fn reset_signal_actions_for_exec(&self, pid: u32) -> ProcessResult<()> {
        let mut state = self.inner.lock_state();
        let record = state
            .entries
            .get_mut(&pid)
            .ok_or_else(|| ProcessTableError::no_such_process(pid))?;
        for action in &mut record.signal_actions {
            if action.disposition == SignalDisposition::User {
                *action = SignalAction::DEFAULT;
            }
        }
        reset_signal_threads_for_exec(record);
        Ok(())
    }

    pub fn register_signal_thread(
        &self,
        pid: u32,
        thread_id: u32,
        inherit_from: u32,
    ) -> ProcessResult<()> {
        let deliveries = {
            let mut state = self.inner.lock_state();
            let record = state
                .entries
                .get_mut(&pid)
                .ok_or_else(|| ProcessTableError::no_such_process(pid))?;
            if record.signal_threads.contains_key(&thread_id) {
                return Err(ProcessTableError::invalid_argument(format!(
                    "signal thread {thread_id} already exists for process {pid}"
                )));
            }
            if record.signal_threads.len() >= MAX_SIGNAL_THREADS_PER_PROCESS {
                return Err(ProcessTableError {
                    code: "EAGAIN",
                    message: format!(
                        "process {pid} exceeded kernel.signal.maxThreadsPerProcess={MAX_SIGNAL_THREADS_PER_PROCESS}"
                    ),
                });
            }
            let inherited = record
                .signal_threads
                .get(&inherit_from)
                .ok_or_else(|| {
                    ProcessTableError::invalid_argument(format!(
                        "signal thread {inherit_from} does not exist for process {pid}"
                    ))
                })?
                .blocked_signals;
            record
                .signal_threads
                .insert(thread_id, ProcessThreadSignalState::new(inherited));
            collect_pending_signal_deliveries(record)?
        };
        deliver_signals(&self.inner, deliveries);
        Ok(())
    }

    pub fn unregister_signal_thread(&self, pid: u32, thread_id: u32) -> ProcessResult<()> {
        if thread_id == MAIN_SIGNAL_THREAD_ID {
            return Err(ProcessTableError::invalid_argument(
                "the main process signal thread cannot be unregistered",
            ));
        }
        let mut state = self.inner.lock_state();
        let record = state
            .entries
            .get_mut(&pid)
            .ok_or_else(|| ProcessTableError::no_such_process(pid))?;
        record.signal_threads.remove(&thread_id).ok_or_else(|| {
            ProcessTableError::invalid_argument(format!(
                "signal thread {thread_id} does not exist for process {pid}"
            ))
        })?;
        Ok(())
    }

    /// Atomically installs the temporary mask used by `ppoll` for one thread.
    pub fn begin_temporary_signal_mask(&self, pid: u32, mask: SignalSet) -> ProcessResult<u64> {
        self.begin_temporary_signal_mask_for_thread(pid, MAIN_SIGNAL_THREAD_ID, mask)
    }

    pub fn begin_temporary_signal_mask_for_thread(
        &self,
        pid: u32,
        thread_id: u32,
        mut mask: SignalSet,
    ) -> ProcessResult<u64> {
        mask.remove(SIGKILL)?;
        mask.remove(SIGSTOP)?;
        let (token, deliveries) = {
            let mut state = self.inner.lock_state();
            let record = state
                .entries
                .get_mut(&pid)
                .ok_or_else(|| ProcessTableError::no_such_process(pid))?;
            let token = record.next_signal_mask_token;
            record.next_signal_mask_token =
                record.next_signal_mask_token.checked_add(1).unwrap_or(1);
            let thread = signal_thread_mut(record, pid, thread_id)?;
            if thread.temporary_signal_masks.len() >= MAX_SIGNAL_HANDLER_DEPTH {
                return Err(ProcessTableError::signal_delivery_depth_exceeded(pid));
            }
            thread.temporary_signal_masks.push(TemporarySignalMask {
                token,
                previous_mask: thread.blocked_signals,
            });
            thread.blocked_signals = mask;
            (token, collect_pending_signal_deliveries(record)?)
        };
        deliver_signals(&self.inner, deliveries);
        Ok(token)
    }

    pub fn end_temporary_signal_mask(&self, pid: u32, token: u64) -> ProcessResult<()> {
        self.end_temporary_signal_mask_for_thread(pid, MAIN_SIGNAL_THREAD_ID, token)
    }

    pub fn end_temporary_signal_mask_for_thread(
        &self,
        pid: u32,
        thread_id: u32,
        token: u64,
    ) -> ProcessResult<()> {
        let deliveries = {
            let mut state = self.inner.lock_state();
            let record = state
                .entries
                .get_mut(&pid)
                .ok_or_else(|| ProcessTableError::no_such_process(pid))?;
            let thread = signal_thread_mut(record, pid, thread_id)?;
            let Some(scope) = thread.temporary_signal_masks.last().copied() else {
                return Err(ProcessTableError::invalid_signal_delivery_token(pid, token));
            };
            if scope.token != token {
                return Err(ProcessTableError::invalid_signal_delivery_token(pid, token));
            }
            thread.temporary_signal_masks.pop();
            thread.blocked_signals = scope.previous_mask;
            collect_pending_signal_deliveries(record)?
        };
        deliver_signals(&self.inner, deliveries);
        Ok(())
    }

    pub fn end_temporary_signal_mask_and_begin_signal_delivery(
        &self,
        pid: u32,
        token: u64,
    ) -> ProcessResult<Option<SignalDelivery>> {
        self.end_temporary_signal_mask_and_begin_signal_delivery_for_thread(
            pid,
            MAIN_SIGNAL_THREAD_ID,
            token,
        )
    }

    pub fn end_temporary_signal_mask_and_begin_signal_delivery_for_thread(
        &self,
        pid: u32,
        thread_id: u32,
        token: u64,
    ) -> ProcessResult<Option<SignalDelivery>> {
        let mut state = self.inner.lock_state();
        let record = state
            .entries
            .get_mut(&pid)
            .ok_or_else(|| ProcessTableError::no_such_process(pid))?;
        let selected = {
            let pending_signals = record.pending_signals;
            let signal_actions = record.signal_actions;
            let thread = signal_thread_mut(record, pid, thread_id)?;
            let Some(scope) = thread.temporary_signal_masks.last().copied() else {
                return Err(ProcessTableError::invalid_signal_delivery_token(pid, token));
            };
            if scope.token != token {
                return Err(ProcessTableError::invalid_signal_delivery_token(pid, token));
            }
            if thread.signal_deliveries.len() >= MAX_SIGNAL_HANDLER_DEPTH {
                return Err(ProcessTableError::signal_delivery_depth_exceeded(pid));
            }
            let selected = pending_signals
                .difference(thread.blocked_signals)
                .signals()
                .into_iter()
                .find(|signal| {
                    signal_actions[(*signal - 1) as usize].disposition == SignalDisposition::User
                });
            thread.temporary_signal_masks.pop();
            thread.blocked_signals = scope.previous_mask;
            selected
        };
        selected
            .map(|signal| claim_signal_for_thread(record, pid, thread_id, signal))
            .transpose()
    }

    /// Claims one process-directed signal and deterministically selects the
    /// lowest registered thread that does not block it.
    pub fn begin_signal_delivery(&self, pid: u32) -> ProcessResult<Option<SignalDelivery>> {
        let mut state = self.inner.lock_state();
        let record = state
            .entries
            .get_mut(&pid)
            .ok_or_else(|| ProcessTableError::no_such_process(pid))?;
        let Some((signal, thread_id)) = select_signal_target(record) else {
            return Ok(None);
        };
        claim_signal_for_thread(record, pid, thread_id, signal).map(Some)
    }

    pub fn begin_signal_delivery_for_thread(
        &self,
        pid: u32,
        thread_id: u32,
    ) -> ProcessResult<Option<SignalDelivery>> {
        let mut state = self.inner.lock_state();
        let record = state
            .entries
            .get_mut(&pid)
            .ok_or_else(|| ProcessTableError::no_such_process(pid))?;
        let thread = signal_thread(record, pid, thread_id)?;
        let selected = record
            .pending_signals
            .difference(thread.blocked_signals)
            .signals()
            .into_iter()
            .find(|signal| {
                record.signal_actions[(*signal - 1) as usize].disposition == SignalDisposition::User
            });
        selected
            .map(|signal| claim_signal_for_thread(record, pid, thread_id, signal))
            .transpose()
    }

    pub fn end_signal_delivery(&self, pid: u32, token: u64) -> ProcessResult<()> {
        self.end_signal_delivery_for_thread(pid, MAIN_SIGNAL_THREAD_ID, token)
    }

    pub fn end_signal_delivery_for_thread(
        &self,
        pid: u32,
        thread_id: u32,
        token: u64,
    ) -> ProcessResult<()> {
        let deliveries = {
            let mut state = self.inner.lock_state();
            let record = state
                .entries
                .get_mut(&pid)
                .ok_or_else(|| ProcessTableError::no_such_process(pid))?;
            let thread = signal_thread_mut(record, pid, thread_id)?;
            let Some(delivery) = thread.signal_deliveries.last().copied() else {
                return Err(ProcessTableError::invalid_signal_delivery_token(pid, token));
            };
            if delivery.token != token {
                return Err(ProcessTableError::invalid_signal_delivery_token(pid, token));
            }
            thread.signal_deliveries.pop();
            thread.blocked_signals = delivery.previous_mask;
            collect_pending_signal_deliveries(record)?
        };
        deliver_signals(&self.inner, deliveries);
        Ok(())
    }

    pub fn sigprocmask(
        &self,
        pid: u32,
        how: SigmaskHow,
        set: SignalSet,
    ) -> ProcessResult<SignalSet> {
        self.sigprocmask_for_thread(pid, MAIN_SIGNAL_THREAD_ID, how, set)
    }

    pub fn sigprocmask_for_thread(
        &self,
        pid: u32,
        thread_id: u32,
        how: SigmaskHow,
        set: SignalSet,
    ) -> ProcessResult<SignalSet> {
        let (previous, deliveries) = {
            let mut state = self.inner.lock_state();
            let record = state
                .entries
                .get_mut(&pid)
                .ok_or_else(|| ProcessTableError::no_such_process(pid))?;
            let thread = signal_thread_mut(record, pid, thread_id)?;
            let previous = thread.blocked_signals;
            let mut next = match how {
                SigmaskHow::Block => previous.union(set),
                SigmaskHow::Unblock => previous.difference(set),
                SigmaskHow::SetMask => set,
            };
            next.remove(SIGKILL)?;
            next.remove(SIGSTOP)?;
            thread.blocked_signals = next;
            let deliveries = collect_pending_signal_deliveries(record)?;
            (previous, deliveries)
        };
        deliver_signals(&self.inner, deliveries);
        Ok(previous)
    }

    pub fn sigpending(&self, pid: u32) -> ProcessResult<SignalSet> {
        self.inner
            .lock_state()
            .entries
            .get(&pid)
            .map(|record| record.pending_signals)
            .ok_or_else(|| ProcessTableError::no_such_process(pid))
    }
}

impl ProcessExitSink for ProcessTable {
    fn report_exit(
        &self,
        identity: ProcessRuntimeIdentity,
        termination: ProcessExit,
    ) -> Result<(), ProcessRuntimeEndpointError> {
        mark_exited_inner(&self.inner, identity.pid, Some(identity), termination, None)
            .map_err(|error| ProcessRuntimeEndpointError::new(error.code, error.message))
    }

    fn report_runtime_fault(
        &self,
        identity: ProcessRuntimeIdentity,
        fault: ProcessRuntimeFault,
    ) -> Result<(), ProcessRuntimeEndpointError> {
        mark_exited_inner(
            &self.inner,
            identity.pid,
            Some(identity),
            ProcessExit::Exited(1),
            Some(fault),
        )
        .map_err(|error| ProcessRuntimeEndpointError::new(error.code, error.message))
    }
}

fn to_process_info(entry: &ProcessEntry) -> ProcessInfo {
    ProcessInfo {
        pid: entry.pid,
        ppid: entry.ppid,
        pgid: entry.pgid,
        sid: entry.sid,
        driver: entry.driver.clone(),
        command: entry.command.clone(),
        status: entry.status,
        exit_code: entry.exit_code,
        pending_termination: entry.pending_termination,
        termination: entry.termination,
        runtime_fault: entry.runtime_fault.clone(),
        identity: entry.identity.clone(),
    }
}

fn mark_exited_inner(
    inner: &Arc<ProcessTableInner>,
    pid: u32,
    expected_identity: Option<ProcessRuntimeIdentity>,
    termination: ProcessExit,
    runtime_fault: Option<ProcessRuntimeFault>,
) -> ProcessResult<()> {
    let (callback, zombie_ttl, should_schedule, deliveries) = {
        let mut state = inner.lock_state();
        let (ppid, pgid) = {
            let Some(record) = state.entries.get_mut(&pid) else {
                return expected_identity.map_or(Ok(()), |identity| {
                    Err(ProcessTableError::stale_runtime_identity(identity))
                });
            };

            if let Some(expected_identity) = expected_identity {
                if record.runtime_endpoint.identity() != Some(expected_identity) {
                    return Err(ProcessTableError::stale_runtime_identity(expected_identity));
                }
            }

            if record.entry.status == ProcessStatus::Exited {
                return Ok(());
            }

            record.entry.status = ProcessStatus::Exited;
            record.entry.exit_code = Some(termination.shell_status());
            record.entry.pending_termination = None;
            record.entry.termination = Some(termination);
            record.entry.runtime_fault = runtime_fault;
            record.entry.exit_time_ms = Some(now_ms());
            // Child wait state is level-like: a terminal transition supersedes
            // an unconsumed stop/continue notification for the same child.
            record.pending_wait_events.clear();
            let ppid = record.entry.ppid;
            let pgid = record.entry.pgid;
            (ppid, pgid)
        };
        let mut affected_pgids = BTreeSet::from([pgid]);
        reparent_children_to_init(&mut state, pid, &mut affected_pgids);

        let orphaned_group_targets = collect_orphaned_group_signal_targets(&state, &affected_pgids);

        let should_schedule = !state.terminating_all;
        let mut deliveries = Vec::new();
        if should_schedule {
            if let Some(parent) = state
                .entries
                .get_mut(&ppid)
                .filter(|parent| parent.entry.status == ProcessStatus::Running)
            {
                if let Some(delivery) =
                    queue_or_schedule_signal(parent, SIGCHLD).expect("SIGCHLD should be valid")
                {
                    deliveries.push(delivery);
                }
            }
        }

        for target_pid in orphaned_group_targets {
            if let Some(record) = state.entries.get_mut(&target_pid) {
                if let Some(delivery) =
                    queue_or_schedule_signal(record, SIGHUP).expect("SIGHUP should be valid")
                {
                    deliveries.push(delivery);
                }
                if let Some(delivery) =
                    queue_or_schedule_signal(record, SIGCONT).expect("SIGCONT should be valid")
                {
                    deliveries.push(delivery);
                }
            }
        }

        (
            state.on_process_exit.clone(),
            state.zombie_ttl,
            should_schedule,
            deliveries,
        )
    };

    if should_schedule {
        inner.reaper.schedule(pid, zombie_ttl);
    } else {
        inner.reaper.cancel(pid);
    }

    deliver_signals(inner, deliveries);

    if let Some(on_process_exit) = callback {
        on_process_exit(pid);
    }

    inner.notify_waiters();
    Ok(())
}

fn reparent_children_to_init(
    state: &mut ProcessTableState,
    exiting_pid: u32,
    affected_pgids: &mut BTreeSet<u32>,
) {
    let new_parent = reparent_target_pid(state, exiting_pid);
    for record in state.entries.values_mut() {
        if record.entry.ppid != exiting_pid {
            continue;
        }
        record.entry.ppid = new_parent;
        affected_pgids.insert(record.entry.pgid);
    }
}

fn reparent_target_pid(state: &ProcessTableState, exiting_pid: u32) -> u32 {
    if exiting_pid != INIT_PID
        && state
            .entries
            .get(&INIT_PID)
            .map(|record| record.entry.status != ProcessStatus::Exited)
            .unwrap_or(false)
    {
        INIT_PID
    } else {
        0
    }
}

fn collect_orphaned_group_signal_targets(
    state: &ProcessTableState,
    candidate_pgids: &BTreeSet<u32>,
) -> Vec<u32> {
    let mut targets = Vec::new();
    for &pgid in candidate_pgids {
        if !process_group_is_orphaned(state, pgid) || !process_group_has_stopped_member(state, pgid)
        {
            continue;
        }

        for record in state.entries.values() {
            if record.entry.pgid == pgid && record.entry.status != ProcessStatus::Exited {
                targets.push(record.entry.pid);
            }
        }
    }
    targets
}

fn process_group_is_orphaned(state: &ProcessTableState, pgid: u32) -> bool {
    let mut has_member = false;
    for record in state.entries.values() {
        if record.entry.pgid != pgid || record.entry.status == ProcessStatus::Exited {
            continue;
        }
        has_member = true;
        if has_parent_outside_group_in_same_session(state, &record.entry) {
            return false;
        }
    }

    has_member
}

fn has_parent_outside_group_in_same_session(
    state: &ProcessTableState,
    entry: &ProcessEntry,
) -> bool {
    match entry.ppid {
        0 | INIT_PID => false,
        ppid => state
            .entries
            .get(&ppid)
            .map(|parent| {
                parent.entry.status != ProcessStatus::Exited
                    && parent.entry.sid == entry.sid
                    && parent.entry.pgid != entry.pgid
            })
            .unwrap_or(false),
    }
}

fn process_group_has_stopped_member(state: &ProcessTableState, pgid: u32) -> bool {
    state
        .entries
        .values()
        .any(|record| record.entry.pgid == pgid && record.entry.status == ProcessStatus::Stopped)
}

fn mark_wait_event_inner(
    inner: &Arc<ProcessTableInner>,
    pid: u32,
    expected_identity: Option<ProcessRuntimeIdentity>,
    next_status: ProcessStatus,
    event: PendingWaitEvent,
) -> ProcessResult<()> {
    let deliveries = {
        let mut state = inner.lock_state();
        let ppid = {
            let Some(record) = state.entries.get_mut(&pid) else {
                return expected_identity.map_or(Ok(()), |identity| {
                    Err(ProcessTableError::stale_runtime_identity(identity))
                });
            };

            if let Some(expected_identity) = expected_identity {
                if record.runtime_endpoint.identity() != Some(expected_identity) {
                    return Err(ProcessTableError::stale_runtime_identity(expected_identity));
                }
            }

            if record.entry.status == ProcessStatus::Exited || record.entry.status == next_status {
                return Ok(());
            }

            record.entry.status = next_status;
            // Wait state is level-like per child: only the latest unconsumed
            // nonterminal transition is observable. Ordering across children
            // remains the process-table iteration order used by waitpid.
            record.pending_wait_events.clear();
            record.pending_wait_events.push_back(event);
            record.entry.ppid
        };

        state
            .entries
            .get_mut(&ppid)
            .filter(|parent| parent.entry.status == ProcessStatus::Running)
            .and_then(|parent| {
                queue_or_schedule_signal(parent, SIGCHLD)
                    .expect("SIGCHLD should be valid")
                    .into_iter()
                    .next()
            })
            .into_iter()
            .collect::<Vec<_>>()
    };

    deliver_signals(inner, deliveries);

    inner.notify_waiters();
    Ok(())
}

fn signal_bit(signal: i32) -> ProcessResult<u64> {
    if !(1..=MAX_SIGNAL).contains(&signal) {
        return Err(ProcessTableError::invalid_signal(signal));
    }
    Ok(1u64 << (signal - 1))
}

fn normalize_next_pid(pid: u32) -> u32 {
    if (INIT_PID..=MAX_ALLOCATED_PID).contains(&pid) {
        pid
    } else {
        INIT_PID
    }
}

fn next_allocated_pid_after(pid: u32) -> u32 {
    if pid >= MAX_ALLOCATED_PID {
        INIT_PID
    } else {
        pid + 1
    }
}

fn next_pid_after_registered(current: u32, registered: u32) -> u32 {
    let current = normalize_next_pid(current);
    if !(INIT_PID..=MAX_ALLOCATED_PID).contains(&registered) {
        return current;
    }

    if current <= registered {
        next_allocated_pid_after(registered)
    } else {
        current
    }
}

fn signal_can_be_blocked(signal: i32) -> bool {
    !matches!(signal, SIGKILL | SIGSTOP)
}

fn signal_thread(
    record: &ProcessRecord,
    pid: u32,
    thread_id: u32,
) -> ProcessResult<&ProcessThreadSignalState> {
    record.signal_threads.get(&thread_id).ok_or_else(|| {
        ProcessTableError::invalid_argument(format!(
            "signal thread {thread_id} does not exist for process {pid}"
        ))
    })
}

fn signal_thread_mut(
    record: &mut ProcessRecord,
    pid: u32,
    thread_id: u32,
) -> ProcessResult<&mut ProcessThreadSignalState> {
    record.signal_threads.get_mut(&thread_id).ok_or_else(|| {
        ProcessTableError::invalid_argument(format!(
            "signal thread {thread_id} does not exist for process {pid}"
        ))
    })
}

fn select_signal_target(record: &ProcessRecord) -> Option<(i32, u32)> {
    record
        .pending_signals
        .signals()
        .into_iter()
        .find_map(|signal| {
            if record.signal_actions[(signal - 1) as usize].disposition != SignalDisposition::User {
                return None;
            }
            record
                .signal_threads
                .iter()
                .find(|(_, thread)| !thread.blocked_signals.contains(signal))
                .map(|(thread_id, _)| (signal, *thread_id))
        })
}

fn claim_signal_for_thread(
    record: &mut ProcessRecord,
    pid: u32,
    thread_id: u32,
    signal: i32,
) -> ProcessResult<SignalDelivery> {
    let action = record.signal_actions[(signal - 1) as usize];
    let token = record.next_signal_delivery_token;
    record.next_signal_delivery_token = record
        .next_signal_delivery_token
        .checked_add(1)
        .unwrap_or(1);
    let thread = signal_thread(record, pid, thread_id)?;
    if thread.signal_deliveries.len() >= MAX_SIGNAL_HANDLER_DEPTH {
        return Err(ProcessTableError::signal_delivery_depth_exceeded(pid));
    }
    record.pending_signals.remove(signal)?;
    let thread = signal_thread_mut(record, pid, thread_id)?;
    let previous_mask = thread.blocked_signals;
    thread.blocked_signals = thread.blocked_signals.union(action.mask);
    if action.flags & SA_NODEFER == 0 {
        thread.blocked_signals.insert(signal)?;
    }
    thread.blocked_signals.remove(SIGKILL)?;
    thread.blocked_signals.remove(SIGSTOP)?;
    thread.signal_deliveries.push(InProgressSignalDelivery {
        token,
        previous_mask,
    });
    if action.flags & SA_RESETHAND != 0 {
        record.signal_actions[(signal - 1) as usize] = SignalAction::DEFAULT;
    }
    Ok(SignalDelivery {
        token,
        signal,
        action,
        thread_id,
    })
}

fn reset_signal_threads_for_exec(record: &mut ProcessRecord) {
    let blocked = record
        .signal_threads
        .get(&MAIN_SIGNAL_THREAD_ID)
        .map(|thread| thread.blocked_signals)
        .unwrap_or_else(SignalSet::empty);
    record.signal_threads.clear();
    record.signal_threads.insert(
        MAIN_SIGNAL_THREAD_ID,
        ProcessThreadSignalState::new(blocked),
    );
}

fn queue_or_schedule_signal(
    record: &mut ProcessRecord,
    signal: i32,
) -> ProcessResult<Option<ScheduledSignalDelivery>> {
    let action = if matches!(signal, SIGKILL | SIGSTOP) {
        SignalAction::DEFAULT
    } else {
        record.signal_actions[(signal - 1) as usize]
    };
    let mut controls = Vec::with_capacity(2);

    // SIGCONT must also supersede a stop that has been requested but not yet
    // acknowledged by the runtime. Sending an idempotent Continue while the
    // process is still running lets the endpoint's last-writer-wins control
    // cell cancel that in-flight Stop.
    if signal == SIGCONT {
        controls.push(ProcessControlRequest::Continue);
    }

    if action.disposition == SignalDisposition::Ignore {
        return scheduled_signal_delivery(record, signal, controls);
    }

    if signal_can_be_blocked(signal)
        && record
            .signal_threads
            .values()
            .all(|thread| thread.blocked_signals.contains(signal))
    {
        record.pending_signals.insert(signal)?;
        return scheduled_signal_delivery(record, signal, controls);
    }

    match action.disposition {
        SignalDisposition::Ignore => unreachable!("ignore handled above"),
        SignalDisposition::User => {
            record.pending_signals.insert(signal)?;
            controls.push(ProcessControlRequest::Checkpoint);
        }
        SignalDisposition::Default => match signal {
            SIGCHLD | SIGWINCH | SIGURG => {}
            SIGCONT => {}
            SIGSTOP | SIGTSTP | SIGTTIN | SIGTTOU => {
                controls.push(ProcessControlRequest::Stop { signal })
            }
            signal => {
                let termination = ProcessTermination::Signal {
                    signal,
                    force: signal == SIGKILL,
                };
                record.entry.pending_termination = Some(prefer_pending_termination(
                    record.entry.pending_termination,
                    termination,
                ));
                controls.push(ProcessControlRequest::Terminate(termination));
            }
        },
    }

    scheduled_signal_delivery(record, signal, controls)
}

fn prefer_pending_termination(
    current: Option<ProcessTermination>,
    requested: ProcessTermination,
) -> ProcessTermination {
    match (current, requested) {
        (
            Some(current @ ProcessTermination::Signal { force: true, .. }),
            ProcessTermination::Signal { force: false, .. },
        ) => current,
        (_, requested) => requested,
    }
}

fn scheduled_signal_delivery(
    record: &ProcessRecord,
    signal: i32,
    controls: Vec<ProcessControlRequest>,
) -> ProcessResult<Option<ScheduledSignalDelivery>> {
    if controls.is_empty() {
        return Ok(None);
    }
    Ok(Some(ScheduledSignalDelivery {
        pid: record.entry.pid,
        signal,
        runtime_endpoint: Arc::clone(&record.runtime_endpoint),
        controls,
    }))
}

fn collect_signal_deliveries(
    state: &mut ProcessTableState,
    target_pids: &[u32],
    signal: i32,
) -> ProcessResult<Vec<ScheduledSignalDelivery>> {
    let mut deliveries = Vec::new();
    for pid in target_pids {
        let Some(record) = state.entries.get_mut(pid) else {
            continue;
        };
        if let Some(delivery) = queue_or_schedule_signal(record, signal)? {
            deliveries.push(delivery);
        }
    }
    Ok(deliveries)
}

fn collect_pending_signal_deliveries(
    record: &mut ProcessRecord,
) -> ProcessResult<Vec<ScheduledSignalDelivery>> {
    let mut deliveries = Vec::new();
    let signals = record
        .pending_signals
        .signals()
        .into_iter()
        .filter(|signal| {
            record
                .signal_threads
                .values()
                .any(|thread| !thread.blocked_signals.contains(*signal))
        })
        .collect::<Vec<_>>();
    for signal in signals {
        record.pending_signals.remove(signal)?;
        if let Some(delivery) = queue_or_schedule_signal(record, signal)? {
            deliveries.push(delivery);
        }
    }
    Ok(deliveries)
}

fn deliver_signals(_inner: &Arc<ProcessTableInner>, deliveries: Vec<ScheduledSignalDelivery>) {
    for delivery in &deliveries {
        for request in &delivery.controls {
            if let Err(error) = delivery.runtime_endpoint.request_control(*request) {
                eprintln!(
                    "failed to request runtime control for kernel pid {} signal {}: {}",
                    delivery.pid, delivery.signal, error
                );
            }
        }
    }
}

impl ProcessControlAckSink for ProcessTable {
    fn acknowledge_stop_state(
        &self,
        identity: ProcessRuntimeIdentity,
        stopped: bool,
        stop_signal: Option<i32>,
    ) -> Result<(), ProcessRuntimeEndpointError> {
        let (status, event) = if stopped {
            let signal = stop_signal.ok_or_else(|| {
                ProcessRuntimeEndpointError::new(
                    "EINVAL",
                    "stopped runtime control acknowledgement is missing its signal",
                )
            })?;
            (
                ProcessStatus::Stopped,
                PendingWaitEvent {
                    status: signal,
                    event: ProcessWaitEvent::Stopped,
                },
            )
        } else {
            (
                ProcessStatus::Running,
                PendingWaitEvent {
                    status: SIGCONT,
                    event: ProcessWaitEvent::Continued,
                },
            )
        };
        mark_wait_event_inner(&self.inner, identity.pid, Some(identity), status, event)
            .map_err(|error| ProcessRuntimeEndpointError::new(error.code, error.message))
    }
}

fn resolve_wait_selector(
    state: &ProcessTableState,
    waiter_pid: u32,
    pid: i32,
) -> ProcessResult<WaitSelector> {
    let waiter = state
        .entries
        .get(&waiter_pid)
        .ok_or_else(|| ProcessTableError::no_such_process(waiter_pid))?;

    Ok(match pid {
        -1 => WaitSelector::AnyChild,
        0 => WaitSelector::ProcessGroup(waiter.entry.pgid),
        p if p < -1 => WaitSelector::ProcessGroup(p.unsigned_abs()),
        p => WaitSelector::ChildPid(p as u32),
    })
}

fn matching_child_pids(
    state: &ProcessTableState,
    waiter_pid: u32,
    selector: WaitSelector,
) -> Vec<u32> {
    state
        .entries
        .values()
        .filter(|record| record.entry.ppid == waiter_pid)
        .filter(|record| match selector {
            WaitSelector::AnyChild => true,
            WaitSelector::ChildPid(pid) => record.entry.pid == pid,
            WaitSelector::ProcessGroup(pgid) => record.entry.pgid == pgid,
        })
        .map(|record| record.entry.pid)
        .collect()
}

fn take_waitable_transition(
    state: &mut ProcessTableState,
    matching_children: &[u32],
    flags: WaitPidFlags,
) -> Option<ProcessWaitTransition> {
    for child_pid in matching_children {
        let mut non_exit_result = None;
        let mut should_reap = false;
        {
            let record = state.entries.get_mut(child_pid)?;
            if let Some(index) = record
                .pending_wait_events
                .iter()
                .position(|event| is_waitable_event(event.event, flags))
            {
                let event = record
                    .pending_wait_events
                    .remove(index)
                    .expect("pending wait event should exist");
                non_exit_result = Some(ProcessWaitTransition {
                    result: ProcessWaitResult {
                        pid: *child_pid,
                        status: event.status,
                        event: event.event,
                    },
                    termination: None,
                });
            } else if record.entry.status == ProcessStatus::Exited {
                should_reap = true;
            }
        }

        if let Some(result) = non_exit_result {
            return Some(result);
        }

        if should_reap {
            let record = state
                .entries
                .remove(child_pid)
                .expect("exited child should still exist");
            return Some(ProcessWaitTransition {
                result: ProcessWaitResult {
                    pid: *child_pid,
                    status: record.entry.exit_code.unwrap_or_default(),
                    event: ProcessWaitEvent::Exited,
                },
                termination: record.entry.termination,
            });
        }
    }

    None
}

fn is_waitable_event(event: ProcessWaitEvent, flags: WaitPidFlags) -> bool {
    match event {
        ProcessWaitEvent::Exited => true,
        ProcessWaitEvent::Stopped => flags.contains(WaitPidFlags::WUNTRACED),
        ProcessWaitEvent::Continued => flags.contains(WaitPidFlags::WCONTINUED),
    }
}

/// Reap a single due zombie pid. The kernel remains runtime-neutral: the
/// sidecar drives this cooperatively from its process event turn.
fn reap_due_pid(inner: &ProcessTableInner, reaper: &ZombieReaper, pid: u32) {
    let mut state = inner.lock_state();
    let should_reap = state
        .entries
        .get(&pid)
        .map(|record| {
            record.entry.status == ProcessStatus::Exited
                && !has_living_parent(&state, record.entry.ppid)
        })
        .unwrap_or(false);
    if should_reap {
        state.entries.remove(&pid);
    } else if state
        .entries
        .get(&pid)
        .map(|record| record.entry.status == ProcessStatus::Exited)
        .unwrap_or(false)
    {
        reaper.schedule(pid, state.zombie_ttl);
    }
    drop(state);
    inner.notify_waiters();
}

fn has_living_parent(state: &ProcessTableState, ppid: u32) -> bool {
    ppid != 0
        && state
            .entries
            .get(&ppid)
            .map(|record| record.entry.status != ProcessStatus::Exited)
            .unwrap_or(false)
}

impl ProcessTableInner {
    fn notify_waiters(&self) {
        {
            let mut generation = lock_or_recover(&self.wait_generation);
            *generation = generation.wrapping_add(1);
        }
        self.waiters.notify_all();
        self.async_waiters.notify(usize::MAX);
    }

    fn lock_state(&self) -> MutexGuard<'_, ProcessTableState> {
        lock_or_recover(&self.state)
    }

    fn wait_for_state<'a>(
        &self,
        guard: MutexGuard<'a, ProcessTableState>,
    ) -> MutexGuard<'a, ProcessTableState> {
        wait_or_recover(&self.waiters, guard)
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

impl Default for ZombieReaper {
    fn default() -> Self {
        Self {
            state: Mutex::new(ZombieReaperState::default()),
        }
    }
}

impl ZombieReaper {
    fn schedule(&self, pid: u32, ttl: Duration) {
        let mut state = lock_or_recover(&self.state);
        state.deadlines.insert(pid, Instant::now() + ttl);
    }

    fn cancel(&self, pid: u32) {
        lock_or_recover(&self.state).deadlines.remove(&pid);
    }

    fn clear(&self) {
        lock_or_recover(&self.state).deadlines.clear();
    }

    fn scheduled_count(&self) -> usize {
        lock_or_recover(&self.state).deadlines.len()
    }

    fn next_deadline(&self) -> Option<Instant> {
        lock_or_recover(&self.state)
            .deadlines
            .values()
            .min()
            .copied()
    }

    /// Return one due pid without blocking. Runtime adapters drain this method
    /// through `ProcessTable::reap_due_zombies`.
    fn take_due_pid_now(&self) -> Option<u32> {
        let mut state = lock_or_recover(&self.state);
        let now = Instant::now();
        let due = state
            .deadlines
            .iter()
            .filter(|(_, deadline)| **deadline <= now)
            .min_by_key(|(_, deadline)| **deadline)
            .map(|(&pid, _)| pid);
        if let Some(pid) = due {
            state.deadlines.remove(&pid);
        }
        due
    }
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
) -> MutexGuard<'a, T> {
    match condvar.wait_timeout(guard, timeout) {
        Ok((guard, _)) => guard,
        Err(poisoned) => poisoned.into_inner().0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_wait_timeout_fails_before_process_lookup() {
        let error = ProcessTable::new()
            .wait_for_exit(42, Duration::MAX)
            .expect_err("an unrepresentable deadline must fail");

        assert_eq!(error.code(), "EINVAL");
    }
    use crate::process_runtime::{
        ProcessExitReporter, ProcessRuntimeEndpointError, ProcessRuntimeFault,
        ProcessRuntimeIdentity, RuntimeControlCell,
    };

    #[derive(Default)]
    struct TestRuntimeEndpoint {
        identity: Option<ProcessRuntimeIdentity>,
        controls: Mutex<Vec<ProcessControlRequest>>,
    }

    impl ProcessRuntimeEndpoint for TestRuntimeEndpoint {
        fn identity(&self) -> Option<ProcessRuntimeIdentity> {
            self.identity
        }

        fn request_control(
            &self,
            request: ProcessControlRequest,
        ) -> Result<(), ProcessRuntimeEndpointError> {
            self.controls
                .lock()
                .expect("test endpoint lock poisoned")
                .push(request);
            Ok(())
        }
    }

    impl TestRuntimeEndpoint {
        fn take_controls(&self) -> Vec<ProcessControlRequest> {
            std::mem::take(&mut *self.controls.lock().expect("test endpoint lock poisoned"))
        }
    }

    fn endpoint() -> Arc<TestRuntimeEndpoint> {
        Arc::new(TestRuntimeEndpoint::default())
    }

    fn identified_endpoint(identity: ProcessRuntimeIdentity) -> Arc<TestRuntimeEndpoint> {
        Arc::new(TestRuntimeEndpoint {
            identity: Some(identity),
            controls: Mutex::new(Vec::new()),
        })
    }

    fn context(ppid: u32) -> ProcessContext {
        ProcessContext {
            ppid,
            ..ProcessContext::default()
        }
    }

    #[test]
    fn async_wait_generation_closes_the_probe_to_listener_lost_wake() {
        use std::future::Future;
        use std::task::{Context, Poll, Waker};

        let table = ProcessTable::with_zombie_ttl(Duration::from_secs(3600));
        table.register(10, "test", "parent", Vec::new(), context(0), endpoint());
        table.register(11, "test", "child", Vec::new(), context(10), endpoint());

        let wait_handle = table.wait_handle();
        let observed = wait_handle.snapshot();
        assert!(table
            .waitpid_for(10, 11, WaitPidFlags::WNOHANG)
            .expect("nonblocking probe")
            .is_none());

        // Deliberately publish the only transition before constructing the
        // listener. A notification-only design would now sleep forever; the
        // pre-probe generation must make the future immediately ready.
        table.mark_exited(11, 0).expect("publish child exit");
        let mut future = Box::pin(wait_handle.wait_for_change_async(observed));
        let waker = Waker::noop();
        let mut task_context = Context::from_waker(waker);
        assert_eq!(future.as_mut().poll(&mut task_context), Poll::Ready(()));
        assert!(table
            .waitpid_for(10, 11, WaitPidFlags::WNOHANG)
            .expect("ready probe")
            .is_some());
    }

    #[test]
    fn child_wait_state_is_coalesced_and_terminal_supersedes_it() {
        let table = ProcessTable::with_zombie_ttl(Duration::from_secs(3600));
        table.register(10, "test", "parent", Vec::new(), context(0), endpoint());
        table.register(11, "test", "child", Vec::new(), context(10), endpoint());

        for _ in 0..1_000 {
            table.mark_stopped(11, SIGTSTP).expect("stop child");
            table.mark_continued(11).expect("continue child");
        }
        {
            let state = table.inner.lock_state();
            let child = state.entries.get(&11).expect("child record");
            assert_eq!(child.pending_wait_events.len(), 1);
            assert_eq!(
                child.pending_wait_events.front().map(|event| event.event),
                Some(ProcessWaitEvent::Continued)
            );
        }

        table
            .report_exit(11, ProcessExit::Exited(23))
            .expect("publish terminal status");
        {
            let state = table.inner.lock_state();
            assert!(state
                .entries
                .get(&11)
                .expect("child zombie")
                .pending_wait_events
                .is_empty());
        }
        let transition = table
            .waitpid_for(
                10,
                11,
                WaitPidFlags::WNOHANG | WaitPidFlags::WUNTRACED | WaitPidFlags::WCONTINUED,
            )
            .expect("wait for terminal child")
            .expect("terminal transition");
        assert_eq!(transition.event, ProcessWaitEvent::Exited);
        assert_eq!(transition.status, 23);
    }

    #[test]
    fn first_exact_exit_report_wins() {
        let table = ProcessTable::with_zombie_ttl(Duration::from_secs(3600));
        table.register(
            10,
            "test",
            "already-exited",
            Vec::new(),
            context(0),
            endpoint(),
        );
        table
            .report_exit(10, ProcessExit::Exited(27))
            .expect("publish first exit");
        table
            .report_exit(
                10,
                ProcessExit::Signaled {
                    signal: SIGKILL,
                    core_dumped: false,
                },
            )
            .expect("ignore later exact exit without losing first");

        let entry = table.get(10).expect("registered process remains a zombie");
        assert_eq!(entry.status, ProcessStatus::Exited);
        assert_eq!(entry.exit_code, Some(27));
        assert_eq!(entry.termination, Some(ProcessExit::Exited(27)));
        assert_eq!(entry.runtime_fault, None);
    }

    #[test]
    fn first_runtime_fault_report_wins_and_stays_typed() {
        let table = ProcessTable::with_zombie_ttl(Duration::from_secs(3600));
        let identity = ProcessRuntimeIdentity {
            generation: 19,
            pid: 10,
        };
        table.register(
            10,
            "test",
            "faulted",
            Vec::new(),
            context(0),
            identified_endpoint(identity),
        );
        let reporter = ProcessExitReporter::new(identity, Arc::new(table.clone()));
        let fault = ProcessRuntimeFault::try_new(
            "ERR_AGENTOS_WASM_TRAP",
            "integer divide by zero",
            Some(serde_json::json!({ "trap": "integer_division_by_zero" })),
        )
        .expect("bounded typed fault");
        reporter
            .report_runtime_fault(fault.clone())
            .expect("current reporter should fault its process");
        reporter
            .report_exit(ProcessExit::Exited(0))
            .expect("late terminal report is idempotent");

        let entry = table.get(10).expect("faulted process remains a zombie");
        assert_eq!(entry.status, ProcessStatus::Exited);
        assert_eq!(entry.exit_code, Some(1));
        assert_eq!(entry.termination, Some(ProcessExit::Exited(1)));
        assert_eq!(entry.runtime_fault, Some(fault));
    }

    #[test]
    fn stale_exit_reporter_cannot_exit_reused_pid() {
        let table = ProcessTable::with_zombie_ttl(Duration::from_secs(3600));
        let first_identity = ProcessRuntimeIdentity {
            generation: 41,
            pid: 10,
        };
        table.register(
            10,
            "test",
            "first",
            Vec::new(),
            context(0),
            identified_endpoint(first_identity),
        );
        let reporter = ProcessExitReporter::new(first_identity, Arc::new(table.clone()));
        reporter
            .report_exit(ProcessExit::Exited(7))
            .expect("current reporter should finish its process");
        table.waitpid(10).expect("reap first process");

        let replacement_identity = ProcessRuntimeIdentity {
            generation: 42,
            pid: 10,
        };
        table.register(
            10,
            "test",
            "replacement",
            Vec::new(),
            context(0),
            identified_endpoint(replacement_identity),
        );

        let error = reporter
            .report_exit(ProcessExit::Signaled {
                signal: SIGKILL,
                core_dumped: false,
            })
            .expect_err("stale reporter must not target a reused pid");
        assert_eq!(error.code(), "ESTALE");
        assert_eq!(
            table.get(10).expect("replacement remains live").status,
            ProcessStatus::Running
        );
    }

    #[test]
    fn spawn_process_group_is_applied_atomically() {
        let table = ProcessTable::with_zombie_ttl(Duration::from_secs(3600));
        table.register(10, "test", "parent", Vec::new(), context(0), endpoint());

        let leader = table
            .register_with_process_group(
                11,
                "test",
                "leader",
                Vec::new(),
                context(10),
                endpoint(),
                Some(0),
            )
            .expect("spawn should create a new process group");
        assert_eq!(leader.pgid, 11);

        let peer = table
            .register_with_process_group(
                12,
                "test",
                "peer",
                Vec::new(),
                context(10),
                endpoint(),
                Some(11),
            )
            .expect("spawn should join an existing group in the same session");
        assert_eq!(peer.pgid, 11);

        let error = table
            .register_with_process_group(
                13,
                "test",
                "invalid",
                Vec::new(),
                context(10),
                endpoint(),
                Some(999),
            )
            .expect_err("spawn must reject a nonexistent process group");
        assert_eq!(error.code(), "EPERM");
        assert!(
            table.get(13).is_none(),
            "failed spawn must not register a child"
        );

        table.register(
            20,
            "test",
            "other-session",
            Vec::new(),
            context(0),
            endpoint(),
        );
        let error = table
            .register_with_process_group(
                14,
                "test",
                "cross-session",
                Vec::new(),
                context(10),
                endpoint(),
                Some(20),
            )
            .expect_err("spawn must reject a process group in another session");
        assert_eq!(error.code(), "EPERM");
        assert!(table.get(14).is_none(), "failed spawn must remain atomic");
    }

    #[test]
    fn allocate_pid_wraps_without_reusing_live_or_zombie_processes() {
        let table = ProcessTable::with_zombie_ttl(Duration::from_secs(3600));
        let live_high = endpoint();
        let zombie_high = endpoint();
        let live_one = endpoint();
        let max_pid = MAX_ALLOCATED_PID;

        table.register(
            max_pid - 1,
            "test",
            "live-high",
            Vec::new(),
            context(0),
            live_high,
        );
        table.register(
            max_pid,
            "test",
            "zombie-high",
            Vec::new(),
            context(0),
            zombie_high.clone(),
        );
        table.register(1, "test", "live-one", Vec::new(), context(0), live_one);
        table
            .report_exit(max_pid, ProcessExit::Exited(0))
            .expect("publish high-pid exit");

        table.inner.lock_state().next_pid = max_pid - 1;

        assert_eq!(table.allocate_pid().expect("allocate pid"), 2);
        assert_eq!(table.allocate_pid().expect("allocate pid"), 3);
    }

    #[test]
    fn caught_signal_is_kernel_pending_until_handler_checkpoint() {
        let table = ProcessTable::with_zombie_ttl(Duration::from_secs(3600));
        let endpoint = endpoint();
        table.register(
            10,
            "test",
            "signals",
            Vec::new(),
            context(0),
            endpoint.clone(),
        );
        let handler_mask = SignalSet::from_signal(SIGTERM).expect("handler mask");
        table
            .signal_action(
                10,
                SIGPIPE,
                Some(SignalAction {
                    disposition: SignalDisposition::User,
                    mask: handler_mask,
                    flags: SA_RESETHAND,
                }),
            )
            .expect("install action");

        table.kill(10, SIGPIPE).expect("queue caught signal");
        assert_eq!(
            endpoint.take_controls(),
            vec![ProcessControlRequest::Checkpoint]
        );
        assert!(table.sigpending(10).expect("pending").contains(SIGPIPE));

        let delivery = table
            .begin_signal_delivery(10)
            .expect("begin delivery")
            .expect("caught signal");
        assert_eq!(delivery.signal, SIGPIPE);
        assert!(!table.sigpending(10).expect("pending").contains(SIGPIPE));
        assert_eq!(
            table
                .signal_action(10, SIGPIPE, None)
                .expect("query")
                .disposition,
            SignalDisposition::Default
        );
        table
            .end_signal_delivery(10, delivery.token)
            .expect("end delivery");
    }

    #[test]
    fn different_caught_signals_keep_delivery_tokens_strictly_lifo() {
        let table = ProcessTable::with_zombie_ttl(Duration::from_secs(3600));
        table.register(10, "test", "signals", Vec::new(), context(0), endpoint());
        for signal in [SIGPIPE, SIGTERM] {
            table
                .signal_action(
                    10,
                    signal,
                    Some(SignalAction {
                        disposition: SignalDisposition::User,
                        ..SignalAction::DEFAULT
                    }),
                )
                .expect("install caught action");
            table.kill(10, signal).expect("queue caught signal");
        }

        let first = table
            .begin_signal_delivery(10)
            .expect("claim first signal")
            .expect("first delivery");
        assert_eq!(first.signal, SIGPIPE, "standard signals use numeric order");
        let nested = table
            .begin_signal_delivery(10)
            .expect("claim nested signal")
            .expect("nested delivery");
        assert_eq!(nested.signal, SIGTERM);
        assert_eq!(
            table
                .end_signal_delivery(10, first.token)
                .expect_err("outer token cannot end before nested delivery")
                .code(),
            "EINVAL"
        );
        table
            .end_signal_delivery(10, nested.token)
            .expect("end nested delivery");
        table
            .end_signal_delivery(10, first.token)
            .expect("end outer delivery");
    }

    #[test]
    fn blocked_caught_signal_wakes_only_after_unblock() {
        let table = ProcessTable::with_zombie_ttl(Duration::from_secs(3600));
        let endpoint = endpoint();
        table.register(
            10,
            "test",
            "signals",
            Vec::new(),
            context(0),
            endpoint.clone(),
        );
        table
            .signal_action(
                10,
                SIGTERM,
                Some(SignalAction {
                    disposition: SignalDisposition::User,
                    ..SignalAction::DEFAULT
                }),
            )
            .expect("install action");
        let mask = SignalSet::from_signal(SIGTERM).expect("mask");
        table
            .sigprocmask(10, SigmaskHow::Block, mask)
            .expect("block");
        table.kill(10, SIGTERM).expect("queue blocked signal");
        assert!(endpoint.take_controls().is_empty());
        assert!(table.sigpending(10).expect("pending").contains(SIGTERM));

        table
            .sigprocmask(10, SigmaskHow::Unblock, mask)
            .expect("unblock");
        assert_eq!(
            endpoint.take_controls(),
            vec![ProcessControlRequest::Checkpoint]
        );
    }

    #[test]
    fn process_signal_selects_an_unblocked_thread_and_keeps_masks_per_thread() {
        let table = ProcessTable::with_zombie_ttl(Duration::from_secs(3600));
        let endpoint = endpoint();
        table.register(
            10,
            "test",
            "pthread-signals",
            Vec::new(),
            context(0),
            endpoint.clone(),
        );
        table
            .register_signal_thread(10, 1, MAIN_SIGNAL_THREAD_ID)
            .expect("register pthread signal record");
        table
            .signal_action(
                10,
                SIGTERM,
                Some(SignalAction {
                    disposition: SignalDisposition::User,
                    ..SignalAction::DEFAULT
                }),
            )
            .expect("install caught action");
        let mask = SignalSet::from_signal(SIGTERM).expect("mask");
        table
            .sigprocmask_for_thread(10, MAIN_SIGNAL_THREAD_ID, SigmaskHow::Block, mask)
            .expect("block signal only on main thread");
        table.kill(10, SIGTERM).expect("queue process signal");
        assert_eq!(
            endpoint.take_controls(),
            vec![ProcessControlRequest::Checkpoint]
        );
        let delivery = table
            .begin_signal_delivery(10)
            .expect("select signal thread")
            .expect("caught delivery");
        assert_eq!(delivery.thread_id, 1);
        table
            .end_signal_delivery_for_thread(10, 1, delivery.token)
            .expect("settle on selected thread");
        let main_mask = table
            .sigprocmask_for_thread(
                10,
                MAIN_SIGNAL_THREAD_ID,
                SigmaskHow::Block,
                SignalSet::empty(),
            )
            .expect("query main mask");
        let worker_mask = table
            .sigprocmask_for_thread(10, 1, SigmaskHow::Block, SignalSet::empty())
            .expect("query worker mask");
        assert!(main_mask.contains(SIGTERM));
        assert!(!worker_mask.contains(SIGTERM));
        table
            .unregister_signal_thread(10, 1)
            .expect("remove pthread signal record");
    }

    #[test]
    fn temporary_ppoll_mask_restores_atomically_and_releases_pending_signal() {
        let table = ProcessTable::with_zombie_ttl(Duration::from_secs(3600));
        let endpoint = endpoint();
        table.register(
            10,
            "test",
            "ppoll",
            Vec::new(),
            context(0),
            endpoint.clone(),
        );
        table
            .signal_action(
                10,
                SIGTERM,
                Some(SignalAction {
                    disposition: SignalDisposition::User,
                    ..SignalAction::DEFAULT
                }),
            )
            .expect("install caught signal");

        let token = table
            .begin_temporary_signal_mask(
                10,
                SignalSet::from_signal(SIGTERM).expect("temporary mask"),
            )
            .expect("begin ppoll mask");
        table.kill(10, SIGTERM).expect("queue during ppoll");
        assert!(endpoint.take_controls().is_empty());
        assert!(table.sigpending(10).expect("pending").contains(SIGTERM));

        let error = table
            .end_temporary_signal_mask(10, token + 1)
            .expect_err("out-of-order token must not restore the mask");
        assert_eq!(error.code(), "EINVAL");
        assert!(endpoint.take_controls().is_empty());

        table
            .end_temporary_signal_mask(10, token)
            .expect("restore ppoll mask");
        assert_eq!(
            endpoint.take_controls(),
            vec![ProcessControlRequest::Checkpoint]
        );
    }

    #[test]
    fn ppoll_claims_signal_under_temporary_mask_but_builds_handler_from_original_mask() {
        let table = ProcessTable::with_zombie_ttl(Duration::from_secs(3600));
        let endpoint = endpoint();
        table.register(
            10,
            "test",
            "ppoll-delivery",
            Vec::new(),
            context(0),
            endpoint.clone(),
        );
        table
            .signal_action(
                10,
                SIGTERM,
                Some(SignalAction {
                    disposition: SignalDisposition::User,
                    ..SignalAction::DEFAULT
                }),
            )
            .expect("install caught signal");
        let original = SignalSet::from_signals([SIGPIPE, SIGTERM]).expect("original mask");
        table
            .sigprocmask(10, SigmaskHow::Block, original)
            .expect("block caught signal in caller mask");
        table.kill(10, SIGTERM).expect("queue blocked signal");
        assert!(endpoint.take_controls().is_empty());

        let token = table
            .begin_temporary_signal_mask(10, SignalSet::empty())
            .expect("install unblocking ppoll mask");
        assert_eq!(
            endpoint.take_controls(),
            vec![ProcessControlRequest::Checkpoint]
        );
        let delivery = table
            .end_temporary_signal_mask_and_begin_signal_delivery(10, token)
            .expect("restore mask and claim ppoll signal")
            .expect("caught signal delivery");
        assert_eq!(delivery.signal, SIGTERM);
        assert!(!table
            .sigpending(10)
            .expect("pending signals")
            .contains(SIGTERM));
        let handler_mask = table
            .sigprocmask(10, SigmaskHow::Block, SignalSet::empty())
            .expect("query handler mask");
        assert!(handler_mask.contains(SIGPIPE));
        assert!(handler_mask.contains(SIGTERM));

        table
            .end_signal_delivery(10, delivery.token)
            .expect("complete handler");
        let restored = table
            .sigprocmask(10, SigmaskHow::Block, SignalSet::empty())
            .expect("query restored mask");
        assert_eq!(restored, original);
    }

    #[test]
    fn spawn_and_exec_preserve_only_ignored_signal_dispositions() {
        let table = ProcessTable::with_zombie_ttl(Duration::from_secs(3600));
        table.register(10, "test", "parent", Vec::new(), context(0), endpoint());
        table
            .signal_action(
                10,
                SIGPIPE,
                Some(SignalAction {
                    disposition: SignalDisposition::User,
                    ..SignalAction::DEFAULT
                }),
            )
            .expect("install caught action");
        table
            .signal_action(
                10,
                SIGTERM,
                Some(SignalAction {
                    disposition: SignalDisposition::Ignore,
                    ..SignalAction::DEFAULT
                }),
            )
            .expect("install ignored action");

        table.register(11, "test", "child", Vec::new(), context(10), endpoint());
        assert_eq!(
            table
                .signal_action(11, SIGPIPE, None)
                .expect("query caught action")
                .disposition,
            SignalDisposition::Default
        );
        assert_eq!(
            table
                .signal_action(11, SIGTERM, None)
                .expect("query ignored action")
                .disposition,
            SignalDisposition::Ignore
        );

        table
            .signal_action(
                11,
                SIGPIPE,
                Some(SignalAction {
                    disposition: SignalDisposition::User,
                    ..SignalAction::DEFAULT
                }),
            )
            .expect("install child handler");
        table
            .exec(
                11,
                "test",
                "replacement",
                Vec::new(),
                BTreeMap::new(),
                String::from("/"),
                None,
            )
            .expect("exec replacement image");
        assert_eq!(
            table
                .signal_action(11, SIGPIPE, None)
                .expect("query reset action")
                .disposition,
            SignalDisposition::Default
        );
        assert_eq!(
            table
                .signal_action(11, SIGTERM, None)
                .expect("query retained ignore")
                .disposition,
            SignalDisposition::Ignore
        );
    }

    #[test]
    fn permission_tier_inherits_and_exec_can_only_restrict() {
        let table = ProcessTable::with_zombie_ttl(Duration::from_secs(3600));
        let mut parent_context = context(0);
        parent_context.permission_tier = ProcessPermissionTier::ReadWrite;
        table.register(
            40,
            "runtime",
            "parent",
            Vec::new(),
            parent_context,
            endpoint(),
        );

        let child_context = table.inherited_context(40).expect("inherit parent context");
        assert_eq!(
            child_context.permission_tier,
            ProcessPermissionTier::ReadWrite
        );
        table.register(
            41,
            "runtime",
            "child",
            Vec::new(),
            child_context,
            endpoint(),
        );
        assert_eq!(
            table.permission_tier(41).expect("child tier"),
            ProcessPermissionTier::ReadWrite
        );

        table
            .exec(
                41,
                "runtime",
                "restricted",
                Vec::new(),
                BTreeMap::new(),
                String::from("/"),
                Some(ProcessPermissionTier::ReadOnly),
            )
            .expect("restrict tier on exec");
        assert_eq!(
            table.permission_tier(41).expect("restricted tier"),
            ProcessPermissionTier::ReadOnly
        );

        table
            .exec(
                41,
                "runtime",
                "cannot-escalate",
                Vec::new(),
                BTreeMap::new(),
                String::from("/"),
                Some(ProcessPermissionTier::Full),
            )
            .expect("exec with broader image ceiling");
        assert_eq!(
            table.permission_tier(41).expect("non-escalated tier"),
            ProcessPermissionTier::ReadOnly
        );
    }

    #[test]
    fn default_signal_actions_are_decided_by_kernel() {
        let table = ProcessTable::with_zombie_ttl(Duration::from_secs(3600));
        let identity = ProcessRuntimeIdentity {
            generation: 7,
            pid: 10,
        };
        let endpoint = identified_endpoint(identity);
        table.register(
            10,
            "test",
            "signals",
            Vec::new(),
            context(0),
            endpoint.clone(),
        );

        table.kill(10, SIGCHLD).expect("default ignored signal");
        assert!(endpoint.take_controls().is_empty());
        table.kill(10, SIGTSTP).expect("default stop signal");
        assert_eq!(
            endpoint.take_controls(),
            vec![ProcessControlRequest::Stop { signal: SIGTSTP }]
        );
        assert_eq!(
            table.get(10).expect("process").status,
            ProcessStatus::Running,
            "requesting stop must not publish wait state before runtime acknowledgement"
        );
        ProcessControlAckSink::acknowledge_stop_state(&table, identity, true, Some(SIGTSTP))
            .expect("acknowledge stop");
        assert_eq!(
            table.get(10).expect("process").status,
            ProcessStatus::Stopped
        );
        table.kill(10, SIGCONT).expect("continue signal");
        assert_eq!(
            endpoint.take_controls(),
            vec![ProcessControlRequest::Continue]
        );
        assert_eq!(
            table.get(10).expect("process").status,
            ProcessStatus::Stopped,
            "requesting continue must not publish wait state before runtime acknowledgement"
        );
        ProcessControlAckSink::acknowledge_stop_state(&table, identity, false, None)
            .expect("acknowledge continue");
        assert_eq!(
            table.get(10).expect("process").status,
            ProcessStatus::Running
        );
        table.kill(10, SIGKILL).expect("fatal signal");
        assert_eq!(
            endpoint.take_controls(),
            vec![ProcessControlRequest::Terminate(
                ProcessTermination::Signal {
                    signal: SIGKILL,
                    force: true,
                }
            )]
        );
        assert_eq!(
            table
                .get(10)
                .expect("terminating process")
                .pending_termination,
            Some(ProcessTermination::Signal {
                signal: SIGKILL,
                force: true,
            }),
            "kernel process state must publish the durable termination request"
        );
        table.kill(10, SIGTERM).expect("later graceful signal");
        assert_eq!(
            table
                .get(10)
                .expect("terminating process")
                .pending_termination,
            Some(ProcessTermination::Signal {
                signal: SIGKILL,
                force: true,
            }),
            "a forced termination request cannot be downgraded"
        );
        assert_eq!(
            table
                .exec(
                    10,
                    "test",
                    "replacement",
                    Vec::new(),
                    BTreeMap::new(),
                    String::from("/"),
                    None,
                )
                .expect_err("termination must prevent image replacement")
                .code(),
            "EINTR"
        );
        ProcessExitReporter::new(identity, Arc::new(table.clone()))
            .report_exit(ProcessExit::Signaled {
                signal: SIGKILL,
                core_dumped: false,
            })
            .expect("runtime reports terminal signal");
        assert_eq!(
            table.get(10).expect("exited process").pending_termination,
            None,
            "terminal state replaces the pending termination request"
        );
    }

    #[test]
    fn continue_supersedes_unacknowledged_stop_without_phantom_wait_state() {
        let table = ProcessTable::with_zombie_ttl(Duration::from_secs(3600));
        let identity = ProcessRuntimeIdentity {
            generation: 9,
            pid: 10,
        };
        let cell = RuntimeControlCell::new_with_ack_sink(9, Arc::new(table.clone()));
        cell.bind_pid(10).expect("bind runtime endpoint");
        let receiver = cell.attach(Arc::new(|| {})).expect("attach runtime");
        table.register(
            10,
            "test",
            "signals",
            Vec::new(),
            context(0),
            Arc::new(cell),
        );

        table.kill(10, SIGTSTP).expect("request stop");
        table.kill(10, SIGCONT).expect("supersede stop");

        assert_eq!(
            table.get(10).expect("process").status,
            ProcessStatus::Running
        );
        let controls = receiver.pending();
        assert_eq!(controls.stopped, Some(false));
        receiver
            .acknowledge(controls)
            .expect("acknowledge final running state");
        assert_eq!(
            table.get(10).expect("process").status,
            ProcessStatus::Running
        );
        assert!(table
            .inner
            .lock_state()
            .entries
            .get(&identity.pid)
            .expect("process record")
            .pending_wait_events
            .is_empty());
    }

    #[test]
    fn resource_limits_cover_all_kinds_inherit_and_survive_exec() {
        let table = ProcessTable::with_zombie_ttl(Duration::from_secs(3600));
        let mut parent_context = context(0);
        parent_context.resource_limits = ProcessResourceLimits::with_open_files(256);
        table.register(10, "test", "parent", Vec::new(), parent_context, endpoint());

        let kinds = [
            ProcessResourceLimitKind::AddressSpace,
            ProcessResourceLimitKind::Core,
            ProcessResourceLimitKind::Cpu,
            ProcessResourceLimitKind::Data,
            ProcessResourceLimitKind::FileSize,
            ProcessResourceLimitKind::LockedMemory,
            ProcessResourceLimitKind::OpenFiles,
            ProcessResourceLimitKind::Processes,
            ProcessResourceLimitKind::ResidentSet,
            ProcessResourceLimitKind::Stack,
        ];
        for (index, kind) in kinds.into_iter().enumerate() {
            let hard = if kind == ProcessResourceLimitKind::OpenFiles {
                200
            } else {
                1_000 + index as u64
            };
            table
                .set_resource_limit(
                    10,
                    kind,
                    ProcessResourceLimit {
                        soft: Some(hard - 1),
                        hard: Some(hard),
                    },
                )
                .expect("set resource limit");
        }

        let child_context = table.inherited_context(10).expect("inherit context");
        table.register(11, "test", "child", Vec::new(), child_context, endpoint());
        table
            .exec(
                11,
                "test",
                "replacement",
                Vec::new(),
                BTreeMap::new(),
                String::from("/"),
                None,
            )
            .expect("exec child");

        for (index, kind) in kinds.into_iter().enumerate() {
            let hard = if kind == ProcessResourceLimitKind::OpenFiles {
                200
            } else {
                1_000 + index as u64
            };
            assert_eq!(
                table.get_resource_limit(11, kind).expect("get child limit"),
                ProcessResourceLimit {
                    soft: Some(hard - 1),
                    hard: Some(hard),
                }
            );
        }
    }
}
