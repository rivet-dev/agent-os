use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::error::Error;
use std::fmt;
use std::ops::{BitOr, BitOrAssign};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, WaitTimeoutResult, Weak};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const ZOMBIE_TTL: Duration = Duration::from_secs(60);
const INIT_PID: u32 = 1;
pub const SIGHUP: i32 = 1;
pub const SIGCHLD: i32 = 17;
pub const SIGCONT: i32 = 18;
pub const SIGSTOP: i32 = 19;
pub const SIGTERM: i32 = 15;
pub const SIGKILL: i32 = 9;
pub const SIGPIPE: i32 = 13;

pub type ProcessResult<T> = Result<T, ProcessTableError>;
pub type ProcessExitCallback = Arc<dyn Fn(i32) + Send + Sync + 'static>;

pub trait DriverProcess: Send + Sync {
    fn kill(&self, signal: i32);
    fn wait(&self, timeout: Duration) -> Option<i32>;
    fn set_on_exit(&self, callback: ProcessExitCallback);
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

    fn permission_denied(message: impl Into<String>) -> Self {
        Self {
            code: "EPERM",
            message: message.into(),
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
    pub fds: ProcessFileDescriptors,
}

impl Default for ProcessContext {
    fn default() -> Self {
        Self {
            pid: 0,
            ppid: 0,
            env: BTreeMap::new(),
            cwd: String::from("/"),
            fds: ProcessFileDescriptors::default(),
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
    pub exit_time_ms: Option<u64>,
    pub env: BTreeMap<String, String>,
    pub cwd: String,
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
}

#[derive(Clone)]
pub struct ProcessTable {
    inner: Arc<ProcessTableInner>,
}

struct ProcessTableInner {
    state: Mutex<ProcessTableState>,
    waiters: Condvar,
    reaper: Arc<ZombieReaper>,
}

struct ProcessRecord {
    entry: ProcessEntry,
    driver_process: Arc<dyn DriverProcess>,
    pending_wait_events: VecDeque<PendingWaitEvent>,
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
    wake: Condvar,
    thread_spawns: AtomicUsize,
}

#[derive(Default)]
struct ZombieReaperState {
    deadlines: BTreeMap<u32, Instant>,
    shutdown: bool,
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
            inner: {
                let inner = Arc::new(ProcessTableInner {
                    state: Mutex::new(ProcessTableState::default()),
                    waiters: Condvar::new(),
                    reaper,
                });
                start_zombie_reaper(Arc::downgrade(&inner), Arc::clone(&inner.reaper));
                inner
            },
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

    pub fn allocate_pid(&self) -> u32 {
        let mut state = self.inner.lock_state();
        let pid = state.next_pid;
        state.next_pid += 1;
        pid
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
        driver_process: Arc<dyn DriverProcess>,
    ) -> ProcessEntry {
        let (pgid, sid) = {
            let state = self.inner.lock_state();
            match state.entries.get(&ctx.ppid) {
                Some(parent) => (parent.entry.pgid, parent.entry.sid),
                None => (pid, pid),
            }
        };

        let entry = ProcessEntry {
            pid,
            ppid: ctx.ppid,
            pgid,
            sid,
            driver: driver.into(),
            command: command.into(),
            args,
            status: ProcessStatus::Running,
            exit_code: None,
            exit_time_ms: None,
            env: ctx.env,
            cwd: ctx.cwd,
        };

        let weak = Arc::downgrade(&self.inner);
        driver_process.set_on_exit(Arc::new(move |code| {
            if let Some(inner) = weak.upgrade() {
                mark_exited_inner(&inner, pid, code);
            }
        }));

        self.inner.lock_state().entries.insert(
            pid,
            ProcessRecord {
                entry: entry.clone(),
                driver_process,
                pending_wait_events: VecDeque::new(),
            },
        );

        entry
    }

    pub fn get(&self, pid: u32) -> Option<ProcessEntry> {
        self.inner
            .lock_state()
            .entries
            .get(&pid)
            .map(|record| record.entry.clone())
    }

    pub fn zombie_timer_count(&self) -> usize {
        self.inner.reaper.scheduled_count()
    }

    pub fn zombie_reaper_thread_spawn_count(&self) -> usize {
        self.inner.reaper.thread_spawn_count()
    }

    pub fn running_count(&self) -> usize {
        self.inner
            .lock_state()
            .entries
            .values()
            .filter(|record| record.entry.status == ProcessStatus::Running)
            .count()
    }

    pub fn mark_exited(&self, pid: u32, exit_code: i32) {
        mark_exited_inner(&self.inner, pid, exit_code);
    }

    pub fn mark_stopped(&self, pid: u32, signal: i32) {
        mark_wait_event_inner(
            &self.inner,
            pid,
            ProcessStatus::Stopped,
            PendingWaitEvent {
                status: signal,
                event: ProcessWaitEvent::Stopped,
            },
        );
    }

    pub fn mark_continued(&self, pid: u32) {
        mark_wait_event_inner(
            &self.inner,
            pid,
            ProcessStatus::Running,
            PendingWaitEvent {
                status: SIGCONT,
                event: ProcessWaitEvent::Continued,
            },
        );
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
                self.inner.waiters.notify_all();
                return Ok((pid, status));
            }

            state = self.inner.wait_for_state(state);
        }
    }

    pub fn waitpid_for(
        &self,
        waiter_pid: u32,
        pid: i32,
        flags: WaitPidFlags,
    ) -> ProcessResult<Option<ProcessWaitResult>> {
        let mut state = self.inner.lock_state();
        loop {
            let selector = resolve_wait_selector(&state, waiter_pid, pid)?;
            let matching_children = matching_child_pids(&state, waiter_pid, selector);
            if matching_children.is_empty() {
                return Err(ProcessTableError::no_matching_child(waiter_pid, pid));
            }

            if let Some(result) = take_waitable_event(&mut state, &matching_children, flags) {
                let should_reap = result.event == ProcessWaitEvent::Exited;
                drop(state);
                if should_reap {
                    self.inner.reaper.cancel(result.pid);
                    self.inner.waiters.notify_all();
                }
                return Ok(Some(result));
            }

            if flags.contains(WaitPidFlags::WNOHANG) {
                return Ok(None);
            }

            state = self.inner.wait_for_state(state);
        }
    }

    pub fn kill(&self, pid: i32, signal: i32) -> ProcessResult<()> {
        if !(0..=64).contains(&signal) {
            return Err(ProcessTableError::invalid_signal(signal));
        }

        let targets = {
            let state = self.inner.lock_state();
            if pid < 0 {
                let pgid = pid.unsigned_abs();
                let grouped: Vec<_> = state
                    .entries
                    .values()
                    .filter(|record| record.entry.pgid == pgid)
                    .map(|record| Arc::clone(&record.driver_process))
                    .collect();
                if grouped.is_empty() {
                    return Err(ProcessTableError::no_such_process_group(pgid));
                }
                grouped
            } else {
                let pid = pid as u32;
                let Some(record) = state.entries.get(&pid) else {
                    return Err(ProcessTableError::no_such_process(pid));
                };
                if record.entry.status == ProcessStatus::Exited || signal == 0 {
                    return Ok(());
                }
                vec![Arc::clone(&record.driver_process)]
            }
        };

        if signal == 0 {
            return Ok(());
        }

        for driver in targets {
            driver.kill(signal);
        }
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

    pub fn has_process_group(&self, pgid: u32) -> bool {
        self.inner
            .lock_state()
            .entries
            .values()
            .any(|record| record.entry.pgid == pgid && record.entry.status != ProcessStatus::Exited)
    }

    pub fn list_processes(&self) -> BTreeMap<u32, ProcessInfo> {
        self.inner
            .lock_state()
            .entries
            .values()
            .map(|record| (record.entry.pid, to_process_info(&record.entry)))
            .collect()
    }

    pub fn terminate_all(&self) {
        let running = {
            let mut state = self.inner.lock_state();
            state.terminating_all = true;
            self.inner.reaper.clear();
            state
                .entries
                .values()
                .filter(|record| record.entry.status == ProcessStatus::Running)
                .map(|record| (record.entry.pid, Arc::clone(&record.driver_process)))
                .collect::<Vec<_>>()
        };

        for (_, driver) in &running {
            driver.kill(SIGTERM);
        }
        for (pid, driver) in &running {
            if let Some(exit_code) = driver.wait(Duration::from_secs(1)) {
                self.mark_exited(*pid, exit_code);
            }
        }

        let survivors = {
            let state = self.inner.lock_state();
            running
                .iter()
                .filter(|(pid, _)| {
                    state
                        .entries
                        .get(pid)
                        .map(|record| record.entry.status == ProcessStatus::Running)
                        .unwrap_or(false)
                })
                .cloned()
                .collect::<Vec<_>>()
        };

        for (_, driver) in &survivors {
            driver.kill(SIGKILL);
        }
        for (pid, driver) in &survivors {
            if let Some(exit_code) = driver.wait(Duration::from_millis(500)) {
                self.mark_exited(*pid, exit_code);
            }
        }

        self.inner.lock_state().terminating_all = false;
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
    }
}

fn mark_exited_inner(inner: &Arc<ProcessTableInner>, pid: u32, exit_code: i32) {
    let (callback, zombie_ttl, should_schedule, parent_driver, orphaned_group_targets) = {
        let mut state = inner.lock_state();
        let (ppid, pgid) = {
            let Some(record) = state.entries.get_mut(&pid) else {
                return;
            };

            if record.entry.status == ProcessStatus::Exited {
                return;
            }

            record.entry.status = ProcessStatus::Exited;
            record.entry.exit_code = Some(exit_code);
            record.entry.exit_time_ms = Some(now_ms());
            let ppid = record.entry.ppid;
            let pgid = record.entry.pgid;
            (ppid, pgid)
        };
        let mut affected_pgids = BTreeSet::from([pgid]);
        reparent_children_to_init(&mut state, pid, &mut affected_pgids);

        let orphaned_group_targets = collect_orphaned_group_signal_targets(&state, &affected_pgids);

        let should_schedule = !state.terminating_all;
        let parent_driver = if should_schedule {
            state
                .entries
                .get(&ppid)
                .filter(|parent| parent.entry.status == ProcessStatus::Running)
                .map(|parent| Arc::clone(&parent.driver_process))
        } else {
            None
        };

        (
            state.on_process_exit.clone(),
            state.zombie_ttl,
            should_schedule,
            parent_driver,
            orphaned_group_targets,
        )
    };

    if should_schedule {
        inner.reaper.schedule(pid, zombie_ttl);
    } else {
        inner.reaper.cancel(pid);
    }

    if let Some(parent_driver) = parent_driver {
        parent_driver.kill(SIGCHLD);
    }

    for driver in &orphaned_group_targets {
        driver.kill(SIGHUP);
    }
    for driver in &orphaned_group_targets {
        driver.kill(SIGCONT);
    }

    if let Some(on_process_exit) = callback {
        on_process_exit(pid);
    }

    inner.waiters.notify_all();
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
) -> Vec<Arc<dyn DriverProcess>> {
    let mut targets = Vec::new();
    for &pgid in candidate_pgids {
        if !process_group_is_orphaned(state, pgid) || !process_group_has_stopped_member(state, pgid)
        {
            continue;
        }

        for record in state.entries.values() {
            if record.entry.pgid == pgid && record.entry.status != ProcessStatus::Exited {
                targets.push(Arc::clone(&record.driver_process));
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
    next_status: ProcessStatus,
    event: PendingWaitEvent,
) {
    let parent_driver = {
        let mut state = inner.lock_state();
        let ppid = {
            let Some(record) = state.entries.get_mut(&pid) else {
                return;
            };

            if record.entry.status == ProcessStatus::Exited || record.entry.status == next_status {
                return;
            }

            record.entry.status = next_status;
            record.pending_wait_events.push_back(event);
            record.entry.ppid
        };

        state
            .entries
            .get(&ppid)
            .filter(|parent| parent.entry.status == ProcessStatus::Running)
            .map(|parent| Arc::clone(&parent.driver_process))
    };

    if let Some(parent_driver) = parent_driver {
        parent_driver.kill(SIGCHLD);
    }

    inner.waiters.notify_all();
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

fn take_waitable_event(
    state: &mut ProcessTableState,
    matching_children: &[u32],
    flags: WaitPidFlags,
) -> Option<ProcessWaitResult> {
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
                non_exit_result = Some(ProcessWaitResult {
                    pid: *child_pid,
                    status: event.status,
                    event: event.event,
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
            return Some(ProcessWaitResult {
                pid: *child_pid,
                status: record.entry.exit_code.unwrap_or_default(),
                event: ProcessWaitEvent::Exited,
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

fn start_zombie_reaper(inner: Weak<ProcessTableInner>, reaper: Arc<ZombieReaper>) {
    reaper.thread_spawns.fetch_add(1, Ordering::SeqCst);
    thread::spawn(move || loop {
        let Some(pid) = reaper.take_next_due_pid() else {
            return;
        };

        let Some(inner) = inner.upgrade() else {
            return;
        };

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
        inner.waiters.notify_all();
    });
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
            wake: Condvar::new(),
            thread_spawns: AtomicUsize::new(0),
        }
    }
}

impl ZombieReaper {
    fn schedule(&self, pid: u32, ttl: Duration) {
        let mut state = lock_or_recover(&self.state);
        state.deadlines.insert(pid, Instant::now() + ttl);
        drop(state);
        self.wake.notify_all();
    }

    fn cancel(&self, pid: u32) {
        let mut state = lock_or_recover(&self.state);
        let removed = state.deadlines.remove(&pid).is_some();
        drop(state);
        if removed {
            self.wake.notify_all();
        }
    }

    fn clear(&self) {
        let mut state = lock_or_recover(&self.state);
        let changed = !state.deadlines.is_empty();
        state.deadlines.clear();
        drop(state);
        if changed {
            self.wake.notify_all();
        }
    }

    fn shutdown(&self) {
        let mut state = lock_or_recover(&self.state);
        state.shutdown = true;
        drop(state);
        self.wake.notify_all();
    }

    fn scheduled_count(&self) -> usize {
        lock_or_recover(&self.state).deadlines.len()
    }

    fn thread_spawn_count(&self) -> usize {
        self.thread_spawns.load(Ordering::SeqCst)
    }

    fn take_next_due_pid(&self) -> Option<u32> {
        let mut state = lock_or_recover(&self.state);
        loop {
            if state.shutdown {
                return None;
            }

            let Some((pid, deadline)) = state
                .deadlines
                .iter()
                .min_by_key(|(_, deadline)| **deadline)
                .map(|(&pid, &deadline)| (pid, deadline))
            else {
                state = wait_or_recover(&self.wake, state);
                continue;
            };

            let now = Instant::now();
            if deadline <= now {
                state.deadlines.remove(&pid);
                return Some(pid);
            }

            let timeout = deadline.saturating_duration_since(now);
            let (next_state, _) = wait_timeout_or_recover(&self.wake, state, timeout);
            state = next_state;
        }
    }
}

impl Drop for ProcessTableInner {
    fn drop(&mut self) {
        self.reaper.shutdown();
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
) -> (MutexGuard<'a, T>, WaitTimeoutResult) {
    match condvar.wait_timeout(guard, timeout) {
        Ok(result) => result,
        Err(poisoned) => poisoned.into_inner(),
    }
}
