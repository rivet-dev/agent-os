use crate::bridge::LifecycleState;
use crate::command_registry::{CommandDriver, CommandRegistry};
use crate::device_layer::{create_device_layer, DeviceLayer};
use crate::dns::{
    format_dns_resource, resolve_dns, resolve_dns_records, DnsConfig, DnsLookupPolicy,
    DnsRecordResolution, DnsResolution, DnsResolverErrorKind, HickoryDnsResolver,
    SharedDnsResolver,
};
use crate::fd_table::{
    AnonymousFile, AnonymousFileUsage, FdEntry, FdStat, FdTableError, FdTableManager,
    FileDescription, FileLockManager, FileLockTarget, FlockOperation, ProcessFdTable, RecordLock,
    RecordLockType, SharedAnonymousFile, TransferredFd, FD_CLOEXEC, FILETYPE_CHARACTER_DEVICE,
    FILETYPE_DIRECTORY, FILETYPE_PIPE, FILETYPE_REGULAR_FILE, FILETYPE_SOCKET_DGRAM,
    FILETYPE_SOCKET_STREAM, FILETYPE_SYMBOLIC_LINK, F_DUPFD, O_APPEND, O_CREAT, O_DIRECT,
    O_DIRECTORY, O_EXCL, O_NOFOLLOW, O_NONBLOCK, O_RDONLY, O_RDWR, O_TRUNC, O_WRONLY,
};
use crate::mount_table::{MountEntry, MountOptions, MountTable, MountedFileSystem};
use crate::network_policy::format_tcp_resource;
use crate::permissions::{
    check_command_execution, check_network_access, FsOperation, NetworkOperation, PermissionError,
    PermissionedFileSystem, Permissions,
};
use crate::pipe_manager::{PipeError, PipeManager};
use crate::poll::{
    PollEvents, PollFd, PollNotifier, PollResult, PollTarget, PollTargetEntry, PollTargetResult,
    POLLERR, POLLHUP, POLLIN, POLLNVAL, POLLOUT,
};
use crate::process_table::{
    DriverProcess, ProcessContext, ProcessExitCallback, ProcessInfo, ProcessStatus, ProcessTable,
    ProcessTableError, ProcessWaitResult, SigmaskHow, SignalSet, DEFAULT_PROCESS_UMASK, SIGCONT,
    SIGPIPE, SIGSTOP, SIGTSTP, SIGWINCH,
};
use crate::pty::{
    LineDisciplineConfig, PartialTermios, PtyError, PtyManager, PtyWindowSize, Termios,
};
use crate::resource_accounting::{
    measure_filesystem_usage, FileSystemStats, FileSystemUsage, ResourceAccountant, ResourceError,
    ResourceLimits, ResourceSnapshot, DEFAULT_MAX_OPEN_FDS,
};
use crate::root_fs::{
    encode_snapshot, RootFileSystem, RootFilesystemError, RootFilesystemSnapshot,
};
use crate::socket_table::{
    DatagramSocketOption, InetSocketAddress, OpaqueTransferredRight, ReceivedDatagram, SocketId,
    SocketMulticastMembership, SocketReadiness, SocketRecord, SocketShutdown, SocketSpec,
    SocketState, SocketTable, SocketTableError, SocketType, TransferredSocketRight,
};
use crate::user::{ProcessIdentity, UserConfig, UserManager};
use crate::vfs::{
    normalize_path, VfsError, VfsResult, VirtualDirEntry, VirtualFileSystem, VirtualStat,
    VirtualTimeSpec, VirtualUtimeSpec, MAX_PATH_LENGTH, RENAME_EXCHANGE, S_IFDIR, S_IFLNK, S_IFREG,
};
use hickory_proto::rr::RecordType;
use std::any::Any;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::error::Error;
use std::fmt;
#[cfg(test)]
use std::sync::OnceLock;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, WaitTimeoutResult};
use std::time::Duration;
use web_time::{Instant, SystemTime, UNIX_EPOCH};

pub type KernelResult<T> = Result<T, KernelError>;
pub use crate::process_table::{ProcessWaitEvent as WaitPidEvent, WaitPidFlags};

pub const SEEK_SET: u8 = 0;
pub const SEEK_CUR: u8 = 1;
pub const SEEK_END: u8 = 2;
const EXECUTABLE_PERMISSION_BITS: u32 = 0o111;
const SHEBANG_LINE_MAX_BYTES: usize = 256;
const MAX_EXEC_INTERPRETER_DEPTH: usize = 4;
const MAX_UNIX_SOCKET_SYMLINKS: usize = 40;
const UNIX_SOCKET_FILE_TYPE: u32 = 0o140000;
const UNIX_DAC_WRITE: u32 = 0o2;
const UNIX_DAC_SEARCH: u32 = 0o1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelError {
    code: &'static str,
    message: String,
}

impl KernelError {
    pub fn code(&self) -> &'static str {
        self.code
    }

    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn disposed() -> Self {
        Self::new("EINVAL", "kernel VM is disposed")
    }

    fn no_such_process(pid: u32) -> Self {
        Self::new("ESRCH", format!("no such process {pid}"))
    }

    fn bad_file_descriptor(fd: u32) -> Self {
        Self::new("EBADF", format!("bad file descriptor {fd}"))
    }

    fn permission_denied(message: impl Into<String>) -> Self {
        Self::new("EPERM", message)
    }

    fn command_not_found(command: &str) -> Self {
        Self::new("ENOENT", format!("command not found: {command}"))
    }
}

impl fmt::Display for KernelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl Error for KernelError {}

fn linux_shebang_interpreter(header: &[u8], path: &str) -> KernelResult<Option<String>> {
    if !header.starts_with(b"#!") {
        return Ok(None);
    }

    let payload = &header[2..];
    let newline = payload.iter().position(|byte| *byte == b'\n');
    let line = newline.map_or(payload, |index| &payload[..index]);
    let line_end = line
        .iter()
        .rposition(|byte| !matches!(*byte, b' ' | b'\t'))
        .map(|index| index + 1)
        .ok_or_else(|| KernelError::new("ENOEXEC", format!("invalid shebang line: {path}")))?;
    let line = &line[..line_end];
    let interpreter_start = line
        .iter()
        .position(|byte| !matches!(*byte, b' ' | b'\t'))
        .ok_or_else(|| KernelError::new("ENOEXEC", format!("invalid shebang line: {path}")))?;
    let interpreter_tail = &line[interpreter_start..];
    let separator = interpreter_tail
        .iter()
        .position(|byte| matches!(*byte, b' ' | b'\t'));

    // Linux accepts a truncated optional argument, but it rejects a shebang
    // whose interpreter pathname itself reaches the end of BINPRM_BUF_SIZE.
    if newline.is_none() && header.len() >= SHEBANG_LINE_MAX_BYTES && separator.is_none() {
        return Err(KernelError::new(
            "ENOEXEC",
            format!("shebang interpreter path exceeds the Linux header limit: {path}"),
        ));
    }

    let interpreter_end = separator.unwrap_or(interpreter_tail.len());
    let interpreter = std::str::from_utf8(&interpreter_tail[..interpreter_end])
        .map_err(|_| KernelError::new("ENOEXEC", format!("invalid shebang line: {path}")))?;
    if interpreter.is_empty() {
        return Err(KernelError::new(
            "ENOEXEC",
            format!("invalid shebang line: {path}"),
        ));
    }
    Ok(Some(interpreter.to_owned()))
}

#[derive(Clone)]
pub struct KernelVmConfig {
    pub vm_id: String,
    pub env: BTreeMap<String, String>,
    pub cwd: String,
    pub user: UserConfig,
    pub permissions: Permissions,
    pub loopback_exempt_ports: BTreeSet<u16>,
    pub dns: DnsConfig,
    pub dns_resolver: SharedDnsResolver,
    pub resources: ResourceLimits,
    pub zombie_ttl: Duration,
}

impl KernelVmConfig {
    pub fn new(vm_id: impl Into<String>) -> Self {
        Self {
            vm_id: vm_id.into(),
            env: BTreeMap::new(),
            cwd: String::from("/workspace"),
            user: UserConfig::default(),
            permissions: Permissions::default(),
            loopback_exempt_ports: BTreeSet::new(),
            dns: DnsConfig::default(),
            dns_resolver: Arc::new(HickoryDnsResolver::default()),
            resources: ResourceLimits::default(),
            zombie_ttl: Duration::from_secs(60),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SpawnOptions {
    pub requester_driver: Option<String>,
    pub parent_pid: Option<u32>,
    pub env: BTreeMap<String, String>,
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VirtualProcessOptions {
    pub parent_pid: Option<u32>,
    pub env: BTreeMap<String, String>,
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecOptions {
    pub requester_driver: Option<String>,
    pub parent_pid: Option<u32>,
    pub env: BTreeMap<String, String>,
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecursiveDirEntry {
    pub path: String,
    pub is_directory: bool,
    pub is_symbolic_link: bool,
    pub size: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenShellOptions {
    pub requester_driver: Option<String>,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitPidResult {
    pub pid: u32,
    pub status: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitPidEventResult {
    pub pid: u32,
    pub status: i32,
    pub event: WaitPidEvent,
}

#[derive(Debug)]
pub struct ReceivedFdMessage {
    pub payload: Vec<u8>,
    pub rights: Vec<ReceivedFdRight>,
    pub payload_truncated: bool,
    pub control_truncated: bool,
    pub full_length: usize,
}

#[derive(Clone)]
pub enum FdTransferRequest {
    Fd(u32),
    Opaque(OpaqueTransferredRight),
}

impl fmt::Debug for FdTransferRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fd(fd) => f.debug_tuple("Fd").field(fd).finish(),
            Self::Opaque(resource) => f
                .debug_tuple("Opaque")
                .field(&(Arc::as_ptr(resource) as *const ()))
                .finish(),
        }
    }
}

pub enum ReceivedFdRight {
    Fd(u32),
    Opaque(OpaqueTransferredRight),
}

impl fmt::Debug for ReceivedFdRight {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fd(fd) => f.debug_tuple("Fd").field(fd).finish(),
            Self::Opaque(resource) => f
                .debug_tuple("Opaque")
                .field(&(Arc::as_ptr(resource) as *const ()))
                .finish(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessFdSnapshotEntry {
    pub fd: u32,
    pub fd_flags: u32,
    pub status_flags: u32,
    pub filetype: u8,
    pub is_socket: bool,
    pub is_pipe: bool,
    pub is_pty: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessFdDirEntry {
    pub name: String,
    pub ino: u64,
    pub is_directory: bool,
    pub is_symbolic_link: bool,
}

/// The canonical VFS object selected by Linux-style AF_UNIX pathname lookup.
///
/// `canonical_path` preserves the actual directory reached through symlinks
/// while `stat` carries the `(dev, ino)` identity the sidecar must use to
/// distinguish socket nodes across mounts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnixSocketPathNode {
    pub canonical_path: String,
    pub stat: VirtualStat,
}

struct UnixSocketBindTarget {
    canonical_path: String,
    parent: UnixSocketPathNode,
    identity: ProcessIdentity,
}

#[derive(Debug, Clone)]
struct FdSocketEntry {
    description: Arc<FileDescription>,
    socket_id: SocketId,
    mode: u32,
    uid: u32,
    gid: u32,
}

type FdSocketRegistry = Arc<Mutex<BTreeMap<u64, FdSocketEntry>>>;

enum OpenFileRemovalBacking {
    Anonymous {
        descriptions: Vec<Arc<FileDescription>>,
        backing: SharedAnonymousFile,
    },
    LinkedAlias {
        descriptions: Vec<Arc<FileDescription>>,
        live_path: String,
    },
}

#[derive(Debug, Clone)]
struct ResolvedSpawnCommand {
    command: String,
    args: Vec<String>,
    driver: CommandDriver,
}

#[derive(Debug, Clone)]
struct ShebangCommand {
    interpreter: String,
    args: Vec<String>,
}

#[derive(Clone)]
pub struct KernelProcessHandle {
    pid: u32,
    driver: String,
    process: Arc<StubDriverProcess>,
}

impl fmt::Debug for KernelProcessHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KernelProcessHandle")
            .field("pid", &self.pid)
            .field("driver", &self.driver)
            .finish_non_exhaustive()
    }
}

impl KernelProcessHandle {
    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn driver(&self) -> &str {
        &self.driver
    }

    pub fn finish(&self, exit_code: i32) {
        self.process.finish(exit_code);
    }

    pub fn kill(&self, signal: i32) {
        self.process.kill(signal);
    }

    pub fn wait(&self, timeout: Duration) -> Option<i32> {
        self.process.wait(timeout)
    }

    pub fn kill_signals(&self) -> Vec<i32> {
        self.process.kill_signals()
    }
}

#[derive(Debug, Clone)]
pub struct OpenShellHandle {
    process: KernelProcessHandle,
    master_fd: u32,
    slave_fd: u32,
    pty_path: String,
}

impl OpenShellHandle {
    pub fn process(&self) -> &KernelProcessHandle {
        &self.process
    }

    pub fn pid(&self) -> u32 {
        self.process.pid()
    }

    pub fn master_fd(&self) -> u32 {
        self.master_fd
    }

    pub fn slave_fd(&self) -> u32 {
        self.slave_fd
    }

    pub fn pty_path(&self) -> &str {
        &self.pty_path
    }
}

pub struct KernelVm<F> {
    vm_id: String,
    boot_time_ms: u64,
    boot_instant: Instant,
    filesystem: PermissionedFileSystem<DeviceLayer<F>>,
    permissions: Permissions,
    loopback_exempt_ports: BTreeSet<u16>,
    dns: DnsConfig,
    dns_resolver: SharedDnsResolver,
    env: BTreeMap<String, String>,
    cwd: String,
    commands: CommandRegistry,
    fd_tables: Arc<Mutex<FdTableManager>>,
    processes: ProcessTable,
    pipes: PipeManager,
    ptys: PtyManager,
    sockets: SocketTable,
    fd_sockets: FdSocketRegistry,
    poll_notifier: PollNotifier,
    users: UserManager,
    resources: ResourceAccountant,
    filesystem_usage_cache: Option<FileSystemUsage>,
    no_posix_acl_cache: BTreeSet<(u64, u64, u64, u32, u32, u32, u32)>,
    anonymous_file_usage: Arc<AnonymousFileUsage>,
    file_locks: FileLockManager,
    unnamed_files: BTreeMap<u64, UnnamedFile>,
    next_unnamed_file_id: u64,
    driver_pids: Arc<Mutex<BTreeMap<String, BTreeSet<u32>>>>,
    terminated: bool,
}

const UNNAMED_FILE_PREFIX: &str = ".agentos-tmpfile-";

#[derive(Debug, Clone)]
struct UnnamedFile {
    path: String,
    linkable: bool,
}

pub fn is_internal_unnamed_file_name(name: &str) -> bool {
    name.starts_with(UNNAMED_FILE_PREFIX)
}

// Cleanup spans every independently owned kernel resource table. Keeping the
// tables explicit makes teardown ordering visible at the call sites.
#[allow(clippy::too_many_arguments)]
fn cleanup_process_resources(
    fd_tables: &Mutex<FdTableManager>,
    file_locks: &FileLockManager,
    pipes: &PipeManager,
    ptys: &PtyManager,
    sockets: &SocketTable,
    fd_sockets: &FdSocketRegistry,
    driver_pids: &Mutex<BTreeMap<String, BTreeSet<u32>>>,
    pid: u32,
) {
    let mut cleanup = Vec::new();
    {
        let mut tables = lock_or_recover(fd_tables);
        let descriptors = tables
            .get(pid)
            .map(|table| {
                table
                    .iter()
                    .map(|entry| (entry.fd, Arc::clone(&entry.description), entry.filetype))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        cleanup_process_resources_test_hook();

        if let Some(table) = tables.get_mut(pid) {
            for (fd, description, filetype) in &descriptors {
                table.close(*fd);
                cleanup.push((Arc::clone(description), *filetype));
            }
        }
        tables.remove(pid);
    }

    for (description, filetype) in cleanup {
        close_special_resource_if_needed(
            file_locks,
            pipes,
            ptys,
            sockets,
            fd_sockets,
            &description,
            filetype,
        );
    }
    file_locks.release_process(pid);

    sockets.remove_all_for_pid(pid);

    let mut owners = lock_or_recover(driver_pids);
    for pids in owners.values_mut() {
        pids.remove(&pid);
    }
}

fn dispose_kernel_vm_resources<F>(kernel: &mut KernelVm<F>) {
    kernel.processes.terminate_all();
    let pids = lock_or_recover(&kernel.fd_tables).pids();
    for pid in pids {
        cleanup_process_resources(
            kernel.fd_tables.as_ref(),
            &kernel.file_locks,
            &kernel.pipes,
            &kernel.ptys,
            &kernel.sockets,
            &kernel.fd_sockets,
            kernel.driver_pids.as_ref(),
            pid,
        );
    }
    lock_or_recover(&kernel.driver_pids).clear();
    kernel.terminated = true;
}

#[cfg(test)]
type CleanupProcessResourcesHook = Arc<dyn Fn() + Send + Sync + 'static>;

#[cfg(test)]
fn cleanup_process_resources_test_hook() {
    let hook = lock_or_recover(cleanup_process_resources_test_hook_slot()).clone();
    if let Some(hook) = hook {
        hook();
    }
}

#[cfg(not(test))]
fn cleanup_process_resources_test_hook() {}

#[cfg(test)]
fn cleanup_process_resources_test_hook_slot() -> &'static Mutex<Option<CleanupProcessResourcesHook>>
{
    static HOOK: OnceLock<Mutex<Option<CleanupProcessResourcesHook>>> = OnceLock::new();
    HOOK.get_or_init(|| Mutex::new(None))
}

#[cfg(test)]
fn set_cleanup_process_resources_test_hook(hook: Option<CleanupProcessResourcesHook>) {
    *lock_or_recover(cleanup_process_resources_test_hook_slot()) = hook;
}

fn close_special_resource_if_needed(
    file_locks: &FileLockManager,
    pipes: &PipeManager,
    ptys: &PtyManager,
    sockets: &SocketTable,
    fd_sockets: &FdSocketRegistry,
    description: &Arc<FileDescription>,
    filetype: u8,
) {
    if description.ref_count() != 0 {
        return;
    }

    file_locks.release_owner(description.id());

    if filetype == FILETYPE_PIPE && pipes.is_pipe(description.id()) {
        pipes.close(description.id());
    }

    if ptys.is_pty(description.id()) {
        ptys.close(description.id());
    }

    prune_fd_sockets(sockets, fd_sockets);
}

fn prune_fd_sockets(sockets: &SocketTable, fd_sockets: &FdSocketRegistry) {
    loop {
        let socket_ids = {
            let mut registry = lock_or_recover(fd_sockets);
            let closed = registry
                .iter()
                .filter_map(|(description_id, entry)| {
                    (entry.description.ref_count() == 0)
                        .then_some((*description_id, entry.socket_id))
                })
                .collect::<Vec<_>>();
            for (description_id, _) in &closed {
                registry.remove(description_id);
            }
            closed
                .into_iter()
                .map(|(_, socket_id)| socket_id)
                .collect::<Vec<_>>()
        };
        if socket_ids.is_empty() {
            return;
        }
        for socket_id in socket_ids {
            if let Err(error) = sockets.remove(socket_id) {
                eprintln!(
                    "[agentos] failed to remove closed descriptor-owned socket {socket_id}: {error}"
                );
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProcNode {
    RootDir,
    MountsFile,
    CpuInfoFile,
    MemInfoFile,
    LoadAvgFile,
    UptimeFile,
    VersionFile,
    SelfLink { pid: u32 },
    PidDir { pid: u32 },
    PidFdDir { pid: u32 },
    PidCmdline { pid: u32 },
    PidEnviron { pid: u32 },
    PidCwdLink { pid: u32 },
    PidStatFile { pid: u32 },
    PidStatusFile { pid: u32 },
    PidFdLink { pid: u32, fd: u32 },
}

impl<F: VirtualFileSystem + 'static> KernelVm<F> {
    pub fn new(filesystem: F, config: KernelVmConfig) -> Self {
        let vm_id = config.vm_id;
        let boot_time_ms = now_ms();
        let boot_instant = Instant::now();
        let permissions = config.permissions.clone();
        let users = UserManager::from_config(config.user);
        let process_table = ProcessTable::with_zombie_ttl(config.zombie_ttl);
        let process_table_for_pty = process_table.clone();
        let max_open_fds = config
            .resources
            .max_open_fds
            .unwrap_or(DEFAULT_MAX_OPEN_FDS);
        let fd_tables = Arc::new(Mutex::new(FdTableManager::with_max_fds(max_open_fds)));
        // A descriptor may own several disjoint byte ranges. Keep the
        // VM-wide table bounded and use the existing open-fd resource knob as
        // the explicit way to raise that derived limit.
        let file_locks =
            FileLockManager::with_record_lock_limit(max_open_fds.saturating_mul(16).max(16));
        let driver_pids = Arc::new(Mutex::new(BTreeMap::new()));
        let poll_notifier = PollNotifier::default();
        let pipes = PipeManager::with_notifier(poll_notifier.clone());
        let ptys = PtyManager::with_signal_handler_and_notifier(
            Arc::new(move |pgid, signal| {
                let _ = process_table_for_pty.kill(-(pgid as i32), signal);
            }),
            poll_notifier.clone(),
        );
        let sockets = SocketTable::new();
        let fd_sockets = Arc::new(Mutex::new(BTreeMap::new()));

        let fd_tables_for_exit = Arc::clone(&fd_tables);
        let file_locks_for_exit = file_locks.clone();
        let driver_pids_for_exit = Arc::clone(&driver_pids);
        let pipes_for_exit = pipes.clone();
        let ptys_for_exit = ptys.clone();
        let sockets_for_exit = sockets.clone();
        let fd_sockets_for_exit = Arc::clone(&fd_sockets);
        process_table.set_on_process_exit(Some(Arc::new(move |pid| {
            cleanup_process_resources(
                fd_tables_for_exit.as_ref(),
                &file_locks_for_exit,
                &pipes_for_exit,
                &ptys_for_exit,
                &sockets_for_exit,
                &fd_sockets_for_exit,
                driver_pids_for_exit.as_ref(),
                pid,
            );
        })));

        let filesystem = PermissionedFileSystem::new(
            create_device_layer(filesystem),
            vm_id.clone(),
            permissions.clone(),
        );
        // Usage accounting is kernel-internal: the cache is populated lazily by
        // `filesystem_usage()` through the RAW filesystem so no guest-attributable
        // permission check fires at construction (or ever) for quota bookkeeping.
        let filesystem_usage_cache = None;
        let anonymous_file_usage = Arc::new(AnonymousFileUsage::default());

        Self {
            vm_id: vm_id.clone(),
            boot_time_ms,
            boot_instant,
            filesystem,
            permissions,
            loopback_exempt_ports: config.loopback_exempt_ports,
            dns: config.dns,
            dns_resolver: config.dns_resolver,
            env: config.env,
            cwd: config.cwd,
            commands: CommandRegistry::new(),
            fd_tables,
            processes: process_table,
            pipes,
            ptys,
            sockets,
            fd_sockets,
            poll_notifier,
            users,
            resources: ResourceAccountant::new(config.resources),
            filesystem_usage_cache,
            no_posix_acl_cache: BTreeSet::new(),
            anonymous_file_usage,
            file_locks,
            unnamed_files: BTreeMap::new(),
            next_unnamed_file_id: 0,
            driver_pids,
            terminated: false,
        }
    }

    pub fn vm_id(&self) -> &str {
        &self.vm_id
    }

    pub fn state(&self) -> LifecycleState {
        if self.terminated {
            LifecycleState::Terminated
        } else if self.processes.running_count() > 0 {
            LifecycleState::Busy
        } else {
            LifecycleState::Ready
        }
    }

    pub fn commands(&self) -> BTreeMap<String, String> {
        self.commands.list()
    }

    pub fn filesystem(&self) -> &PermissionedFileSystem<DeviceLayer<F>> {
        &self.filesystem
    }

    pub fn filesystem_mut(&mut self) -> &mut PermissionedFileSystem<DeviceLayer<F>> {
        &mut self.filesystem
    }

    pub fn user_manager(&self) -> &UserManager {
        &self.users
    }

    pub fn environment(&self) -> &BTreeMap<String, String> {
        &self.env
    }

    pub fn process_identity(
        &self,
        requester_driver: &str,
        pid: u32,
    ) -> KernelResult<ProcessIdentity> {
        self.assert_driver_owns(requester_driver, pid)?;
        Ok(self
            .processes
            .get(pid)
            .ok_or_else(|| KernelError::no_such_process(pid))?
            .identity)
    }

    pub fn user_profile(&self) -> UserManager {
        self.users.clone()
    }

    pub fn getuid(&self, requester_driver: &str, pid: u32) -> KernelResult<u32> {
        Ok(self.process_identity(requester_driver, pid)?.uid)
    }

    pub fn getgid(&self, requester_driver: &str, pid: u32) -> KernelResult<u32> {
        Ok(self.process_identity(requester_driver, pid)?.gid)
    }

    pub fn geteuid(&self, requester_driver: &str, pid: u32) -> KernelResult<u32> {
        Ok(self.process_identity(requester_driver, pid)?.euid)
    }

    pub fn getegid(&self, requester_driver: &str, pid: u32) -> KernelResult<u32> {
        Ok(self.process_identity(requester_driver, pid)?.egid)
    }

    pub fn getgroups(&self, requester_driver: &str, pid: u32) -> KernelResult<Vec<u32>> {
        Ok(self
            .process_identity(requester_driver, pid)?
            .supplementary_gids)
    }

    pub fn getresuid(&self, requester_driver: &str, pid: u32) -> KernelResult<(u32, u32, u32)> {
        let identity = self.process_identity(requester_driver, pid)?;
        Ok((identity.uid, identity.euid, identity.suid))
    }

    pub fn getresgid(&self, requester_driver: &str, pid: u32) -> KernelResult<(u32, u32, u32)> {
        let identity = self.process_identity(requester_driver, pid)?;
        Ok((identity.gid, identity.egid, identity.sgid))
    }

    pub fn setuid(&self, requester_driver: &str, pid: u32, uid: u32) -> KernelResult<()> {
        let current = self.process_identity(requester_driver, pid)?;
        if current.euid == 0 {
            self.setresuid(requester_driver, pid, Some(uid), Some(uid), Some(uid))
        } else if uid == current.uid || uid == current.suid {
            self.setresuid(requester_driver, pid, None, Some(uid), None)
        } else {
            Err(credential_transition_denied("setuid", uid))
        }
    }

    pub fn seteuid(&self, requester_driver: &str, pid: u32, euid: u32) -> KernelResult<()> {
        self.setresuid(requester_driver, pid, None, Some(euid), None)
    }

    pub fn setreuid(
        &self,
        requester_driver: &str,
        pid: u32,
        uid: Option<u32>,
        euid: Option<u32>,
    ) -> KernelResult<()> {
        let current = self.process_identity(requester_driver, pid)?;
        let next_uid = uid.unwrap_or(current.uid);
        let next_euid = euid.unwrap_or(current.euid);
        let update_saved = uid.is_some() || (euid.is_some() && next_euid != current.uid);
        self.setresuid(
            requester_driver,
            pid,
            uid,
            euid,
            update_saved.then_some(next_euid),
        )?;
        debug_assert_eq!(self.process_identity(requester_driver, pid)?.uid, next_uid);
        Ok(())
    }

    pub fn setresuid(
        &self,
        requester_driver: &str,
        pid: u32,
        uid: Option<u32>,
        euid: Option<u32>,
        suid: Option<u32>,
    ) -> KernelResult<()> {
        let mut identity = self.process_identity(requester_driver, pid)?;
        if identity.euid != 0 {
            let allowed = [identity.uid, identity.euid, identity.suid];
            for requested in [uid, euid, suid].into_iter().flatten() {
                if !allowed.contains(&requested) {
                    return Err(credential_transition_denied("setresuid", requested));
                }
            }
        }
        if let Some(uid) = uid {
            identity.uid = uid;
        }
        if let Some(euid) = euid {
            identity.euid = euid;
        }
        if let Some(suid) = suid {
            identity.suid = suid;
        }
        self.processes.set_identity(pid, identity)?;
        Ok(())
    }

    pub fn setgid(&self, requester_driver: &str, pid: u32, gid: u32) -> KernelResult<()> {
        let current = self.process_identity(requester_driver, pid)?;
        if current.euid == 0 {
            self.setresgid(requester_driver, pid, Some(gid), Some(gid), Some(gid))
        } else if gid == current.gid || gid == current.sgid {
            self.setresgid(requester_driver, pid, None, Some(gid), None)
        } else {
            Err(credential_transition_denied("setgid", gid))
        }
    }

    pub fn setegid(&self, requester_driver: &str, pid: u32, egid: u32) -> KernelResult<()> {
        self.setresgid(requester_driver, pid, None, Some(egid), None)
    }

    pub fn setregid(
        &self,
        requester_driver: &str,
        pid: u32,
        gid: Option<u32>,
        egid: Option<u32>,
    ) -> KernelResult<()> {
        let current = self.process_identity(requester_driver, pid)?;
        let next_egid = egid.unwrap_or(current.egid);
        let update_saved = gid.is_some() || (egid.is_some() && next_egid != current.gid);
        self.setresgid(
            requester_driver,
            pid,
            gid,
            egid,
            update_saved.then_some(next_egid),
        )
    }

    pub fn setresgid(
        &self,
        requester_driver: &str,
        pid: u32,
        gid: Option<u32>,
        egid: Option<u32>,
        sgid: Option<u32>,
    ) -> KernelResult<()> {
        let mut identity = self.process_identity(requester_driver, pid)?;
        if identity.euid != 0 {
            let allowed = [identity.gid, identity.egid, identity.sgid];
            for requested in [gid, egid, sgid].into_iter().flatten() {
                if !allowed.contains(&requested) {
                    return Err(credential_transition_denied("setresgid", requested));
                }
            }
        }
        if let Some(gid) = gid {
            identity.gid = gid;
        }
        if let Some(egid) = egid {
            identity.egid = egid;
        }
        if let Some(sgid) = sgid {
            identity.sgid = sgid;
        }
        self.processes.set_identity(pid, identity)?;
        Ok(())
    }

    pub fn setgroups(
        &self,
        requester_driver: &str,
        pid: u32,
        groups: Vec<u32>,
    ) -> KernelResult<()> {
        const MAX_SUPPLEMENTARY_GROUPS: usize = 64;
        let mut identity = self.process_identity(requester_driver, pid)?;
        if identity.euid != 0 {
            return Err(KernelError::new(
                "EPERM",
                "setgroups requires effective uid 0",
            ));
        }
        if groups.len() > MAX_SUPPLEMENTARY_GROUPS {
            return Err(KernelError::new(
                "EINVAL",
                format!(
                    "setgroups count {} exceeds limit {MAX_SUPPLEMENTARY_GROUPS}",
                    groups.len()
                ),
            ));
        }
        let mut normalized = Vec::with_capacity(groups.len());
        for gid in groups {
            if !normalized.contains(&gid) {
                normalized.push(gid);
            }
        }
        identity.supplementary_gids = normalized;
        self.processes.set_identity(pid, identity)?;
        Ok(())
    }

    pub fn switch_user(&self, requester_driver: &str, pid: u32, uid: u32) -> KernelResult<()> {
        let current = self.process_identity(requester_driver, pid)?;
        if current.euid != 0 {
            return Err(KernelError::new(
                "EPERM",
                "switch_user requires effective uid 0",
            ));
        }
        let account = self
            .users
            .account(uid)
            .cloned()
            .ok_or_else(|| KernelError::new("ENOENT", format!("unknown uid {uid}")))?;
        let identity = ProcessIdentity {
            uid: account.uid,
            gid: account.gid,
            euid: account.uid,
            egid: account.gid,
            suid: account.uid,
            sgid: account.gid,
            supplementary_gids: account.supplementary_gids,
        };
        self.processes.set_identity(pid, identity)?;
        Ok(())
    }

    pub fn getpwuid(&self, uid: u32) -> KernelResult<String> {
        self.users
            .getpwuid(uid)
            .ok_or_else(|| KernelError::new("ENOENT", format!("unknown uid {uid}")))
    }

    pub fn getpwnam(&self, username: &str) -> KernelResult<String> {
        self.users
            .getpwnam(username)
            .ok_or_else(|| KernelError::new("ENOENT", format!("unknown user {username}")))
    }

    pub fn getpwent(&self, index: usize) -> KernelResult<String> {
        self.users
            .passwd_entries()
            .get(index)
            .cloned()
            .ok_or_else(|| KernelError::new("ENOENT", "end of passwd database"))
    }

    pub fn getgrgid(&self, gid: u32) -> KernelResult<String> {
        self.users
            .getgrgid(gid)
            .ok_or_else(|| KernelError::new("ENOENT", format!("unknown gid {gid}")))
    }

    pub fn getgrnam(&self, name: &str) -> KernelResult<String> {
        self.users
            .getgrnam(name)
            .ok_or_else(|| KernelError::new("ENOENT", format!("unknown group {name}")))
    }

    pub fn getgrent(&self, index: usize) -> KernelResult<String> {
        self.users
            .group_entries()
            .get(index)
            .cloned()
            .ok_or_else(|| KernelError::new("ENOENT", "end of group database"))
    }

    pub fn resource_snapshot(&self) -> ResourceSnapshot {
        let fd_tables = lock_or_recover(&self.fd_tables);
        self.resources.snapshot(
            &self.processes,
            &fd_tables,
            &self.pipes,
            &self.ptys,
            &self.sockets,
        )
    }

    pub fn resource_limits(&self) -> &ResourceLimits {
        self.resources.limits()
    }

    pub fn set_permissions(&mut self, permissions: Permissions) {
        self.filesystem.set_permissions(permissions.clone());
        self.permissions = permissions;
    }

    pub fn set_loopback_exempt_ports(&mut self, ports: BTreeSet<u16>) {
        self.loopback_exempt_ports = ports;
    }

    pub fn extend_loopback_exempt_ports(&mut self, ports: impl IntoIterator<Item = u16>) {
        self.loopback_exempt_ports.extend(ports);
    }

    pub fn resolve_dns(
        &self,
        hostname: &str,
        policy: DnsLookupPolicy,
    ) -> KernelResult<DnsResolution> {
        self.assert_not_terminated()?;
        if matches!(policy, DnsLookupPolicy::CheckPermissions) {
            let resource = format_dns_resource(hostname).map_err(map_dns_resolver_error)?;
            check_network_access(
                &self.vm_id,
                &self.permissions,
                NetworkOperation::Dns,
                &resource,
            )?;
        }

        resolve_dns(&self.dns, self.dns_resolver.as_ref(), hostname).map_err(map_dns_resolver_error)
    }

    pub fn resolve_dns_records(
        &self,
        hostname: &str,
        record_type: RecordType,
        policy: DnsLookupPolicy,
    ) -> KernelResult<DnsRecordResolution> {
        self.assert_not_terminated()?;
        if matches!(policy, DnsLookupPolicy::CheckPermissions) {
            let resource = format_dns_resource(hostname).map_err(map_dns_resolver_error)?;
            check_network_access(
                &self.vm_id,
                &self.permissions,
                NetworkOperation::Dns,
                &resource,
            )?;
        }

        resolve_dns_records(&self.dns, self.dns_resolver.as_ref(), hostname, record_type)
            .map_err(map_dns_resolver_error)
    }

    pub fn register_driver(&mut self, driver: CommandDriver) -> KernelResult<()> {
        self.assert_not_terminated()?;
        let driver_name = driver.name().to_owned();
        let populate_driver = driver.clone();
        self.commands.register(driver)?;
        lock_or_recover(&self.driver_pids)
            .entry(driver_name)
            .or_default();
        self.commands
            .populate_driver_bin(&mut self.filesystem, &populate_driver)?;
        Ok(())
    }

    pub fn exec(
        &mut self,
        command: &str,
        options: ExecOptions,
    ) -> KernelResult<KernelProcessHandle> {
        self.spawn_process(
            "sh",
            vec![String::from("-c"), String::from(command)],
            SpawnOptions {
                requester_driver: options.requester_driver,
                parent_pid: options.parent_pid,
                env: options.env,
                cwd: options.cwd,
            },
        )
    }

    pub fn open_shell(&mut self, options: OpenShellOptions) -> KernelResult<OpenShellHandle> {
        let command = options.command.unwrap_or_else(|| String::from("sh"));
        let requester_driver = options.requester_driver.clone();
        let process = self.spawn_process(
            &command,
            options.args,
            SpawnOptions {
                requester_driver: requester_driver.clone(),
                parent_pid: None,
                env: options.env,
                cwd: options.cwd,
            },
        )?;
        let owner = requester_driver.as_deref().unwrap_or(process.driver());
        let (master_fd, slave_fd, pty_path) = self.open_pty(owner, process.pid())?;
        self.setpgid(owner, process.pid(), process.pid())?;
        self.pty_set_foreground_pgid(owner, process.pid(), master_fd, process.pid())?;
        Ok(OpenShellHandle {
            process,
            master_fd,
            slave_fd,
            pty_path,
        })
    }

    pub fn read_file(&mut self, path: &str) -> KernelResult<Vec<u8>> {
        self.assert_not_terminated()?;
        self.read_file_internal(None, path)
    }

    pub fn pread_file(&mut self, path: &str, offset: u64, length: usize) -> KernelResult<Vec<u8>> {
        self.assert_not_terminated()?;
        self.reject_unix_socket_data_path(path, "ENXIO")?;
        self.resources.check_pread_length(length)?;
        Ok(VirtualFileSystem::pread(
            &mut self.filesystem,
            path,
            offset,
            length,
        )?)
    }

    pub fn pread_file_for_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        path: &str,
        offset: u64,
        length: usize,
    ) -> KernelResult<Vec<u8>> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        self.check_dac_access(pid, path, DAC_READ)?;
        self.reject_unix_socket_data_path(path, "ENXIO")?;
        self.resources.check_pread_length(length)?;
        Ok(VirtualFileSystem::pread(
            &mut self.filesystem,
            path,
            offset,
            length,
        )?)
    }

    pub fn read_file_for_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        path: &str,
    ) -> KernelResult<Vec<u8>> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        self.check_dac_access(pid, path, DAC_READ)?;
        self.read_file_internal(Some(pid), path)
    }

    pub fn write_file(&mut self, path: &str, content: impl Into<Vec<u8>>) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.reject_read_only_resolved_write_path(path)?;
        self.reject_unix_socket_data_path(path, "ENXIO")?;
        let content = content.into();
        let new_size = content.len() as u64;
        let existing = self.storage_stat(path)?;
        self.check_write_file_limits_with_existing(path, existing.as_ref(), new_size)?;
        self.filesystem.write_file(path, content)?;
        self.update_filesystem_usage_cache_for_write(path, existing.as_ref(), new_size);
        Ok(())
    }

    /// Resolve the canonical candidate for an AF_UNIX pathname bind without
    /// mutating the filesystem.
    ///
    /// This is the preflight used by sidecars to enforce host-mount policy on
    /// the actual symlink-resolved destination before the socket inode is
    /// created. The subsequent bind call repeats every lookup and DAC check.
    pub fn resolve_unix_socket_bind_target_for_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        cwd: &str,
        path: &str,
    ) -> KernelResult<String> {
        Ok(self
            .resolve_unix_socket_bind_target(requester_driver, pid, cwd, path)?
            .canonical_path)
    }

    fn resolve_unix_socket_bind_target(
        &mut self,
        requester_driver: &str,
        pid: u32,
        cwd: &str,
        path: &str,
    ) -> KernelResult<UnixSocketBindTarget> {
        self.assert_not_terminated()?;
        let identity = self.process_identity(requester_driver, pid)?;
        let (absolute_path, mut components, trailing_slash) =
            unix_socket_absolute_components(cwd, path)?;

        let Some(basename) = components.pop_back() else {
            return Err(unix_socket_address_in_use(&absolute_path));
        };

        // A trailing slash or a final dot component asks pathname lookup for
        // an existing directory. bind(2) cannot replace that entry: Linux
        // reports EADDRINUSE when it resolves and the lookup error otherwise.
        if trailing_slash || matches!(basename.as_str(), "." | "..") {
            let (_, full_components, _) = unix_socket_absolute_components(cwd, path)?;
            resolve_unix_socket_components(
                self.raw_filesystem_mut(),
                &identity,
                full_components,
                false,
                false,
            )?;
            return Err(unix_socket_address_in_use(&absolute_path));
        }

        let parent = resolve_unix_socket_components(
            self.raw_filesystem_mut(),
            &identity,
            components,
            true,
            true,
        )?;
        check_unix_dac(
            &identity,
            &parent.stat,
            UNIX_DAC_WRITE | UNIX_DAC_SEARCH,
            "bind",
            &parent.canonical_path,
        )?;

        let canonical_path = join_absolute_path(&parent.canonical_path, &basename);
        self.reject_read_only_entry_write_path(&canonical_path)?;
        self.filesystem
            .check_virtual_path(FsOperation::Write, &canonical_path)
            .map_err(KernelError::from)?;

        match self.raw_filesystem_mut().lstat(&canonical_path) {
            Ok(_) => return Err(unix_socket_address_in_use(&canonical_path)),
            Err(error) if error.code() == "ENOENT" => {}
            Err(error) => return Err(error.into()),
        }

        Ok(UnixSocketBindTarget {
            canonical_path,
            parent,
            identity,
        })
    }

    /// Perform Linux pathname lookup and materialize the persistent inode for
    /// an AF_UNIX bind.
    ///
    /// Unlike the generic file helpers, this preserves raw `.`/`..`
    /// traversal, follows symlinks only in the dirname, checks POSIX DAC with
    /// the process's effective credentials, and never creates missing parent
    /// directories. The returned canonical path and `(dev, ino)` identify the
    /// exact dentry the sidecar must register.
    pub fn bind_unix_socket_path_for_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        cwd: &str,
        path: &str,
    ) -> KernelResult<UnixSocketPathNode> {
        let target = self.resolve_unix_socket_bind_target(requester_driver, pid, cwd, path)?;
        let UnixSocketBindTarget {
            canonical_path,
            parent,
            identity,
        } = target;
        let umask = self.processes.get_umask(pid)?;

        self.check_write_file_limits(&canonical_path, 0)?;
        if let Err(error) = VirtualFileSystem::create_file_exclusive(
            &mut self.filesystem,
            &canonical_path,
            Vec::new(),
        ) {
            return if error.code() == "EEXIST" {
                Err(unix_socket_address_in_use(&canonical_path))
            } else {
                Err(error.into())
            };
        }

        let mode = UNIX_SOCKET_FILE_TYPE | (0o777 & !(umask & 0o777));
        let gid = if parent.stat.mode & 0o2000 != 0 {
            parent.stat.gid
        } else {
            identity.egid
        };
        let metadata_result = (|| -> VfsResult<VirtualStat> {
            let filesystem = self.raw_filesystem_mut();
            filesystem.chown(&canonical_path, identity.euid, gid)?;
            filesystem.chmod(&canonical_path, mode)?;
            filesystem.lstat(&canonical_path)
        })();
        let stat = match metadata_result {
            Ok(stat) => stat,
            Err(error) => {
                let cleanup = self.raw_filesystem_mut().remove_file(&canonical_path);
                return match cleanup {
                    Ok(()) => Err(error.into()),
                    Err(cleanup_error) => Err(KernelError::new(
                        error.code(),
                        format!(
                            "failed to initialize Unix socket inode metadata: {error}; \
                             rollback also failed: {cleanup_error}"
                        ),
                    )),
                };
            }
        };

        self.update_filesystem_usage_cache_for_inode_create(&canonical_path, 0);
        Ok(UnixSocketPathNode {
            canonical_path,
            stat,
        })
    }

    /// Resolve an AF_UNIX connect target with Linux pathname and DAC rules.
    /// The final symlink is followed, search permission is required on every
    /// traversed directory, and the selected socket inode must be writable by
    /// the process's effective credentials.
    pub fn resolve_unix_socket_connect_target_for_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        cwd: &str,
        path: &str,
    ) -> KernelResult<UnixSocketPathNode> {
        self.assert_not_terminated()?;
        let identity = self.process_identity(requester_driver, pid)?;
        let (_, components, trailing_slash) = unix_socket_absolute_components(cwd, path)?;
        let target = resolve_unix_socket_components(
            self.raw_filesystem_mut(),
            &identity,
            components,
            true,
            trailing_slash,
        )?;
        self.filesystem
            .check_virtual_path(FsOperation::Write, &target.canonical_path)
            .map_err(KernelError::from)?;
        check_unix_dac(
            &identity,
            &target.stat,
            UNIX_DAC_WRITE,
            "connect",
            &target.canonical_path,
        )?;
        if target.stat.mode & 0o170000 != UNIX_SOCKET_FILE_TYPE {
            return Err(KernelError::new(
                "ECONNREFUSED",
                format!(
                    "Unix socket connect target is not a socket: {}",
                    target.canonical_path
                ),
            ));
        }
        Ok(target)
    }

    /// Writes `content` at `offset` within an existing file, growing (and
    /// zero-filling) it as needed. This is the positional counterpart to
    /// [`Self::pread_file`]: it lets a descriptor-based caller (the shared WASI
    /// runner over the browser wire, which has no kernel fd offsets) write a
    /// region without the lossy, non-atomic read-modify-write it would
    /// otherwise have to do client-side. Enforcement matches `write_file`:
    /// read-only paths are rejected and the resulting file size is charged
    /// against the resource limits before the write.
    pub fn pwrite_file(
        &mut self,
        path: &str,
        offset: u64,
        content: impl Into<Vec<u8>>,
    ) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.reject_read_only_resolved_write_path(path)?;
        self.reject_unix_socket_data_path(path, "ENXIO")?;
        let content = content.into();
        let existing = self.storage_stat(path)?;
        let existing_size = existing.as_ref().map(|stat| stat.size).unwrap_or(0);
        let end = offset.saturating_add(content.len() as u64);
        self.check_write_file_limits_with_existing(
            path,
            existing.as_ref(),
            existing_size.max(end),
        )?;
        self.filesystem.pwrite(path, content, offset)?;
        self.update_filesystem_usage_cache_for_write(
            path,
            existing.as_ref(),
            existing_size.max(end),
        );
        Ok(())
    }

    pub fn write_file_for_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        path: &str,
        content: impl Into<Vec<u8>>,
        mode: Option<u32>,
    ) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        let existed = self.exists_internal(Some(pid), path)?;
        if existed {
            self.check_dac_access(pid, path, DAC_WRITE)?;
        } else {
            self.check_dac_parent_access(pid, path, DAC_WRITE | DAC_EXECUTE)?;
        }
        let content = content.into();
        let new_size = content.len() as u64;
        self.reject_read_only_resolved_write_path(path)?;
        self.reject_unix_socket_data_path(path, "ENXIO")?;
        let existing = self.storage_stat(path)?;
        self.check_write_file_limits_with_existing(path, existing.as_ref(), new_size)?;
        VirtualFileSystem::write_file_with_mode(&mut self.filesystem, path, content, mode)
            .map_err(|error| {
                KernelError::new(
                    error.code(),
                    format!("create storage write for '{path}' failed: {error}"),
                )
            })?;
        self.update_filesystem_usage_cache_for_write(path, existing.as_ref(), new_size);
        if !existed {
            let umask = self.processes.get_umask(pid)?;
            self.apply_process_creation_metadata(pid, path, mode.unwrap_or(0o666), umask, false)
                .map_err(|error| {
                    KernelError::new(
                        error.code(),
                        format!("create metadata for '{path}' failed: {error}"),
                    )
                })?;
        } else {
            self.clear_setid_after_write(pid, path)?;
        }
        Ok(())
    }

    pub fn create_dir(&mut self, path: &str) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.reject_read_only_entry_write_path(path)?;
        self.check_create_dir_limits(path)?;
        self.filesystem.create_dir(path)?;
        self.update_filesystem_usage_cache_for_inode_create(path, 0);
        Ok(())
    }

    pub fn create_dir_for_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        path: &str,
        mode: Option<u32>,
    ) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        let existed = self.exists_internal(Some(pid), path)?;
        if !existed {
            self.check_dac_parent_access(pid, path, DAC_WRITE | DAC_EXECUTE)?;
        }
        self.reject_read_only_entry_write_path(path)?;
        self.check_create_dir_limits(path)?;
        VirtualFileSystem::create_dir_with_mode(&mut self.filesystem, path, mode)?;
        self.update_filesystem_usage_cache_for_inode_create(path, 0);
        if !existed {
            let umask = self.processes.get_umask(pid)?;
            self.apply_process_creation_metadata(pid, path, mode.unwrap_or(0o777), umask, true)?;
        }
        Ok(())
    }

    pub fn mkdir(&mut self, path: &str, recursive: bool) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.reject_read_only_entry_write_path(path)?;
        let created_paths = self.missing_directory_paths(path, recursive)?;
        self.check_mkdir_limits(path, recursive)?;
        self.filesystem.mkdir(path, recursive)?;
        self.update_filesystem_usage_cache_for_inode_creates(path, created_paths.len());
        Ok(())
    }

    pub fn mkdir_for_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        path: &str,
        recursive: bool,
        mode: Option<u32>,
    ) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        let created_paths = self.missing_directory_paths(path, recursive)?;
        if let Some(first_created) = created_paths.first() {
            self.check_dac_parent_access(pid, first_created, DAC_WRITE | DAC_EXECUTE)?;
        } else {
            self.check_dac_access(pid, path, DAC_EXECUTE)?;
        }
        self.reject_read_only_entry_write_path(path)?;
        self.check_mkdir_limits(path, recursive)?;
        VirtualFileSystem::mkdir_with_mode(&mut self.filesystem, path, recursive, mode)?;
        if !created_paths.is_empty() {
            let umask = self.processes.get_umask(pid)?;
            let mode = mode.unwrap_or(0o777);
            for created_path in &created_paths {
                self.apply_process_creation_metadata(pid, created_path, mode, umask, true)?;
            }
        }
        self.update_filesystem_usage_cache_for_inode_creates(path, created_paths.len());
        Ok(())
    }

    pub fn mknod_for_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        path: &str,
        mode: u32,
        rdev: u64,
    ) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        if !matches!(mode & 0o170000, 0o010000 | 0o020000 | 0o060000) {
            return Err(KernelError::new(
                "EOPNOTSUPP",
                format!("unsupported special inode type for {path}"),
            ));
        }
        self.check_dac_parent_access(pid, path, DAC_WRITE | DAC_EXECUTE)?;
        self.reject_read_only_entry_write_path(path)?;
        self.check_create_dir_limits(path)?;
        self.filesystem.mknod(path, mode, rdev)?;
        self.update_filesystem_usage_cache_for_inode_create(path, 0);
        let umask = self.processes.get_umask(pid)?;
        self.apply_process_creation_metadata(pid, path, mode & 0o7777, umask, false)?;
        Ok(())
    }

    pub fn umask(
        &self,
        requester_driver: &str,
        pid: u32,
        new_mask: Option<u32>,
    ) -> KernelResult<u32> {
        self.assert_driver_owns(requester_driver, pid)?;
        match new_mask {
            Some(mask) => Ok(self.processes.set_umask(pid, mask)?),
            None => Ok(self.processes.get_umask(pid)?),
        }
    }

    pub fn exists(&self, path: &str) -> KernelResult<bool> {
        self.assert_not_terminated()?;
        self.exists_internal(None, path)
    }

    pub fn exists_for_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        path: &str,
    ) -> KernelResult<bool> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        if let Err(error) = self.check_dac_traversal(pid, path) {
            if matches!(error.code(), "EACCES" | "ENOENT" | "ENOTDIR" | "ELOOP") {
                return Ok(false);
            }
            return Err(error);
        }
        self.exists_internal(Some(pid), path)
    }

    pub fn stat(&mut self, path: &str) -> KernelResult<VirtualStat> {
        self.assert_not_terminated()?;
        self.stat_internal(None, path)
    }

    pub fn stat_for_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        path: &str,
    ) -> KernelResult<VirtualStat> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        self.check_dac_traversal(pid, path)?;
        self.stat_internal(Some(pid), path)
    }

    pub fn filesystem_stats_for_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        path: &str,
    ) -> KernelResult<FileSystemStats> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        self.check_dac_traversal(pid, path)?;
        self.stat_internal(Some(pid), path)?;

        let max_bytes = self.resource_limits().max_filesystem_bytes;
        let max_inodes = self.resource_limits().max_inode_count;
        let filesystem = self.raw_filesystem_mut();
        let filesystem_any = filesystem as &mut dyn Any;
        if let Some(mount_table) = filesystem_any.downcast_mut::<MountTable>() {
            return mount_table
                .path_stats(path, max_bytes, max_inodes)
                .map_err(KernelError::from);
        }

        let usage = measure_filesystem_usage(filesystem)?;
        let total_bytes = max_bytes
            .unwrap_or(usage.total_bytes)
            .max(usage.total_bytes);
        let total_inodes = max_inodes
            .map(|value| value as u64)
            .unwrap_or(usage.inode_count as u64)
            .max(usage.inode_count as u64);
        Ok(FileSystemStats {
            total_bytes,
            used_bytes: usage.total_bytes,
            available_bytes: total_bytes.saturating_sub(usage.total_bytes),
            total_inodes,
            free_inodes: total_inodes.saturating_sub(usage.inode_count as u64),
        })
    }

    pub fn access_for_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        path: &str,
        access: u32,
        effective_ids: bool,
    ) -> KernelResult<()> {
        self.assert_driver_owns(requester_driver, pid)?;
        self.check_dac_traversal(pid, path)?;
        let mut identity = self.process_identity(requester_driver, pid)?;
        if !effective_ids {
            identity.euid = identity.uid;
            identity.egid = identity.gid;
        }
        let stat = self.filesystem.stat(path)?;
        self.check_dac_mode_with_acl(&identity, &stat, access & 0o7, path)
    }

    pub fn lstat(&self, path: &str) -> KernelResult<VirtualStat> {
        self.assert_not_terminated()?;
        self.lstat_internal(None, path)
    }

    pub fn lstat_for_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        path: &str,
    ) -> KernelResult<VirtualStat> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        self.check_dac_traversal(pid, path)?;
        self.lstat_internal(Some(pid), path)
    }

    pub fn read_link(&self, path: &str) -> KernelResult<String> {
        self.assert_not_terminated()?;
        self.read_link_internal(None, path)
    }

    pub fn read_link_for_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        path: &str,
    ) -> KernelResult<String> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        self.check_dac_traversal(pid, path)?;
        self.read_link_internal(Some(pid), path)
    }

    pub fn read_dir(&mut self, path: &str) -> KernelResult<Vec<String>> {
        self.assert_not_terminated()?;
        let entries = self.read_dir_internal(None, path)?;
        self.resources.check_readdir_entries(entries.len())?;
        Ok(entries)
    }

    pub fn read_dir_for_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        path: &str,
    ) -> KernelResult<Vec<String>> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        self.check_dac_access(pid, path, DAC_READ | DAC_EXECUTE)?;
        let mut entries = self.read_dir_internal(Some(pid), path)?;
        entries.retain(|entry| !is_internal_unnamed_file_name(entry));
        self.resources.check_readdir_entries(entries.len())?;
        Ok(entries)
    }

    pub fn read_dir_with_types_for_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        path: &str,
    ) -> KernelResult<Vec<VirtualDirEntry>> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        self.check_dac_access(pid, path, DAC_READ | DAC_EXECUTE)?;
        let mut entries = self.read_dir_with_types_internal(Some(pid), path)?;
        entries.retain(|entry| !is_internal_unnamed_file_name(&entry.name));
        self.resources.check_readdir_entries(entries.len())?;
        Ok(entries)
    }

    /// Lists a directory with each child's file type in one call. This is the
    /// typed counterpart to [`Self::read_dir`]: it lets a descriptor-based caller
    /// recover Dirent kinds (`readdir({ withFileTypes })`) without an extra
    /// `lstat` round-trip per entry. Reuses `read_dir_internal` so proc, the
    /// readdir-entry limit, and read-permission checks behave identically; the
    /// per-entry `lstat` is in-process (no wire hops).
    pub fn read_dir_with_types(&mut self, path: &str) -> KernelResult<Vec<VirtualDirEntry>> {
        self.assert_not_terminated()?;
        let names = self.read_dir_internal(None, path)?;
        self.resources.check_readdir_entries(names.len())?;
        let mut entries = Vec::with_capacity(names.len());
        for name in names {
            let child = normalize_path(&format!("{path}/{name}"));
            let stat = self.lstat_internal(None, &child)?;
            entries.push(VirtualDirEntry {
                name,
                is_directory: stat.is_directory,
                is_symbolic_link: stat.is_symbolic_link,
            });
        }
        Ok(entries)
    }

    pub fn read_dir_recursive(
        &mut self,
        path: &str,
        max_depth: Option<usize>,
    ) -> KernelResult<Vec<RecursiveDirEntry>> {
        self.assert_not_terminated()?;
        let depth_limit = self.effective_recursive_fs_depth(max_depth)?;
        let caller_limited = max_depth.is_some();
        let mut entries = Vec::new();
        let mut queue = VecDeque::from([(normalize_path(path), 0usize)]);

        while let Some((dir_path, depth)) = queue.pop_front() {
            self.resources.check_recursive_fs_depth(depth)?;
            let names = self.read_dir_internal(None, &dir_path)?;
            self.resources.check_readdir_entries(names.len())?;

            for name in names {
                if matches!(name.as_str(), "." | "..") {
                    continue;
                }
                let child = join_child_path(&dir_path, &name);
                let stat = self.lstat_internal(None, &child)?;
                let entry = RecursiveDirEntry {
                    path: child.clone(),
                    is_directory: stat.is_directory,
                    is_symbolic_link: stat.is_symbolic_link,
                    size: stat.size,
                };
                entries.push(entry);
                self.resources.check_recursive_fs_entries(entries.len())?;

                if stat.is_directory && !stat.is_symbolic_link {
                    let child_depth = depth.saturating_add(1);
                    if child_depth <= depth_limit {
                        queue.push_back((child, child_depth));
                    } else if !caller_limited {
                        self.resources.check_recursive_fs_depth(child_depth)?;
                    }
                }
            }
        }

        Ok(entries)
    }

    pub fn copy_path(&mut self, from: &str, to: &str, recursive: bool) -> KernelResult<()> {
        self.assert_not_terminated()?;
        let mut entries = 0usize;
        self.copy_path_inner(from, to, recursive, 0, &mut entries)?;
        Ok(())
    }

    pub fn remove_path(&mut self, path: &str, recursive: bool) -> KernelResult<()> {
        self.assert_not_terminated()?;
        let mut entries = 0usize;
        self.remove_path_inner(path, recursive, 0, &mut entries)
    }

    pub fn move_path(&mut self, from: &str, to: &str) -> KernelResult<()> {
        self.assert_not_terminated()?;
        match self.rename(from, to) {
            Ok(()) => Ok(()),
            Err(error) if error.code() == "EXDEV" => {
                self.copy_path(from, to, true)?;
                self.remove_path(from, true)
            }
            Err(error) => Err(error),
        }
    }

    pub fn remove_file(&mut self, path: &str) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.reject_read_only_entry_write_path(path)?;
        let removed = self.storage_lstat(path)?;
        let detached = self.prepare_anonymous_file_backing(path, removed.as_ref())?;
        self.filesystem.remove_file(path)?;
        match detached {
            Some(OpenFileRemovalBacking::Anonymous {
                descriptions,
                backing,
            }) => {
                for description in descriptions {
                    description.detach_path(path, Arc::clone(&backing));
                }
            }
            Some(OpenFileRemovalBacking::LinkedAlias {
                descriptions,
                live_path,
            }) => {
                for description in descriptions {
                    description.rebind_deleted_path(path, &live_path);
                }
            }
            None => {}
        }
        self.update_filesystem_usage_cache_for_remove(path, removed.as_ref());
        Ok(())
    }

    pub fn remove_file_for_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        path: &str,
    ) -> KernelResult<()> {
        self.assert_driver_owns(requester_driver, pid)?;
        self.check_dac_parent_access(pid, path, DAC_WRITE | DAC_EXECUTE)?;
        self.check_sticky_directory_removal(pid, path)?;
        self.remove_file(path)
    }

    pub fn remove_dir(&mut self, path: &str) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.reject_read_only_entry_write_path(path)?;
        let removed = self.storage_lstat(path)?;
        let detached = self.prepare_detached_directory_backing(path, removed.as_ref());
        self.filesystem.remove_dir(path)?;
        if let Some((descriptions, stat)) = detached {
            for description in descriptions {
                description.detach_directory(path, stat.clone());
            }
        }
        if removed.as_ref().is_some_and(|stat| stat.is_directory) {
            self.update_filesystem_usage_cache_for_inode_delete(path, 0);
        }
        Ok(())
    }

    pub fn remove_dir_for_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        path: &str,
    ) -> KernelResult<()> {
        self.assert_driver_owns(requester_driver, pid)?;
        self.check_dac_parent_access(pid, path, DAC_WRITE | DAC_EXECUTE)?;
        self.check_sticky_directory_removal(pid, path)?;
        self.remove_dir(path)
    }

    pub fn rename(&mut self, old_path: &str, new_path: &str) -> KernelResult<()> {
        self.rename_at2(old_path, new_path, 0)
    }

    pub fn rename_at2(&mut self, old_path: &str, new_path: &str, flags: u32) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.reject_read_only_entry_write_path(old_path)?;
        self.reject_read_only_entry_write_path(new_path)?;
        self.check_rename_copy_up_limits(old_path, new_path)?;
        let replaced = self.storage_lstat(new_path)?;
        let detached_destination =
            self.prepare_anonymous_file_backing(new_path, replaced.as_ref())?;
        let detached_directory_destination =
            self.prepare_detached_directory_backing(new_path, replaced.as_ref());
        self.filesystem.rename_at2(old_path, new_path, flags)?;
        if flags == RENAME_EXCHANGE {
            let temporary = format!(
                "/.agentos-open-rename-exchange-{}",
                self.next_unnamed_file_id
            );
            self.next_unnamed_file_id = self.next_unnamed_file_id.saturating_add(1);
            self.rename_open_file_descriptions(old_path, &temporary);
            self.rename_open_file_descriptions(new_path, old_path);
            self.rename_open_file_descriptions(&temporary, new_path);
            self.invalidate_filesystem_usage_cache();
            return Ok(());
        }
        match detached_destination {
            Some(OpenFileRemovalBacking::Anonymous {
                descriptions,
                backing,
            }) => {
                for description in descriptions {
                    description.detach_path(new_path, Arc::clone(&backing));
                }
            }
            Some(OpenFileRemovalBacking::LinkedAlias {
                descriptions,
                live_path,
            }) => {
                for description in descriptions {
                    description.rebind_deleted_path(new_path, &live_path);
                }
            }
            None => {}
        }
        if let Some((descriptions, stat)) = detached_directory_destination {
            for description in descriptions {
                description.detach_directory(new_path, stat.clone());
            }
        }
        self.rename_open_file_descriptions(old_path, new_path);
        // Rename can be a pure metadata move, a destination replacement, or an
        // overlay copy-up/removal with hard-link aliasing. Drop the cached root
        // usage because the local byte/inode delta is not knowable here.
        self.invalidate_filesystem_usage_cache();
        Ok(())
    }

    pub fn rename_for_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        old_path: &str,
        new_path: &str,
    ) -> KernelResult<()> {
        self.rename_at2_for_process(requester_driver, pid, old_path, new_path, 0)
    }

    pub fn rename_at2_for_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        old_path: &str,
        new_path: &str,
        flags: u32,
    ) -> KernelResult<()> {
        self.assert_driver_owns(requester_driver, pid)?;
        self.check_dac_parent_access(pid, old_path, DAC_WRITE | DAC_EXECUTE)?;
        self.check_dac_parent_access(pid, new_path, DAC_WRITE | DAC_EXECUTE)?;
        self.check_sticky_directory_removal(pid, old_path)?;
        if self.exists_internal(Some(pid), new_path)? {
            self.check_sticky_directory_removal(pid, new_path)?;
        }
        self.rename_at2(old_path, new_path, flags)
    }

    pub fn realpath(&self, path: &str) -> KernelResult<String> {
        self.assert_not_terminated()?;
        self.realpath_internal(None, path)
    }

    pub fn realpath_for_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        path: &str,
    ) -> KernelResult<String> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        self.check_dac_traversal(pid, path)?;
        self.realpath_internal(Some(pid), path)
    }

    pub fn symlink(&mut self, target: &str, link_path: &str) -> KernelResult<()> {
        self.assert_not_terminated()?;
        if is_proc_path(target) {
            self.filesystem
                .check_virtual_path(FsOperation::Write, link_path)
                .map_err(KernelError::from)?;
            return Err(read_only_filesystem_error(link_path));
        }
        self.reject_read_only_entry_write_path(link_path)?;
        self.check_symlink_limits(target, link_path)?;
        self.filesystem.symlink(target, link_path)?;
        self.update_filesystem_usage_cache_for_inode_create(link_path, target.len() as u64);
        Ok(())
    }

    pub fn symlink_for_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        target: &str,
        link_path: &str,
    ) -> KernelResult<()> {
        self.assert_driver_owns(requester_driver, pid)?;
        self.check_dac_parent_access(pid, link_path, DAC_WRITE | DAC_EXECUTE)?;
        self.symlink(target, link_path)
    }

    pub fn chmod(&mut self, path: &str, mode: u32) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.reject_read_only_resolved_write_path(path)?;
        self.filesystem.chmod(path, mode)?;
        self.sync_access_acl_mode(path, mode)?;
        self.no_posix_acl_cache.clear();
        Ok(())
    }

    pub fn chmod_for_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        path: &str,
        mut mode: u32,
    ) -> KernelResult<()> {
        let identity = self.process_identity(requester_driver, pid)?;
        self.check_dac_traversal(pid, path)?;
        let stat = self.filesystem.stat(path)?;
        if identity.euid != 0 && identity.euid != stat.uid {
            return Err(KernelError::new(
                "EPERM",
                format!("chmod requires ownership of {path}"),
            ));
        }
        if identity.euid != 0
            && identity.egid != stat.gid
            && !identity.supplementary_gids.contains(&stat.gid)
        {
            mode &= !0o2000;
        }
        self.chmod(path, mode)
    }

    pub fn link(&mut self, old_path: &str, new_path: &str) -> KernelResult<()> {
        self.assert_not_terminated()?;
        if is_proc_path(old_path) {
            self.filesystem
                .check_virtual_path(FsOperation::Write, new_path)
                .map_err(KernelError::from)?;
            return Err(read_only_filesystem_error(new_path));
        }
        self.reject_read_only_resolved_write_path(old_path)?;
        self.reject_read_only_entry_write_path(new_path)?;
        self.filesystem.link(old_path, new_path)?;
        // Hard-link creation makes another directory entry for an already
        // reachable inode, so measured root usage is unchanged.
        Ok(())
    }

    pub fn link_for_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        old_path: &str,
        new_path: &str,
    ) -> KernelResult<()> {
        self.assert_driver_owns(requester_driver, pid)?;
        self.check_dac_traversal(pid, old_path)?;
        self.check_dac_parent_access(pid, new_path, DAC_WRITE | DAC_EXECUTE)?;
        self.link(old_path, new_path)
    }

    pub fn chown(&mut self, path: &str, uid: u32, gid: u32) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.reject_read_only_resolved_write_path(path)?;
        self.filesystem.chown(path, uid, gid)?;
        self.no_posix_acl_cache.clear();
        Ok(())
    }

    pub fn chown_for_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        path: &str,
        uid: u32,
        gid: u32,
        follow_symlinks: bool,
    ) -> KernelResult<()> {
        self.assert_not_terminated()?;
        let identity = self.process_identity(requester_driver, pid)?;
        let stat = if follow_symlinks {
            self.stat_for_process(requester_driver, pid, path)?
        } else {
            self.lstat_for_process(requester_driver, pid, path)?
        };
        let (next_uid, next_gid) = validate_chown_request(&identity, &stat, uid, gid, path)?;

        if follow_symlinks {
            self.reject_read_only_resolved_write_path(path)?;
        } else {
            self.reject_read_only_entry_write_path(path)?;
        }
        self.filesystem
            .chown_spec(path, next_uid, next_gid, follow_symlinks)?;
        if let Some(mode) = linux_chown_cleared_mode(&stat) {
            self.filesystem.chmod(path, mode)?;
        }
        Ok(())
    }

    pub fn lchown_for_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        path: &str,
        uid: u32,
        gid: u32,
    ) -> KernelResult<()> {
        let identity = self.process_identity(requester_driver, pid)?;
        self.check_dac_traversal(pid, path)?;
        let stat = self.filesystem.lstat(path)?;
        if identity.euid != 0 {
            let owns_file = identity.euid == stat.uid;
            let keeps_owner = uid == stat.uid;
            let allowed_group = gid == identity.egid || identity.supplementary_gids.contains(&gid);
            if !owns_file || !keeps_owner || !allowed_group {
                return Err(KernelError::new(
                    "EPERM",
                    format!("lchown is not permitted for {path}"),
                ));
            }
        }
        self.reject_read_only_entry_write_path(path)?;
        self.filesystem.lchown(path, uid, gid)?;
        Ok(())
    }

    pub fn get_xattr(
        &mut self,
        path: &str,
        name: &str,
        follow_symlinks: bool,
    ) -> KernelResult<Vec<u8>> {
        self.assert_not_terminated()?;
        Ok(self.filesystem.get_xattr(path, name, follow_symlinks)?)
    }

    pub fn get_xattr_for_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        path: &str,
        name: &str,
        follow_symlinks: bool,
    ) -> KernelResult<Vec<u8>> {
        let identity = self.process_identity(requester_driver, pid)?;
        self.check_dac_traversal(pid, path)?;
        check_xattr_namespace(&identity, name, false, path)?;
        let stat = if follow_symlinks {
            self.filesystem.stat(path)?
        } else {
            self.filesystem.lstat(path)?
        };
        self.check_dac_mode_with_acl(&identity, &stat, DAC_READ, path)?;
        self.get_xattr(path, name, follow_symlinks)
    }

    pub fn list_xattrs(&mut self, path: &str, follow_symlinks: bool) -> KernelResult<Vec<String>> {
        self.assert_not_terminated()?;
        Ok(self.filesystem.list_xattrs(path, follow_symlinks)?)
    }

    pub fn list_xattrs_for_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        path: &str,
        follow_symlinks: bool,
    ) -> KernelResult<Vec<String>> {
        let identity = self.process_identity(requester_driver, pid)?;
        self.check_dac_traversal(pid, path)?;
        let stat = if follow_symlinks {
            self.filesystem.stat(path)?
        } else {
            self.filesystem.lstat(path)?
        };
        self.check_dac_mode_with_acl(&identity, &stat, DAC_READ, path)?;
        let mut names = self.list_xattrs(path, follow_symlinks)?;
        if identity.euid != 0 {
            names.retain(|name| !name.starts_with("trusted.") && !name.starts_with("security."));
        }
        Ok(names)
    }

    pub fn set_xattr(
        &mut self,
        path: &str,
        name: &str,
        value: Vec<u8>,
        flags: u32,
        follow_symlinks: bool,
    ) -> KernelResult<()> {
        self.assert_not_terminated()?;
        if follow_symlinks {
            self.reject_read_only_resolved_write_path(path)?;
        } else {
            self.reject_read_only_entry_write_path(path)?;
        }
        self.filesystem
            .set_xattr(path, name, value, flags, follow_symlinks)?;
        self.no_posix_acl_cache.clear();
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn set_xattr_for_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        path: &str,
        name: &str,
        value: Vec<u8>,
        flags: u32,
        follow_symlinks: bool,
    ) -> KernelResult<()> {
        let identity = self.process_identity(requester_driver, pid)?;
        self.check_dac_traversal(pid, path)?;
        check_xattr_namespace(&identity, name, true, path)?;
        let stat = if follow_symlinks {
            self.filesystem.stat(path)?
        } else {
            self.filesystem.lstat(path)?
        };
        check_xattr_inode_write_policy(&stat, name, path)?;
        if name.starts_with("system.posix_acl_") {
            if identity.euid != 0 && identity.euid != stat.uid {
                return Err(KernelError::new(
                    "EPERM",
                    format!("setting {name} requires ownership of {path}"),
                ));
            }
        } else {
            self.check_dac_mode_with_acl(&identity, &stat, DAC_WRITE, path)?;
        }
        let acl = if name == POSIX_ACL_ACCESS || name == POSIX_ACL_DEFAULT {
            let acl = PosixAcl::parse(&value, path)?;
            if name == POSIX_ACL_DEFAULT && !stat.is_directory {
                return Err(KernelError::new(
                    "EACCES",
                    format!("default ACL requires a directory: {path}"),
                ));
            }
            Some(acl)
        } else {
            None
        };
        self.set_xattr(path, name, value, flags, follow_symlinks)?;
        if name == POSIX_ACL_ACCESS {
            let mode = acl.expect("access ACL was parsed").mode(stat.mode);
            self.filesystem.chmod(path, mode)?;
        }
        Ok(())
    }

    pub fn remove_xattr(
        &mut self,
        path: &str,
        name: &str,
        follow_symlinks: bool,
    ) -> KernelResult<()> {
        self.assert_not_terminated()?;
        if follow_symlinks {
            self.reject_read_only_resolved_write_path(path)?;
        } else {
            self.reject_read_only_entry_write_path(path)?;
        }
        self.filesystem.remove_xattr(path, name, follow_symlinks)?;
        self.no_posix_acl_cache.clear();
        Ok(())
    }

    pub fn remove_xattr_for_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        path: &str,
        name: &str,
        follow_symlinks: bool,
    ) -> KernelResult<()> {
        let identity = self.process_identity(requester_driver, pid)?;
        self.check_dac_traversal(pid, path)?;
        check_xattr_namespace(&identity, name, true, path)?;
        let stat = if follow_symlinks {
            self.filesystem.stat(path)?
        } else {
            self.filesystem.lstat(path)?
        };
        check_xattr_inode_write_policy(&stat, name, path)?;
        if name.starts_with("system.posix_acl_") {
            if identity.euid != 0 && identity.euid != stat.uid {
                return Err(KernelError::new(
                    "EPERM",
                    format!("removing {name} requires ownership of {path}"),
                ));
            }
        } else {
            self.check_dac_mode_with_acl(&identity, &stat, DAC_WRITE, path)?;
        }
        self.remove_xattr(path, name, follow_symlinks)
    }

    pub fn utimes(&mut self, path: &str, atime_ms: u64, mtime_ms: u64) -> KernelResult<()> {
        self.utimes_spec(
            path,
            VirtualUtimeSpec::Set(VirtualTimeSpec::from_millis(atime_ms)),
            VirtualUtimeSpec::Set(VirtualTimeSpec::from_millis(mtime_ms)),
        )
    }

    pub fn utimes_spec(
        &mut self,
        path: &str,
        atime: VirtualUtimeSpec,
        mtime: VirtualUtimeSpec,
    ) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.reject_read_only_resolved_write_path(path)?;
        Ok(self.filesystem.utimes_spec(path, atime, mtime, true)?)
    }

    pub fn utimes_spec_for_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        path: &str,
        atime: VirtualUtimeSpec,
        mtime: VirtualUtimeSpec,
        follow_symlinks: bool,
    ) -> KernelResult<()> {
        let identity = self.process_identity(requester_driver, pid)?;
        self.check_dac_traversal(pid, path)?;
        let stat = if follow_symlinks {
            self.filesystem.stat(path)?
        } else {
            self.filesystem.lstat(path)?
        };
        let owns_file = identity.euid == 0 || identity.euid == stat.uid;
        let sets_both_to_now =
            matches!(atime, VirtualUtimeSpec::Now) && matches!(mtime, VirtualUtimeSpec::Now);
        let omits_both =
            matches!(atime, VirtualUtimeSpec::Omit) && matches!(mtime, VirtualUtimeSpec::Omit);
        if !owns_file && sets_both_to_now {
            self.check_dac_mode_with_acl(&identity, &stat, DAC_WRITE, path)?;
        } else if !owns_file && !omits_both {
            return Err(KernelError::new(
                "EPERM",
                format!("changing timestamps requires ownership of {path}"),
            ));
        }
        if follow_symlinks {
            self.utimes_spec(path, atime, mtime)
        } else {
            self.lutimes(path, atime, mtime)
        }
    }

    pub fn lutimes(
        &mut self,
        path: &str,
        atime: VirtualUtimeSpec,
        mtime: VirtualUtimeSpec,
    ) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.reject_read_only_entry_write_path(path)?;
        Ok(self.filesystem.utimes_spec(path, atime, mtime, false)?)
    }

    pub fn futimes(
        &mut self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
        atime: VirtualUtimeSpec,
        mtime: VirtualUtimeSpec,
    ) -> KernelResult<()> {
        self.assert_not_terminated()?;
        let path = self
            .description_for_fd(requester_driver, pid, fd)?
            .path()
            .to_owned();
        self.reject_read_only_resolved_write_path(&path)?;
        Ok(self.filesystem.utimes_spec(&path, atime, mtime, true)?)
    }

    pub fn truncate(&mut self, path: &str, length: u64) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.reject_read_only_resolved_write_path(path)?;
        self.reject_unix_socket_data_path(path, "EINVAL")?;
        let existing = self.storage_stat(path)?;
        self.check_truncate_limits_with_existing(path, existing.as_ref(), length)?;
        self.filesystem.truncate(path, length)?;
        self.update_filesystem_usage_cache_for_write(path, existing.as_ref(), length);
        Ok(())
    }

    pub fn truncate_for_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        path: &str,
        length: u64,
    ) -> KernelResult<()> {
        self.assert_driver_owns(requester_driver, pid)?;
        self.check_dac_access(pid, path, DAC_WRITE)?;
        self.truncate(path, length)?;
        self.clear_setid_after_write(pid, path)
    }

    pub fn fd_truncate(
        &mut self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
        length: u64,
    ) -> KernelResult<()> {
        self.assert_driver_owns(requester_driver, pid)?;
        let entry = {
            let tables = lock_or_recover(&self.fd_tables);
            tables
                .get(pid)
                .and_then(|table| table.get(fd))
                .cloned()
                .ok_or_else(|| KernelError::bad_file_descriptor(fd))?
        };
        if let Some(stat) = entry.description.anonymous_stat() {
            self.check_path_resize_limits_with_existing(stat.size, length)?;
            entry
                .description
                .anonymous_truncate(length)
                .expect("anonymous stat and backing must agree")?;
            return Ok(());
        }
        if entry.description.flags() & 0b11 == crate::fd_table::O_RDONLY {
            return Err(KernelError::bad_file_descriptor(fd));
        }
        let path = entry.description.path().to_owned();
        self.truncate(&path, length)?;
        self.clear_setid_after_write(pid, &path)
    }

    pub fn fd_allocate(
        &mut self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
        offset: u64,
        length: u64,
    ) -> KernelResult<()> {
        self.assert_driver_owns(requester_driver, pid)?;
        let entry = {
            let tables = lock_or_recover(&self.fd_tables);
            tables
                .get(pid)
                .and_then(|table| table.get(fd))
                .cloned()
                .ok_or_else(|| KernelError::bad_file_descriptor(fd))?
        };
        if entry.description.flags() & 0b11 == crate::fd_table::O_RDONLY {
            return Err(KernelError::bad_file_descriptor(fd));
        }
        let end = offset
            .checked_add(length)
            .ok_or_else(|| KernelError::new("EINVAL", "allocation range overflows"))?;
        let path = entry.description.path().to_owned();
        self.reject_read_only_resolved_write_path(&path)?;
        if length == 0 {
            return Ok(());
        }
        let existing = self.storage_stat(&path)?;
        let new_size = existing.as_ref().map_or(end, |stat| stat.size.max(end));
        self.check_truncate_limits_with_existing(&path, existing.as_ref(), new_size)?;
        self.filesystem.allocate(&path, offset, length)?;
        self.update_filesystem_usage_cache_for_write(&path, existing.as_ref(), new_size);
        self.clear_setid_after_write(pid, &path)
    }

    pub fn fd_punch_hole(
        &mut self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
        offset: u64,
        length: u64,
    ) -> KernelResult<()> {
        self.assert_driver_owns(requester_driver, pid)?;
        let entry = {
            let tables = lock_or_recover(&self.fd_tables);
            tables
                .get(pid)
                .and_then(|table| table.get(fd))
                .cloned()
                .ok_or_else(|| KernelError::bad_file_descriptor(fd))?
        };
        if entry.description.flags() & 0b11 == crate::fd_table::O_RDONLY {
            return Err(KernelError::bad_file_descriptor(fd));
        }
        offset
            .checked_add(length)
            .ok_or_else(|| KernelError::new("EINVAL", "hole-punch range overflows"))?;
        let path = entry.description.path().to_owned();
        self.reject_read_only_resolved_write_path(&path)?;
        self.filesystem.punch_hole(&path, offset, length)?;
        self.clear_setid_after_write(pid, &path)
    }

    pub fn fd_zero_range(
        &mut self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
        offset: u64,
        length: u64,
        keep_size: bool,
    ) -> KernelResult<()> {
        self.assert_driver_owns(requester_driver, pid)?;
        let entry = {
            let tables = lock_or_recover(&self.fd_tables);
            tables
                .get(pid)
                .and_then(|table| table.get(fd))
                .cloned()
                .ok_or_else(|| KernelError::bad_file_descriptor(fd))?
        };
        if entry.description.flags() & 0b11 == crate::fd_table::O_RDONLY {
            return Err(KernelError::bad_file_descriptor(fd));
        }
        let end = offset
            .checked_add(length)
            .ok_or_else(|| KernelError::new("EINVAL", "zero range overflows"))?;
        let path = entry.description.path().to_owned();
        self.reject_read_only_resolved_write_path(&path)?;
        let existing = self.storage_stat(&path)?;
        let old_size = existing
            .as_ref()
            .ok_or_else(|| KernelError::new("ENOENT", format!("no such file: {path}")))?
            .size;
        let new_size = if keep_size {
            old_size
        } else {
            old_size.max(end)
        };
        self.check_truncate_limits_with_existing(&path, existing.as_ref(), new_size)?;
        self.filesystem
            .zero_range(&path, offset, length, keep_size)?;
        self.update_filesystem_usage_cache_for_write(&path, existing.as_ref(), new_size);
        self.clear_setid_after_write(pid, &path)
    }

    pub fn fd_insert_range(
        &mut self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
        offset: u64,
        length: u64,
    ) -> KernelResult<()> {
        self.assert_driver_owns(requester_driver, pid)?;
        let entry = {
            let tables = lock_or_recover(&self.fd_tables);
            tables
                .get(pid)
                .and_then(|table| table.get(fd))
                .cloned()
                .ok_or_else(|| KernelError::bad_file_descriptor(fd))?
        };
        if entry.description.flags() & 0b11 == crate::fd_table::O_RDONLY {
            return Err(KernelError::bad_file_descriptor(fd));
        }
        let path = entry.description.path().to_owned();
        self.reject_read_only_resolved_write_path(&path)?;
        let existing = self.storage_stat(&path)?;
        let old_size = existing
            .as_ref()
            .ok_or_else(|| KernelError::new("ENOENT", format!("no such file: {path}")))?
            .size;
        let new_size = old_size
            .checked_add(length)
            .ok_or_else(|| KernelError::new("EINVAL", "insert range size overflows"))?;
        self.check_truncate_limits_with_existing(&path, existing.as_ref(), new_size)?;
        self.filesystem.insert_range(&path, offset, length)?;
        self.update_filesystem_usage_cache_for_write(&path, existing.as_ref(), new_size);
        self.clear_setid_after_write(pid, &path)
    }

    pub fn fd_collapse_range(
        &mut self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
        offset: u64,
        length: u64,
    ) -> KernelResult<()> {
        self.assert_driver_owns(requester_driver, pid)?;
        let entry = {
            let tables = lock_or_recover(&self.fd_tables);
            tables
                .get(pid)
                .and_then(|table| table.get(fd))
                .cloned()
                .ok_or_else(|| KernelError::bad_file_descriptor(fd))?
        };
        if entry.description.flags() & 0b11 == crate::fd_table::O_RDONLY {
            return Err(KernelError::bad_file_descriptor(fd));
        }
        let path = entry.description.path().to_owned();
        self.reject_read_only_resolved_write_path(&path)?;
        let existing = self.storage_stat(&path)?;
        let old_size = existing
            .as_ref()
            .ok_or_else(|| KernelError::new("ENOENT", format!("no such file: {path}")))?
            .size;
        self.filesystem.collapse_range(&path, offset, length)?;
        let new_size = old_size.saturating_sub(length);
        self.update_filesystem_usage_cache_for_write(&path, existing.as_ref(), new_size);
        self.clear_setid_after_write(pid, &path)
    }

    pub fn fd_allocated_ranges(
        &mut self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
    ) -> KernelResult<Vec<(u64, u64)>> {
        self.assert_driver_owns(requester_driver, pid)?;
        let path = {
            let tables = lock_or_recover(&self.fd_tables);
            tables
                .get(pid)
                .and_then(|table| table.get(fd))
                .map(|entry| entry.description.path().to_owned())
                .ok_or_else(|| KernelError::bad_file_descriptor(fd))?
        };
        Ok(self.filesystem.allocated_ranges(&path)?)
    }

    pub fn fd_unwritten_ranges(
        &mut self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
    ) -> KernelResult<Vec<(u64, u64)>> {
        self.assert_driver_owns(requester_driver, pid)?;
        let path = {
            let tables = lock_or_recover(&self.fd_tables);
            tables
                .get(pid)
                .and_then(|table| table.get(fd))
                .map(|entry| entry.description.path().to_owned())
                .ok_or_else(|| KernelError::bad_file_descriptor(fd))?
        };
        Ok(self.filesystem.unwritten_ranges(&path)?)
    }

    pub fn check_execute_for_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        path: &str,
    ) -> KernelResult<()> {
        self.assert_driver_owns(requester_driver, pid)?;
        let stat = self.filesystem.stat(path)?;
        if stat.is_directory {
            return Err(KernelError::new(
                "EACCES",
                format!("permission denied, execute '{path}'"),
            ));
        }
        self.check_dac_access(pid, path, DAC_EXECUTE)
    }

    pub fn list_processes(&self) -> BTreeMap<u32, ProcessInfo> {
        self.processes.list_processes()
    }

    pub fn zombie_timer_count(&self) -> usize {
        self.processes.zombie_timer_count()
    }

    pub fn reap_due_zombies(&self) {
        self.processes.reap_due_zombies();
    }

    pub fn next_zombie_reap_deadline(&self) -> Option<std::time::Instant> {
        self.processes.next_zombie_reap_deadline()
    }

    pub fn spawn_process(
        &mut self,
        command: &str,
        args: Vec<String>,
        options: SpawnOptions,
    ) -> KernelResult<KernelProcessHandle> {
        self.spawn_process_with_process_group(command, args, options, None)
    }

    pub fn spawn_process_with_process_group(
        &mut self,
        command: &str,
        args: Vec<String>,
        options: SpawnOptions,
        requested_pgid: Option<u32>,
    ) -> KernelResult<KernelProcessHandle> {
        self.spawn_process_with_process_group_and_cloexec(
            command,
            args,
            options,
            requested_pgid,
            false,
        )
    }

    /// Create the fork half of a process whose exec is deferred until the
    /// caller has applied POSIX spawn file actions.
    ///
    /// Unlike ordinary combined spawn, this preserves `FD_CLOEXEC` sources.
    /// The caller must invoke [`Self::close_process_cloexec_fds`] after all
    /// file actions succeed and before exposing the new process image.
    pub fn spawn_process_with_process_group_preserving_cloexec(
        &mut self,
        command: &str,
        args: Vec<String>,
        options: SpawnOptions,
        requested_pgid: Option<u32>,
    ) -> KernelResult<KernelProcessHandle> {
        self.spawn_process_with_process_group_and_cloexec(
            command,
            args,
            options,
            requested_pgid,
            true,
        )
    }

    fn spawn_process_with_process_group_and_cloexec(
        &mut self,
        command: &str,
        args: Vec<String>,
        options: SpawnOptions,
        requested_pgid: Option<u32>,
        preserve_cloexec: bool,
    ) -> KernelResult<KernelProcessHandle> {
        self.assert_not_terminated()?;
        if let (Some(requester), Some(parent_pid)) =
            (options.requester_driver.as_deref(), options.parent_pid)
        {
            self.assert_driver_owns(requester, parent_pid)?;
        }

        let parent_context = options
            .parent_pid
            .map(|pid| self.processes.inherited_context(pid))
            .transpose()?;
        let cwd = options.cwd.clone().unwrap_or_else(|| {
            parent_context
                .as_ref()
                .map(|context| context.cwd.clone())
                .unwrap_or_else(|| self.cwd.clone())
        });
        let resolved = self.resolve_spawn_command(command, &args, &cwd, options.parent_pid)?;

        self.resources
            .check_process_argv_bytes(&resolved.command, &resolved.args)?;
        self.resources
            .check_process_env_bytes(&self.env, &options.env)?;

        let mut env = parent_context
            .as_ref()
            .map(|context| context.env.clone())
            .unwrap_or_else(|| self.env.clone());
        env.extend(options.env.clone());
        check_command_execution(
            &self.vm_id,
            &self.permissions,
            &resolved.command,
            &resolved.args,
            Some(&cwd),
            &env,
        )?;

        let inherited_fds = {
            let tables = lock_or_recover(&self.fd_tables);
            options
                .parent_pid
                .and_then(|pid| tables.get(pid).map(ProcessFdTable::len_for_exec))
                .unwrap_or(3)
        };
        self.resources
            .check_process_spawn(&self.resource_snapshot(), inherited_fds)?;
        let process_umask = match options.parent_pid {
            Some(parent_pid) => self.processes.get_umask(parent_pid)?,
            None => DEFAULT_PROCESS_UMASK,
        };

        let mut context = parent_context.unwrap_or_else(|| ProcessContext {
            identity: self.users.identity(),
            ..ProcessContext::default()
        });
        context.ppid = options.parent_pid.unwrap_or(0);
        context.env = env;
        context.cwd = cwd;
        context.umask = process_umask;

        self.register_process(
            resolved.driver.name().to_owned(),
            resolved.command,
            resolved.args,
            context,
            options.requester_driver.as_deref(),
            requested_pgid,
            preserve_cloexec,
        )
    }

    /// Replace a running process image without allocating a new PID or FD
    /// table. This is the kernel half of execve(2): supplied argv/env replace
    /// the old image, cwd and process relationships remain attached to the
    /// same process, and only FD_CLOEXEC descriptors are closed.
    pub fn exec_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        command: &str,
        args: Vec<String>,
        env: BTreeMap<String, String>,
        cwd: String,
    ) -> KernelResult<()> {
        self.exec_process_retaining_internal_fds(
            requester_driver,
            pid,
            command,
            args,
            env,
            cwd,
            &[],
            &[],
            None,
        )
    }

    /// Validate the literal pathname supplied to execve(2) without committing
    /// a process-image replacement. This preserves Linux pathname/type/mode
    /// errno behavior for sidecars that launch the file through an internal
    /// language-runtime driver.
    pub fn validate_executable_path(&mut self, path: &str, cwd: &str) -> KernelResult<String> {
        self.assert_not_terminated()?;
        self.resolve_executable_path(path, cwd, None)?
            .ok_or_else(|| KernelError::command_not_found(path))
    }

    /// Validate the image chain for an in-place WASM exec replacement. Linux
    /// applies the same pathname/type/mode checks to each `#!` interpreter as
    /// it does to the originally requested script. The runner compiles the
    /// resulting WASM image before asking the sidecar to commit, but the
    /// trusted kernel remains responsible for enforcing those guest-visible
    /// checks and errno values.
    pub fn validate_wasm_exec_image(&mut self, path: &str, cwd: &str) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.validate_wasm_exec_image_inner(path, cwd, 0)
    }

    /// Sidecar variant of [`Self::exec_process`] which keeps host-only
    /// plumbing descriptors that are stored in the process FD table as an
    /// implementation detail. Those descriptors are never part of the guest's
    /// Linux-visible FD set; all guest descriptors still obey FD_CLOEXEC.
    #[allow(clippy::too_many_arguments)]
    pub fn exec_process_retaining_internal_fds(
        &mut self,
        requester_driver: &str,
        pid: u32,
        command: &str,
        args: Vec<String>,
        env: BTreeMap<String, String>,
        _cwd: String,
        retained_internal_fds: &[u32],
        additional_cloexec_fds: &[u32],
        image_command: Option<&str>,
    ) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        // execve has no cwd argument. Resolve the new image from the process's
        // existing working directory and retain that directory across the
        // image replacement regardless of what an internal caller supplied.
        let cwd = self
            .processes
            .get(pid)
            .ok_or_else(|| KernelError::no_such_process(pid))?
            .cwd;
        if let Some(image_command) = image_command {
            // The sidecar launches the image through its runtime driver, but
            // Linux still applies execve pathname checks to the guest file.
            // Validate the literal path (including directory and execute-bit
            // checks) before the commit point.
            self.validate_executable_path(image_command, &cwd)?;
        }
        let resolved = self.resolve_spawn_command(command, &args, &cwd, Some(pid))?;
        let image_command = image_command.unwrap_or(&resolved.command);
        let (committed_argv0, committed_args) = resolved
            .args
            .split_first()
            .map(|(argv0, args)| (argv0.clone(), args.to_vec()))
            .unwrap_or_else(|| (String::new(), Vec::new()));
        self.resources
            .check_process_argv_bytes(&committed_argv0, &committed_args)?;
        self.resources
            .check_process_env_bytes(&BTreeMap::new(), &env)?;
        check_command_execution(
            &self.vm_id,
            &self.permissions,
            image_command,
            &committed_args,
            Some(&cwd),
            &env,
        )?;

        // Keep the FD table locked across the only fallible commit operation.
        // If ProcessTable::exec rejects the replacement, no descriptor has
        // changed. Once it succeeds, removing entries already present in this
        // table is infallible, so callers can safely treat Ok as the execve
        // point of no return.
        let closed_entries = {
            let mut tables = lock_or_recover(&self.fd_tables);
            let table = tables
                .get_mut(pid)
                .ok_or_else(|| KernelError::no_such_process(pid))?;
            let mut fds = table
                .close_on_exec_fds()
                .into_iter()
                .collect::<BTreeSet<_>>();
            // libc also tracks CLOEXEC for runner-local descriptors. Close any
            // forwarded descriptor that belongs to the kernel table and leave
            // runner-local handles for the in-place image swap to close.
            fds.extend(
                additional_cloexec_fds
                    .iter()
                    .copied()
                    .filter(|fd| table.get(*fd).is_some()),
            );

            self.processes.exec(
                pid,
                resolved.driver.name().to_owned(),
                committed_argv0,
                committed_args,
                env,
                cwd,
            )?;

            let mut closed_entries = Vec::with_capacity(fds.len());
            for fd in fds {
                if retained_internal_fds.contains(&fd) {
                    continue;
                }
                if let Some(entry) = table.get(fd).cloned() {
                    // Presence was checked while holding the table lock. This
                    // cannot fail and therefore cannot leave exec half-committed.
                    let closed = table.close(fd);
                    debug_assert!(closed);
                    closed_entries.push((entry.description, entry.filetype));
                }
            }
            closed_entries
        };
        for (description, filetype) in closed_entries {
            if let Some(target) = description.lock_target() {
                self.file_locks.release_process_target(pid, target);
            }
            self.close_special_resource_if_needed(&description, filetype);
        }
        Ok(())
    }

    pub fn create_virtual_process(
        &mut self,
        requester_driver: &str,
        driver: &str,
        command: &str,
        args: Vec<String>,
        options: VirtualProcessOptions,
    ) -> KernelResult<KernelProcessHandle> {
        self.create_virtual_process_with_process_group(
            requester_driver,
            driver,
            command,
            args,
            options,
            None,
        )
    }

    pub fn create_virtual_process_with_process_group(
        &mut self,
        requester_driver: &str,
        driver: &str,
        command: &str,
        args: Vec<String>,
        options: VirtualProcessOptions,
        requested_pgid: Option<u32>,
    ) -> KernelResult<KernelProcessHandle> {
        self.assert_not_terminated()?;
        if let Some(parent_pid) = options.parent_pid {
            self.assert_driver_owns(requester_driver, parent_pid)?;
        }

        let parent_context = options
            .parent_pid
            .map(|pid| self.processes.inherited_context(pid))
            .transpose()?;
        let cwd = options.cwd.clone().unwrap_or_else(|| {
            parent_context
                .as_ref()
                .map(|context| context.cwd.clone())
                .unwrap_or_else(|| self.cwd.clone())
        });
        self.resources.check_process_argv_bytes(command, &args)?;
        self.resources
            .check_process_env_bytes(&self.env, &options.env)?;

        let mut env = parent_context
            .as_ref()
            .map(|context| context.env.clone())
            .unwrap_or_else(|| self.env.clone());
        env.extend(options.env.clone());
        check_command_execution(
            &self.vm_id,
            &self.permissions,
            command,
            &args,
            Some(&cwd),
            &env,
        )?;

        let inherited_fds = {
            let tables = lock_or_recover(&self.fd_tables);
            options
                .parent_pid
                .and_then(|pid| tables.get(pid).map(ProcessFdTable::len))
                .unwrap_or(3)
        };
        self.resources
            .check_process_spawn(&self.resource_snapshot(), inherited_fds)?;
        let process_umask = match options.parent_pid {
            Some(parent_pid) => self.processes.get_umask(parent_pid)?,
            None => DEFAULT_PROCESS_UMASK,
        };

        let mut context = parent_context.unwrap_or_else(|| ProcessContext {
            identity: self.users.identity(),
            ..ProcessContext::default()
        });
        context.ppid = options.parent_pid.unwrap_or(0);
        context.env = env;
        context.cwd = cwd;
        context.umask = process_umask;

        self.register_process(
            String::from(driver),
            String::from(command),
            args,
            context,
            Some(requester_driver),
            requested_pgid,
            false,
        )
    }

    pub fn read_process_stdin(
        &mut self,
        requester_driver: &str,
        pid: u32,
        length: usize,
        timeout: Option<Duration>,
    ) -> KernelResult<Option<Vec<u8>>> {
        self.fd_read_with_timeout_result(requester_driver, pid, 0, length, timeout)
    }

    pub fn write_process_stdout(
        &mut self,
        requester_driver: &str,
        pid: u32,
        data: &[u8],
    ) -> KernelResult<usize> {
        self.fd_write(requester_driver, pid, 1, data)
    }

    pub fn write_process_stderr(
        &mut self,
        requester_driver: &str,
        pid: u32,
        data: &[u8],
    ) -> KernelResult<usize> {
        self.fd_write(requester_driver, pid, 2, data)
    }

    pub fn exit_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        exit_code: i32,
    ) -> KernelResult<()> {
        self.assert_driver_owns(requester_driver, pid)?;
        self.processes.mark_exited(pid, exit_code);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn register_process(
        &mut self,
        driver_name: String,
        command: String,
        args: Vec<String>,
        mut ctx: ProcessContext,
        requester_driver: Option<&str>,
        requested_pgid: Option<u32>,
        preserve_cloexec: bool,
    ) -> KernelResult<KernelProcessHandle> {
        let pid = self.processes.allocate_pid()?;
        ctx.pid = pid;

        if let (Some(requester), Some(target_pgid)) = (requester_driver, requested_pgid) {
            if target_pgid != 0 && target_pgid != pid {
                if let Some(group_owner) =
                    self.processes
                        .list_processes()
                        .into_values()
                        .find(|process| {
                            process.pgid == target_pgid && process.status == ProcessStatus::Running
                        })
                {
                    if group_owner.driver != requester {
                        return Err(KernelError::permission_denied(format!(
                            "driver \"{requester}\" cannot join process group {target_pgid} owned by \"{}\"",
                            group_owner.driver
                        )));
                    }
                }
            }
        }

        let process = Arc::new(StubDriverProcess::default());
        self.processes.register_with_process_group(
            pid,
            driver_name.clone(),
            command,
            args,
            ctx.clone(),
            process.clone(),
            requested_pgid,
        )?;

        {
            let mut tables = lock_or_recover(&self.fd_tables);
            if ctx.ppid != 0 {
                let parent_pid = ctx.ppid;
                if preserve_cloexec {
                    tables.fork_preserving_cloexec(parent_pid, pid);
                } else {
                    tables.fork(parent_pid, pid);
                }
            } else {
                tables.create(pid);
            }
        }

        let mut owners = lock_or_recover(&self.driver_pids);
        owners.entry(driver_name.clone()).or_default().insert(pid);
        if let Some(requester) = requester_driver {
            owners
                .entry(String::from(requester))
                .or_default()
                .insert(pid);
        }

        Ok(KernelProcessHandle {
            pid,
            driver: driver_name,
            process,
        })
    }

    pub fn waitpid(&mut self, pid: u32) -> KernelResult<WaitPidResult> {
        let (pid, status) = self.processes.waitpid(pid)?;
        self.cleanup_process_resources(pid);
        Ok(WaitPidResult { pid, status })
    }

    pub fn waitpid_with_options(
        &mut self,
        requester_driver: &str,
        waiter_pid: u32,
        pid: i32,
        flags: WaitPidFlags,
    ) -> KernelResult<Option<WaitPidEventResult>> {
        self.assert_driver_owns(requester_driver, waiter_pid)?;
        let result = self.processes.waitpid_for(waiter_pid, pid, flags)?;
        Ok(result.map(|result| self.finish_waitpid_event(result)))
    }

    pub fn take_nonterminal_wait_event(
        &self,
        requester_driver: &str,
        waiter_pid: u32,
        pid: i32,
        flags: WaitPidFlags,
    ) -> KernelResult<Option<WaitPidEventResult>> {
        self.assert_driver_owns(requester_driver, waiter_pid)?;
        let result = self
            .processes
            .take_nonterminal_wait_event_for(waiter_pid, pid, flags)?;
        Ok(result.map(|result| WaitPidEventResult {
            pid: result.pid,
            status: result.status,
            event: result.event,
        }))
    }

    pub fn wait_and_reap(&mut self, pid: u32) -> KernelResult<(u32, i32)> {
        let result = self.waitpid(pid)?;
        Ok((result.pid, result.status))
    }

    pub fn open_pipe(&mut self, requester_driver: &str, pid: u32) -> KernelResult<(u32, u32)> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        let identity = self.process_identity(requester_driver, pid)?;
        self.resources
            .check_pipe_allocation(&self.resource_snapshot())?;
        let (read_fd, write_fd, read_description_id) = {
            let mut tables = lock_or_recover(&self.fd_tables);
            let table = tables
                .get_mut(pid)
                .ok_or_else(|| KernelError::no_such_process(pid))?;
            let (read_fd, write_fd) = self.pipes.create_pipe_fds(table)?;
            let read_description_id = table
                .get(read_fd)
                .expect("new pipe read descriptor must exist")
                .description
                .id();
            (read_fd, write_fd, read_description_id)
        };
        self.pipes
            .set_owner(read_description_id, identity.euid, identity.egid)?;
        Ok((read_fd, write_fd))
    }

    pub fn fd_pipe_has_reader_in_other_process(
        &self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
    ) -> KernelResult<bool> {
        self.assert_driver_owns(requester_driver, pid)?;
        let tables = lock_or_recover(&self.fd_tables);
        let write_description_id = tables
            .get(pid)
            .and_then(|table| table.get(fd))
            .ok_or_else(|| KernelError::bad_file_descriptor(fd))?
            .description
            .id();
        Ok(tables.pids().into_iter().any(|other_pid| {
            other_pid != pid
                && tables.get(other_pid).is_some_and(|table| {
                    table.values().any(|entry| {
                        self.pipes
                            .is_write_to_read_pair(write_description_id, entry.description.id())
                    })
                })
        }))
    }

    pub fn fd_snapshot(
        &self,
        requester_driver: &str,
        pid: u32,
    ) -> KernelResult<Vec<ProcessFdSnapshotEntry>> {
        self.assert_driver_owns(requester_driver, pid)?;
        let tables = lock_or_recover(&self.fd_tables);
        let table = tables
            .get(pid)
            .ok_or_else(|| KernelError::no_such_process(pid))?;
        Ok(table
            .values()
            .map(|entry| ProcessFdSnapshotEntry {
                fd: entry.fd,
                fd_flags: entry.fd_flags,
                status_flags: entry.status_flags | entry.description.flags(),
                filetype: entry.filetype,
                is_socket: self.fd_socket_id(&entry.description).is_some(),
                is_pipe: self.pipes.is_pipe(entry.description.id()),
                is_pty: self.ptys.is_pty(entry.description.id()),
            })
            .collect())
    }

    /// Create a connected AF_UNIX socket pair whose endpoints live in the
    /// process descriptor table. The socket records are owned by their open
    /// file descriptions rather than by a PID so SCM_RIGHTS and spawn
    /// inheritance preserve them after the creating process exits.
    pub fn fd_socketpair(
        &mut self,
        requester_driver: &str,
        pid: u32,
        socket_type: SocketType,
        nonblocking: bool,
        close_on_exec: bool,
    ) -> KernelResult<(u32, u32)> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        let identity = self.process_identity(requester_driver, pid)?;
        let spec = match socket_type {
            SocketType::Stream => SocketSpec::unix_stream(),
            SocketType::Datagram => SocketSpec::unix_datagram(),
            SocketType::SeqPacket => SocketSpec::unix_seqpacket(),
        };

        let mut snapshot = self.resource_snapshot();
        self.resources.check_fd_allocation(&snapshot, 2)?;
        for _ in 0..2 {
            self.resources.check_socket_allocation(&snapshot)?;
            snapshot.sockets = snapshot.sockets.saturating_add(1);
            self.resources.check_socket_state_transition(
                &snapshot,
                SocketState::Created,
                SocketState::Connected,
            )?;
            snapshot.socket_connections = snapshot.socket_connections.saturating_add(1);
        }

        let status_flags = if nonblocking { O_NONBLOCK } else { 0 };
        let fd_flags = if close_on_exec { FD_CLOEXEC } else { 0 };
        let filetype = if socket_type == SocketType::Datagram {
            FILETYPE_SOCKET_DGRAM
        } else {
            FILETYPE_SOCKET_STREAM
        };
        let (first_fd, second_fd, first_description, second_description) = {
            let mut tables = lock_or_recover(&self.fd_tables);
            let table = tables
                .get_mut(pid)
                .ok_or_else(|| KernelError::no_such_process(pid))?;
            table.open_pair_with_details(
                "socketpair:first",
                "socketpair:second",
                status_flags,
                fd_flags,
                filetype,
            )?
        };

        // PID 0 is reserved for description-owned sockets. Process cleanup
        // must not tear them down while another process or ancillary message
        // still retains the open file description.
        let first_socket = match self.sockets.allocate(0, spec) {
            Ok(socket) => socket.id(),
            Err(error) => {
                let mut tables = lock_or_recover(&self.fd_tables);
                if let Some(table) = tables.get_mut(pid) {
                    table.close(first_fd);
                    table.close(second_fd);
                }
                return Err(error.into());
            }
        };
        let second_socket = match self.sockets.allocate(0, spec) {
            Ok(socket) => socket.id(),
            Err(error) => {
                if let Err(cleanup_error) = self.sockets.remove(first_socket) {
                    eprintln!(
                        "[agentos] failed to roll back first socketpair socket {first_socket}: {cleanup_error}"
                    );
                }
                let mut tables = lock_or_recover(&self.fd_tables);
                if let Some(table) = tables.get_mut(pid) {
                    table.close(first_fd);
                    table.close(second_fd);
                }
                return Err(error.into());
            }
        };
        if let Err(error) = self.sockets.connect_pair(first_socket, second_socket) {
            for socket_id in [first_socket, second_socket] {
                if let Err(cleanup_error) = self.sockets.remove(socket_id) {
                    eprintln!(
                        "[agentos] failed to roll back socketpair socket {socket_id}: {cleanup_error}"
                    );
                }
            }
            let mut tables = lock_or_recover(&self.fd_tables);
            if let Some(table) = tables.get_mut(pid) {
                table.close(first_fd);
                table.close(second_fd);
            }
            return Err(error.into());
        }

        {
            let mut registry = lock_or_recover(&self.fd_sockets);
            registry.insert(
                first_description.id(),
                FdSocketEntry {
                    description: first_description,
                    socket_id: first_socket,
                    mode: 0o777,
                    uid: identity.euid,
                    gid: identity.egid,
                },
            );
            registry.insert(
                second_description.id(),
                FdSocketEntry {
                    description: second_description,
                    socket_id: second_socket,
                    mode: 0o777,
                    uid: identity.euid,
                    gid: identity.egid,
                },
            );
        }
        self.poll_notifier.notify();
        Ok((first_fd, second_fd))
    }

    /// Attach an existing kernel socket to a description-owned fd. Sidecar
    /// transports use this when a raw socket becomes transferable through
    /// SCM_RIGHTS; owner 0 keeps process teardown from destroying the socket
    /// while the open description is queued in another process.
    pub fn fd_adopt_socket(
        &mut self,
        requester_driver: &str,
        pid: u32,
        socket_id: SocketId,
        status_flags: u32,
    ) -> KernelResult<u32> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        let identity = self.process_identity(requester_driver, pid)?;
        let socket = self
            .sockets
            .get(socket_id)
            .ok_or_else(|| KernelError::new("ENOENT", format!("no such socket {socket_id}")))?;
        if socket.owner_pid() != pid && socket.owner_pid() != 0 {
            return Err(KernelError::permission_denied(format!(
                "process {pid} does not own socket {socket_id}"
            )));
        }
        self.resources
            .check_fd_allocation(&self.resource_snapshot(), 1)?;
        let filetype = if socket.spec().socket_type == SocketType::Datagram {
            FILETYPE_SOCKET_DGRAM
        } else {
            FILETYPE_SOCKET_STREAM
        };
        let (fd, description) = {
            let mut tables = lock_or_recover(&self.fd_tables);
            let table = tables
                .get_mut(pid)
                .ok_or_else(|| KernelError::no_such_process(pid))?;
            let fd = table.open_with_details(
                &format!("socket:{socket_id}"),
                status_flags,
                filetype,
                None,
            )?;
            let description = Arc::clone(
                &table
                    .get(fd)
                    .expect("newly adopted socket fd must exist")
                    .description,
            );
            (fd, description)
        };
        self.sockets.reassign_owner(socket_id, 0)?;
        lock_or_recover(&self.fd_sockets).insert(
            description.id(),
            FdSocketEntry {
                description,
                socket_id,
                mode: 0o777,
                uid: identity.euid,
                gid: identity.egid,
            },
        );
        Ok(fd)
    }

    /// Attach an existing kernel socket directly to a transferable open file
    /// description. Unlike `fd_adopt_socket`, this does not allocate a
    /// temporary descriptor in the sender, matching SCM_RIGHTS behavior when
    /// the sender is already at its per-process fd limit.
    pub fn fd_adopt_socket_transfer(
        &mut self,
        requester_driver: &str,
        pid: u32,
        socket_id: SocketId,
        status_flags: u32,
    ) -> KernelResult<TransferredFd> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        let identity = self.process_identity(requester_driver, pid)?;
        let socket = self
            .sockets
            .get(socket_id)
            .ok_or_else(|| KernelError::new("ENOENT", format!("no such socket {socket_id}")))?;
        if socket.owner_pid() != pid && socket.owner_pid() != 0 {
            return Err(KernelError::permission_denied(format!(
                "process {pid} does not own socket {socket_id}"
            )));
        }
        let filetype = if socket.spec().socket_type == SocketType::Datagram {
            FILETYPE_SOCKET_DGRAM
        } else {
            FILETYPE_SOCKET_STREAM
        };
        let transfer = {
            let tables = lock_or_recover(&self.fd_tables);
            let table = tables
                .get(pid)
                .ok_or_else(|| KernelError::no_such_process(pid))?;
            table.create_transfer(&format!("socket:{socket_id}"), status_flags, filetype)
        };
        self.sockets.reassign_owner(socket_id, 0)?;
        lock_or_recover(&self.fd_sockets).insert(
            transfer.description_id(),
            FdSocketEntry {
                description: transfer.description(),
                socket_id,
                mode: 0o777,
                uid: identity.euid,
                gid: identity.egid,
            },
        );
        Ok(transfer)
    }

    pub fn fd_transfer(
        &self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
    ) -> KernelResult<TransferredFd> {
        self.assert_driver_owns(requester_driver, pid)?;
        let tables = lock_or_recover(&self.fd_tables);
        let table = tables
            .get(pid)
            .ok_or_else(|| KernelError::no_such_process(pid))?;
        Ok(table.transfer(fd)?)
    }

    /// Install a transferred open file description at an exact descriptor in
    /// another process. Unlike reopening `TransferredFd`'s path, this preserves
    /// the same description identity, offset, status flags, and special-resource
    /// ownership that existed when the transfer was captured.
    pub fn fd_install_transfer_at(
        &mut self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
        fd_flags: u32,
        transfer: &TransferredFd,
    ) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        let replaced = {
            let mut tables = lock_or_recover(&self.fd_tables);
            let table = tables
                .get_mut(pid)
                .ok_or_else(|| KernelError::no_such_process(pid))?;
            let replaced = table
                .get(fd)
                .map(|entry| (Arc::clone(&entry.description), entry.filetype));
            table.install_transferred_at(transfer, fd, fd_flags)?;
            replaced
        };
        if let Some((description, filetype)) = replaced {
            self.close_special_resource_if_needed(&description, filetype);
        }
        Ok(())
    }

    pub fn fd_socket_sendmsg(
        &mut self,
        requester_driver: &str,
        pid: u32,
        socket_fd: u32,
        data: &[u8],
        rights_fds: &[u32],
    ) -> KernelResult<usize> {
        let rights = rights_fds
            .iter()
            .copied()
            .map(FdTransferRequest::Fd)
            .collect::<Vec<_>>();
        self.fd_socket_sendmsg_transfers(requester_driver, pid, socket_fd, data, &rights)
    }

    pub fn fd_socket_sendmsg_transfers(
        &mut self,
        requester_driver: &str,
        pid: u32,
        socket_fd: u32,
        data: &[u8],
        transfer_requests: &[FdTransferRequest],
    ) -> KernelResult<usize> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        let (socket_id, rights) = {
            let tables = lock_or_recover(&self.fd_tables);
            let table = tables
                .get(pid)
                .ok_or_else(|| KernelError::no_such_process(pid))?;
            let socket_entry = table
                .get(socket_fd)
                .ok_or_else(|| KernelError::bad_file_descriptor(socket_fd))?;
            let socket_id = self
                .fd_socket_id(&socket_entry.description)
                .ok_or_else(|| KernelError::new("ENOTSOCK", "descriptor is not a socket"))?;
            let rights = transfer_requests
                .iter()
                .map(|request| match request {
                    FdTransferRequest::Fd(fd) => table
                        .transfer(*fd)
                        .map(TransferredSocketRight::Fd)
                        .map_err(KernelError::from),
                    FdTransferRequest::Opaque(resource) => {
                        Ok(TransferredSocketRight::Opaque(Arc::clone(resource)))
                    }
                })
                .collect::<KernelResult<Vec<_>>>()?;
            (socket_id, rights)
        };

        let socket = self
            .sockets
            .get(socket_id)
            .ok_or_else(|| KernelError::bad_file_descriptor(socket_fd))?;
        self.sockets.check_write(socket_id)?;
        let snapshot = self.resource_snapshot();
        if socket.spec().socket_type == SocketType::Stream {
            self.resources
                .check_socket_buffer_growth(&snapshot, data.len())?;
        } else {
            self.resources
                .check_socket_datagram_enqueue(&snapshot, data.len())?;
        }
        let written = self.sockets.send_message(socket_id, data, rights)?;
        if written > 0 || socket.spec().socket_type != SocketType::Stream {
            self.poll_notifier.notify();
        }
        Ok(written)
    }

    // recvmsg(2) exposes independent buffer, rights, and flag controls; keep
    // that syscall-shaped surface explicit for parity with the guest ABI.
    #[allow(clippy::too_many_arguments)]
    pub fn fd_socket_recvmsg(
        &mut self,
        requester_driver: &str,
        pid: u32,
        socket_fd: u32,
        max_bytes: usize,
        max_rights: usize,
        close_on_exec: bool,
        peek: bool,
        dontwait: bool,
        waitall: bool,
    ) -> KernelResult<Option<ReceivedFdMessage>> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        let (socket_id, available_fds, nonblocking, socket_type) = {
            let tables = lock_or_recover(&self.fd_tables);
            let table = tables
                .get(pid)
                .ok_or_else(|| KernelError::no_such_process(pid))?;
            let socket_entry = table
                .get(socket_fd)
                .ok_or_else(|| KernelError::bad_file_descriptor(socket_fd))?;
            let socket_id = self
                .fd_socket_id(&socket_entry.description)
                .ok_or_else(|| KernelError::new("ENOTSOCK", "descriptor is not a socket"))?;
            (
                socket_id,
                table.available_fd_capacity(),
                (socket_entry.description.flags() | socket_entry.status_flags) & O_NONBLOCK != 0,
                self.sockets
                    .get(socket_id)
                    .ok_or_else(|| KernelError::bad_file_descriptor(socket_fd))?
                    .spec()
                    .socket_type,
            )
        };

        let deadline = (!nonblocking && !dontwait)
            .then(|| {
                self.blocking_read_timeout()
                    .map(|wait| Instant::now() + wait)
            })
            .flatten();
        let mut message = loop {
            let generation = self.poll_notifier.snapshot();
            let wait_for_full = waitall
                && socket_type == SocketType::Stream
                && !nonblocking
                && !dontwait
                && max_bytes > 0;
            match self
                .sockets
                .recv_message(socket_id, max_bytes, peek || wait_for_full)
            {
                Ok(Some(message)) => {
                    let peer_closed = self
                        .sockets
                        .poll(socket_id, POLLHUP)
                        .map(|events| events.intersects(POLLHUP))
                        .unwrap_or(false);
                    if wait_for_full && message.full_length < max_bytes && !peer_closed {
                        drop(message);
                    } else if wait_for_full && !peek {
                        break self.sockets.recv_message(socket_id, max_bytes, false)?;
                    } else {
                        break Some(message);
                    }
                }
                Ok(None) => break None,
                Err(error) if error.code() == "EAGAIN" && !nonblocking && !dontwait => {
                    let remaining =
                        deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()));
                    if matches!(remaining, Some(duration) if duration.is_zero())
                        || !self.poll_notifier.wait_for_change(generation, remaining)
                    {
                        return Err(KernelError::new(
                            "EAGAIN",
                            "blocking socket receive timed out; raise limits.resources.maxBlockingReadMs",
                        ));
                    }
                }
                Err(error) => return Err(error.into()),
            }
        };
        let Some(mut message) = message.take() else {
            return Ok(None);
        };
        let mut kernel_fd_count = 0usize;
        let mut install_count = 0usize;
        for right in message.rights.iter().take(max_rights) {
            if matches!(right, TransferredSocketRight::Fd(_)) {
                if kernel_fd_count >= available_fds {
                    break;
                }
                kernel_fd_count += 1;
            }
            install_count += 1;
        }
        let discarded = message.rights.split_off(install_count);
        let control_truncated = !discarded.is_empty();
        let mut fd_transfers = Vec::with_capacity(kernel_fd_count);
        for right in &message.rights {
            if let TransferredSocketRight::Fd(fd) = right {
                fd_transfers.push(fd.clone());
            }
        }
        let installed_fds = if fd_transfers.is_empty() {
            Vec::new()
        } else {
            let mut tables = lock_or_recover(&self.fd_tables);
            let table = tables
                .get_mut(pid)
                .ok_or_else(|| KernelError::no_such_process(pid))?;
            table.install_transferred(&fd_transfers, close_on_exec)?
        };
        let mut installed_fds = installed_fds.into_iter();
        let rights = message
            .rights
            .drain(..)
            .map(|right| match right {
                TransferredSocketRight::Fd(_) => ReceivedFdRight::Fd(
                    installed_fds
                        .next()
                        .expect("every retained fd transfer must be installed"),
                ),
                TransferredSocketRight::Opaque(resource) => ReceivedFdRight::Opaque(resource),
            })
            .collect();
        debug_assert!(installed_fds.next().is_none());
        drop(discarded);
        drop(fd_transfers);
        prune_fd_sockets(&self.sockets, &self.fd_sockets);
        self.poll_notifier.notify();
        Ok(Some(ReceivedFdMessage {
            payload: message.payload,
            rights,
            payload_truncated: message.truncated,
            control_truncated,
            full_length: message.full_length,
        }))
    }

    pub fn fd_socket_shutdown(
        &mut self,
        requester_driver: &str,
        pid: u32,
        socket_fd: u32,
        how: SocketShutdown,
    ) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        let socket_id = self.fd_socket_id_for_fd(pid, socket_fd)?;
        self.sockets.shutdown(socket_id, how)?;
        prune_fd_sockets(&self.sockets, &self.fd_sockets);
        self.poll_notifier.notify();
        Ok(())
    }

    pub fn open_pty(
        &mut self,
        requester_driver: &str,
        pid: u32,
    ) -> KernelResult<(u32, u32, String)> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        self.resources
            .check_pty_allocation(&self.resource_snapshot())?;
        let mut tables = lock_or_recover(&self.fd_tables);
        let table = tables
            .get_mut(pid)
            .ok_or_else(|| KernelError::no_such_process(pid))?;
        Ok(self.ptys.create_pty_fds(table)?)
    }

    pub fn socket_create(
        &mut self,
        requester_driver: &str,
        pid: u32,
        spec: SocketSpec,
    ) -> KernelResult<SocketId> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        // Native sidecars admit socket/connection counts through the shared
        // capability registry. Retain the legacy snapshot admission only for
        // kernel consumers that have not injected that ledger (including the
        // browser build).
        if !self.sockets.has_resource_ledger() {
            self.resources
                .check_socket_allocation(&self.resource_snapshot())?;
        }
        Ok(self.sockets.allocate(pid, spec)?.id())
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn set_socket_resource_ledger(
        &mut self,
        resources: Arc<agentos_runtime::accounting::ResourceLedger>,
    ) -> KernelResult<()> {
        self.sockets.set_resource_ledger(resources)?;
        Ok(())
    }

    pub fn set_socket_readiness_sink<S>(&mut self, sink: Option<S>)
    where
        S: Fn(SocketReadiness) + Send + Sync + 'static,
    {
        self.sockets.set_readiness_sink(sink);
    }

    pub fn socket_get(&self, socket_id: SocketId) -> Option<SocketRecord> {
        self.sockets.get(socket_id)
    }

    pub fn socket_records_for_pid(&self, pid: u32) -> Vec<SocketRecord> {
        self.sockets.records_for_owner(pid)
    }

    pub fn socket_bind_inet(
        &mut self,
        requester_driver: &str,
        pid: u32,
        socket_id: SocketId,
        address: InetSocketAddress,
    ) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        let existing = self
            .sockets
            .get(socket_id)
            .ok_or_else(|| KernelError::new("ENOENT", format!("no such socket {socket_id}")))?;
        if existing.owner_pid() != pid && existing.owner_pid() != 0 {
            return Err(KernelError::permission_denied(format!(
                "process {pid} does not own socket {socket_id}"
            )));
        }
        check_network_access(
            &self.vm_id,
            &self.permissions,
            NetworkOperation::Listen,
            &format_tcp_resource(address.host(), address.port()),
        )?;

        self.sockets.bind_inet(socket_id, address)?;
        self.poll_notifier.notify();
        Ok(())
    }

    pub fn socket_bind_unix(
        &mut self,
        requester_driver: &str,
        pid: u32,
        socket_id: SocketId,
        path: impl Into<String>,
    ) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        let existing = self
            .sockets
            .get(socket_id)
            .ok_or_else(|| KernelError::new("ENOENT", format!("no such socket {socket_id}")))?;
        if existing.owner_pid() != pid && existing.owner_pid() != 0 {
            return Err(KernelError::permission_denied(format!(
                "process {pid} does not own socket {socket_id}"
            )));
        }

        self.sockets
            .bind_unix(socket_id, normalize_path(&path.into()))?;
        self.poll_notifier.notify();
        Ok(())
    }

    pub fn socket_listen(
        &mut self,
        requester_driver: &str,
        pid: u32,
        socket_id: SocketId,
        backlog: usize,
    ) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        let existing = self
            .sockets
            .get(socket_id)
            .ok_or_else(|| KernelError::new("ENOENT", format!("no such socket {socket_id}")))?;
        if existing.owner_pid() != pid && existing.owner_pid() != 0 {
            return Err(KernelError::permission_denied(format!(
                "process {pid} does not own socket {socket_id}"
            )));
        }
        if let Some(address) = existing.local_address() {
            check_network_access(
                &self.vm_id,
                &self.permissions,
                NetworkOperation::Listen,
                &format_tcp_resource(address.host(), address.port()),
            )?;
        }

        self.sockets.listen(socket_id, backlog)?;
        self.poll_notifier.notify();
        Ok(())
    }

    pub fn socket_queue_incoming_tcp_connection(
        &mut self,
        requester_driver: &str,
        pid: u32,
        listener_socket_id: SocketId,
        peer_address: InetSocketAddress,
    ) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        let existing = self.sockets.get(listener_socket_id).ok_or_else(|| {
            KernelError::new("ENOENT", format!("no such socket {listener_socket_id}"))
        })?;
        if existing.owner_pid() != pid && existing.owner_pid() != 0 {
            return Err(KernelError::permission_denied(format!(
                "process {pid} does not own socket {listener_socket_id}"
            )));
        }

        self.sockets
            .enqueue_incoming_tcp_connection(listener_socket_id, peer_address)?;
        self.poll_notifier.notify();
        Ok(())
    }

    pub fn socket_accept(
        &mut self,
        requester_driver: &str,
        pid: u32,
        listener_socket_id: SocketId,
    ) -> KernelResult<SocketId> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        let existing = self.sockets.get(listener_socket_id).ok_or_else(|| {
            KernelError::new("ENOENT", format!("no such socket {listener_socket_id}"))
        })?;
        if existing.owner_pid() != pid && existing.owner_pid() != 0 {
            return Err(KernelError::permission_denied(format!(
                "process {pid} does not own socket {listener_socket_id}"
            )));
        }

        if !self.sockets.has_resource_ledger() {
            let snapshot = self.resource_snapshot();
            self.resources.check_socket_allocation(&snapshot)?;
            self.resources.check_socket_state_transition(
                &snapshot,
                SocketState::Created,
                SocketState::Connected,
            )?;
        }

        let socket_id = self.sockets.accept(listener_socket_id)?.id();
        self.poll_notifier.notify();
        Ok(socket_id)
    }

    pub fn socket_connect_pair(
        &mut self,
        requester_driver: &str,
        pid: u32,
        socket_id: SocketId,
        peer_socket_id: SocketId,
    ) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        let existing = self
            .sockets
            .get(socket_id)
            .ok_or_else(|| KernelError::new("ENOENT", format!("no such socket {socket_id}")))?;
        if existing.owner_pid() != pid && existing.owner_pid() != 0 {
            return Err(KernelError::permission_denied(format!(
                "process {pid} does not own socket {socket_id}"
            )));
        }

        let peer = self.sockets.get(peer_socket_id).ok_or_else(|| {
            KernelError::new("ENOENT", format!("no such socket {peer_socket_id}"))
        })?;
        self.assert_driver_owns(requester_driver, peer.owner_pid())?;

        if !self.sockets.has_resource_ledger() {
            let mut snapshot = self.resource_snapshot();
            for current_state in [existing.state(), peer.state()] {
                self.resources.check_socket_state_transition(
                    &snapshot,
                    current_state,
                    SocketState::Connected,
                )?;
                if !current_state.counts_as_connection() {
                    snapshot.socket_connections = snapshot.socket_connections.saturating_add(1);
                }
            }
        }

        self.sockets.connect_pair(socket_id, peer_socket_id)?;
        self.poll_notifier.notify();
        Ok(())
    }

    pub fn socket_connect_unix(
        &mut self,
        requester_driver: &str,
        pid: u32,
        socket_id: SocketId,
        target_path: impl Into<String>,
    ) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        let existing = self
            .sockets
            .get(socket_id)
            .ok_or_else(|| KernelError::new("ENOENT", format!("no such socket {socket_id}")))?;
        if existing.owner_pid() != pid && existing.owner_pid() != 0 {
            return Err(KernelError::permission_denied(format!(
                "process {pid} does not own socket {socket_id}"
            )));
        }

        let target_path = normalize_path(&target_path.into());
        self.sockets
            .find_bound_unix_socket(&target_path)
            .ok_or_else(|| {
                KernelError::new(
                    "ECONNREFUSED",
                    format!("no listening socket bound at path {target_path}"),
                )
            })?;

        if !self.sockets.has_resource_ledger() {
            let mut snapshot = self.resource_snapshot();
            self.resources.check_socket_allocation(&snapshot)?;
            for current_state in [existing.state(), SocketState::Created] {
                self.resources.check_socket_state_transition(
                    &snapshot,
                    current_state,
                    SocketState::Connected,
                )?;
                if !current_state.counts_as_connection() {
                    snapshot.socket_connections = snapshot.socket_connections.saturating_add(1);
                }
            }
        }

        self.sockets
            .connect_to_bound_unix_stream(socket_id, target_path)?;
        self.poll_notifier.notify();
        Ok(())
    }

    pub fn socket_connect_inet_loopback(
        &mut self,
        requester_driver: &str,
        pid: u32,
        socket_id: SocketId,
        target_address: InetSocketAddress,
    ) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        let existing = self
            .sockets
            .get(socket_id)
            .ok_or_else(|| KernelError::new("ENOENT", format!("no such socket {socket_id}")))?;
        if existing.owner_pid() != pid && existing.owner_pid() != 0 {
            return Err(KernelError::permission_denied(format!(
                "process {pid} does not own socket {socket_id}"
            )));
        }
        check_network_access(
            &self.vm_id,
            &self.permissions,
            NetworkOperation::Http,
            &format_tcp_resource(target_address.host(), target_address.port()),
        )?;
        self.check_loopback_port_allowed(
            SocketSpec::tcp(),
            &target_address,
            "TCP loopback connect",
        )?;

        self.sockets
            .find_bound_inet_socket(SocketSpec::tcp(), &target_address)
            .ok_or_else(|| {
                KernelError::new(
                    "ECONNREFUSED",
                    format!(
                        "no listening socket bound at {}:{}",
                        target_address.host(),
                        target_address.port()
                    ),
                )
            })?;

        if !self.sockets.has_resource_ledger() {
            let mut snapshot = self.resource_snapshot();
            self.resources.check_socket_allocation(&snapshot)?;
            for current_state in [existing.state(), SocketState::Created] {
                self.resources.check_socket_state_transition(
                    &snapshot,
                    current_state,
                    SocketState::Connected,
                )?;
                if !current_state.counts_as_connection() {
                    snapshot.socket_connections = snapshot.socket_connections.saturating_add(1);
                }
            }
        }

        self.sockets
            .connect_to_bound_inet_stream(socket_id, target_address)?;
        self.poll_notifier.notify();
        Ok(())
    }

    pub fn socket_send_to_inet_loopback(
        &mut self,
        requester_driver: &str,
        pid: u32,
        socket_id: SocketId,
        target_address: InetSocketAddress,
        data: &[u8],
    ) -> KernelResult<usize> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        let existing = self
            .sockets
            .get(socket_id)
            .ok_or_else(|| KernelError::new("ENOENT", format!("no such socket {socket_id}")))?;
        if existing.owner_pid() != pid && existing.owner_pid() != 0 {
            return Err(KernelError::permission_denied(format!(
                "process {pid} does not own socket {socket_id}"
            )));
        }
        if existing.spec() != SocketSpec::udp()
            || existing.state() != SocketState::Bound
            || existing.local_address().is_none()
        {
            self.sockets
                .check_send_to_bound_udp_socket(socket_id, target_address.clone())?;
        }
        check_network_access(
            &self.vm_id,
            &self.permissions,
            NetworkOperation::Http,
            &format_tcp_resource(target_address.host(), target_address.port()),
        )?;
        self.check_loopback_port_allowed(SocketSpec::udp(), &target_address, "UDP loopback send")?;

        self.sockets
            .check_send_to_bound_udp_socket(socket_id, target_address.clone())?;
        if !self.sockets.has_resource_ledger() {
            self.resources
                .check_socket_datagram_enqueue(&self.resource_snapshot(), data.len())?;
        }
        let written = self
            .sockets
            .send_to_bound_udp_socket(socket_id, target_address, data)?;
        if written > 0 {
            self.poll_notifier.notify();
        }
        Ok(written)
    }

    pub fn socket_connect_udp_loopback(
        &mut self,
        requester_driver: &str,
        pid: u32,
        socket_id: SocketId,
        target_address: InetSocketAddress,
    ) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        let existing = self
            .sockets
            .get(socket_id)
            .ok_or_else(|| KernelError::new("ENOENT", format!("no such socket {socket_id}")))?;
        if existing.owner_pid() != pid {
            return Err(KernelError::permission_denied(format!(
                "process {pid} does not own socket {socket_id}"
            )));
        }
        if existing.spec().socket_type != SocketType::Datagram
            || existing.state() != SocketState::Bound
            || existing.local_address().is_none()
        {
            return Err(KernelError::new(
                "EINVAL",
                format!("UDP socket {socket_id} must be bound before connect"),
            ));
        }
        check_network_access(
            &self.vm_id,
            &self.permissions,
            NetworkOperation::Http,
            &format_tcp_resource(target_address.host(), target_address.port()),
        )?;
        self.check_loopback_port_allowed(existing.spec(), &target_address, "UDP loopback connect")?;
        self.sockets
            .connect_bound_udp_socket(socket_id, target_address)?;
        Ok(())
    }

    pub fn socket_disconnect_udp(
        &mut self,
        requester_driver: &str,
        pid: u32,
        socket_id: SocketId,
    ) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        let existing = self
            .sockets
            .get(socket_id)
            .ok_or_else(|| KernelError::new("ENOENT", format!("no such socket {socket_id}")))?;
        if existing.owner_pid() != pid {
            return Err(KernelError::permission_denied(format!(
                "process {pid} does not own socket {socket_id}"
            )));
        }
        self.sockets.disconnect_bound_udp_socket(socket_id)?;
        Ok(())
    }

    fn check_loopback_port_allowed(
        &self,
        spec: SocketSpec,
        target_address: &InetSocketAddress,
        operation: &str,
    ) -> KernelResult<()> {
        if self
            .sockets
            .find_bound_inet_socket(spec, target_address)
            .is_some()
            || self.loopback_exempt_ports.contains(&target_address.port())
        {
            return Ok(());
        }

        Err(KernelError::permission_denied(format!(
            "{operation} to {}:{} is not owned by this VM and is not loopback-exempt",
            target_address.host(),
            target_address.port()
        )))
    }

    pub fn socket_recv_datagram(
        &mut self,
        requester_driver: &str,
        pid: u32,
        socket_id: SocketId,
        max_bytes: usize,
    ) -> KernelResult<Option<ReceivedDatagram>> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        let existing = self
            .sockets
            .get(socket_id)
            .ok_or_else(|| KernelError::new("ENOENT", format!("no such socket {socket_id}")))?;
        if existing.owner_pid() != pid && existing.owner_pid() != 0 {
            return Err(KernelError::permission_denied(format!(
                "process {pid} does not own socket {socket_id}"
            )));
        }

        let result = self.sockets.recv_datagram(socket_id, max_bytes)?;
        if result.is_some() {
            self.poll_notifier.notify();
        }
        Ok(result)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn socket_recv_datagram_charged(
        &mut self,
        requester_driver: &str,
        pid: u32,
        socket_id: SocketId,
        max_bytes: usize,
    ) -> KernelResult<Option<crate::socket_table::ChargedReceivedDatagram>> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        let existing = self
            .sockets
            .get(socket_id)
            .ok_or_else(|| KernelError::new("ENOENT", format!("no such socket {socket_id}")))?;
        if existing.owner_pid() != pid {
            return Err(KernelError::permission_denied(format!(
                "process {pid} does not own socket {socket_id}"
            )));
        }
        let result = self.sockets.recv_datagram_charged(socket_id, max_bytes)?;
        if result.is_some() {
            self.poll_notifier.notify();
        }
        Ok(result)
    }

    pub fn socket_set_datagram_option(
        &mut self,
        requester_driver: &str,
        pid: u32,
        socket_id: SocketId,
        option: DatagramSocketOption,
        enabled: bool,
    ) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        let existing = self
            .sockets
            .get(socket_id)
            .ok_or_else(|| KernelError::new("ENOENT", format!("no such socket {socket_id}")))?;
        if existing.owner_pid() != pid && existing.owner_pid() != 0 {
            return Err(KernelError::permission_denied(format!(
                "process {pid} does not own socket {socket_id}"
            )));
        }

        self.sockets
            .set_datagram_socket_option(socket_id, option, enabled)?;
        self.poll_notifier.notify();
        Ok(())
    }

    pub fn socket_add_membership(
        &mut self,
        requester_driver: &str,
        pid: u32,
        socket_id: SocketId,
        membership: SocketMulticastMembership,
    ) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        let existing = self
            .sockets
            .get(socket_id)
            .ok_or_else(|| KernelError::new("ENOENT", format!("no such socket {socket_id}")))?;
        if existing.owner_pid() != pid && existing.owner_pid() != 0 {
            return Err(KernelError::permission_denied(format!(
                "process {pid} does not own socket {socket_id}"
            )));
        }

        self.sockets
            .add_multicast_membership(socket_id, membership)?;
        self.poll_notifier.notify();
        Ok(())
    }

    pub fn socket_drop_membership(
        &mut self,
        requester_driver: &str,
        pid: u32,
        socket_id: SocketId,
        membership: SocketMulticastMembership,
    ) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        let existing = self
            .sockets
            .get(socket_id)
            .ok_or_else(|| KernelError::new("ENOENT", format!("no such socket {socket_id}")))?;
        if existing.owner_pid() != pid && existing.owner_pid() != 0 {
            return Err(KernelError::permission_denied(format!(
                "process {pid} does not own socket {socket_id}"
            )));
        }

        self.sockets
            .drop_multicast_membership(socket_id, membership)?;
        self.poll_notifier.notify();
        Ok(())
    }

    pub fn socket_set_state(
        &mut self,
        requester_driver: &str,
        pid: u32,
        socket_id: SocketId,
        state: SocketState,
    ) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        let existing = self
            .sockets
            .get(socket_id)
            .ok_or_else(|| KernelError::new("ENOENT", format!("no such socket {socket_id}")))?;
        if existing.owner_pid() != pid && existing.owner_pid() != 0 {
            return Err(KernelError::permission_denied(format!(
                "process {pid} does not own socket {socket_id}"
            )));
        }

        if !self.sockets.has_resource_ledger() {
            self.resources.check_socket_state_transition(
                &self.resource_snapshot(),
                existing.state(),
                state,
            )?;
        }
        self.sockets.update_state(socket_id, state)?;
        self.poll_notifier.notify();
        Ok(())
    }

    pub fn socket_write(
        &mut self,
        requester_driver: &str,
        pid: u32,
        socket_id: SocketId,
        data: &[u8],
    ) -> KernelResult<usize> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        let existing = self
            .sockets
            .get(socket_id)
            .ok_or_else(|| KernelError::new("ENOENT", format!("no such socket {socket_id}")))?;
        if existing.owner_pid() != pid && existing.owner_pid() != 0 {
            return Err(KernelError::permission_denied(format!(
                "process {pid} does not own socket {socket_id}"
            )));
        }

        self.sockets.check_write(socket_id)?;
        if !self.sockets.has_resource_ledger() {
            self.resources
                .check_socket_buffer_growth(&self.resource_snapshot(), data.len())?;
        }
        let written = self.sockets.write(socket_id, data)?;
        if written > 0 {
            self.poll_notifier.notify();
        }
        Ok(written)
    }

    pub fn socket_read(
        &mut self,
        requester_driver: &str,
        pid: u32,
        socket_id: SocketId,
        max_bytes: usize,
    ) -> KernelResult<Option<Vec<u8>>> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        let existing = self
            .sockets
            .get(socket_id)
            .ok_or_else(|| KernelError::new("ENOENT", format!("no such socket {socket_id}")))?;
        if existing.owner_pid() != pid && existing.owner_pid() != 0 {
            return Err(KernelError::permission_denied(format!(
                "process {pid} does not own socket {socket_id}"
            )));
        }

        let result = self.sockets.read(socket_id, max_bytes)?;
        if result.is_some() {
            self.poll_notifier.notify();
        }
        Ok(result)
    }

    pub fn socket_shutdown(
        &mut self,
        requester_driver: &str,
        pid: u32,
        socket_id: SocketId,
        how: SocketShutdown,
    ) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        let existing = self
            .sockets
            .get(socket_id)
            .ok_or_else(|| KernelError::new("ENOENT", format!("no such socket {socket_id}")))?;
        if existing.owner_pid() != pid && existing.owner_pid() != 0 {
            return Err(KernelError::permission_denied(format!(
                "process {pid} does not own socket {socket_id}"
            )));
        }

        self.sockets.shutdown(socket_id, how)?;
        self.poll_notifier.notify();
        Ok(())
    }

    pub fn socket_close(
        &mut self,
        requester_driver: &str,
        pid: u32,
        socket_id: SocketId,
    ) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        let existing = self
            .sockets
            .get(socket_id)
            .ok_or_else(|| KernelError::new("ENOENT", format!("no such socket {socket_id}")))?;
        if existing.owner_pid() != pid && existing.owner_pid() != 0 {
            return Err(KernelError::permission_denied(format!(
                "process {pid} does not own socket {socket_id}"
            )));
        }

        self.sockets.remove(socket_id)?;
        self.poll_notifier.notify();
        Ok(())
    }

    pub fn fd_open(
        &mut self,
        requester_driver: &str,
        pid: u32,
        path: &str,
        flags: u32,
        mode: Option<u32>,
    ) -> KernelResult<u32> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        self.validate_fd_open_flags(pid, path, flags)?;
        if let Some(existing_fd) = parse_dev_fd_path(path)? {
            {
                let tables = lock_or_recover(&self.fd_tables);
                let table = tables
                    .get(pid)
                    .ok_or_else(|| KernelError::no_such_process(pid))?;
                table
                    .get(existing_fd)
                    .ok_or_else(|| KernelError::bad_file_descriptor(existing_fd))?;
            }
            self.resources
                .check_fd_allocation(&self.resource_snapshot(), 1)?;
            let mut tables = lock_or_recover(&self.fd_tables);
            let table = tables
                .get_mut(pid)
                .ok_or_else(|| KernelError::no_such_process(pid))?;
            let entry = table
                .get(existing_fd)
                .cloned()
                .ok_or_else(|| KernelError::bad_file_descriptor(existing_fd))?;
            return Ok(table.dup_with_status_flags(
                existing_fd,
                Some(entry.status_flags | (flags & O_NONBLOCK)),
            )?);
        }

        if let Some(proc_node) = self.resolve_proc_node(path, Some(pid))? {
            if open_requires_write_access(flags) {
                self.filesystem
                    .check_virtual_path(FsOperation::Write, path)
                    .map_err(KernelError::from)?;
                return Err(read_only_filesystem_error(path));
            }

            if matches!(
                proc_node,
                ProcNode::SelfLink { .. }
                    | ProcNode::PidCwdLink { .. }
                    | ProcNode::PidFdLink { .. }
            ) {
                let target = self.proc_symlink_target(&proc_node)?;
                return self.fd_open(requester_driver, pid, &target, flags, mode);
            }

            self.filesystem
                .check_virtual_path(FsOperation::Read, path)
                .map_err(KernelError::from)?;
            self.resources
                .check_fd_allocation(&self.resource_snapshot(), 1)?;
            let mut tables = lock_or_recover(&self.fd_tables);
            let table = tables
                .get_mut(pid)
                .ok_or_else(|| KernelError::no_such_process(pid))?;
            return Ok(table.open_with_details(
                &self.proc_canonical_path(&proc_node),
                flags,
                proc_filetype(&proc_node),
                None,
            )?);
        }

        if open_requires_write_access(flags) {
            self.reject_read_only_resolved_write_path(path)?;
        }
        let existed = self.exists_internal(Some(pid), path)?;
        if existed {
            let mut access = 0;
            if flags & (O_WRONLY | O_RDWR) != O_WRONLY {
                access |= DAC_READ;
            }
            if flags & (O_WRONLY | O_RDWR) != 0 || flags & O_TRUNC != 0 {
                access |= DAC_WRITE;
            }
            self.check_dac_access(pid, path, access)?;
        } else if flags & O_CREAT != 0 {
            self.check_dac_parent_access(pid, path, DAC_WRITE | DAC_EXECUTE)?;
        }
        if existed {
            let stat = VirtualFileSystem::stat(&mut self.filesystem, path)?;
            if stat.mode & 0o170000 == 0o010000 {
                if flags & O_CREAT != 0 && flags & O_EXCL != 0 {
                    return Err(KernelError::new(
                        "EEXIST",
                        format!("file already exists, open '{path}'"),
                    ));
                }
                let key = (stat.dev, stat.ino);
                if !self.pipes.has_named_pipe(key) {
                    self.resources
                        .check_pipe_allocation(&self.resource_snapshot())?;
                }
                self.resources
                    .check_fd_allocation(&self.resource_snapshot(), 1)?;
                let timeout = self.blocking_read_timeout();
                let pipe = self.pipes.open_named_pipe(key, path, flags, timeout)?;
                let mut tables = lock_or_recover(&self.fd_tables);
                let table = tables
                    .get_mut(pid)
                    .ok_or_else(|| KernelError::no_such_process(pid))?;
                return match table.open_with(Arc::clone(&pipe.description), FILETYPE_PIPE, None) {
                    Ok(fd) => Ok(fd),
                    Err(error) => {
                        self.pipes.close(pipe.description.id());
                        Err(error.into())
                    }
                };
            }
        }
        let (filetype, lock_target) = self.prepare_fd_open(path, flags, mode)?;
        let description_path = if filetype == FILETYPE_DIRECTORY {
            self.realpath_internal(Some(pid), path)?
        } else {
            path.to_owned()
        };
        if flags & O_CREAT != 0 && !existed {
            let umask = self.processes.get_umask(pid)?;
            self.apply_process_creation_metadata(pid, path, mode.unwrap_or(0o666), umask, false)?;
        } else if flags & O_TRUNC != 0 {
            self.clear_setid_after_write(pid, path)?;
        }
        self.resources
            .check_fd_allocation(&self.resource_snapshot(), 1)?;
        let mut tables = lock_or_recover(&self.fd_tables);
        let table = tables
            .get_mut(pid)
            .ok_or_else(|| KernelError::no_such_process(pid))?;
        Ok(table.open_with_details(&description_path, flags, filetype, lock_target)?)
    }

    pub fn fd_open_tmpfile(
        &mut self,
        requester_driver: &str,
        pid: u32,
        directory: &str,
        flags: u32,
        mode: u32,
        linkable: bool,
    ) -> KernelResult<u32> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        if flags & 0b11 == crate::fd_table::O_RDONLY {
            return Err(KernelError::new(
                "EINVAL",
                "O_TMPFILE requires write access",
            ));
        }
        let directory_stat = self.stat_for_process(requester_driver, pid, directory)?;
        if !directory_stat.is_directory {
            return Err(KernelError::new(
                "ENOTDIR",
                format!("O_TMPFILE target is not a directory: {directory}"),
            ));
        }
        self.check_dac_access(pid, directory, DAC_WRITE | DAC_EXECUTE)?;
        self.reject_read_only_resolved_write_path(directory)?;

        let hidden_path = loop {
            let id = self.next_unnamed_file_id;
            self.next_unnamed_file_id =
                self.next_unnamed_file_id.checked_add(1).ok_or_else(|| {
                    KernelError::new("EMFILE", "unnamed-file identifier space exhausted")
                })?;
            let candidate = normalize_path(&format!("{directory}/{UNNAMED_FILE_PREFIX}{pid}-{id}"));
            if !self.exists_internal(Some(pid), &candidate)? {
                break candidate;
            }
        };

        let fd = self.fd_open(
            requester_driver,
            pid,
            &hidden_path,
            (flags & !O_TRUNC) | O_CREAT | O_EXCL,
            Some(mode),
        )?;
        let description_id = self.description_for_fd(requester_driver, pid, fd)?.id();
        self.unnamed_files.insert(
            description_id,
            UnnamedFile {
                path: hidden_path,
                linkable,
            },
        );
        Ok(fd)
    }

    pub fn fd_link_tmpfile_for_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
        destination: &str,
    ) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        let description = self.description_for_fd(requester_driver, pid, fd)?;
        let source = if let Some(unnamed) = self.unnamed_files.get(&description.id()) {
            if !unnamed.linkable {
                return Err(KernelError::new(
                    "ENOENT",
                    "O_EXCL unnamed files cannot be linked",
                ));
            }
            unnamed.path.clone()
        } else {
            description.path().to_string()
        };
        self.check_dac_parent_access(pid, destination, DAC_WRITE | DAC_EXECUTE)?;
        self.link(&source, destination)
    }

    pub fn fd_read(
        &mut self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
        length: usize,
    ) -> KernelResult<Vec<u8>> {
        Ok(self
            .fd_read_with_timeout_result(requester_driver, pid, fd, length, None)?
            .unwrap_or_default())
    }

    pub fn fd_read_with_timeout_result(
        &mut self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
        length: usize,
        timeout: Option<Duration>,
    ) -> KernelResult<Option<Vec<u8>>> {
        self.assert_driver_owns(requester_driver, pid)?;
        let entry = {
            let tables = lock_or_recover(&self.fd_tables);
            tables
                .get(pid)
                .and_then(|table| table.get(fd))
                .cloned()
                .ok_or_else(|| KernelError::bad_file_descriptor(fd))?
        };
        if entry.description.flags() & 0b11 == O_WRONLY {
            return Err(KernelError::bad_file_descriptor(fd));
        }

        if let Some(socket_id) = self.fd_socket_id(&entry.description) {
            self.resources.check_pread_length(length)?;
            let nonblocking = (entry.description.flags() | entry.status_flags) & O_NONBLOCK != 0;
            let wait = if nonblocking {
                Some(Duration::ZERO)
            } else {
                timeout.or_else(|| self.blocking_read_timeout())
            };
            let deadline = wait.map(|wait| Instant::now() + wait);
            let result = loop {
                let generation = self.poll_notifier.snapshot();
                match self.sockets.read(socket_id, length) {
                    Ok(result) => break result,
                    Err(error) if error.code() == "EAGAIN" && !nonblocking => {
                        let remaining = deadline
                            .map(|deadline| deadline.saturating_duration_since(Instant::now()));
                        if matches!(remaining, Some(duration) if duration.is_zero())
                            || !self.poll_notifier.wait_for_change(generation, remaining)
                        {
                            return Err(KernelError::new(
                                "EAGAIN",
                                "blocking socket read timed out; raise limits.resources.maxBlockingReadMs",
                            ));
                        }
                    }
                    Err(error) => return Err(error.into()),
                }
            };
            prune_fd_sockets(&self.sockets, &self.fd_sockets);
            if result.is_some() {
                self.poll_notifier.notify();
            }
            return Ok(result);
        }

        if self.pipes.is_pipe(entry.description.id()) {
            let result = self.pipes.read_with_timeout(
                entry.description.id(),
                length,
                if (entry.description.flags() | entry.status_flags) & O_NONBLOCK != 0 {
                    Some(Duration::ZERO)
                } else {
                    timeout.or_else(|| self.blocking_read_timeout())
                },
            )?;
            return Ok(result);
        }

        if self.ptys.is_pty(entry.description.id()) {
            return Ok(self.ptys.read_with_timeout(
                entry.description.id(),
                length,
                if (entry.description.flags() | entry.status_flags) & O_NONBLOCK != 0 {
                    Some(Duration::ZERO)
                } else {
                    timeout.or_else(|| self.blocking_read_timeout())
                },
            )?);
        }

        self.resources.check_pread_length(length)?;

        let path = entry.description.path();
        if is_proc_path(&path) {
            let bytes = self.proc_read_file_from_open_path(Some(pid), &path)?;
            let start = entry.description.cursor() as usize;
            let end = start.saturating_add(length).min(bytes.len());
            let chunk = if start >= bytes.len() {
                Vec::new()
            } else {
                bytes[start..end].to_vec()
            };
            entry.description.set_cursor(
                entry
                    .description
                    .cursor()
                    .saturating_add(chunk.len() as u64),
            );
            return Ok(Some(chunk));
        }

        let cursor = entry.description.cursor();
        let bytes = if let Some(bytes) = entry.description.anonymous_pread(cursor, length) {
            bytes
        } else {
            VirtualFileSystem::pread(&mut self.filesystem, &path, cursor, length)?
        };
        entry
            .description
            .set_cursor(cursor.saturating_add(bytes.len() as u64));
        Ok(Some(bytes))
    }

    pub fn fd_write(
        &mut self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
        data: &[u8],
    ) -> KernelResult<usize> {
        self.fd_write_with_mode(requester_driver, pid, fd, data, false)
    }

    /// Attempt one write without blocking on bounded kernel transport state.
    ///
    /// This preserves the descriptor's guest-visible status flags. Trusted
    /// sidecar actors use it to park a synchronous guest write and retry after
    /// readiness changes instead of blocking the process-wide reactor.
    pub fn fd_write_nonblocking(
        &mut self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
        data: &[u8],
    ) -> KernelResult<usize> {
        self.fd_write_with_mode(requester_driver, pid, fd, data, true)
    }

    fn fd_write_with_mode(
        &mut self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
        data: &[u8],
        force_nonblocking: bool,
    ) -> KernelResult<usize> {
        self.assert_driver_owns(requester_driver, pid)?;
        self.resources.check_fd_write_size(data.len())?;
        let entry = {
            let tables = lock_or_recover(&self.fd_tables);
            tables
                .get(pid)
                .and_then(|table| table.get(fd))
                .cloned()
                .ok_or_else(|| KernelError::bad_file_descriptor(fd))?
        };
        if let Some(socket_id) = self.fd_socket_id(&entry.description) {
            let socket = self
                .sockets
                .get(socket_id)
                .ok_or_else(|| KernelError::bad_file_descriptor(fd))?;
            self.sockets.check_write(socket_id)?;
            let snapshot = self.resource_snapshot();
            if socket.spec().socket_type == SocketType::Stream {
                self.resources
                    .check_socket_buffer_growth(&snapshot, data.len())?;
            } else {
                self.resources
                    .check_socket_datagram_enqueue(&snapshot, data.len())?;
            }
            let written = self.sockets.write(socket_id, data)?;
            if written > 0 || socket.spec().socket_type != SocketType::Stream {
                self.poll_notifier.notify();
            }
            return Ok(written);
        }

        if self.pipes.is_pipe(entry.description.id()) {
            return match self.pipes.write_with_mode(
                entry.description.id(),
                data,
                force_nonblocking
                    || (entry.description.flags() | entry.status_flags) & O_NONBLOCK != 0,
            ) {
                Ok(bytes) => Ok(bytes),
                Err(error) => {
                    if error.code() == "EPIPE" {
                        self.processes.kill(pid as i32, SIGPIPE)?;
                    }
                    Err(error.into())
                }
            };
        }

        if self.ptys.is_pty(entry.description.id()) {
            return Ok(self.ptys.write(entry.description.id(), data)?);
        }

        let path = entry.description.path();
        if let Some(stat) = entry.description.anonymous_stat() {
            if entry.description.flags() & 0b11 == O_RDONLY {
                return Err(KernelError::bad_file_descriptor(fd));
            }
            let cursor = if entry.description.flags() & O_APPEND != 0 {
                stat.size
            } else {
                entry.description.cursor()
            };
            let required_size = stat.size.max(checked_write_end(cursor, data.len())?);
            self.check_path_resize_limits_with_existing(stat.size, required_size)?;
            let new_size = entry
                .description
                .anonymous_pwrite(cursor, data)
                .expect("anonymous stat and backing must agree")?;
            debug_assert_eq!(new_size, required_size);
            entry
                .description
                .set_cursor(cursor.saturating_add(data.len() as u64));
            return Ok(data.len());
        }
        self.reject_read_only_resolved_write_path(&path)?;
        if entry.description.flags() & 0b11 == O_RDONLY {
            return Err(KernelError::bad_file_descriptor(fd));
        }
        if is_virtual_device_storage_path(&path) {
            VirtualFileSystem::write_file(&mut self.filesystem, &path, data.to_vec())?;
            let cursor = entry.description.cursor();
            entry
                .description
                .set_cursor(cursor.saturating_add(data.len() as u64));
            return Ok(data.len());
        }
        let current_size = self.current_storage_file_size(&path)?;
        let cursor = entry.description.cursor();
        if entry.description.flags() & O_APPEND != 0 {
            check_direct_io_alignment(entry.description.flags(), current_size, data.len())?;
            let required_size = current_size.max(checked_write_end(current_size, data.len())?);
            self.check_path_resize_limits_with_existing(current_size, required_size)?;
            let new_len = VirtualFileSystem::append_file(&mut self.filesystem, &path, data)?;
            self.update_filesystem_usage_cache_for_resize(&path, current_size, new_len);
            self.clear_setid_after_write(pid, &path)?;
            entry.description.set_cursor(new_len);
            return Ok(data.len());
        }

        check_direct_io_alignment(entry.description.flags(), cursor, data.len())?;
        let required_size = current_size.max(checked_write_end(cursor, data.len())?);
        self.check_path_resize_limits_with_existing(current_size, required_size)?;
        VirtualFileSystem::pwrite(&mut self.filesystem, &path, data, cursor)?;
        self.update_filesystem_usage_cache_for_resize(&path, current_size, required_size);
        self.clear_setid_after_write(pid, &path)?;
        entry
            .description
            .set_cursor(cursor.saturating_add(data.len() as u64));
        Ok(data.len())
    }

    /// Probe a pipe write without parking the caller when the pipe is full.
    /// Other descriptor kinds retain the regular `fd_write` behavior.
    pub fn fd_write_nonblocking_pipe(
        &mut self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
        data: &[u8],
    ) -> KernelResult<usize> {
        self.assert_driver_owns(requester_driver, pid)?;
        self.resources.check_fd_write_size(data.len())?;
        let entry = {
            let tables = lock_or_recover(&self.fd_tables);
            tables
                .get(pid)
                .and_then(|table| table.get(fd))
                .cloned()
                .ok_or_else(|| KernelError::bad_file_descriptor(fd))?
        };
        if !self.pipes.is_pipe(entry.description.id()) {
            return self.fd_write(requester_driver, pid, fd, data);
        }
        match self
            .pipes
            .write_with_mode(entry.description.id(), data, true)
        {
            Ok(bytes) => Ok(bytes),
            Err(error) => {
                if error.code() == "EPIPE" {
                    self.processes.kill(pid as i32, SIGPIPE)?;
                }
                Err(error.into())
            }
        }
    }

    pub fn poll_fds(
        &self,
        requester_driver: &str,
        pid: u32,
        fds: Vec<PollFd>,
        timeout_ms: i32,
    ) -> KernelResult<PollResult> {
        let targets = fds
            .into_iter()
            .map(|poll_fd| PollTargetEntry::fd(poll_fd.fd, poll_fd.events))
            .collect::<Vec<_>>();
        let result = self.poll_targets(requester_driver, pid, targets, timeout_ms)?;
        Ok(PollResult {
            ready_count: result.ready_count,
            fds: result
                .targets
                .into_iter()
                .map(|target| match target.target {
                    PollTarget::Fd(fd) => PollFd {
                        fd,
                        events: target.events,
                        revents: target.revents,
                    },
                    PollTarget::Socket(_) => unreachable!("fd poll should only include fd targets"),
                })
                .collect(),
        })
    }

    /// A cloneable, Send handle for waiting on kernel poll-state changes off
    /// the kernel owner's thread. Pair with a zero-timeout `poll_fds` /
    /// `fd_read_with_timeout_result` re-check on the owning thread.
    pub fn poll_wait_handle(&self) -> crate::poll::PollWaitHandle {
        crate::poll::PollWaitHandle::new(self.poll_notifier.clone())
    }

    pub fn poll_targets(
        &self,
        requester_driver: &str,
        pid: u32,
        mut targets: Vec<PollTargetEntry>,
        timeout_ms: i32,
    ) -> KernelResult<PollTargetResult> {
        self.assert_driver_owns(requester_driver, pid)?;
        if timeout_ms < -1 {
            return Err(KernelError::new(
                "EINVAL",
                format!("invalid poll timeout {timeout_ms}"),
            ));
        }

        let timeout = if timeout_ms < 0 {
            None
        } else {
            Some(Duration::from_millis(timeout_ms as u64))
        };
        let deadline = timeout.map(|duration| Instant::now() + duration);

        loop {
            let observed_generation = self.poll_notifier.snapshot();
            let ready_count = self.populate_poll_target_revents(pid, &mut targets)?;
            if ready_count > 0 || matches!(timeout, Some(duration) if duration.is_zero()) {
                return Ok(PollTargetResult {
                    ready_count,
                    targets,
                });
            }

            let remaining = deadline.map(|target| target.saturating_duration_since(Instant::now()));
            if matches!(remaining, Some(duration) if duration.is_zero()) {
                return Ok(PollTargetResult {
                    ready_count,
                    targets,
                });
            }

            if !self
                .poll_notifier
                .wait_for_change(observed_generation, remaining)
            {
                return Ok(PollTargetResult {
                    ready_count,
                    targets,
                });
            }
        }
    }

    pub fn fd_seek(
        &mut self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
        offset: i64,
        whence: u8,
    ) -> KernelResult<u64> {
        self.assert_driver_owns(requester_driver, pid)?;
        let entry = {
            let tables = lock_or_recover(&self.fd_tables);
            tables
                .get(pid)
                .and_then(|table| table.get(fd))
                .cloned()
                .ok_or_else(|| KernelError::bad_file_descriptor(fd))?
        };

        if self.pipes.is_pipe(entry.description.id())
            || self.ptys.is_pty(entry.description.id())
            || self.fd_socket_id(&entry.description).is_some()
        {
            return Err(KernelError::new("ESPIPE", "illegal seek"));
        }

        let base = match whence {
            SEEK_SET => 0_i128,
            SEEK_CUR => i128::from(entry.description.cursor()),
            SEEK_END => {
                let path = entry.description.path();
                let size = if let Some(stat) = entry.description.anonymous_stat() {
                    stat.size
                } else if is_proc_path(&path) {
                    self.proc_stat_from_open_path(Some(pid), &path)?.size
                } else {
                    self.filesystem.stat(&path)?.size
                };
                i128::from(size)
            }
            _ => {
                return Err(KernelError::new(
                    "EINVAL",
                    format!("invalid whence {whence}"),
                ));
            }
        };
        let next = base + i128::from(offset);
        if next < 0 {
            return Err(KernelError::new("EINVAL", "negative seek position"));
        }
        let next = u64::try_from(next)
            .map_err(|_| KernelError::new("EINVAL", "seek position out of range"))?;
        entry.description.set_cursor(next);
        Ok(next)
    }

    pub fn fd_pread(
        &mut self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
        length: usize,
        offset: u64,
    ) -> KernelResult<Vec<u8>> {
        self.assert_driver_owns(requester_driver, pid)?;
        self.resources.check_pread_length(length)?;
        let entry = {
            let tables = lock_or_recover(&self.fd_tables);
            tables
                .get(pid)
                .and_then(|table| table.get(fd))
                .cloned()
                .ok_or_else(|| KernelError::bad_file_descriptor(fd))?
        };
        if entry.description.flags() & 0b11 == O_WRONLY {
            return Err(KernelError::bad_file_descriptor(fd));
        }

        if self.pipes.is_pipe(entry.description.id())
            || self.ptys.is_pty(entry.description.id())
            || self.fd_socket_id(&entry.description).is_some()
        {
            return Err(KernelError::new("ESPIPE", "illegal seek"));
        }

        let path = entry.description.path();
        if is_proc_path(&path) {
            let bytes = self.proc_read_file_from_open_path(Some(pid), &path)?;
            let start = usize::try_from(offset)
                .map_err(|_| KernelError::new("EINVAL", "pread offset out of range"))?;
            let end = start.saturating_add(length).min(bytes.len());
            return Ok(if start >= bytes.len() {
                Vec::new()
            } else {
                bytes[start..end].to_vec()
            });
        }

        if let Some(bytes) = entry.description.anonymous_pread(offset, length) {
            return Ok(bytes);
        }
        Ok(VirtualFileSystem::pread(
            &mut self.filesystem,
            &path,
            offset,
            length,
        )?)
    }

    pub fn fd_pwrite(
        &mut self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
        data: &[u8],
        offset: u64,
    ) -> KernelResult<usize> {
        self.assert_driver_owns(requester_driver, pid)?;
        self.resources.check_fd_write_size(data.len())?;
        let entry = {
            let tables = lock_or_recover(&self.fd_tables);
            tables
                .get(pid)
                .and_then(|table| table.get(fd))
                .cloned()
                .ok_or_else(|| KernelError::bad_file_descriptor(fd))?
        };
        if self.pipes.is_pipe(entry.description.id())
            || self.ptys.is_pty(entry.description.id())
            || self.fd_socket_id(&entry.description).is_some()
        {
            return Err(KernelError::new("ESPIPE", "illegal seek"));
        }

        let path = entry.description.path();
        if let Some(stat) = entry.description.anonymous_stat() {
            let required_size = stat.size.max(checked_write_end(offset, data.len())?);
            self.check_path_resize_limits_with_existing(stat.size, required_size)?;
            if entry.description.flags() & 0b11 == O_RDONLY {
                return Err(KernelError::bad_file_descriptor(fd));
            }
            let new_size = entry
                .description
                .anonymous_pwrite(offset, data)
                .expect("anonymous stat and backing must agree")?;
            debug_assert_eq!(new_size, required_size);
            return Ok(data.len());
        }
        self.reject_read_only_resolved_write_path(&path)?;

        let current_size = self.current_storage_file_size(&path)?;
        let required_size = current_size.max(checked_write_end(offset, data.len())?);
        self.check_path_resize_limits_with_existing(current_size, required_size)?;
        if entry.description.flags() & 0b11 == O_RDONLY {
            return Err(KernelError::bad_file_descriptor(fd));
        }
        VirtualFileSystem::pwrite(&mut self.filesystem, &path, data.to_vec(), offset)?;
        self.update_filesystem_usage_cache_for_resize(&path, current_size, required_size);
        Ok(data.len())
    }

    pub fn fd_chmod(
        &mut self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
        mode: u32,
    ) -> KernelResult<()> {
        let description = self.description_for_fd(requester_driver, pid, fd)?;
        if description.detached_chmod(mode) {
            return Ok(());
        }
        if self.pipes.is_pipe(description.id()) {
            self.pipes.chmod(description.id(), mode)?;
            return Ok(());
        }
        if let Some(socket) = lock_or_recover(&self.fd_sockets).get_mut(&description.id()) {
            socket.mode = mode & 0o7777;
            return Ok(());
        }
        let path = description.path();
        self.chmod(&path, mode)
    }

    pub fn fd_chmod_for_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
        mode: u32,
    ) -> KernelResult<()> {
        let identity = self.process_identity(requester_driver, pid)?;
        let stat = self.dev_fd_stat(requester_driver, pid, fd)?;
        if identity.euid != 0 && identity.euid != stat.uid {
            return Err(KernelError::new(
                "EPERM",
                format!("operation not permitted, process does not own fd {fd}"),
            ));
        }
        self.fd_chmod(requester_driver, pid, fd, mode)
    }

    pub fn fd_chown_for_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
        uid: u32,
        gid: u32,
    ) -> KernelResult<()> {
        let description = self.description_for_fd(requester_driver, pid, fd)?;
        let identity = self.process_identity(requester_driver, pid)?;
        let stat = self.dev_fd_stat(requester_driver, pid, fd)?;
        let (next_uid, next_gid) =
            validate_chown_request(&identity, &stat, uid, gid, &format!("fd {fd}"))?;
        let changed_mode = linux_chown_cleared_mode(&stat);
        if description.detached_chown(next_uid, next_gid, changed_mode) {
            return Ok(());
        }
        if self.pipes.is_pipe(description.id()) {
            self.pipes.set_owner(description.id(), next_uid, next_gid)?;
            return Ok(());
        }
        if let Some(socket) = lock_or_recover(&self.fd_sockets).get_mut(&description.id()) {
            socket.uid = next_uid;
            socket.gid = next_gid;
            return Ok(());
        }

        let fd_type = self.fd_stat(requester_driver, pid, fd)?.filetype;
        if !matches!(fd_type, FILETYPE_REGULAR_FILE | FILETYPE_DIRECTORY) {
            // Character devices have no mutable descriptor-owned inode in
            // AgentOS. The fixed unprivileged guest can only reach this branch
            // for the Linux no-op (-1, -1) request.
            return Ok(());
        }
        let path = description.path();
        self.reject_read_only_resolved_write_path(&path)?;
        self.filesystem.chown(&path, next_uid, next_gid)?;
        if let Some(mode) = changed_mode {
            self.filesystem.chmod(&path, mode)?;
        }
        Ok(())
    }

    pub fn fd_dup(&mut self, requester_driver: &str, pid: u32, fd: u32) -> KernelResult<u32> {
        self.assert_driver_owns(requester_driver, pid)?;
        {
            let tables = lock_or_recover(&self.fd_tables);
            let table = tables
                .get(pid)
                .ok_or_else(|| KernelError::no_such_process(pid))?;
            table
                .get(fd)
                .ok_or_else(|| KernelError::bad_file_descriptor(fd))?;
        }
        self.resources
            .check_fd_allocation(&self.resource_snapshot(), 1)?;
        let mut tables = lock_or_recover(&self.fd_tables);
        let table = tables
            .get_mut(pid)
            .ok_or_else(|| KernelError::no_such_process(pid))?;
        Ok(table.dup(fd)?)
    }

    pub fn fd_dup2(
        &mut self,
        requester_driver: &str,
        pid: u32,
        old_fd: u32,
        new_fd: u32,
    ) -> KernelResult<()> {
        self.assert_driver_owns(requester_driver, pid)?;
        let (replaced, needs_fd_growth) = {
            let tables = lock_or_recover(&self.fd_tables);
            let table = tables
                .get(pid)
                .ok_or_else(|| KernelError::no_such_process(pid))?;
            table
                .get(old_fd)
                .ok_or_else(|| KernelError::bad_file_descriptor(old_fd))?;
            let replaced = if old_fd == new_fd {
                None
            } else {
                table.get(new_fd).cloned()
            };
            if new_fd as usize >= table.max_fds() {
                return Err(KernelError::bad_file_descriptor(new_fd));
            }
            let needs_fd_growth = old_fd != new_fd && replaced.is_none();
            (replaced, needs_fd_growth)
        };
        if needs_fd_growth {
            self.resources
                .check_fd_allocation(&self.resource_snapshot(), 1)?;
        }
        {
            let mut tables = lock_or_recover(&self.fd_tables);
            let table = tables
                .get_mut(pid)
                .ok_or_else(|| KernelError::no_such_process(pid))?;
            table.dup2(old_fd, new_fd)?;
        }

        if let Some(entry) = replaced {
            if let Some(target) = entry.description.lock_target() {
                self.file_locks.release_process_target(pid, target);
            }
            self.close_special_resource_if_needed(&entry.description, entry.filetype);
        }
        Ok(())
    }

    pub fn fd_close(&mut self, requester_driver: &str, pid: u32, fd: u32) -> KernelResult<()> {
        self.assert_driver_owns(requester_driver, pid)?;
        let (description, filetype, lock_target) = {
            let mut tables = lock_or_recover(&self.fd_tables);
            let table = tables
                .get_mut(pid)
                .ok_or_else(|| KernelError::no_such_process(pid))?;
            let entry = table
                .get(fd)
                .cloned()
                .ok_or_else(|| KernelError::bad_file_descriptor(fd))?;
            table.close(fd);
            let lock_target = entry.description.lock_target();
            (entry.description, entry.filetype, lock_target)
        };
        if let Some(target) = lock_target {
            self.file_locks.release_process_target(pid, target);
        }
        self.close_special_resource_if_needed(&description, filetype);
        self.cleanup_unnamed_file_if_closed(&description)?;
        Ok(())
    }

    /// Commit the descriptor half of exec by closing every descriptor still
    /// marked `FD_CLOEXEC` after POSIX spawn file actions have completed.
    pub fn close_process_cloexec_fds(
        &mut self,
        requester_driver: &str,
        pid: u32,
    ) -> KernelResult<()> {
        self.assert_driver_owns(requester_driver, pid)?;
        let closed_entries = {
            let mut tables = lock_or_recover(&self.fd_tables);
            let table = tables
                .get_mut(pid)
                .ok_or_else(|| KernelError::no_such_process(pid))?;
            let fds = table.close_on_exec_fds();
            let mut closed_entries = Vec::with_capacity(fds.len());
            for fd in fds {
                let entry = table
                    .get(fd)
                    .cloned()
                    .expect("close-on-exec snapshot must reference an open descriptor");
                let closed = table.close(fd);
                debug_assert!(closed);
                closed_entries.push((entry.description, entry.filetype));
            }
            closed_entries
        };
        for (description, filetype) in closed_entries {
            if let Some(target) = description.lock_target() {
                self.file_locks.release_process_target(pid, target);
            }
            self.close_special_resource_if_needed(&description, filetype);
        }
        Ok(())
    }

    pub fn fd_fcntl(
        &mut self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
        command: u32,
        arg: u32,
    ) -> KernelResult<u32> {
        self.assert_driver_owns(requester_driver, pid)?;
        if command == F_DUPFD {
            {
                let tables = lock_or_recover(&self.fd_tables);
                let table = tables
                    .get(pid)
                    .ok_or_else(|| KernelError::no_such_process(pid))?;
                table
                    .get(fd)
                    .ok_or_else(|| KernelError::bad_file_descriptor(fd))?;
                if arg as usize >= table.max_fds() {
                    return Err(KernelError::new(
                        "EINVAL",
                        format!("fd {arg} exceeds process fd limit"),
                    ));
                }
            }
            self.resources
                .check_fd_allocation(&self.resource_snapshot(), 1)?;
        }
        let mut tables = lock_or_recover(&self.fd_tables);
        let table = tables
            .get_mut(pid)
            .ok_or_else(|| KernelError::no_such_process(pid))?;
        let result = table.fcntl(fd, command, arg)?;
        if command == F_DUPFD {
            self.poll_notifier.notify();
        }
        Ok(result)
    }

    pub fn fd_named_pipe_peer_ready(
        &self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
    ) -> KernelResult<bool> {
        self.assert_driver_owns(requester_driver, pid)?;
        let entry = lock_or_recover(&self.fd_tables)
            .get(pid)
            .and_then(|table| table.get(fd))
            .cloned()
            .ok_or_else(|| KernelError::bad_file_descriptor(fd))?;
        if entry.filetype != FILETYPE_PIPE {
            return Err(KernelError::new(
                "EINVAL",
                format!("fd {fd} is not a named pipe"),
            ));
        }
        self.pipes
            .named_pipe_peer_ready(entry.description.id())?
            .ok_or_else(|| KernelError::new("EINVAL", format!("fd {fd} is an anonymous pipe")))
    }

    pub fn fd_flock(
        &self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
        operation: u32,
    ) -> KernelResult<()> {
        self.assert_driver_owns(requester_driver, pid)?;
        let entry = {
            let tables = lock_or_recover(&self.fd_tables);
            tables
                .get(pid)
                .and_then(|table| table.get(fd))
                .cloned()
                .ok_or_else(|| KernelError::bad_file_descriptor(fd))?
        };

        if !matches!(entry.filetype, FILETYPE_REGULAR_FILE | FILETYPE_DIRECTORY) {
            return Err(KernelError::new(
                "EBADF",
                format!("file descriptor {fd} does not support advisory locking"),
            ));
        }

        let target = entry.description.lock_target().ok_or_else(|| {
            KernelError::new(
                "EBADF",
                format!("file descriptor {fd} is missing advisory lock metadata"),
            )
        })?;
        let operation = FlockOperation::from_bits(operation)?;
        self.file_locks
            .apply(entry.description.id(), target, operation)?;
        Ok(())
    }

    // The explicit range and query fields mirror the guest fcntl lock shape.
    #[allow(clippy::too_many_arguments)]
    pub fn fd_record_lock(
        &self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
        lock_type: RecordLockType,
        start: u64,
        length: u64,
        query: bool,
    ) -> KernelResult<Option<RecordLock>> {
        self.fd_record_lock_impl(
            requester_driver,
            pid,
            fd,
            lock_type,
            start,
            length,
            query,
            false,
        )
    }

    pub fn fd_record_lock_wait(
        &self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
        lock_type: RecordLockType,
        start: u64,
        length: u64,
    ) -> KernelResult<()> {
        self.fd_record_lock_impl(
            requester_driver,
            pid,
            fd,
            lock_type,
            start,
            length,
            false,
            true,
        )
        .map(|_| ())
    }

    pub fn fd_record_lock_cancel(&self, requester_driver: &str, pid: u32) -> KernelResult<()> {
        self.assert_driver_owns(requester_driver, pid)?;
        self.file_locks.cancel_record_lock_wait(pid);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn fd_record_lock_impl(
        &self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
        lock_type: RecordLockType,
        start: u64,
        length: u64,
        query: bool,
        blocking: bool,
    ) -> KernelResult<Option<RecordLock>> {
        self.assert_driver_owns(requester_driver, pid)?;
        let entry = {
            let tables = lock_or_recover(&self.fd_tables);
            tables
                .get(pid)
                .and_then(|table| table.get(fd))
                .cloned()
                .ok_or_else(|| KernelError::bad_file_descriptor(fd))?
        };
        if !matches!(entry.filetype, FILETYPE_REGULAR_FILE | FILETYPE_DIRECTORY) {
            return Err(KernelError::new(
                "EBADF",
                format!("file descriptor {fd} does not support POSIX record locks"),
            ));
        }
        if query && lock_type == RecordLockType::Unlock {
            return Err(KernelError::new(
                "EINVAL",
                "F_GETLK requires a read or write lock type",
            ));
        }
        if !query {
            let access_mode = entry.description.flags() & 0o3;
            if (lock_type == RecordLockType::Read && access_mode == O_WRONLY)
                || (lock_type == RecordLockType::Write && access_mode == O_RDONLY)
            {
                return Err(KernelError::new(
                    "EBADF",
                    format!("file descriptor {fd} access mode is incompatible with record lock"),
                ));
            }
        }
        let target = entry.description.lock_target().ok_or_else(|| {
            KernelError::new(
                "EBADF",
                format!("file descriptor {fd} is missing record lock metadata"),
            )
        })?;
        let request = RecordLock::new(lock_type, start, length, pid)?;
        if query {
            Ok(self.file_locks.query_record_lock(target, request))
        } else if blocking {
            self.file_locks.set_blocking_record_lock(target, request)?;
            Ok(None)
        } else {
            self.file_locks.set_record_lock(target, request)?;
            Ok(None)
        }
    }

    pub fn fd_stat(&self, requester_driver: &str, pid: u32, fd: u32) -> KernelResult<FdStat> {
        self.assert_driver_owns(requester_driver, pid)?;
        let tables = lock_or_recover(&self.fd_tables);
        Ok(tables
            .get(pid)
            .ok_or_else(|| KernelError::no_such_process(pid))?
            .stat(fd)?)
    }

    /// Synchronize a descriptor's committed data and metadata. The in-memory
    /// VFS applies writes synchronously, so successful regular-file and
    /// directory syncs require no extra flush. Descriptor validation and
    /// Linux type errors still happen here rather than being silently ignored.
    pub fn fd_sync(&self, requester_driver: &str, pid: u32, fd: u32) -> KernelResult<()> {
        let stat = self.fd_stat(requester_driver, pid, fd)?;
        match stat.filetype {
            FILETYPE_REGULAR_FILE | FILETYPE_DIRECTORY => Ok(()),
            _ => Err(KernelError::new(
                "EINVAL",
                format!("file descriptor {fd} cannot be synchronized"),
            )),
        }
    }

    pub fn fd_read_dir_with_types(
        &mut self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
    ) -> KernelResult<Vec<ProcessFdDirEntry>> {
        let stat = self.fd_stat(requester_driver, pid, fd)?;
        if stat.filetype != FILETYPE_DIRECTORY {
            return Err(KernelError::new(
                "ENOTDIR",
                format!("file descriptor {fd} is not a directory"),
            ));
        }
        let description = self.description_for_fd(requester_driver, pid, fd)?;
        if description.detached_directory_stat().is_some() {
            // Linux getdents64(2) on an open directory that has since been
            // removed returns immediate EOF. The OFD remains valid for fstat,
            // but the unlinked dentry no longer enumerates even synthetic dots.
            return Ok(Vec::new());
        }
        let path = self.fd_path(requester_driver, pid, fd)?;
        let children = self.read_dir_with_types_for_process(requester_driver, pid, &path)?;
        self.resources
            .check_readdir_entries(children.len().saturating_add(2))?;

        // fd_readdir is the Linux-like descriptor traversal surface. Unlike
        // path-based Node readdir, it exposes the synthetic current/parent
        // entries and inode identities used by libc readdir/telldir/seekdir.
        let current_stat = self.stat_internal(Some(pid), &path)?;
        let parent = parent_path(&path);
        let parent_stat = self.stat_internal(Some(pid), &parent)?;
        let mut entries = Vec::with_capacity(children.len().saturating_add(2));
        entries.push(ProcessFdDirEntry {
            name: String::from("."),
            ino: required_dirent_ino(&path, current_stat.ino)?,
            is_directory: true,
            is_symbolic_link: false,
        });
        entries.push(ProcessFdDirEntry {
            name: String::from(".."),
            ino: required_dirent_ino(&parent, parent_stat.ino)?,
            is_directory: true,
            is_symbolic_link: false,
        });
        for child in children {
            let child_path = join_child_path(&path, &child.name);
            let child_stat = self.lstat_internal(Some(pid), &child_path)?;
            entries.push(ProcessFdDirEntry {
                name: child.name,
                ino: required_dirent_ino(&child_path, child_stat.ino)?,
                is_directory: child.is_directory,
                is_symbolic_link: child.is_symbolic_link,
            });
        }
        Ok(entries)
    }

    pub fn fd_path(&self, requester_driver: &str, pid: u32, fd: u32) -> KernelResult<String> {
        let description = self.description_for_fd(requester_driver, pid, fd)?;
        Ok(description.path())
    }

    pub fn isatty(&self, requester_driver: &str, pid: u32, fd: u32) -> KernelResult<bool> {
        self.assert_driver_owns(requester_driver, pid)?;
        let entry = {
            let tables = lock_or_recover(&self.fd_tables);
            tables
                .get(pid)
                .and_then(|table| table.get(fd))
                .cloned()
                .ok_or_else(|| KernelError::bad_file_descriptor(fd))?
        };
        Ok(self.ptys.is_slave(entry.description.id()))
    }

    pub fn pty_window_size(
        &self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
    ) -> KernelResult<PtyWindowSize> {
        let description = self.description_for_fd(requester_driver, pid, fd)?;
        Ok(self.ptys.window_size(description.id())?)
    }

    pub fn pty_set_discipline(
        &self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
        config: LineDisciplineConfig,
    ) -> KernelResult<()> {
        let description = self.description_for_fd(requester_driver, pid, fd)?;
        self.ptys.set_discipline(description.id(), config)?;
        Ok(())
    }

    /// Toggle PTY raw mode and, when the caller belongs to the terminal's
    /// foreground process group, create a generation-scoped recovery lease.
    /// The lease can be released during process cleanup without overwriting a
    /// newer terminal mutation from another process.
    pub fn pty_set_raw_mode(
        &self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
        enabled: bool,
    ) -> KernelResult<Option<u64>> {
        let description = self.description_for_fd(requester_driver, pid, fd)?;
        let foreground_pgid = self.ptys.get_foreground_pgid(description.id())?;
        let process_pgid = self.processes.getpgid(pid)?;
        let lease_owner =
            (!enabled || foreground_pgid == 0 || foreground_pgid == process_pgid).then_some(pid);
        Ok(self
            .ptys
            .set_raw_mode(description.id(), lease_owner, enabled)?)
    }

    /// Release a raw-mode recovery lease through any descriptor for the same
    /// PTY. `fd` normally belongs to the terminal owner because the exiting
    /// child may already have closed its own descriptor zero.
    pub fn pty_release_raw_mode(
        &self,
        requester_driver: &str,
        descriptor_owner_pid: u32,
        fd: u32,
        raw_mode_owner_pid: u32,
        generation: u64,
    ) -> KernelResult<bool> {
        self.assert_driver_owns(requester_driver, raw_mode_owner_pid)?;
        let description = self.description_for_fd(requester_driver, descriptor_owner_pid, fd)?;
        Ok(self
            .ptys
            .release_raw_mode(description.id(), raw_mode_owner_pid, generation)?)
    }

    pub fn pty_set_foreground_pgid(
        &self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
        pgid: u32,
    ) -> KernelResult<()> {
        let description = self.description_for_fd(requester_driver, pid, fd)?;
        let requester_sid = self.processes.getsid(pid)?;
        let group = self
            .processes
            .list_processes()
            .into_values()
            .find(|process| process.pgid == pgid && process.status != ProcessStatus::Exited)
            .ok_or_else(|| KernelError::new("ESRCH", format!("no such process group {pgid}")))?;
        if group.sid != requester_sid {
            return Err(KernelError::permission_denied(
                "cannot set foreground process group in different session",
            ));
        }
        self.ptys.set_foreground_pgid(description.id(), pgid)?;
        Ok(())
    }

    pub fn tcgetattr(&self, requester_driver: &str, pid: u32, fd: u32) -> KernelResult<Termios> {
        let description = self.description_for_fd(requester_driver, pid, fd)?;
        Ok(self.ptys.get_termios(description.id())?)
    }

    pub fn tcsetattr(
        &self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
        termios: PartialTermios,
    ) -> KernelResult<()> {
        let description = self.description_for_fd(requester_driver, pid, fd)?;
        self.ptys.set_termios(description.id(), termios)?;
        Ok(())
    }

    pub fn tcgetpgrp(&self, requester_driver: &str, pid: u32, fd: u32) -> KernelResult<u32> {
        let description = self.description_for_fd(requester_driver, pid, fd)?;
        Ok(self.ptys.get_foreground_pgid(description.id())?)
    }

    pub fn pty_resize(
        &self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
        cols: u16,
        rows: u16,
    ) -> KernelResult<()> {
        let description = self.description_for_fd(requester_driver, pid, fd)?;
        let target_pgid = self.ptys.resize(description.id(), cols, rows)?;
        if let Some(pgid) = target_pgid {
            match self.processes.kill(-(pgid as i32), SIGWINCH) {
                Ok(()) => {}
                Err(error) if error.code() == "ESRCH" => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    pub fn signal_process(
        &self,
        requester_driver: &str,
        pid: i32,
        signal: i32,
    ) -> KernelResult<()> {
        if pid < 0 {
            let pgid = pid.unsigned_abs();
            let members = self
                .processes
                .list_processes()
                .into_values()
                .filter(|process| process.pgid == pgid && process.status != ProcessStatus::Exited)
                .collect::<Vec<_>>();
            if members.is_empty() {
                self.processes.kill(pid, signal)?;
                return Ok(());
            }
            if let Some(process) = members
                .iter()
                .find(|process| process.driver != requester_driver)
            {
                return Err(KernelError::permission_denied(format!(
                    "driver \"{requester_driver}\" does not own process group {pgid} containing PID {}",
                    process.pid
                )));
            }
            self.processes.kill(pid, signal)?;
            return Ok(());
        }

        let pid = u32::try_from(pid)
            .map_err(|_| KernelError::new("EINVAL", format!("invalid pid {pid}")))?;
        self.assert_driver_owns(requester_driver, pid)?;
        self.processes.kill(pid as i32, signal)?;
        Ok(())
    }

    pub fn kill_process(&self, requester_driver: &str, pid: u32, signal: i32) -> KernelResult<()> {
        let pid = i32::try_from(pid)
            .map_err(|_| KernelError::new("EINVAL", format!("pid {pid} exceeds i32::MAX")))?;
        self.signal_process(requester_driver, pid, signal)
    }

    pub fn setpgid(&self, requester_driver: &str, pid: u32, pgid: u32) -> KernelResult<()> {
        self.assert_driver_owns(requester_driver, pid)?;
        let target_pgid = if pgid == 0 { pid } else { pgid };
        if target_pgid != pid {
            if let Some(group_owner) =
                self.processes
                    .list_processes()
                    .into_values()
                    .find(|process| {
                        process.pgid == target_pgid && process.status == ProcessStatus::Running
                    })
            {
                if group_owner.driver != requester_driver {
                    return Err(KernelError::permission_denied(format!(
                        "driver \"{requester_driver}\" cannot join process group {target_pgid} owned by \"{}\"",
                        group_owner.driver
                    )));
                }
            }
        }
        self.processes.setpgid(pid, pgid)?;
        Ok(())
    }

    pub fn getpgid(&self, requester_driver: &str, pid: u32) -> KernelResult<u32> {
        self.assert_driver_owns(requester_driver, pid)?;
        Ok(self.processes.getpgid(pid)?)
    }

    pub fn getpid(&self, requester_driver: &str, pid: u32) -> KernelResult<u32> {
        self.assert_driver_owns(requester_driver, pid)?;
        Ok(pid)
    }

    pub fn sigprocmask(
        &self,
        requester_driver: &str,
        pid: u32,
        how: SigmaskHow,
        set: SignalSet,
    ) -> KernelResult<SignalSet> {
        self.assert_driver_owns(requester_driver, pid)?;
        Ok(self.processes.sigprocmask(pid, how, set)?)
    }

    pub fn sigpending(&self, requester_driver: &str, pid: u32) -> KernelResult<SignalSet> {
        self.assert_driver_owns(requester_driver, pid)?;
        Ok(self.processes.sigpending(pid)?)
    }

    pub fn getppid(&self, requester_driver: &str, pid: u32) -> KernelResult<u32> {
        self.assert_driver_owns(requester_driver, pid)?;
        Ok(self.processes.getppid(pid)?)
    }

    pub fn setsid(&self, requester_driver: &str, pid: u32) -> KernelResult<u32> {
        self.assert_driver_owns(requester_driver, pid)?;
        Ok(self.processes.setsid(pid)?)
    }

    pub fn getsid(&self, requester_driver: &str, pid: u32) -> KernelResult<u32> {
        self.assert_driver_owns(requester_driver, pid)?;
        Ok(self.processes.getsid(pid)?)
    }

    pub fn dev_fd_read_dir(&self, requester_driver: &str, pid: u32) -> KernelResult<Vec<String>> {
        self.assert_driver_owns(requester_driver, pid)?;
        let tables = lock_or_recover(&self.fd_tables);
        let table = tables
            .get(pid)
            .ok_or_else(|| KernelError::no_such_process(pid))?;
        let entry_count = table.len();
        self.resources.check_readdir_entries(entry_count)?;
        Ok(table.iter().map(|entry| entry.fd.to_string()).collect())
    }

    pub fn dev_fd_stat(
        &mut self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
    ) -> KernelResult<VirtualStat> {
        self.assert_driver_owns(requester_driver, pid)?;
        let entry = {
            let tables = lock_or_recover(&self.fd_tables);
            tables
                .get(pid)
                .and_then(|table| table.get(fd))
                .cloned()
                .ok_or_else(|| KernelError::bad_file_descriptor(fd))?
        };

        if let Some(pipe_id) = self.pipes.pipe_id_for(entry.description.id()) {
            let metadata = self
                .pipes
                .metadata(entry.description.id())
                .expect("live pipe description must retain inode metadata");
            let mut stat = synthetic_special_file_stat(pipe_id, 0o010000 | metadata.mode, 13);
            stat.uid = metadata.uid;
            stat.gid = metadata.gid;
            return Ok(stat);
        }

        if let Some(socket) = lock_or_recover(&self.fd_sockets).get(&entry.description.id()) {
            let mut stat =
                synthetic_special_file_stat(entry.description.id(), 0o140000 | socket.mode, 9);
            stat.uid = socket.uid;
            stat.gid = socket.gid;
            return Ok(stat);
        }

        if self.ptys.is_pty(entry.description.id()) {
            return Ok(synthetic_character_device_stat(entry.description.id()));
        }

        if let Some(stat) = entry.description.anonymous_stat() {
            return Ok(stat);
        }
        if let Some(stat) = entry.description.detached_directory_stat() {
            return Ok(stat);
        }
        let path = entry.description.path();
        if is_proc_path(&path) {
            return self.proc_stat_from_open_path(Some(pid), &path);
        }

        Ok(self.filesystem.stat(&path)?)
    }

    pub fn dispose(&mut self) -> KernelResult<()> {
        if self.terminated {
            return Ok(());
        }

        dispose_kernel_vm_resources(self);
        Ok(())
    }

    fn prepare_fd_open(
        &mut self,
        path: &str,
        flags: u32,
        mode: Option<u32>,
    ) -> KernelResult<(u8, Option<FileLockTarget>)> {
        if flags & O_CREAT != 0 && flags & O_EXCL != 0 {
            self.check_write_file_limits(path, 0)?;
            VirtualFileSystem::create_file_exclusive_with_mode(
                &mut self.filesystem,
                path,
                Vec::new(),
                mode,
            )?;
            self.update_filesystem_usage_cache_for_inode_create(path, 0);
            let stat = VirtualFileSystem::stat(&mut self.filesystem, path)?;
            return Ok((
                filetype_for_path(path, &stat),
                Some(FileLockTarget::new(stat.dev, stat.ino)),
            ));
        }

        let exists = self.filesystem.exists(path)?;
        if exists {
            let existing_stat = VirtualFileSystem::stat(&mut self.filesystem, path)?;
            if existing_stat.mode & 0o170000 == 0o140000 {
                return Err(KernelError::new(
                    "ENXIO",
                    format!("cannot open Unix socket pathname '{path}'"),
                ));
            }
            if flags & O_TRUNC != 0 {
                let existing_size = self.current_storage_file_size(path)?;
                self.check_path_resize_limits_with_existing(existing_size, 0)?;
                VirtualFileSystem::truncate(&mut self.filesystem, path, 0)?;
                self.update_filesystem_usage_cache_for_resize(path, existing_size, 0);
            }
        } else if flags & O_CREAT != 0 {
            self.check_write_file_limits(path, 0)?;
            VirtualFileSystem::write_file_with_mode(&mut self.filesystem, path, Vec::new(), mode)?;
            self.update_filesystem_usage_cache_for_inode_create(path, 0);
        } else {
            let _ = VirtualFileSystem::stat(&mut self.filesystem, path)?;
            unreachable!("stat should return an error when opening a missing path");
        }

        let stat = VirtualFileSystem::stat(&mut self.filesystem, path)?;
        Ok((
            filetype_for_path(path, &stat),
            Some(FileLockTarget::new(stat.dev, stat.ino)),
        ))
    }

    fn validate_fd_open_flags(&mut self, pid: u32, path: &str, flags: u32) -> KernelResult<()> {
        if flags & O_DIRECTORY != 0 && flags & O_CREAT != 0 {
            return Err(KernelError::new(
                "EINVAL",
                format!("O_DIRECTORY and O_CREAT cannot be combined for '{path}'"),
            ));
        }

        if let Some(existing_fd) = parse_dev_fd_path(path)? {
            let filetype = lock_or_recover(&self.fd_tables)
                .get(pid)
                .and_then(|table| table.get(existing_fd))
                .map(|entry| entry.filetype)
                .ok_or_else(|| {
                    KernelError::new(
                        "ENOENT",
                        format!("no such file or directory, open '{path}'"),
                    )
                })?;
            if flags & O_CREAT != 0 && flags & O_EXCL != 0 {
                return Err(KernelError::new(
                    "EEXIST",
                    format!("file already exists, open '{path}'"),
                ));
            }
            if flags & O_NOFOLLOW != 0 {
                return Err(KernelError::new(
                    "ELOOP",
                    format!("symbolic link not followed, open '{path}'"),
                ));
            }
            if flags & O_DIRECTORY != 0 && filetype != FILETYPE_DIRECTORY {
                return Err(KernelError::new(
                    "ENOTDIR",
                    format!("not a directory, open '{path}'"),
                ));
            }
            return Ok(());
        }

        if flags & O_DIRECTORY != 0 {
            let stat = if flags & O_NOFOLLOW != 0 {
                self.lstat_internal(Some(pid), path)?
            } else {
                self.stat_internal(Some(pid), path)?
            };
            if !stat.is_directory || stat.is_symbolic_link {
                return Err(KernelError::new(
                    "ENOTDIR",
                    format!("not a directory, open '{path}'"),
                ));
            }
        } else if flags & O_NOFOLLOW != 0 && flags & (O_CREAT | O_EXCL) != (O_CREAT | O_EXCL) {
            match self.lstat_internal(Some(pid), path) {
                Ok(stat) if stat.is_symbolic_link => {
                    return Err(KernelError::new(
                        "ELOOP",
                        format!("symbolic link not followed, open '{path}'"),
                    ));
                }
                Ok(_) => {}
                Err(error) if error.code() == "ENOENT" && flags & O_CREAT != 0 => {}
                Err(error) => return Err(error),
            }
        }

        Ok(())
    }

    fn reject_unix_socket_data_path(&mut self, path: &str, code: &'static str) -> KernelResult<()> {
        if self
            .storage_stat(path)?
            .is_some_and(|stat| stat.mode & 0o170000 == 0o140000)
        {
            return Err(KernelError::new(
                code,
                format!("Unix socket pathname does not support file data I/O, '{path}'"),
            ));
        }
        Ok(())
    }

    fn reject_read_only_write_path(&mut self, path: &str) -> KernelResult<()> {
        if is_proc_path(path) {
            self.filesystem
                .check_virtual_path(FsOperation::Write, path)
                .map_err(KernelError::from)?;
            return Err(read_only_filesystem_error(path));
        }

        if is_agentos_path(path) {
            return Err(read_only_filesystem_error(path));
        }

        Ok(())
    }

    fn reject_read_only_resolved_write_path(&mut self, path: &str) -> KernelResult<()> {
        self.reject_read_only_write_path(path)?;

        if let Some(resolved) = self.resolve_write_guard_path(path, true)? {
            if is_agentos_path(&resolved) {
                return Err(read_only_filesystem_error(&resolved));
            }
            if self.has_agentos_hardlink_alias(&resolved)? {
                return Err(read_only_filesystem_error(&resolved));
            }
        }
        if self.has_agentos_hardlink_alias(path)? {
            return Err(read_only_filesystem_error(path));
        }

        Ok(())
    }

    fn reject_read_only_entry_write_path(&mut self, path: &str) -> KernelResult<()> {
        self.reject_read_only_write_path(path)?;

        if let Some(resolved) = self.resolve_write_guard_path(path, false)? {
            if is_agentos_path(&resolved) {
                return Err(read_only_filesystem_error(&resolved));
            }
            if self.has_agentos_hardlink_alias(&resolved)? {
                return Err(read_only_filesystem_error(&resolved));
            }
        }
        if self.has_agentos_hardlink_alias(path)? {
            return Err(read_only_filesystem_error(path));
        }

        Ok(())
    }

    fn has_agentos_hardlink_alias(&mut self, path: &str) -> KernelResult<bool> {
        let Some(target) = self.storage_lstat(path)? else {
            return Ok(false);
        };
        if target.is_directory || target.is_symbolic_link {
            return Ok(false);
        }

        self.agentos_subtree_contains_inode("/etc/agentos", target.dev, target.ino)
    }

    fn agentos_subtree_contains_inode(
        &mut self,
        path: &str,
        target_dev: u64,
        target_ino: u64,
    ) -> KernelResult<bool> {
        let Some(stat) = self.storage_lstat(path)? else {
            return Ok(false);
        };
        if !stat.is_directory && !stat.is_symbolic_link {
            return Ok(stat.dev == target_dev && stat.ino == target_ino);
        }
        if !stat.is_directory {
            return Ok(false);
        }

        let children = self.raw_filesystem_mut().read_dir_with_types(path)?;
        for child in children {
            if child.name == "." || child.name == ".." {
                continue;
            }
            let child_path = join_absolute_path(path, &child.name);
            if self.agentos_subtree_contains_inode(&child_path, target_dev, target_ino)? {
                return Ok(true);
            }
        }

        Ok(false)
    }

    fn resolve_write_guard_path(
        &mut self,
        path: &str,
        follow_final_symlink: bool,
    ) -> KernelResult<Option<String>> {
        let normalized = normalize_path(path);
        if normalized == "/" {
            return Ok(Some(normalized));
        }

        if follow_final_symlink {
            if let Ok(resolved) = self.filesystem.realpath(&normalized) {
                return Ok(Some(resolved));
            }
        }

        let components: Vec<&str> = normalized
            .split('/')
            .filter(|component| !component.is_empty())
            .collect();
        let mut resolved_prefix = String::from("/");
        let mut raw_prefix = String::from("/");

        for (index, component) in components.iter().enumerate() {
            let is_final = index + 1 == components.len();
            if is_final && !follow_final_symlink {
                return Ok(Some(join_absolute_path(&resolved_prefix, component)));
            }

            raw_prefix = join_absolute_path(&raw_prefix, component);
            match self.filesystem.realpath(&raw_prefix) {
                Ok(resolved) => {
                    resolved_prefix = resolved;
                }
                Err(error) if error.code() == "ENOENT" => {
                    let mut resolved = resolved_prefix;
                    for remaining in &components[index..] {
                        resolved = join_absolute_path(&resolved, remaining);
                    }
                    return Ok(Some(resolved));
                }
                Err(error) => return Err(error.into()),
            }
        }

        Ok(Some(resolved_prefix))
    }

    fn populate_poll_target_revents(
        &self,
        pid: u32,
        targets: &mut [PollTargetEntry],
    ) -> KernelResult<usize> {
        let mut ready_count = 0;
        for target in targets.iter_mut() {
            target.revents = self.poll_target_entry(pid, target.target, target.events)?;
            if !target.revents.is_empty() {
                ready_count += 1;
            }
        }

        Ok(ready_count)
    }

    fn poll_target_entry(
        &self,
        pid: u32,
        target: PollTarget,
        requested: PollEvents,
    ) -> KernelResult<PollEvents> {
        match target {
            PollTarget::Fd(fd) => {
                let entry = {
                    let tables = lock_or_recover(&self.fd_tables);
                    tables
                        .get(pid)
                        .ok_or_else(|| KernelError::no_such_process(pid))?
                        .get(fd)
                        .cloned()
                };
                if let Some(entry) = entry {
                    self.poll_entry(&entry, requested)
                } else {
                    Ok(POLLNVAL)
                }
            }
            PollTarget::Socket(socket_id) => {
                let socket = self.sockets.get(socket_id);
                if let Some(socket) = socket {
                    if socket.owner_pid() != pid && socket.owner_pid() != 0 {
                        return Err(KernelError::permission_denied(format!(
                            "process {pid} does not own socket {socket_id}"
                        )));
                    }
                    let mut events = self.sockets.poll(socket_id, requested)?;
                    if events.intersects(POLLOUT)
                        && !self.socket_pollout_has_resource_capacity(&socket)
                    {
                        events = PollEvents::from_bits(events.bits() & !POLLOUT.bits());
                    }
                    Ok(events)
                } else {
                    Ok(POLLNVAL)
                }
            }
        }
    }

    fn socket_pollout_has_resource_capacity(&self, socket: &SocketRecord) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        if self.sockets.has_resource_ledger() {
            return self.sockets.buffered_byte_capacity_available()
                && (socket.spec().socket_type != SocketType::Datagram
                    || self.sockets.datagram_capacity_available());
        }

        let snapshot = self.resource_snapshot();
        if self
            .resources
            .limits()
            .max_socket_buffered_bytes
            .is_some_and(|limit| snapshot.socket_buffered_bytes >= limit)
        {
            return false;
        }

        if socket.spec().socket_type == SocketType::Datagram
            && self
                .resources
                .limits()
                .max_socket_datagram_queue_len
                .is_some_and(|limit| snapshot.socket_datagram_queue_len >= limit)
        {
            return false;
        }

        true
    }

    fn poll_entry(
        &self,
        entry: &crate::fd_table::FdEntry,
        requested: PollEvents,
    ) -> KernelResult<PollEvents> {
        if let Some(socket_id) = self.fd_socket_id(&entry.description) {
            let socket = self
                .sockets
                .get(socket_id)
                .ok_or_else(|| KernelError::bad_file_descriptor(entry.fd))?;
            let mut events = self.sockets.poll(socket_id, requested)?;
            if events.intersects(POLLOUT) && !self.socket_pollout_has_resource_capacity(&socket) {
                events = PollEvents::from_bits(events.bits() & !POLLOUT.bits());
            }
            return Ok(events);
        }

        if self.pipes.is_pipe(entry.description.id()) {
            return Ok(self.pipes.poll(entry.description.id(), requested)?);
        }

        if self.ptys.is_pty(entry.description.id()) {
            return Ok(self.ptys.poll(entry.description.id(), requested)?);
        }

        let access_mode = entry.description.flags() & 0b11;
        let mut events = PollEvents::empty();
        if requested.intersects(POLLIN) && access_mode != crate::fd_table::O_WRONLY {
            events |= POLLIN;
        }
        if requested.intersects(POLLOUT) && access_mode != crate::fd_table::O_RDONLY {
            events |= POLLOUT;
        }
        if entry.filetype == FILETYPE_DIRECTORY && requested.intersects(POLLOUT) {
            events |= POLLERR;
        }
        if self.terminated {
            events |= POLLHUP;
        }
        Ok(events)
    }

    fn description_for_fd(
        &self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
    ) -> KernelResult<Arc<FileDescription>> {
        self.assert_driver_owns(requester_driver, pid)?;
        lock_or_recover(&self.fd_tables)
            .get(pid)
            .and_then(|table| table.get(fd))
            .map(|entry| Arc::clone(&entry.description))
            .ok_or_else(|| KernelError::bad_file_descriptor(fd))
    }

    fn fd_socket_id(&self, description: &Arc<FileDescription>) -> Option<SocketId> {
        lock_or_recover(&self.fd_sockets)
            .get(&description.id())
            .filter(|entry| Arc::ptr_eq(&entry.description, description))
            .map(|entry| entry.socket_id)
    }

    fn fd_socket_id_for_fd(&self, pid: u32, fd: u32) -> KernelResult<SocketId> {
        let description = {
            let tables = lock_or_recover(&self.fd_tables);
            tables
                .get(pid)
                .and_then(|table| table.get(fd))
                .map(|entry| Arc::clone(&entry.description))
                .ok_or_else(|| KernelError::bad_file_descriptor(fd))?
        };
        self.fd_socket_id(&description)
            .ok_or_else(|| KernelError::new("ENOTSOCK", "descriptor is not a socket"))
    }

    fn open_file_descriptions(&self) -> Vec<Arc<FileDescription>> {
        let tables = lock_or_recover(&self.fd_tables);
        let mut descriptions = BTreeMap::new();
        for pid in tables.pids() {
            let Some(table) = tables.get(pid) else {
                continue;
            };
            for entry in table.values() {
                descriptions
                    .entry(entry.description.id())
                    .or_insert_with(|| Arc::clone(&entry.description));
            }
        }
        descriptions.into_values().collect()
    }

    fn prepare_anonymous_file_backing(
        &mut self,
        path: &str,
        stat: Option<&VirtualStat>,
    ) -> KernelResult<Option<OpenFileRemovalBacking>> {
        let descriptions = self
            .open_file_descriptions()
            .into_iter()
            .filter(|description| description.is_path_backed_by(path))
            .collect::<Vec<_>>();
        if descriptions.is_empty() {
            return Ok(None);
        }
        let mut stat = match stat {
            Some(stat) if !stat.is_directory && !stat.is_symbolic_link => stat.clone(),
            _ => return Ok(None),
        };
        if stat.nlink > 1 {
            if let Some(live_path) = self.find_surviving_hard_link(path, &stat)? {
                return Ok(Some(OpenFileRemovalBacking::LinkedAlias {
                    descriptions,
                    live_path,
                }));
            }
        }
        stat.nlink = 0;
        let data = self.filesystem.read_file(path)?;
        let backing: SharedAnonymousFile = Arc::new(Mutex::new(AnonymousFile::new(
            data,
            stat,
            Arc::clone(&self.anonymous_file_usage),
        )));
        Ok(Some(OpenFileRemovalBacking::Anonymous {
            descriptions,
            backing,
        }))
    }

    fn find_surviving_hard_link(
        &mut self,
        removed_path: &str,
        target: &VirtualStat,
    ) -> KernelResult<Option<String>> {
        let removed_path = normalize_path(removed_path);
        let mut queue = VecDeque::from([(String::from("/"), 0usize)]);
        let mut entries = 0usize;
        let per_directory_limit = self.resources.max_readdir_entries().unwrap_or(usize::MAX);

        while let Some((directory, depth)) = queue.pop_front() {
            self.resources.check_recursive_fs_depth(depth)?;
            let names = self
                .raw_filesystem_mut()
                .read_dir_limited(&directory, per_directory_limit)?;
            self.resources.check_readdir_entries(names.len())?;
            for name in names {
                if matches!(name.as_str(), "." | "..") {
                    continue;
                }
                entries = entries.saturating_add(1);
                self.resources.check_recursive_fs_entries(entries)?;
                let path = join_child_path(&directory, &name);
                let stat = match self.raw_filesystem_mut().lstat(&path) {
                    Ok(stat) => stat,
                    Err(error) if error.code() == "ENOENT" => continue,
                    Err(error) => return Err(error.into()),
                };
                if path != removed_path && stat.dev == target.dev && stat.ino == target.ino {
                    return Ok(Some(path));
                }
                if stat.is_directory && !stat.is_symbolic_link {
                    queue.push_back((path, depth.saturating_add(1)));
                }
            }
        }
        Ok(None)
    }

    fn prepare_detached_directory_backing(
        &self,
        path: &str,
        stat: Option<&VirtualStat>,
    ) -> Option<(Vec<Arc<FileDescription>>, VirtualStat)> {
        let mut stat = stat.filter(|stat| stat.is_directory)?.clone();
        stat.nlink = 0;
        let descriptions = self
            .open_file_descriptions()
            .into_iter()
            .filter(|description| {
                description.is_path_backed_by(path)
                    || description
                        .lock_target()
                        .is_some_and(|target| target.ino() == stat.ino)
            })
            .collect::<Vec<_>>();
        (!descriptions.is_empty()).then_some((descriptions, stat))
    }

    fn rename_open_file_descriptions(&self, old_path: &str, new_path: &str) {
        for description in self.open_file_descriptions() {
            description.rename_path_prefix(old_path, new_path);
        }
    }

    fn assert_not_terminated(&self) -> KernelResult<()> {
        if self.terminated {
            Err(KernelError::disposed())
        } else {
            Ok(())
        }
    }

    fn assert_driver_owns(&self, requester_driver: &str, pid: u32) -> KernelResult<()> {
        let driver_pids = lock_or_recover(&self.driver_pids);
        if driver_pids
            .get(requester_driver)
            .map(|pids| pids.contains(&pid))
            .unwrap_or(false)
        {
            return Ok(());
        }

        if driver_pids.values().any(|pids| pids.contains(&pid)) {
            return Err(KernelError::permission_denied(format!(
                "driver \"{requester_driver}\" does not own PID {pid}"
            )));
        }

        Err(KernelError::no_such_process(pid))
    }

    fn cleanup_process_resources(&self, pid: u32) {
        cleanup_process_resources(
            self.fd_tables.as_ref(),
            &self.file_locks,
            &self.pipes,
            &self.ptys,
            &self.sockets,
            &self.fd_sockets,
            self.driver_pids.as_ref(),
            pid,
        );
    }

    fn resolve_spawn_command(
        &mut self,
        command: &str,
        args: &[String],
        cwd: &str,
        parent_pid: Option<u32>,
    ) -> KernelResult<ResolvedSpawnCommand> {
        if let Some(driver) = self.commands.resolve(command).cloned() {
            return Ok(ResolvedSpawnCommand {
                command: command.to_owned(),
                args: args.to_vec(),
                driver,
            });
        }

        let Some(path) = self.resolve_executable_path(command, cwd, parent_pid)? else {
            return Err(KernelError::command_not_found(command));
        };

        if let Some(registered_command) = self.resolve_registered_command_path(&path) {
            let driver = self
                .commands
                .resolve(&registered_command)
                .cloned()
                .ok_or_else(|| KernelError::command_not_found(&registered_command))?;
            return Ok(ResolvedSpawnCommand {
                command: registered_command,
                args: args.to_vec(),
                driver,
            });
        }

        let shebang = self
            .parse_shebang_command(&path)?
            .ok_or_else(|| KernelError::new("ENOEXEC", format!("exec format error: {path}")))?;
        self.resolve_shebang_command(&path, args, shebang)
    }

    fn resolve_executable_path(
        &mut self,
        command: &str,
        cwd: &str,
        parent_pid: Option<u32>,
    ) -> KernelResult<Option<String>> {
        if !command.contains('/') {
            return Ok(None);
        }

        let path = if command.starts_with('/') {
            normalize_path(command)
        } else {
            normalize_path(&format!("{cwd}/{command}"))
        };
        // exec(2) follows symlinks, and a symlink target may live in a different
        // mount (e.g. `/opt/agentos/bin/<cmd>` is its own single-symlink mount
        // pointing into a package tar mount). Resolve the real path before
        // stat-ing / reading the executable so cross-mount symlinked commands
        // exec their real target instead of failing to read the symlink node.
        let path = self.filesystem.realpath(&path).unwrap_or(path);
        let stat = self.filesystem.stat(&path)?;
        if stat.is_directory {
            return Err(KernelError::new(
                "EACCES",
                format!("permission denied, execute '{path}'"),
            ));
        }
        // Registered command projections are executable kernel objects even
        // when their host/package backing blob is stored as 0644. Ordinary
        // VFS files must still carry a real execute bit, as Linux requires.
        let registered = self.resolve_registered_command_path(&path).is_some();
        if let Some(pid) = parent_pid {
            if !registered {
                self.check_dac_access(pid, &path, DAC_EXECUTE)?;
            }
        } else if stat.mode & EXECUTABLE_PERMISSION_BITS == 0 && !registered {
            return Err(KernelError::new(
                "EACCES",
                format!("permission denied, execute '{path}'"),
            ));
        }
        Ok(Some(path))
    }

    fn validate_wasm_exec_image_inner(
        &mut self,
        path: &str,
        cwd: &str,
        interpreter_depth: usize,
    ) -> KernelResult<()> {
        let resolved = self.validate_executable_path(path, cwd)?;
        // `/bin/<name>` and `/__agentos/commands/.../<name>` may be
        // kernel-owned command stubs whose backing bytes are a self-referential
        // launcher rather than the projected WASM blob the runner loads. Once
        // the exact path has resolved to a registered command, its trusted
        // command driver is the validated executable image; parsing the stub's
        // `#!` bytes would incorrectly report ELOOP for paths such as `/bin/sh`.
        if self.resolve_registered_command_path(&resolved).is_some() {
            return Ok(());
        }
        let header = self
            .filesystem
            .pread(&resolved, 0, SHEBANG_LINE_MAX_BYTES)?;
        if header.starts_with(b"\0asm") {
            return Ok(());
        }

        let Some(interpreter) = linux_shebang_interpreter(&header, &resolved)? else {
            return Err(KernelError::new(
                "ENOEXEC",
                format!("exec format error: {resolved}"),
            ));
        };
        if interpreter_depth >= MAX_EXEC_INTERPRETER_DEPTH {
            return Err(KernelError::new(
                "ELOOP",
                format!("too many levels of symbolic links or interpreters: {resolved}"),
            ));
        }

        self.validate_wasm_exec_image_inner(&interpreter, cwd, interpreter_depth + 1)
    }

    fn resolve_registered_command_path(&self, path: &str) -> Option<String> {
        let normalized = normalize_path(path);
        for prefix in ["/bin/", "/usr/bin/", "/usr/local/bin/"] {
            let Some(name) = normalized.strip_prefix(prefix) else {
                continue;
            };
            if !name.is_empty() && !name.contains('/') && self.commands.resolve(name).is_some() {
                return Some(name.to_owned());
            }
        }

        if let Some(name) = normalized
            .strip_prefix("/__agentos/commands/")
            .and_then(|suffix| suffix.rsplit('/').next())
        {
            if !name.is_empty() && !name.contains('/') && self.commands.resolve(name).is_some() {
                return Some(name.to_owned());
            }
        }

        None
    }

    fn parse_shebang_command(&mut self, path: &str) -> KernelResult<Option<ShebangCommand>> {
        let header = self.filesystem.pread(path, 0, SHEBANG_LINE_MAX_BYTES + 1)?;
        if !header.starts_with(b"#!") {
            return Ok(None);
        }

        let line_end = match header.iter().position(|byte| *byte == b'\n') {
            Some(index) => index,
            None if header.len() <= SHEBANG_LINE_MAX_BYTES => header.len(),
            None => {
                return Err(KernelError::new(
                    "ENOEXEC",
                    format!("shebang line exceeds {SHEBANG_LINE_MAX_BYTES} bytes: {path}"),
                ));
            }
        };
        let line = header[2..line_end]
            .strip_suffix(b"\r")
            .unwrap_or(&header[2..line_end]);
        let text = std::str::from_utf8(line)
            .map_err(|_| KernelError::new("ENOEXEC", format!("invalid shebang line: {path}")))?;
        let mut parts = text.split_ascii_whitespace();
        let interpreter = parts
            .next()
            .ok_or_else(|| KernelError::new("ENOEXEC", format!("invalid shebang line: {path}")))?;
        Ok(Some(ShebangCommand {
            interpreter: interpreter.to_owned(),
            args: parts.map(ToOwned::to_owned).collect(),
        }))
    }

    fn resolve_shebang_command(
        &self,
        path: &str,
        args: &[String],
        shebang: ShebangCommand,
    ) -> KernelResult<ResolvedSpawnCommand> {
        let mut interpreter_args = shebang.args;
        let interpreter = normalize_path(&shebang.interpreter);
        let command = if interpreter == "/usr/bin/env" || interpreter == "/bin/env" {
            if interpreter_args.is_empty() {
                return Err(KernelError::new(
                    "ENOENT",
                    format!("missing interpreter after /usr/bin/env in shebang: {path}"),
                ));
            }
            interpreter_args.remove(0)
        } else if let Some(command) = self.resolve_registered_command_path(&interpreter) {
            command
        } else if self.commands.resolve(&shebang.interpreter).is_some() {
            shebang.interpreter
        } else {
            return Err(KernelError::command_not_found(&shebang.interpreter));
        };

        let driver = self
            .commands
            .resolve(&command)
            .cloned()
            .ok_or_else(|| KernelError::command_not_found(&command))?;
        let mut resolved_args = interpreter_args;
        resolved_args.push(path.to_owned());
        resolved_args.extend(args.iter().cloned());
        Ok(ResolvedSpawnCommand {
            command,
            args: resolved_args,
            driver,
        })
    }

    fn finish_waitpid_event(&mut self, result: ProcessWaitResult) -> WaitPidEventResult {
        if result.event == WaitPidEvent::Exited {
            self.cleanup_process_resources(result.pid);
        }
        WaitPidEventResult {
            pid: result.pid,
            status: result.status,
            event: result.event,
        }
    }

    fn raw_filesystem_mut(&mut self) -> &mut F {
        self.filesystem.inner_mut().inner_mut()
    }

    fn read_file_internal(
        &mut self,
        current_pid: Option<u32>,
        path: &str,
    ) -> KernelResult<Vec<u8>> {
        if let Some(proc_node) = self.resolve_proc_node(path, current_pid)? {
            self.filesystem
                .check_virtual_path(FsOperation::Read, path)
                .map_err(KernelError::from)?;
            return self.proc_read_file(current_pid, &proc_node);
        }

        self.reject_unix_socket_data_path(path, "ENXIO")?;
        Ok(self.filesystem.read_file(path)?)
    }

    fn effective_recursive_fs_depth(
        &self,
        requested_max_depth: Option<usize>,
    ) -> KernelResult<usize> {
        match (requested_max_depth, self.resources.max_recursive_fs_depth()) {
            (Some(requested), Some(limit)) if requested > limit => Err(KernelError::new(
                "EINVAL",
                format!(
                    "requested recursive filesystem max depth {requested} exceeds configured limit {limit}"
                ),
            )),
            (Some(requested), _) => Ok(requested),
            (None, Some(limit)) => Ok(limit),
            (None, None) => Ok(usize::MAX),
        }
    }

    fn copy_path_inner(
        &mut self,
        from: &str,
        to: &str,
        recursive: bool,
        depth: usize,
        entries: &mut usize,
    ) -> KernelResult<()> {
        self.resources.check_recursive_fs_depth(depth)?;
        *entries = entries.saturating_add(1);
        self.resources.check_recursive_fs_entries(*entries)?;
        let source_stat = self.lstat_internal(None, from)?;

        if source_stat.is_symbolic_link {
            let target = self.read_link_internal(None, from)?;
            self.symlink(&target, to)?;
            return Ok(());
        }

        if source_stat.is_directory {
            if !recursive {
                return Err(KernelError::new(
                    "EISDIR",
                    format!("illegal operation on a directory, copy '{from}'"),
                ));
            }

            let source_root = normalize_path(from);
            let destination_root = normalize_path(to);
            if destination_root.starts_with(&(source_root.clone() + "/")) {
                return Err(KernelError::new(
                    "EINVAL",
                    format!("cannot copy '{from}' into its own descendant '{to}'"),
                ));
            }

            self.mkdir(&parent_path(&destination_root), true)?;
            if !self.exists_internal(None, &destination_root)? {
                self.create_dir(&destination_root)?;
            }
            self.chmod(&destination_root, source_stat.mode)?;
            self.chown(&destination_root, source_stat.uid, source_stat.gid)?;

            let names = self.read_dir_internal(None, from)?;
            self.resources.check_readdir_entries(names.len())?;
            for name in names {
                if matches!(name.as_str(), "." | "..") {
                    continue;
                }
                let child_from = join_child_path(from, &name);
                let child_to = join_child_path(to, &name);
                self.copy_path_inner(
                    &child_from,
                    &child_to,
                    true,
                    depth.saturating_add(1),
                    entries,
                )?;
            }
            return Ok(());
        }

        let content = self.read_file_internal(None, from)?;
        self.write_file(to, content)?;
        self.chmod(to, source_stat.mode)?;
        self.chown(to, source_stat.uid, source_stat.gid)
    }

    fn remove_path_inner(
        &mut self,
        path: &str,
        recursive: bool,
        depth: usize,
        entries: &mut usize,
    ) -> KernelResult<()> {
        self.resources.check_recursive_fs_depth(depth)?;
        *entries = entries.saturating_add(1);
        self.resources.check_recursive_fs_entries(*entries)?;
        let stat = self.lstat_internal(None, path)?;
        if stat.is_directory && !stat.is_symbolic_link {
            if recursive {
                let names = self.read_dir_internal(None, path)?;
                self.resources.check_readdir_entries(names.len())?;
                for name in names {
                    if matches!(name.as_str(), "." | "..") {
                        continue;
                    }
                    let child = join_child_path(path, &name);
                    self.remove_path_inner(&child, true, depth.saturating_add(1), entries)?;
                }
            }
            return self.remove_dir(path);
        }

        self.remove_file(path)
    }

    fn exists_internal(&self, current_pid: Option<u32>, path: &str) -> KernelResult<bool> {
        match self.resolve_proc_node(path, current_pid) {
            Ok(Some(_)) => {
                self.filesystem
                    .check_virtual_path(FsOperation::Read, path)
                    .map_err(KernelError::from)?;
                Ok(true)
            }
            Ok(None) => Ok(self.filesystem.exists(path)?),
            Err(error) if error.code() == "ENOENT" => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn stat_internal(&mut self, current_pid: Option<u32>, path: &str) -> KernelResult<VirtualStat> {
        if let Some(proc_node) = self.resolve_proc_node(path, current_pid)? {
            self.filesystem
                .check_virtual_path(FsOperation::Read, path)
                .map_err(KernelError::from)?;
            return self.proc_stat(current_pid, &proc_node);
        }

        Ok(self.filesystem.stat(path)?)
    }

    fn lstat_internal(&self, current_pid: Option<u32>, path: &str) -> KernelResult<VirtualStat> {
        if let Some(proc_node) = self.resolve_proc_node(path, current_pid)? {
            self.filesystem
                .check_virtual_path(FsOperation::Read, path)
                .map_err(KernelError::from)?;
            return self.proc_lstat(&proc_node);
        }

        Ok(self.filesystem.lstat(path)?)
    }

    fn read_link_internal(&self, current_pid: Option<u32>, path: &str) -> KernelResult<String> {
        if let Some(proc_node) = self.resolve_proc_node(path, current_pid)? {
            self.filesystem
                .check_virtual_path(FsOperation::Read, path)
                .map_err(KernelError::from)?;
            return self.proc_read_link(&proc_node);
        }

        Ok(self.filesystem.read_link(path)?)
    }

    fn read_dir_internal(
        &mut self,
        current_pid: Option<u32>,
        path: &str,
    ) -> KernelResult<Vec<String>> {
        if let Some(proc_node) = self.resolve_proc_node(path, current_pid)? {
            self.filesystem
                .check_virtual_path(FsOperation::Read, path)
                .map_err(KernelError::from)?;
            return self.proc_read_dir(current_pid, &proc_node);
        }

        if let Some(limit) = self.resources.max_readdir_entries() {
            Ok(self.filesystem.read_dir_limited(path, limit)?)
        } else {
            Ok(self.filesystem.read_dir(path)?)
        }
    }

    fn read_dir_with_types_internal(
        &mut self,
        current_pid: Option<u32>,
        path: &str,
    ) -> KernelResult<Vec<VirtualDirEntry>> {
        if let Some(proc_node) = self.resolve_proc_node(path, current_pid)? {
            self.filesystem
                .check_virtual_path(FsOperation::Read, path)
                .map_err(KernelError::from)?;
            return Ok(self
                .proc_read_dir(current_pid, &proc_node)?
                .into_iter()
                .map(|name| VirtualDirEntry {
                    name,
                    is_directory: false,
                    is_symbolic_link: false,
                })
                .collect());
        }

        Ok(self.filesystem.read_dir_with_types(path)?)
    }

    fn realpath_internal(&self, current_pid: Option<u32>, path: &str) -> KernelResult<String> {
        if let Some(proc_node) = self.resolve_proc_node(path, current_pid)? {
            self.filesystem
                .check_virtual_path(FsOperation::Read, path)
                .map_err(KernelError::from)?;
            return self.proc_realpath(current_pid, &proc_node);
        }

        Ok(self.filesystem.realpath(path)?)
    }

    fn resolve_proc_node(
        &self,
        path: &str,
        current_pid: Option<u32>,
    ) -> KernelResult<Option<ProcNode>> {
        let normalized = normalize_path(path);
        if !is_proc_path(&normalized) {
            return Ok(None);
        }

        if normalized == "/proc" {
            return Ok(Some(ProcNode::RootDir));
        }

        let suffix = normalized
            .strip_prefix("/proc/")
            .expect("proc path should have /proc prefix");
        let parts = suffix.split('/').collect::<Vec<_>>();
        if parts.is_empty() {
            return Ok(Some(ProcNode::RootDir));
        }

        let root_node = match parts.as_slice() {
            ["mounts"] => Some(ProcNode::MountsFile),
            ["cpuinfo"] => Some(ProcNode::CpuInfoFile),
            ["meminfo"] => Some(ProcNode::MemInfoFile),
            ["loadavg"] => Some(ProcNode::LoadAvgFile),
            ["uptime"] => Some(ProcNode::UptimeFile),
            ["version"] => Some(ProcNode::VersionFile),
            _ => None,
        };
        if let Some(node) = root_node {
            return Ok(Some(node));
        }

        let pid = match parts[0] {
            "self" => current_pid.ok_or_else(|| proc_not_found_error(&normalized))?,
            raw => raw
                .parse::<u32>()
                .map_err(|_| proc_not_found_error(&normalized))?,
        };
        self.proc_entry(pid)?;

        let node = match parts.as_slice() {
            ["self"] => ProcNode::SelfLink { pid },
            [_pid] => ProcNode::PidDir { pid },
            [_pid, "fd"] => ProcNode::PidFdDir { pid },
            [_pid, "cmdline"] => ProcNode::PidCmdline { pid },
            [_pid, "environ"] => ProcNode::PidEnviron { pid },
            [_pid, "cwd"] => ProcNode::PidCwdLink { pid },
            [_pid, "stat"] => ProcNode::PidStatFile { pid },
            [_pid, "status"] => ProcNode::PidStatusFile { pid },
            [_pid, "fd", fd] => {
                let fd = fd
                    .parse::<u32>()
                    .map_err(|_| proc_not_found_error(&normalized))?;
                self.proc_fd_entry(pid, fd)?;
                ProcNode::PidFdLink { pid, fd }
            }
            _ => return Err(proc_not_found_error(&normalized)),
        };

        Ok(Some(node))
    }

    fn proc_entry(&self, pid: u32) -> KernelResult<crate::process_table::ProcessEntry> {
        self.processes
            .get(pid)
            .ok_or_else(|| proc_not_found_error(&format!("/proc/{pid}")))
    }

    fn proc_fd_entry(&self, pid: u32, fd: u32) -> KernelResult<FdEntry> {
        lock_or_recover(&self.fd_tables)
            .get(pid)
            .and_then(|table| table.get(fd))
            .cloned()
            .ok_or_else(|| proc_not_found_error(&format!("/proc/{pid}/fd/{fd}")))
    }

    fn proc_read_file(
        &mut self,
        current_pid: Option<u32>,
        node: &ProcNode,
    ) -> KernelResult<Vec<u8>> {
        match node {
            ProcNode::SelfLink { .. }
            | ProcNode::PidCwdLink { .. }
            | ProcNode::PidFdLink { .. } => {
                let target = self.proc_symlink_target(node)?;
                self.read_file_internal(current_pid, &target)
            }
            ProcNode::MountsFile => Ok(self.proc_mounts_bytes()),
            ProcNode::CpuInfoFile => Ok(self.proc_cpuinfo_bytes()),
            ProcNode::MemInfoFile => Ok(self.proc_meminfo_bytes()),
            ProcNode::LoadAvgFile => Ok(self.proc_loadavg_bytes()),
            ProcNode::UptimeFile => Ok(self.proc_uptime_bytes()),
            ProcNode::VersionFile => Ok(self.proc_version_bytes()),
            ProcNode::PidCmdline { pid } => Ok(self.proc_cmdline_bytes(*pid)),
            ProcNode::PidEnviron { pid } => Ok(self.proc_environ_bytes(*pid)),
            ProcNode::PidStatFile { pid } => Ok(self.proc_stat_bytes(*pid)),
            ProcNode::PidStatusFile { pid } => Ok(self.proc_status_bytes(*pid)),
            ProcNode::RootDir | ProcNode::PidDir { .. } | ProcNode::PidFdDir { .. } => {
                Err(KernelError::new(
                    "EISDIR",
                    format!(
                        "illegal operation on a directory, read '{}'",
                        self.proc_canonical_path(node)
                    ),
                ))
            }
        }
    }

    fn proc_stat(
        &mut self,
        current_pid: Option<u32>,
        node: &ProcNode,
    ) -> KernelResult<VirtualStat> {
        match node {
            ProcNode::SelfLink { .. }
            | ProcNode::PidCwdLink { .. }
            | ProcNode::PidFdLink { .. } => {
                let target = self.proc_symlink_target(node)?;
                self.stat_internal(current_pid, &target)
            }
            _ => self.proc_lstat(node),
        }
    }

    fn proc_lstat(&self, node: &ProcNode) -> KernelResult<VirtualStat> {
        match node {
            ProcNode::RootDir | ProcNode::PidDir { .. } | ProcNode::PidFdDir { .. } => {
                Ok(proc_dir_stat(proc_inode(node)))
            }
            ProcNode::MountsFile => Ok(proc_file_stat(
                proc_inode(node),
                self.proc_mounts_bytes().len() as u64,
            )),
            ProcNode::CpuInfoFile => Ok(proc_file_stat(
                proc_inode(node),
                self.proc_cpuinfo_bytes().len() as u64,
            )),
            ProcNode::MemInfoFile => Ok(proc_file_stat(
                proc_inode(node),
                self.proc_meminfo_bytes().len() as u64,
            )),
            ProcNode::LoadAvgFile => Ok(proc_file_stat(
                proc_inode(node),
                self.proc_loadavg_bytes().len() as u64,
            )),
            ProcNode::UptimeFile => Ok(proc_file_stat(
                proc_inode(node),
                self.proc_uptime_bytes().len() as u64,
            )),
            ProcNode::VersionFile => Ok(proc_file_stat(
                proc_inode(node),
                self.proc_version_bytes().len() as u64,
            )),
            ProcNode::PidCmdline { pid } => Ok(proc_file_stat(
                proc_inode(node),
                self.proc_cmdline_bytes(*pid).len() as u64,
            )),
            ProcNode::PidEnviron { pid } => Ok(proc_file_stat(
                proc_inode(node),
                self.proc_environ_bytes(*pid).len() as u64,
            )),
            ProcNode::PidStatFile { pid } => Ok(proc_file_stat(
                proc_inode(node),
                self.proc_stat_bytes(*pid).len() as u64,
            )),
            ProcNode::PidStatusFile { pid } => Ok(proc_file_stat(
                proc_inode(node),
                self.proc_status_bytes(*pid).len() as u64,
            )),
            ProcNode::SelfLink { .. }
            | ProcNode::PidCwdLink { .. }
            | ProcNode::PidFdLink { .. } => Ok(proc_symlink_stat(
                proc_inode(node),
                self.proc_read_link(node)?.len() as u64,
            )),
        }
    }

    fn proc_read_link(&self, node: &ProcNode) -> KernelResult<String> {
        match node {
            ProcNode::SelfLink { .. }
            | ProcNode::PidCwdLink { .. }
            | ProcNode::PidFdLink { .. } => self.proc_symlink_target(node),
            _ => Err(KernelError::new(
                "EINVAL",
                format!(
                    "invalid argument, readlink '{}'",
                    self.proc_canonical_path(node)
                ),
            )),
        }
    }

    fn proc_read_dir(
        &mut self,
        current_pid: Option<u32>,
        node: &ProcNode,
    ) -> KernelResult<Vec<String>> {
        match node {
            ProcNode::SelfLink { .. }
            | ProcNode::PidCwdLink { .. }
            | ProcNode::PidFdLink { .. } => {
                let target = self.proc_symlink_target(node)?;
                self.read_dir_internal(current_pid, &target)
            }
            ProcNode::RootDir => {
                let mut entries = self
                    .processes
                    .list_processes()
                    .keys()
                    .map(|pid| pid.to_string())
                    .collect::<Vec<_>>();
                entries.push(String::from("cpuinfo"));
                entries.push(String::from("loadavg"));
                entries.push(String::from("meminfo"));
                entries.push(String::from("mounts"));
                entries.push(String::from("self"));
                entries.push(String::from("uptime"));
                entries.push(String::from("version"));
                entries.sort();
                Ok(entries)
            }
            ProcNode::PidDir { .. } => Ok(vec![
                String::from("cmdline"),
                String::from("cwd"),
                String::from("environ"),
                String::from("fd"),
                String::from("stat"),
                String::from("status"),
            ]),
            ProcNode::PidFdDir { pid } => {
                let tables = lock_or_recover(&self.fd_tables);
                let table = tables
                    .get(*pid)
                    .ok_or_else(|| proc_not_found_error(&format!("/proc/{pid}/fd")))?;
                Ok(table.iter().map(|entry| entry.fd.to_string()).collect())
            }
            _ => Err(KernelError::new(
                "ENOTDIR",
                format!(
                    "not a directory, scandir '{}'",
                    self.proc_canonical_path(node)
                ),
            )),
        }
    }

    fn proc_realpath(&self, current_pid: Option<u32>, node: &ProcNode) -> KernelResult<String> {
        match node {
            ProcNode::SelfLink { .. }
            | ProcNode::PidCwdLink { .. }
            | ProcNode::PidFdLink { .. } => {
                let target = self.proc_symlink_target(node)?;
                self.realpath_internal(current_pid, &target)
            }
            _ => Ok(self.proc_canonical_path(node)),
        }
    }

    fn proc_symlink_target(&self, node: &ProcNode) -> KernelResult<String> {
        match node {
            ProcNode::SelfLink { pid } => Ok(format!("/proc/{pid}")),
            ProcNode::PidCwdLink { pid } => Ok(self.proc_entry(*pid)?.cwd),
            ProcNode::PidFdLink { pid, fd } => Ok(self
                .proc_fd_entry(*pid, *fd)?
                .description
                .proc_display_path()),
            _ => Err(KernelError::new(
                "EINVAL",
                format!(
                    "'{}' is not a symbolic link",
                    self.proc_canonical_path(node)
                ),
            )),
        }
    }

    fn proc_canonical_path(&self, node: &ProcNode) -> String {
        match node {
            ProcNode::RootDir => String::from("/proc"),
            ProcNode::MountsFile => String::from("/proc/mounts"),
            ProcNode::CpuInfoFile => String::from("/proc/cpuinfo"),
            ProcNode::MemInfoFile => String::from("/proc/meminfo"),
            ProcNode::LoadAvgFile => String::from("/proc/loadavg"),
            ProcNode::UptimeFile => String::from("/proc/uptime"),
            ProcNode::VersionFile => String::from("/proc/version"),
            ProcNode::SelfLink { pid } => format!("/proc/{pid}"),
            ProcNode::PidDir { pid } => format!("/proc/{pid}"),
            ProcNode::PidFdDir { pid } => format!("/proc/{pid}/fd"),
            ProcNode::PidCmdline { pid } => format!("/proc/{pid}/cmdline"),
            ProcNode::PidEnviron { pid } => format!("/proc/{pid}/environ"),
            ProcNode::PidCwdLink { pid } => format!("/proc/{pid}/cwd"),
            ProcNode::PidStatFile { pid } => format!("/proc/{pid}/stat"),
            ProcNode::PidStatusFile { pid } => format!("/proc/{pid}/status"),
            ProcNode::PidFdLink { pid, fd } => format!("/proc/{pid}/fd/{fd}"),
        }
    }

    fn proc_cmdline_bytes(&self, pid: u32) -> Vec<u8> {
        let entry = self
            .processes
            .get(pid)
            .expect("process must exist while procfs path is resolved");
        let mut argv = vec![entry.command];
        argv.extend(entry.args);
        null_separated_bytes(argv)
    }

    fn proc_environ_bytes(&self, pid: u32) -> Vec<u8> {
        let entry = self
            .processes
            .get(pid)
            .expect("process must exist while procfs path is resolved");
        null_separated_bytes(
            entry
                .env
                .into_iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect(),
        )
    }

    fn proc_stat_bytes(&self, pid: u32) -> Vec<u8> {
        let entry = self
            .processes
            .get(pid)
            .expect("process must exist while procfs path is resolved");
        let command = entry.command.replace(')', "]");
        let state = match entry.status {
            ProcessStatus::Running => 'R',
            ProcessStatus::Stopped => 'T',
            ProcessStatus::Exited => 'Z',
        };
        format!(
            "{pid} ({command}) {state} {ppid} {pgid} {sid} 0 0 0 0 0 0 0 0 0 0 20 0 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0",
            ppid = entry.ppid,
            pgid = entry.pgid,
            sid = entry.sid,
        )
        .into_bytes()
    }

    fn proc_mounts_bytes(&self) -> Vec<u8> {
        let mounts = if let Some(table) =
            (self.filesystem.inner().inner() as &dyn Any).downcast_ref::<MountTable>()
        {
            table.get_mounts()
        } else {
            vec![MountEntry {
                path: String::from("/"),
                plugin_id: String::from("root"),
                guest_source: String::from("root"),
                guest_fstype: String::from("root"),
                read_only: false,
                access_time: crate::mount_table::AccessTimePolicy::Relatime,
                no_dir_atime: false,
            }]
        };

        mounts
            .into_iter()
            .map(|mount| {
                let options = mount.option_string();
                format!(
                    "{source} {target} {fstype} {options} 0 0\n",
                    source = mount.guest_source,
                    target = mount.path,
                    fstype = mount.guest_fstype,
                )
            })
            .collect::<String>()
            .into_bytes()
    }

    fn proc_cpu_count(&self) -> usize {
        self.resource_limits().virtual_cpu_count.unwrap_or(1)
    }

    fn proc_cpuinfo_bytes(&self) -> Vec<u8> {
        let mut body = String::new();
        for processor in 0..self.proc_cpu_count() {
            body.push_str(&format!(
                "processor\t: {processor}\nmodel name\t: agentos Virtual CPU\ncpu MHz\t\t: 1000.000\nsiblings\t: 1\ncpu cores\t: 1\n\n"
            ));
        }
        body.into_bytes()
    }

    fn proc_mem_total_bytes(&self) -> u64 {
        self.resource_limits()
            .max_wasm_memory_bytes
            .or(self.resource_limits().max_filesystem_bytes)
            .unwrap_or(DEFAULT_MAX_OPEN_FDS as u64 * 1024 * 1024)
    }

    fn proc_meminfo_bytes(&self) -> Vec<u8> {
        let total_kb = self.proc_mem_total_bytes().div_ceil(1024);
        let zero_kb = 0;
        format!(
            "MemTotal:{total_kb:>8} kB\nMemFree:{total_kb:>9} kB\nMemAvailable:{total_kb:>4} kB\nBuffers:{zero_kb:>9} kB\nCached:{zero_kb:>10} kB\n"
        )
        .into_bytes()
    }

    fn proc_loadavg_bytes(&self) -> Vec<u8> {
        let processes = self.processes.list_processes();
        let running = processes
            .values()
            .filter(|process| process.status == ProcessStatus::Running)
            .count();
        let total = processes.len().max(1);
        let last_pid = processes.keys().next_back().copied().unwrap_or(0);
        format!("0.00 0.00 0.00 {running}/{total} {last_pid}\n").into_bytes()
    }

    fn proc_uptime_bytes(&self) -> Vec<u8> {
        let uptime = self.boot_instant.elapsed().as_secs_f64();
        format!("{uptime:.2} {uptime:.2}\n").into_bytes()
    }

    fn proc_version_bytes(&self) -> Vec<u8> {
        format!(
            "Linux version 6.8.0-agentos (agentos@localhost) #1 SMP boot={}\n",
            self.boot_time_ms
        )
        .into_bytes()
    }

    fn proc_status_bytes(&self, pid: u32) -> Vec<u8> {
        let entry = self
            .processes
            .get(pid)
            .expect("process must exist while procfs path is resolved");
        let (state_code, state_name) = match entry.status {
            ProcessStatus::Running => ('R', "running"),
            ProcessStatus::Stopped => ('T', "stopped"),
            ProcessStatus::Exited => ('Z', "zombie"),
        };
        format!(
            "Name:\t{name}\nState:\t{state_code} ({state_name})\nPid:\t{pid}\nPPid:\t{ppid}\nUid:\t{uid}\t{euid}\t{euid}\t{euid}\nGid:\t{gid}\t{egid}\t{egid}\t{egid}\nVmSize:\t{:>8} kB\nVmRSS:\t{:>9} kB\nThreads:\t1\n",
            0,
            0,
            name = entry.command,
            ppid = entry.ppid,
            uid = entry.identity.uid,
            euid = entry.identity.euid,
            gid = entry.identity.gid,
            egid = entry.identity.egid,
        )
        .into_bytes()
    }

    fn proc_read_file_from_open_path(
        &mut self,
        current_pid: Option<u32>,
        path: &str,
    ) -> KernelResult<Vec<u8>> {
        let node = self
            .resolve_proc_node(path, current_pid)?
            .ok_or_else(|| proc_not_found_error(path))?;
        self.proc_read_file(current_pid, &node)
    }

    fn proc_stat_from_open_path(
        &mut self,
        current_pid: Option<u32>,
        path: &str,
    ) -> KernelResult<VirtualStat> {
        let node = self
            .resolve_proc_node(path, current_pid)?
            .ok_or_else(|| proc_not_found_error(path))?;
        self.proc_stat(current_pid, &node)
    }

    fn filesystem_usage(&mut self) -> KernelResult<FileSystemUsage> {
        if let Some(linked_usage) = self.filesystem_usage_cache.clone() {
            return Ok(FileSystemUsage {
                total_bytes: linked_usage
                    .total_bytes
                    .saturating_add(self.anonymous_file_usage.bytes()),
                inode_count: linked_usage
                    .inode_count
                    .saturating_add(self.anonymous_file_usage.inodes()),
            });
        }
        let filesystem = self.raw_filesystem_mut();
        let filesystem_any = filesystem as &mut dyn Any;
        let linked_usage = if let Some(mount_table) = filesystem_any.downcast_mut::<MountTable>() {
            mount_table.root_usage()?
        } else {
            measure_filesystem_usage(filesystem)?
        };
        self.filesystem_usage_cache = Some(linked_usage.clone());
        Ok(FileSystemUsage {
            total_bytes: linked_usage
                .total_bytes
                .saturating_add(self.anonymous_file_usage.bytes()),
            inode_count: linked_usage
                .inode_count
                .saturating_add(self.anonymous_file_usage.inodes()),
        })
    }

    fn invalidate_filesystem_usage_cache(&mut self) {
        self.filesystem_usage_cache = None;
    }

    fn path_uses_root_filesystem(&mut self, path: &str) -> bool {
        let filesystem = self.raw_filesystem_mut();
        let filesystem_any = filesystem as &mut dyn Any;
        filesystem_any
            .downcast_mut::<MountTable>()
            .is_none_or(|mount_table| mount_table.path_uses_root_filesystem(path))
    }

    fn update_filesystem_usage_cache_for_resize(
        &mut self,
        path: &str,
        old_size: u64,
        new_size: u64,
    ) {
        if !self.path_uses_root_filesystem(path) {
            return;
        }
        if let Some(usage) = self.filesystem_usage_cache.as_mut() {
            usage.total_bytes = usage
                .total_bytes
                .saturating_sub(old_size)
                .saturating_add(new_size);
        }
    }

    fn update_filesystem_usage_cache_for_write(
        &mut self,
        path: &str,
        existing: Option<&VirtualStat>,
        new_size: u64,
    ) {
        if is_storage_directory(existing) {
            return;
        }

        if let Some(stat) = existing {
            self.update_filesystem_usage_cache_for_resize(path, stat.size, new_size);
        } else {
            self.update_filesystem_usage_cache_for_inode_create(path, new_size);
        }
    }

    fn update_filesystem_usage_cache_for_inode_create(&mut self, path: &str, size: u64) {
        if !self.path_uses_root_filesystem(path) {
            return;
        }
        if let Some(usage) = self.filesystem_usage_cache.as_mut() {
            usage.total_bytes = usage.total_bytes.saturating_add(size);
            usage.inode_count = usage.inode_count.saturating_add(1);
        }
    }

    fn update_filesystem_usage_cache_for_inode_creates(&mut self, path: &str, count: usize) {
        if count == 0 {
            return;
        }
        if !self.path_uses_root_filesystem(path) {
            return;
        }
        if let Some(usage) = self.filesystem_usage_cache.as_mut() {
            usage.inode_count = usage.inode_count.saturating_add(count);
        }
    }

    fn update_filesystem_usage_cache_for_inode_delete(&mut self, path: &str, size: u64) {
        if !self.path_uses_root_filesystem(path) {
            return;
        }
        if let Some(usage) = self.filesystem_usage_cache.as_mut() {
            usage.total_bytes = usage.total_bytes.saturating_sub(size);
            usage.inode_count = usage.inode_count.saturating_sub(1);
        }
    }

    fn update_filesystem_usage_cache_for_remove(
        &mut self,
        path: &str,
        removed: Option<&VirtualStat>,
    ) {
        let Some(stat) = removed else {
            return;
        };
        if stat.is_directory || stat.nlink > 1 {
            return;
        }
        self.update_filesystem_usage_cache_for_inode_delete(path, stat.size);
    }

    fn storage_stat(&mut self, path: &str) -> KernelResult<Option<VirtualStat>> {
        if is_virtual_device_storage_path(path) {
            return Ok(None);
        }

        match self.raw_filesystem_mut().stat(path) {
            Ok(stat) => Ok(Some(stat)),
            Err(error) if error.code() == "ENOENT" => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn storage_lstat(&mut self, path: &str) -> KernelResult<Option<VirtualStat>> {
        if is_virtual_device_storage_path(path) {
            return Ok(None);
        }

        match self.raw_filesystem_mut().lstat(path) {
            Ok(stat) => Ok(Some(stat)),
            Err(error) if error.code() == "ENOENT" => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn current_storage_file_size(&mut self, path: &str) -> KernelResult<u64> {
        Ok(self
            .storage_stat(path)?
            .filter(|stat| !stat.is_directory)
            .map(|stat| stat.size)
            .unwrap_or(0))
    }

    fn check_dac_traversal(&mut self, pid: u32, path: &str) -> KernelResult<()> {
        if is_proc_path(path) {
            return Ok(());
        }
        let identity = self
            .processes
            .get(pid)
            .ok_or_else(|| KernelError::no_such_process(pid))?
            .identity;
        let normalized = normalize_path(path);
        let components = normalized
            .split('/')
            .filter(|component| !component.is_empty())
            .collect::<Vec<_>>();
        let mut current = String::from("/");
        for component in components.iter().take(components.len().saturating_sub(1)) {
            current = join_child_path(&current, component);
            let stat = self.filesystem.stat(&current)?;
            if !stat.is_directory {
                return Err(KernelError::new(
                    "ENOTDIR",
                    format!("path component is not a directory: {current}"),
                ));
            }
            self.check_dac_mode_with_acl(&identity, &stat, DAC_EXECUTE, &current)?;
        }
        Ok(())
    }

    fn check_dac_access(&mut self, pid: u32, path: &str, access: u32) -> KernelResult<()> {
        if is_proc_path(path) {
            return Ok(());
        }
        self.check_dac_traversal(pid, path)?;
        if access == 0 {
            return Ok(());
        }
        let identity = self
            .processes
            .get(pid)
            .ok_or_else(|| KernelError::no_such_process(pid))?
            .identity;
        let stat = self.filesystem.stat(path)?;
        self.check_dac_mode_with_acl(&identity, &stat, access, path)
    }

    fn check_dac_mode_with_acl(
        &mut self,
        identity: &ProcessIdentity,
        stat: &VirtualStat,
        access: u32,
        path: &str,
    ) -> KernelResult<()> {
        // POSIX ACL USER_OBJ permissions are mirrored in the inode's owner mode
        // bits. Named ACL entries and the ACL mask never override the file
        // owner's class, so reading the ACL xattr for the owner is redundant.
        // Avoiding that second filesystem walk matters for host-mounted trees,
        // where runtimes perform thousands of metadata probes during startup.
        if identity.euid == 0 || identity.euid == stat.uid {
            return check_dac_mode(identity, stat, access, path);
        }
        let cache_key = (
            stat.dev,
            stat.ino,
            stat.ctime_ms,
            stat.ctime_nsec,
            stat.mode,
            stat.uid,
            stat.gid,
        );
        if self.no_posix_acl_cache.contains(&cache_key) {
            return check_dac_mode(identity, stat, access, path);
        }
        match self.read_posix_acl(path, POSIX_ACL_ACCESS)? {
            Some(acl) => acl.check_access(identity, stat, access, path),
            None => {
                // Most executable/package trees have no POSIX ACL. Cache that
                // negative lookup by inode metadata so repeated traversal does
                // not turn every stat into a second filesystem/database query.
                // Mutating xattr/ownership/mode operations clear the cache, and
                // externally changed inodes naturally produce a different key.
                if self.no_posix_acl_cache.len() >= 4_096 {
                    self.no_posix_acl_cache.clear();
                }
                self.no_posix_acl_cache.insert(cache_key);
                check_dac_mode(identity, stat, access, path)
            }
        }
    }

    fn read_posix_acl(&mut self, path: &str, name: &str) -> KernelResult<Option<PosixAcl>> {
        match self.filesystem.get_xattr(path, name, true) {
            Ok(value) => PosixAcl::parse(&value, path).map(Some),
            Err(error) if matches!(error.code(), "ENODATA" | "EOPNOTSUPP") => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn sync_access_acl_mode(&mut self, path: &str, mode: u32) -> KernelResult<()> {
        let Some(mut acl) = self.read_posix_acl(path, POSIX_ACL_ACCESS)? else {
            return Ok(());
        };
        acl.apply_mode(mode);
        self.filesystem
            .set_xattr(path, POSIX_ACL_ACCESS, acl.encode(), 2, true)?;
        Ok(())
    }

    fn check_dac_parent_access(&mut self, pid: u32, path: &str, access: u32) -> KernelResult<()> {
        let mut parent = parent_path(path);
        loop {
            match self.check_dac_access(pid, &parent, access) {
                Err(error) if error.code() == "ENOENT" && parent != "/" => {
                    parent = parent_path(&parent);
                }
                result => return result,
            }
        }
    }

    fn check_sticky_directory_removal(&mut self, pid: u32, path: &str) -> KernelResult<()> {
        let identity = self
            .processes
            .get(pid)
            .ok_or_else(|| KernelError::no_such_process(pid))?
            .identity;
        if identity.euid == 0 {
            return Ok(());
        }
        let parent = self.filesystem.stat(&parent_path(path))?;
        if parent.mode & 0o1000 == 0 || identity.euid == parent.uid {
            return Ok(());
        }
        let target = self.filesystem.lstat(path)?;
        if identity.euid == target.uid {
            Ok(())
        } else {
            Err(KernelError::new(
                "EPERM",
                format!("sticky directory prevents removing {path}"),
            ))
        }
    }

    fn apply_process_creation_metadata(
        &mut self,
        pid: u32,
        path: &str,
        mode: u32,
        umask: u32,
        is_directory: bool,
    ) -> KernelResult<()> {
        let identity = self
            .processes
            .get(pid)
            .ok_or_else(|| KernelError::no_such_process(pid))?
            .identity;
        let parent_path = parent_path(path);
        let parent = self.filesystem.stat(&parent_path).map_err(|error| {
            KernelError::new(
                error.code(),
                format!("creation parent stat for '{parent_path}' failed: {error}"),
            )
        })?;
        let inherit_setgid = parent.mode & 0o2000 != 0;
        let gid = if inherit_setgid {
            parent.gid
        } else {
            identity.egid
        };
        self.filesystem
            .chown(path, identity.euid, gid)
            .map_err(|error| {
                KernelError::new(
                    error.code(),
                    format!("creation ownership for '{path}' failed: {error}"),
                )
            })?;
        let inherited_acl = self.read_posix_acl(&parent_path, POSIX_ACL_DEFAULT)?;
        let mut masked_mode = if let Some(acl) = inherited_acl.as_ref() {
            acl.restrict_to_mode(mode).mode(mode)
        } else {
            (mode & !0o777) | ((mode & 0o777) & !(umask & 0o777))
        };
        if is_directory && inherit_setgid {
            masked_mode |= 0o2000;
        }
        self.filesystem.chmod(path, masked_mode).map_err(|error| {
            KernelError::new(
                error.code(),
                format!("creation mode for '{path}' failed: {error}"),
            )
        })?;
        if let Some(default_acl) = inherited_acl {
            let access_acl = default_acl.restrict_to_mode(mode);
            self.filesystem
                .set_xattr(path, POSIX_ACL_ACCESS, access_acl.encode(), 0, true)?;
            if is_directory {
                self.filesystem.set_xattr(
                    path,
                    POSIX_ACL_DEFAULT,
                    default_acl.encode(),
                    0,
                    true,
                )?;
            }
        }
        Ok(())
    }

    fn clear_setid_after_write(&mut self, pid: u32, path: &str) -> KernelResult<()> {
        let identity = self
            .processes
            .get(pid)
            .ok_or_else(|| KernelError::no_such_process(pid))?
            .identity;
        if identity.euid == 0 {
            return Ok(());
        }
        let stat = self.filesystem.stat(path)?;
        if stat.mode & 0o6000 != 0 {
            self.filesystem.chmod(path, stat.mode & !0o6000)?;
        }
        Ok(())
    }

    fn missing_directory_paths(
        &mut self,
        path: &str,
        recursive: bool,
    ) -> KernelResult<Vec<String>> {
        let normalized = normalize_path(path);
        if normalized == "/" {
            return Ok(Vec::new());
        }

        if !recursive {
            return Ok(if self.storage_lstat(&normalized)?.is_none() {
                vec![normalized]
            } else {
                Vec::new()
            });
        }

        let mut created = Vec::new();
        let mut current = String::from("/");
        for component in normalized
            .split('/')
            .filter(|component| !component.is_empty())
        {
            current = if current == "/" {
                format!("/{component}")
            } else {
                format!("{current}/{component}")
            };
            if self.storage_lstat(&current)?.is_none() {
                created.push(current.clone());
            }
        }
        Ok(created)
    }

    fn check_write_file_limits(&mut self, path: &str, new_size: u64) -> KernelResult<()> {
        let existing = self.storage_stat(path)?;
        self.check_write_file_limits_with_existing(path, existing.as_ref(), new_size)
    }

    fn check_write_file_limits_with_existing(
        &mut self,
        path: &str,
        existing: Option<&VirtualStat>,
        new_size: u64,
    ) -> KernelResult<()> {
        if is_virtual_device_storage_path(path) {
            return Ok(());
        }

        if let Some(existing) = existing {
            if is_storage_directory(Some(existing)) {
                return Ok(());
            }
            if new_size <= existing.size {
                return Ok(());
            }

            let usage = self.filesystem_usage()?;
            self.resources.check_filesystem_usage(
                &usage,
                usage
                    .total_bytes
                    .saturating_sub(existing.size)
                    .saturating_add(new_size),
                usage.inode_count,
            )?;
            return Ok(());
        }

        let usage = self.filesystem_usage()?;
        self.resources.check_filesystem_usage(
            &usage,
            usage.total_bytes.saturating_add(new_size),
            usage.inode_count.saturating_add(1),
        )?;
        Ok(())
    }

    fn check_create_dir_limits(&mut self, path: &str) -> KernelResult<()> {
        if is_virtual_device_storage_path(path) || self.storage_lstat(path)?.is_some() {
            return Ok(());
        }

        let parent = parent_path(path);
        let Some(parent_stat) = self.storage_stat(&parent)? else {
            return Ok(());
        };
        if !parent_stat.is_directory {
            return Ok(());
        }

        let usage = self.filesystem_usage()?;
        self.resources.check_filesystem_usage(
            &usage,
            usage.total_bytes,
            usage.inode_count.saturating_add(1),
        )?;
        Ok(())
    }

    fn check_mkdir_limits(&mut self, path: &str, recursive: bool) -> KernelResult<()> {
        if is_virtual_device_storage_path(path) {
            return Ok(());
        }

        if !recursive {
            return self.check_create_dir_limits(path);
        }

        let usage = self.filesystem_usage()?;
        let new_inodes = count_missing_directory_components(self.raw_filesystem_mut(), path, true)?;
        self.resources.check_filesystem_usage(
            &usage,
            usage.total_bytes,
            usage.inode_count.saturating_add(new_inodes),
        )?;
        Ok(())
    }

    fn check_symlink_limits(&mut self, target: &str, link_path: &str) -> KernelResult<()> {
        if is_virtual_device_storage_path(link_path) || self.storage_lstat(link_path)?.is_some() {
            return Ok(());
        }

        let parent = parent_path(link_path);
        let Some(parent_stat) = self.storage_stat(&parent)? else {
            return Ok(());
        };
        if !parent_stat.is_directory {
            return Ok(());
        }

        let usage = self.filesystem_usage()?;
        self.resources.check_filesystem_usage(
            &usage,
            usage.total_bytes.saturating_add(target.len() as u64),
            usage.inode_count.saturating_add(1),
        )?;
        Ok(())
    }

    fn check_truncate_limits_with_existing(
        &mut self,
        path: &str,
        existing: Option<&VirtualStat>,
        length: u64,
    ) -> KernelResult<()> {
        if is_virtual_device_storage_path(path) {
            return Ok(());
        }

        let Some(existing) = existing else {
            return Ok(());
        };
        if is_storage_directory(Some(existing)) {
            return Ok(());
        }
        self.check_path_resize_limits_with_existing(existing.size, length)
    }

    fn check_rename_copy_up_limits(&mut self, old_path: &str, new_path: &str) -> KernelResult<()> {
        let max_bytes = self.resource_limits().max_filesystem_bytes;
        let max_inodes = self.resource_limits().max_inode_count;
        let filesystem_any = self.raw_filesystem_mut() as &mut dyn Any;

        if let Some(root) = filesystem_any.downcast_mut::<RootFileSystem>() {
            root.check_rename_copy_up_limits(old_path, new_path, max_bytes, max_inodes)?;
            return Ok(());
        }

        if let Some(mount_table) = filesystem_any.downcast_mut::<MountTable>() {
            mount_table.check_rename_copy_up_limits(old_path, new_path, max_bytes, max_inodes)?;
        }

        Ok(())
    }

    fn check_path_resize_limits_with_existing(
        &mut self,
        existing_size: u64,
        new_size: u64,
    ) -> KernelResult<()> {
        if new_size <= existing_size {
            return Ok(());
        }

        let usage = self.filesystem_usage()?;
        self.resources.check_filesystem_usage(
            &usage,
            usage
                .total_bytes
                .saturating_sub(existing_size)
                .saturating_add(new_size),
            usage.inode_count,
        )?;
        Ok(())
    }

    fn blocking_read_timeout(&self) -> Option<Duration> {
        self.resources
            .limits()
            .max_blocking_read_ms
            .map(Duration::from_millis)
    }

    fn close_special_resource_if_needed(&self, description: &Arc<FileDescription>, filetype: u8) {
        close_special_resource_if_needed(
            &self.file_locks,
            &self.pipes,
            &self.ptys,
            &self.sockets,
            &self.fd_sockets,
            description,
            filetype,
        );
    }

    fn cleanup_unnamed_file_if_closed(
        &mut self,
        description: &Arc<FileDescription>,
    ) -> KernelResult<()> {
        if description.ref_count() != 0 {
            return Ok(());
        }
        let Some(unnamed) = self.unnamed_files.get(&description.id()).cloned() else {
            return Ok(());
        };
        match self.filesystem.remove_file(&unnamed.path) {
            Ok(()) => {
                self.unnamed_files.remove(&description.id());
                self.invalidate_filesystem_usage_cache();
                Ok(())
            }
            Err(error) if error.code() == "ENOENT" => {
                self.unnamed_files.remove(&description.id());
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }
}

impl KernelVm<MountTable> {
    fn check_mount_permissions(&self, path: &str) -> KernelResult<()> {
        self.filesystem
            .check_path(FsOperation::Write, path)
            .map_err(KernelError::from)?;
        if is_sensitive_mount_path(path) {
            self.filesystem
                .check_path(FsOperation::MountSensitive, path)
                .map_err(KernelError::from)?;
        }
        Ok(())
    }

    pub fn mount_filesystem(
        &mut self,
        path: &str,
        filesystem: impl VirtualFileSystem + 'static,
        options: MountOptions,
    ) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.check_mount_permissions(path)?;
        self.filesystem
            .inner_mut()
            .inner_mut()
            .mount(path, filesystem, options)
            .map_err(KernelError::from)?;
        self.invalidate_filesystem_usage_cache();
        Ok(())
    }

    pub fn mount_boxed_filesystem(
        &mut self,
        path: &str,
        filesystem: Box<dyn MountedFileSystem>,
        options: MountOptions,
    ) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.check_mount_permissions(path)?;
        self.filesystem
            .inner_mut()
            .inner_mut()
            .mount_boxed(path, filesystem, options)
            .map_err(KernelError::from)?;
        self.invalidate_filesystem_usage_cache();
        Ok(())
    }

    pub fn unmount_filesystem(&mut self, path: &str) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.check_mount_permissions(path)?;
        self.filesystem
            .inner_mut()
            .inner_mut()
            .unmount(path)
            .map_err(KernelError::from)?;
        self.invalidate_filesystem_usage_cache();
        Ok(())
    }

    pub fn remount_filesystem_for_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        path: &str,
        options: &str,
    ) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        if self.process_identity(requester_driver, pid)?.euid != 0 {
            return Err(KernelError::new(
                "EPERM",
                "remount requires effective uid 0",
            ));
        }
        self.check_mount_permissions(path)?;
        self.filesystem
            .inner_mut()
            .inner_mut()
            .remount(path, options)
            .map_err(KernelError::from)
    }

    pub fn mounted_filesystems(&self) -> Vec<MountEntry> {
        self.filesystem.inner().inner().get_mounts()
    }

    pub fn root_filesystem_mut(&mut self) -> Option<&mut RootFileSystem> {
        self.filesystem
            .inner_mut()
            .inner_mut()
            .root_virtual_filesystem_mut::<RootFileSystem>()
    }

    pub fn snapshot_root_filesystem(&mut self) -> KernelResult<RootFilesystemSnapshot> {
        let usage = self.filesystem_usage()?;
        self.resources
            .check_filesystem_usage(&usage, usage.total_bytes, usage.inode_count)?;
        let root = self
            .root_filesystem_mut()
            .ok_or_else(|| KernelError::new("EINVAL", "native root filesystem is not available"))?;
        root.snapshot().map_err(KernelError::from)
    }

    /// Snapshot the root filesystem without allowing caller-selected export
    /// work to materialize or return more than `max_bytes`. Raw content usage
    /// is checked before traversal; the encoded snapshot is checked before it
    /// can leave the kernel.
    pub fn snapshot_root_filesystem_bounded(
        &mut self,
        max_bytes: u64,
    ) -> KernelResult<RootFilesystemSnapshot> {
        if max_bytes == 0 {
            return Err(KernelError::new(
                "EINVAL",
                "maxBytes must be greater than zero",
            ));
        }
        let usage = self.filesystem_usage()?;
        self.resources
            .check_filesystem_usage(&usage, usage.total_bytes, usage.inode_count)?;
        if usage.total_bytes > max_bytes {
            return Err(KernelError::new(
                "EFBIG",
                format!(
                    "root filesystem export exceeds maxBytes: {} content bytes > {max_bytes}; raise maxBytes",
                    usage.total_bytes
                ),
            ));
        }
        let root = self
            .root_filesystem_mut()
            .ok_or_else(|| KernelError::new("EINVAL", "native root filesystem is not available"))?;
        let snapshot = root.snapshot().map_err(KernelError::from)?;
        let encoded_len = encode_snapshot(&snapshot).map_err(KernelError::from)?.len();
        if u64::try_from(encoded_len).unwrap_or(u64::MAX) > max_bytes {
            return Err(KernelError::new(
                "EFBIG",
                format!(
                    "root filesystem export exceeds maxBytes: {encoded_len} encoded bytes > {max_bytes}; raise maxBytes"
                ),
            ));
        }
        Ok(snapshot)
    }
}

#[derive(Default)]
struct StubDriverState {
    exit_code: Option<i32>,
    on_exit: Option<ProcessExitCallback>,
    kill_signals: Vec<i32>,
}

#[derive(Default)]
struct StubDriverProcess {
    state: Mutex<StubDriverState>,
    waiters: Condvar,
}

impl StubDriverProcess {
    fn finish(&self, exit_code: i32) {
        let callback = {
            let mut state = lock_or_recover(&self.state);
            if state.exit_code.is_some() {
                return;
            }
            state.exit_code = Some(exit_code);
            self.waiters.notify_all();
            state.on_exit.clone()
        };

        if let Some(callback) = callback {
            callback(exit_code);
        }
    }

    fn kill_signals(&self) -> Vec<i32> {
        lock_or_recover(&self.state).kill_signals.clone()
    }
}

impl DriverProcess for StubDriverProcess {
    fn kill(&self, signal: i32) {
        {
            let mut state = lock_or_recover(&self.state);
            state.kill_signals.push(signal);
        }
        if matches!(
            signal,
            crate::process_table::SIGCHLD | SIGCONT | SIGSTOP | SIGTSTP | SIGWINCH
        ) {
            return;
        }
        self.finish(128 + signal);
    }

    fn wait(&self, timeout: Duration) -> Option<i32> {
        let state = lock_or_recover(&self.state);
        if let Some(code) = state.exit_code {
            return Some(code);
        }

        let (state, _) = wait_timeout_or_recover(&self.waiters, state, timeout);
        state.exit_code
    }

    fn set_on_exit(&self, callback: ProcessExitCallback) {
        let maybe_exit = {
            let mut state = lock_or_recover(&self.state);
            state.on_exit = Some(callback.clone());
            state.exit_code
        };

        if let Some(code) = maybe_exit {
            callback(code);
        }
    }
}

fn unix_socket_absolute_components(
    cwd: &str,
    path: &str,
) -> KernelResult<(String, VecDeque<String>, bool)> {
    if path.is_empty() {
        return Err(KernelError::new(
            "ENOENT",
            "Unix socket pathname must not be empty",
        ));
    }
    if path.as_bytes().contains(&0) {
        return Err(KernelError::new(
            "EINVAL",
            "Unix socket pathname contains a NUL byte",
        ));
    }
    // Linux copies at most PATH_MAX bytes from the caller, including the
    // terminating NUL. Check the raw pathname before any `..` processing so a
    // long spelling cannot become valid merely by normalizing shorter.
    if path.len() >= MAX_PATH_LENGTH {
        return Err(KernelError::new(
            "ENAMETOOLONG",
            format!(
                "Unix socket pathname is {} bytes; Linux permits at most {}",
                path.len(),
                MAX_PATH_LENGTH - 1
            ),
        ));
    }
    if !path.starts_with('/') && !cwd.starts_with('/') {
        return Err(KernelError::new(
            "EINVAL",
            format!("Unix socket cwd must be absolute: {cwd}"),
        ));
    }

    let absolute = if path.starts_with('/') {
        path.to_owned()
    } else if cwd == "/" {
        format!("/{path}")
    } else {
        format!("{}/{path}", cwd.trim_end_matches('/'))
    };
    let trailing_slash = absolute.len() > 1 && absolute.ends_with('/');
    let components = absolute
        .split('/')
        .filter(|component| !component.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    Ok((absolute, components, trailing_slash))
}

fn resolve_unix_socket_components<F: VirtualFileSystem>(
    filesystem: &mut F,
    identity: &ProcessIdentity,
    mut remaining: VecDeque<String>,
    follow_final_symlink: bool,
    mut require_final_directory: bool,
) -> KernelResult<UnixSocketPathNode> {
    let mut resolved = Vec::<String>::new();
    let mut followed_symlinks = 0usize;

    if remaining.is_empty() {
        let stat = filesystem.stat("/")?;
        if require_final_directory && !stat.is_directory {
            return Err(KernelError::new("ENOTDIR", "root is not a directory"));
        }
        return Ok(UnixSocketPathNode {
            canonical_path: String::from("/"),
            stat,
        });
    }

    while let Some(component) = remaining.pop_front() {
        let current_path = unix_socket_components_path(&resolved);
        let current_stat = filesystem.stat(&current_path)?;
        if !current_stat.is_directory {
            return Err(KernelError::new(
                "ENOTDIR",
                format!("not a directory while resolving Unix socket path: {current_path}"),
            ));
        }
        check_unix_dac(
            identity,
            &current_stat,
            UNIX_DAC_SEARCH,
            "search",
            &current_path,
        )?;

        match component.as_str() {
            "." => continue,
            ".." => {
                resolved.pop();
                continue;
            }
            _ => {}
        }

        let candidate = join_absolute_path(&current_path, &component);
        let stat = filesystem.lstat(&candidate)?;
        let is_final = remaining.is_empty();
        if stat.is_symbolic_link && (!is_final || follow_final_symlink) {
            followed_symlinks = followed_symlinks.saturating_add(1);
            if followed_symlinks > MAX_UNIX_SOCKET_SYMLINKS {
                return Err(KernelError::new(
                    "ELOOP",
                    format!("too many symbolic links while resolving '{candidate}'"),
                ));
            }
            let target = filesystem.read_link(&candidate)?;
            if target.is_empty() {
                return Err(KernelError::new(
                    "ENOENT",
                    format!("empty symbolic link target while resolving '{candidate}'"),
                ));
            }
            if target.starts_with('/') {
                resolved.clear();
            }
            // A slash at the end of a symlink target carries the same
            // directory requirement as a slash in the caller's pathname.
            // Preserve it when the link supplies the final component.
            if target.len() > 1 && target.ends_with('/') && remaining.is_empty() {
                require_final_directory = true;
            }
            let target_components = target
                .split('/')
                .filter(|target_component| !target_component.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>();
            for target_component in target_components.into_iter().rev() {
                remaining.push_front(target_component);
            }
            continue;
        }

        if is_final {
            if require_final_directory && !stat.is_directory {
                return Err(KernelError::new(
                    "ENOTDIR",
                    format!("not a directory while resolving Unix socket path: {candidate}"),
                ));
            }
            return Ok(UnixSocketPathNode {
                canonical_path: candidate,
                stat,
            });
        }
        if !stat.is_directory {
            return Err(KernelError::new(
                "ENOTDIR",
                format!("not a directory while resolving Unix socket path: {candidate}"),
            ));
        }
        resolved.push(component);
    }

    let canonical_path = unix_socket_components_path(&resolved);
    let stat = filesystem.stat(&canonical_path)?;
    if require_final_directory && !stat.is_directory {
        return Err(KernelError::new(
            "ENOTDIR",
            format!("not a directory while resolving Unix socket path: {canonical_path}"),
        ));
    }
    Ok(UnixSocketPathNode {
        canonical_path,
        stat,
    })
}

fn unix_socket_components_path(components: &[String]) -> String {
    if components.is_empty() {
        String::from("/")
    } else {
        format!("/{}", components.join("/"))
    }
}

fn check_unix_dac(
    identity: &ProcessIdentity,
    stat: &VirtualStat,
    requested: u32,
    operation: &str,
    path: &str,
) -> KernelResult<()> {
    // AgentOS has no fsuid/fsgid or capability mutation. Match Linux's
    // ordinary case with euid/egid, and model uid 0 as CAP_DAC_OVERRIDE for
    // the write/search checks used by AF_UNIX pathname operations.
    if identity.euid == 0 {
        return Ok(());
    }
    let shift = if identity.euid == stat.uid {
        6
    } else if identity.egid == stat.gid || identity.supplementary_gids.contains(&stat.gid) {
        3
    } else {
        0
    };
    let granted = (stat.mode >> shift) & 0o7;
    if granted & requested == requested {
        Ok(())
    } else {
        Err(KernelError::new(
            "EACCES",
            format!("permission denied, {operation} Unix socket path '{path}'"),
        ))
    }
}

fn validate_chown_request(
    identity: &ProcessIdentity,
    stat: &VirtualStat,
    requested_uid: u32,
    requested_gid: u32,
    subject: &str,
) -> KernelResult<(u32, u32)> {
    const UNCHANGED_ID: u32 = u32::MAX;
    let next_uid = if requested_uid == UNCHANGED_ID {
        stat.uid
    } else {
        requested_uid
    };
    let next_gid = if requested_gid == UNCHANGED_ID {
        stat.gid
    } else {
        requested_gid
    };

    if identity.euid == 0 || (requested_uid == UNCHANGED_ID && requested_gid == UNCHANGED_ID) {
        return Ok((next_uid, next_gid));
    }
    if identity.euid != stat.uid {
        return Err(KernelError::new(
            "EPERM",
            format!("operation not permitted, process does not own '{subject}'"),
        ));
    }
    if requested_uid != UNCHANGED_ID && requested_uid != stat.uid {
        return Err(KernelError::new(
            "EPERM",
            format!("operation not permitted, cannot change owner of '{subject}'"),
        ));
    }
    if requested_gid != UNCHANGED_ID
        && requested_gid != identity.egid
        && !identity.supplementary_gids.contains(&requested_gid)
    {
        return Err(KernelError::new(
            "EPERM",
            format!(
                "operation not permitted, gid {requested_gid} is not a process group for '{subject}'"
            ),
        ));
    }
    Ok((next_uid, next_gid))
}

/// Linux clears S_ISUID on a regular file after chown, but preserves S_ISGID
/// when the group-execute bit is clear because that combination represents
/// mandatory-locking metadata rather than set-group-ID execution.
fn linux_chown_cleared_mode(stat: &VirtualStat) -> Option<u32> {
    if stat.mode & 0o170000 != 0o100000 {
        return None;
    }
    let mut mode = stat.mode & !0o4000;
    if stat.mode & 0o0010 != 0 {
        mode &= !0o2000;
    }
    (mode != stat.mode).then_some(mode)
}

fn unix_socket_address_in_use(path: &str) -> KernelError {
    KernelError::new(
        "EADDRINUSE",
        format!("Unix socket pathname is already in use: {path}"),
    )
}

impl From<VfsError> for KernelError {
    fn from(error: VfsError) -> Self {
        map_error(error.code(), error.to_string())
    }
}

fn lock_or_recover<'a, T>(mutex: &'a Mutex<T>) -> MutexGuard<'a, T> {
    match mutex.lock() {
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

fn is_sensitive_mount_path(path: &str) -> bool {
    let normalized = crate::vfs::normalize_path(path);
    normalized == "/"
        || normalized == "/etc"
        || normalized.starts_with("/etc/")
        || normalized == "/proc"
        || normalized.starts_with("/proc/")
}

impl From<FdTableError> for KernelError {
    fn from(error: FdTableError) -> Self {
        map_error(error.code(), error.to_string())
    }
}

impl From<PipeError> for KernelError {
    fn from(error: PipeError) -> Self {
        map_error(error.code(), error.to_string())
    }
}

impl From<PtyError> for KernelError {
    fn from(error: PtyError) -> Self {
        map_error(error.code(), error.to_string())
    }
}

impl From<ProcessTableError> for KernelError {
    fn from(error: ProcessTableError) -> Self {
        map_error(error.code(), error.to_string())
    }
}

impl From<PermissionError> for KernelError {
    fn from(error: PermissionError) -> Self {
        map_error(error.code(), error.to_string())
    }
}

impl From<ResourceError> for KernelError {
    fn from(error: ResourceError) -> Self {
        map_error(error.code(), error.to_string())
    }
}

impl From<SocketTableError> for KernelError {
    fn from(error: SocketTableError) -> Self {
        map_error(error.code(), error.to_string())
    }
}

impl From<RootFilesystemError> for KernelError {
    fn from(error: RootFilesystemError) -> Self {
        map_error("EINVAL", error.to_string())
    }
}

fn map_dns_resolver_error(error: crate::dns::DnsResolverError) -> KernelError {
    let code = match error.kind() {
        DnsResolverErrorKind::InvalidInput => "EINVAL",
        DnsResolverErrorKind::NxDomain => "ENOENT",
        DnsResolverErrorKind::NoData => "ENODATA",
        DnsResolverErrorKind::LookupFailed => "EHOSTUNREACH",
    };
    map_error(code, error.to_string())
}

fn map_error(code: &'static str, message: String) -> KernelError {
    let trimmed = strip_error_prefix(code, &message)
        .map(ToOwned::to_owned)
        .unwrap_or(message);
    KernelError::new(code, trimmed)
}

fn strip_error_prefix<'a>(code: &str, message: &'a str) -> Option<&'a str> {
    let prefix = format!("{code}: ");
    message.strip_prefix(&prefix)
}

fn parse_dev_fd_path(path: &str) -> KernelResult<Option<u32>> {
    let Some(raw_fd) = path.strip_prefix("/dev/fd/") else {
        return Ok(None);
    };
    if raw_fd.is_empty() {
        return Err(KernelError::new(
            "EBADF",
            format!("bad file descriptor: {path}"),
        ));
    }
    let fd = raw_fd
        .parse::<u32>()
        .map_err(|_| KernelError::new("EBADF", format!("bad file descriptor: {path}")))?;
    Ok(Some(fd))
}

fn count_missing_directory_components<F: VirtualFileSystem>(
    filesystem: &mut F,
    path: &str,
    include_final: bool,
) -> VfsResult<usize> {
    let normalized = normalize_path(path);
    let parts = normalized
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    let limit = if include_final {
        parts.len()
    } else {
        parts.len().saturating_sub(1)
    };

    let mut current = String::from("/");
    for (index, part) in parts.iter().take(limit).enumerate() {
        let candidate = if current == "/" {
            format!("/{}", part)
        } else {
            format!("{current}/{}", part)
        };

        match filesystem.stat(&candidate) {
            Ok(stat) => {
                if !stat.is_directory {
                    return Err(VfsError::new(
                        "ENOTDIR",
                        format!("not a directory, mkdir '{candidate}'"),
                    ));
                }
                current = candidate;
            }
            Err(error) if error.code() == "ENOENT" => {
                return Ok(limit.saturating_sub(index));
            }
            Err(error) => return Err(error),
        }
    }

    Ok(0)
}

fn parent_path(path: &str) -> String {
    let normalized = normalize_path(path);
    let Some((head, _)) = normalized.rsplit_once('/') else {
        return String::from("/");
    };

    if head.is_empty() {
        String::from("/")
    } else {
        String::from(head)
    }
}

fn required_dirent_ino(path: &str, ino: u64) -> KernelResult<u64> {
    if ino == 0 {
        return Err(KernelError::new(
            "EIO",
            format!("filesystem returned an invalid zero inode for directory entry {path}"),
        ));
    }
    Ok(ino)
}

fn join_absolute_path(parent: &str, child: &str) -> String {
    if parent == "/" {
        format!("/{child}")
    } else {
        format!("{parent}/{child}")
    }
}

fn join_child_path(parent: &str, child: &str) -> String {
    normalize_path(&join_absolute_path(parent, child))
}

fn is_virtual_device_storage_path(path: &str) -> bool {
    matches!(
        path,
        "/dev/null" | "/dev/zero" | "/dev/stdin" | "/dev/stdout" | "/dev/stderr" | "/dev/urandom"
    ) || path == "/dev"
        || path == "/dev/fd"
        || path == "/dev/pts"
        || path.starts_with("/dev/fd/")
        || path.starts_with("/dev/pts/")
}

fn is_storage_directory(stat: Option<&VirtualStat>) -> bool {
    stat.is_some_and(|stat| stat.is_directory && !stat.is_symbolic_link)
}

fn is_proc_path(path: &str) -> bool {
    let normalized = normalize_path(path);
    normalized == "/proc" || normalized.starts_with("/proc/")
}

fn is_agentos_path(path: &str) -> bool {
    let normalized = normalize_path(path);
    normalized == "/etc/agentos" || normalized.starts_with("/etc/agentos/")
}

fn open_requires_write_access(flags: u32) -> bool {
    flags & (O_CREAT | O_EXCL | O_TRUNC) != 0 || (flags & 0b11) != crate::fd_table::O_RDONLY
}

const DAC_EXECUTE: u32 = 0o1;
const DAC_WRITE: u32 = 0o2;
const DAC_READ: u32 = 0o4;
const POSIX_ACL_ACCESS: &str = "system.posix_acl_access";
const POSIX_ACL_DEFAULT: &str = "system.posix_acl_default";
const POSIX_ACL_XATTR_VERSION: u32 = 2;
const POSIX_ACL_ENTRY_LIMIT: usize = 25;
const XATTR_NAME_MAX: usize = 255;
const ACL_USER_OBJ: u16 = 0x01;
const ACL_USER: u16 = 0x02;
const ACL_GROUP_OBJ: u16 = 0x04;
const ACL_GROUP: u16 = 0x08;
const ACL_MASK: u16 = 0x10;
const ACL_OTHER: u16 = 0x20;
const ACL_UNDEFINED_ID: u32 = u32::MAX;

#[derive(Clone, Debug, PartialEq, Eq)]
struct PosixAclEntry {
    tag: u16,
    perm: u16,
    id: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PosixAcl {
    entries: Vec<PosixAclEntry>,
}

impl PosixAcl {
    fn parse(value: &[u8], path: &str) -> KernelResult<Self> {
        if value.len() < 4 || !(value.len() - 4).is_multiple_of(8) {
            return Err(invalid_acl(path, "invalid xattr length"));
        }
        let entry_count = (value.len() - 4) / 8;
        if entry_count > POSIX_ACL_ENTRY_LIMIT {
            return Err(KernelError::new(
                "E2BIG",
                format!(
                    "POSIX ACL for {path} has {entry_count} entries; limit is {POSIX_ACL_ENTRY_LIMIT}"
                ),
            ));
        }
        let version = u32::from_le_bytes(value[0..4].try_into().expect("four ACL version bytes"));
        if version != POSIX_ACL_XATTR_VERSION {
            return Err(invalid_acl(path, "unsupported xattr version"));
        }
        let entries = value[4..]
            .chunks_exact(8)
            .map(|bytes| PosixAclEntry {
                tag: u16::from_le_bytes([bytes[0], bytes[1]]),
                perm: u16::from_le_bytes([bytes[2], bytes[3]]),
                id: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            })
            .collect::<Vec<_>>();
        let acl = Self { entries };
        acl.validate(path)?;
        Ok(acl)
    }

    fn validate(&self, path: &str) -> KernelResult<()> {
        if self.entries.len() < 3 {
            return Err(invalid_acl(path, "missing required entries"));
        }
        for entry in &self.entries {
            if entry.perm > 0o7 {
                return Err(invalid_acl(path, "permission bits exceed rwx"));
            }
            let named = matches!(entry.tag, ACL_USER | ACL_GROUP);
            if (named && entry.id == ACL_UNDEFINED_ID) || (!named && entry.id != ACL_UNDEFINED_ID) {
                return Err(invalid_acl(path, "entry id does not match its tag"));
            }
        }

        let mut index = 0;
        if self.entries.get(index).map(|entry| entry.tag) != Some(ACL_USER_OBJ) {
            return Err(invalid_acl(path, "ACL must start with user::"));
        }
        index += 1;
        let mut last_id = None;
        while self
            .entries
            .get(index)
            .is_some_and(|entry| entry.tag == ACL_USER)
        {
            let id = self.entries[index].id;
            if last_id.is_some_and(|last| id <= last) {
                return Err(invalid_acl(path, "named users are not strictly sorted"));
            }
            last_id = Some(id);
            index += 1;
        }
        if self.entries.get(index).map(|entry| entry.tag) != Some(ACL_GROUP_OBJ) {
            return Err(invalid_acl(path, "ACL is missing group::"));
        }
        index += 1;
        last_id = None;
        while self
            .entries
            .get(index)
            .is_some_and(|entry| entry.tag == ACL_GROUP)
        {
            let id = self.entries[index].id;
            if last_id.is_some_and(|last| id <= last) {
                return Err(invalid_acl(path, "named groups are not strictly sorted"));
            }
            last_id = Some(id);
            index += 1;
        }
        let has_named = self
            .entries
            .iter()
            .any(|entry| matches!(entry.tag, ACL_USER | ACL_GROUP));
        let has_mask = self
            .entries
            .get(index)
            .is_some_and(|entry| entry.tag == ACL_MASK);
        if has_mask {
            index += 1;
        }
        if has_named && !has_mask {
            return Err(invalid_acl(path, "named entries require a mask"));
        }
        if self.entries.get(index).map(|entry| entry.tag) != Some(ACL_OTHER)
            || index + 1 != self.entries.len()
        {
            return Err(invalid_acl(path, "ACL must end with other::"));
        }
        Ok(())
    }

    fn encode(&self) -> Vec<u8> {
        let mut value = Vec::with_capacity(4 + self.entries.len() * 8);
        value.extend_from_slice(&POSIX_ACL_XATTR_VERSION.to_le_bytes());
        for entry in &self.entries {
            value.extend_from_slice(&entry.tag.to_le_bytes());
            value.extend_from_slice(&entry.perm.to_le_bytes());
            value.extend_from_slice(&entry.id.to_le_bytes());
        }
        value
    }

    fn entry(&self, tag: u16) -> &PosixAclEntry {
        self.entries
            .iter()
            .find(|entry| entry.tag == tag)
            .expect("validated ACL required entry")
    }

    fn mask(&self) -> u32 {
        self.entries
            .iter()
            .find(|entry| entry.tag == ACL_MASK)
            .map_or(0o7, |entry| u32::from(entry.perm))
    }

    fn mode(&self, original_mode: u32) -> u32 {
        let owner = u32::from(self.entry(ACL_USER_OBJ).perm);
        let group = self
            .entries
            .iter()
            .find(|entry| entry.tag == ACL_MASK)
            .unwrap_or_else(|| self.entry(ACL_GROUP_OBJ));
        let other = u32::from(self.entry(ACL_OTHER).perm);
        (original_mode & !0o777) | (owner << 6) | (u32::from(group.perm) << 3) | other
    }

    fn apply_mode(&mut self, mode: u32) {
        let has_mask = self.entries.iter().any(|entry| entry.tag == ACL_MASK);
        for entry in &mut self.entries {
            let permissions = match entry.tag {
                ACL_USER_OBJ => Some((mode >> 6) & 0o7),
                ACL_MASK => Some((mode >> 3) & 0o7),
                ACL_GROUP_OBJ if !has_mask => Some((mode >> 3) & 0o7),
                ACL_OTHER => Some(mode & 0o7),
                _ => None,
            };
            if let Some(permissions) = permissions {
                entry.perm = permissions as u16;
            }
        }
    }

    fn restrict_to_mode(&self, mode: u32) -> Self {
        let mut acl = self.clone();
        let has_mask = acl.entries.iter().any(|entry| entry.tag == ACL_MASK);
        for entry in &mut acl.entries {
            let restriction = match entry.tag {
                ACL_USER_OBJ => Some((mode >> 6) & 0o7),
                ACL_MASK => Some((mode >> 3) & 0o7),
                ACL_GROUP_OBJ if !has_mask => Some((mode >> 3) & 0o7),
                ACL_OTHER => Some(mode & 0o7),
                _ => None,
            };
            if let Some(restriction) = restriction {
                entry.perm &= restriction as u16;
            }
        }
        acl
    }

    fn check_access(
        &self,
        identity: &ProcessIdentity,
        stat: &VirtualStat,
        access: u32,
        path: &str,
    ) -> KernelResult<()> {
        let mask = self.mask();
        let granted = if identity.euid == stat.uid {
            u32::from(self.entry(ACL_USER_OBJ).perm)
        } else if let Some(entry) = self
            .entries
            .iter()
            .find(|entry| entry.tag == ACL_USER && entry.id == identity.euid)
        {
            u32::from(entry.perm) & mask
        } else {
            let in_group = |gid| identity.egid == gid || identity.supplementary_gids.contains(&gid);
            let mut matched = false;
            let mut group_permissions = 0;
            if in_group(stat.gid) {
                matched = true;
                group_permissions |= u32::from(self.entry(ACL_GROUP_OBJ).perm);
            }
            for entry in self.entries.iter().filter(|entry| entry.tag == ACL_GROUP) {
                if in_group(entry.id) {
                    matched = true;
                    group_permissions |= u32::from(entry.perm);
                }
            }
            if matched {
                group_permissions & mask
            } else {
                u32::from(self.entry(ACL_OTHER).perm)
            }
        };
        if granted & access == access {
            Ok(())
        } else {
            Err(KernelError::new(
                "EACCES",
                format!(
                    "ACL permission denied: {path} requires {access:o}, granted={granted:o}, euid={}, egid={}",
                    identity.euid, identity.egid
                ),
            ))
        }
    }
}

fn invalid_acl(path: &str, reason: &str) -> KernelError {
    KernelError::new("EINVAL", format!("invalid POSIX ACL for {path}: {reason}"))
}

fn check_xattr_namespace(
    identity: &ProcessIdentity,
    name: &str,
    write: bool,
    path: &str,
) -> KernelResult<()> {
    if name.is_empty() || name.len() > XATTR_NAME_MAX {
        return Err(KernelError::new(
            "EINVAL",
            format!(
                "extended attribute name for {path} is {} bytes; maximum is {XATTR_NAME_MAX}",
                name.len()
            ),
        ));
    }
    let supported = name.starts_with("user.")
        || name.starts_with("trusted.")
        || name.starts_with("security.")
        || name == "system.posix_acl_access"
        || name == "system.posix_acl_default";
    if !supported {
        return Err(KernelError::new(
            "EOPNOTSUPP",
            format!("unsupported extended attribute namespace for {name} on {path}"),
        ));
    }
    if identity.euid != 0 && (name.starts_with("trusted.") || name.starts_with("security.")) {
        return Err(KernelError::permission_denied(format!(
            "{} {name} requires root privileges on {path}",
            if write { "modifying" } else { "reading" }
        )));
    }
    Ok(())
}

fn check_xattr_inode_write_policy(stat: &VirtualStat, name: &str, path: &str) -> KernelResult<()> {
    if name.starts_with("user.") && !stat.is_directory && stat.mode & 0o170000 != S_IFREG {
        return Err(KernelError::new(
            "EPERM",
            format!("user extended attributes require a regular file or directory: {path}"),
        ));
    }
    Ok(())
}

fn check_dac_mode(
    identity: &ProcessIdentity,
    stat: &VirtualStat,
    access: u32,
    path: &str,
) -> KernelResult<()> {
    if identity.euid == 0 {
        if access & DAC_EXECUTE != 0 && !stat.is_directory && stat.mode & 0o111 == 0 {
            return Err(KernelError::new(
                "EACCES",
                format!("execute permission denied: {path}"),
            ));
        }
        return Ok(());
    }
    let shift = if identity.euid == stat.uid {
        6
    } else if identity.egid == stat.gid || identity.supplementary_gids.contains(&stat.gid) {
        3
    } else {
        0
    };
    let granted = (stat.mode >> shift) & 0o7;
    if granted & access == access {
        Ok(())
    } else {
        Err(KernelError::new(
            "EACCES",
            format!(
                "permission denied: {path} requires {access:o}, mode={:o}, euid={}, egid={}",
                stat.mode & 0o7777,
                identity.euid,
                identity.egid
            ),
        ))
    }
}

fn credential_transition_denied(operation: &str, id: u32) -> KernelError {
    KernelError::new(
        "EPERM",
        format!("{operation} is not permitted for credential id {id}"),
    )
}

fn checked_write_end(offset: u64, len: usize) -> KernelResult<u64> {
    offset
        .checked_add(len as u64)
        .ok_or_else(|| KernelError::new("EINVAL", "write offset out of range"))
}

fn check_direct_io_alignment(flags: u32, offset: u64, len: usize) -> KernelResult<()> {
    const DIRECT_IO_ALIGNMENT: u64 = 512;
    if flags & O_DIRECT == 0 {
        return Ok(());
    }
    if !offset.is_multiple_of(DIRECT_IO_ALIGNMENT)
        || !(len as u64).is_multiple_of(DIRECT_IO_ALIGNMENT)
    {
        return Err(KernelError::new(
            "EINVAL",
            format!("O_DIRECT I/O requires {DIRECT_IO_ALIGNMENT}-byte aligned offset and length"),
        ));
    }
    Ok(())
}

fn filetype_for_path(path: &str, stat: &VirtualStat) -> u8 {
    if stat.is_directory {
        FILETYPE_DIRECTORY
    } else if stat.mode & 0o170000 == 0o140000 {
        FILETYPE_SOCKET_STREAM
    } else if path.starts_with("/dev/") {
        FILETYPE_CHARACTER_DEVICE
    } else if stat.is_symbolic_link {
        FILETYPE_SYMBOLIC_LINK
    } else {
        FILETYPE_REGULAR_FILE
    }
}

fn synthetic_character_device_stat(ino: u64) -> VirtualStat {
    synthetic_special_file_stat(ino, 0o020666, 2)
}

fn synthetic_special_file_stat(ino: u64, mode: u32, dev: u64) -> VirtualStat {
    let now = now_ms();
    VirtualStat {
        mode,
        size: 0,
        blocks: 0,
        dev,
        rdev: 0,
        is_directory: false,
        is_symbolic_link: false,
        atime_ms: now,
        atime_nsec: 0,
        mtime_ms: now,
        mtime_nsec: 0,
        ctime_ms: now,
        ctime_nsec: 0,
        birthtime_ms: now,
        ino,
        nlink: 1,
        uid: 0,
        gid: 0,
    }
}

fn proc_dir_stat(ino: u64) -> VirtualStat {
    let now = now_ms();
    VirtualStat {
        mode: S_IFDIR | 0o555,
        size: 0,
        blocks: 0,
        dev: 3,
        rdev: 0,
        is_directory: true,
        is_symbolic_link: false,
        atime_ms: now,
        atime_nsec: 0,
        mtime_ms: now,
        mtime_nsec: 0,
        ctime_ms: now,
        ctime_nsec: 0,
        birthtime_ms: now,
        ino,
        nlink: 2,
        uid: 0,
        gid: 0,
    }
}

fn proc_file_stat(ino: u64, size: u64) -> VirtualStat {
    let now = now_ms();
    VirtualStat {
        mode: S_IFREG | 0o444,
        size,
        blocks: if size == 0 { 0 } else { size.div_ceil(512) },
        dev: 3,
        rdev: 0,
        is_directory: false,
        is_symbolic_link: false,
        atime_ms: now,
        atime_nsec: 0,
        mtime_ms: now,
        mtime_nsec: 0,
        ctime_ms: now,
        ctime_nsec: 0,
        birthtime_ms: now,
        ino,
        nlink: 1,
        uid: 0,
        gid: 0,
    }
}

fn proc_symlink_stat(ino: u64, size: u64) -> VirtualStat {
    let now = now_ms();
    VirtualStat {
        mode: S_IFLNK | 0o777,
        size,
        blocks: if size == 0 { 0 } else { size.div_ceil(512) },
        dev: 3,
        rdev: 0,
        is_directory: false,
        is_symbolic_link: true,
        atime_ms: now,
        atime_nsec: 0,
        mtime_ms: now,
        mtime_nsec: 0,
        ctime_ms: now,
        ctime_nsec: 0,
        birthtime_ms: now,
        ino,
        nlink: 1,
        uid: 0,
        gid: 0,
    }
}

fn proc_filetype(node: &ProcNode) -> u8 {
    match node {
        ProcNode::RootDir | ProcNode::PidDir { .. } | ProcNode::PidFdDir { .. } => {
            FILETYPE_DIRECTORY
        }
        ProcNode::SelfLink { .. } | ProcNode::PidCwdLink { .. } | ProcNode::PidFdLink { .. } => {
            FILETYPE_SYMBOLIC_LINK
        }
        ProcNode::MountsFile
        | ProcNode::CpuInfoFile
        | ProcNode::MemInfoFile
        | ProcNode::LoadAvgFile
        | ProcNode::UptimeFile
        | ProcNode::VersionFile
        | ProcNode::PidCmdline { .. }
        | ProcNode::PidEnviron { .. }
        | ProcNode::PidStatFile { .. }
        | ProcNode::PidStatusFile { .. } => FILETYPE_REGULAR_FILE,
    }
}

fn proc_inode(node: &ProcNode) -> u64 {
    match node {
        ProcNode::RootDir => 0xfffe_0001,
        ProcNode::MountsFile => 0xfffe_0002,
        ProcNode::CpuInfoFile => 0xfffe_0003,
        ProcNode::MemInfoFile => 0xfffe_0004,
        ProcNode::LoadAvgFile => 0xfffe_0005,
        ProcNode::UptimeFile => 0xfffe_0006,
        ProcNode::VersionFile => 0xfffe_0007,
        ProcNode::SelfLink { pid } => 0xfffe_1000 + u64::from(*pid),
        ProcNode::PidDir { pid } => 0xfffe_2000 + u64::from(*pid),
        ProcNode::PidFdDir { pid } => 0xfffe_3000 + u64::from(*pid),
        ProcNode::PidCmdline { pid } => 0xfffe_4000 + u64::from(*pid),
        ProcNode::PidEnviron { pid } => 0xfffe_5000 + u64::from(*pid),
        ProcNode::PidCwdLink { pid } => 0xfffe_6000 + u64::from(*pid),
        ProcNode::PidStatFile { pid } => 0xfffe_7000 + u64::from(*pid),
        ProcNode::PidStatusFile { pid } => 0xfffe_8000 + u64::from(*pid),
        ProcNode::PidFdLink { pid, fd } => 0xffff_0000 + ((u64::from(*pid)) << 8) + u64::from(*fd),
    }
}

fn null_separated_bytes(parts: Vec<String>) -> Vec<u8> {
    if parts.is_empty() {
        return Vec::new();
    }

    let mut bytes = parts.join("\0").into_bytes();
    bytes.push(0);
    bytes
}

fn proc_not_found_error(path: &str) -> KernelError {
    KernelError::new(
        "ENOENT",
        format!("no such file or directory, stat '{path}'"),
    )
}

fn read_only_filesystem_error(path: &str) -> KernelError {
    KernelError::new("EROFS", format!("read-only filesystem: {path}"))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

impl<F> Drop for KernelVm<F> {
    fn drop(&mut self) {
        if !self.terminated {
            dispose_kernel_vm_resources(self);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fd_table::{FD_CLOEXEC, F_GETFD, F_SETFD, O_RDONLY};
    use crate::process_table::SIGTERM;
    use crate::vfs::MemoryFileSystem;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::thread;

    fn kernel_with_process() -> (KernelVm<MemoryFileSystem>, KernelProcessHandle) {
        let mut config = KernelVmConfig::new("vm-fd-socket-test");
        config.permissions = Permissions::allow_all();
        let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
        kernel
            .register_driver(CommandDriver::new("wasm", ["socket-test"]))
            .expect("register wasm driver");
        let process = kernel
            .spawn_process(
                "socket-test",
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from("wasm")),
                    ..SpawnOptions::default()
                },
            )
            .expect("spawn socket test process");
        (kernel, process)
    }

    #[test]
    fn fd_socketpair_preserves_messages_and_transfers_descriptions() {
        let (mut kernel, process) = kernel_with_process();
        let pid = process.pid();
        let (left, right) = kernel
            .fd_socketpair("wasm", pid, SocketType::Stream, true, false)
            .expect("create stream socketpair");
        assert_ne!(left, right);
        assert_eq!(
            kernel
                .fd_stat("wasm", pid, left)
                .expect("stat socket")
                .filetype,
            FILETYPE_SOCKET_STREAM
        );
        assert_ne!(
            kernel
                .fd_stat("wasm", pid, left)
                .expect("stat socket")
                .flags
                & O_NONBLOCK,
            0
        );

        kernel
            .fd_write("wasm", pid, left, b"hello")
            .expect("write socketpair");
        assert_eq!(
            kernel
                .fd_read("wasm", pid, right, 32)
                .expect("read socketpair"),
            b"hello"
        );

        let (pipe_read, pipe_write) = kernel.open_pipe("wasm", pid).expect("open pipe");
        kernel
            .fd_socket_sendmsg("wasm", pid, left, b"x", &[pipe_read])
            .expect("send pipe description");
        let received = kernel
            .fd_socket_recvmsg("wasm", pid, right, 1, 1, true, false, false, false)
            .expect("receive rights")
            .expect("message available");
        assert_eq!(received.payload, b"x");
        assert!(!received.payload_truncated);
        assert!(!received.control_truncated);
        assert_eq!(received.rights.len(), 1);
        let passed_read = match received.rights[0] {
            ReceivedFdRight::Fd(fd) => fd,
            ReceivedFdRight::Opaque(_) => panic!("expected transferred pipe fd"),
        };
        assert_eq!(
            kernel
                .fd_fcntl("wasm", pid, passed_read, F_GETFD, 0)
                .expect("get received fd flags"),
            FD_CLOEXEC
        );

        kernel
            .fd_close("wasm", pid, pipe_read)
            .expect("close original pipe read end");
        kernel
            .fd_write("wasm", pid, pipe_write, b"through-rights")
            .expect("write pipe");
        assert_eq!(
            kernel
                .fd_read("wasm", pid, passed_read, 64)
                .expect("read transferred pipe"),
            b"through-rights"
        );

        for fd in [passed_read, pipe_write, left, right] {
            kernel.fd_close("wasm", pid, fd).expect("close fd");
        }
        assert_eq!(kernel.sockets.snapshot().sockets, 0);
        assert!(lock_or_recover(&kernel.fd_sockets).is_empty());
    }

    #[test]
    fn closed_socket_identity_cannot_poison_reused_regular_fd() {
        let (mut kernel, process) = kernel_with_process();
        let pid = process.pid();
        let (left, right) = kernel
            .fd_socketpair("wasm", pid, SocketType::Stream, false, false)
            .expect("create socketpair");
        let stale_socket_entry = {
            let tables = lock_or_recover(&kernel.fd_tables);
            let description = &tables.get(pid).unwrap().get(left).unwrap().description;
            lock_or_recover(&kernel.fd_sockets)
                .get(&description.id())
                .cloned()
                .expect("registered socket description")
        };
        kernel
            .fd_close("wasm", pid, left)
            .expect("close left socket");
        kernel
            .fd_close("wasm", pid, right)
            .expect("close right socket");
        assert!(lock_or_recover(&kernel.fd_sockets).is_empty());

        kernel
            .write_file("/regular", b"regular-data".to_vec())
            .expect("seed regular file");
        let regular_fd = kernel
            .fd_open("wasm", pid, "/regular", O_RDONLY, None)
            .expect("open regular file after socket close");
        let regular_description = {
            let tables = lock_or_recover(&kernel.fd_tables);
            Arc::clone(
                &tables
                    .get(pid)
                    .unwrap()
                    .get(regular_fd)
                    .unwrap()
                    .description,
            )
        };
        assert_ne!(
            regular_description.id(),
            stale_socket_entry.description.id(),
            "open-file-description ids must be globally unique"
        );

        // Defense in depth: even a corrupt/stale numeric registry key cannot
        // classify a different description as a socket.
        lock_or_recover(&kernel.fd_sockets).insert(regular_description.id(), stale_socket_entry);
        assert_eq!(
            kernel
                .fd_read("wasm", pid, regular_fd, 32)
                .expect("read regular file despite stale socket key"),
            b"regular-data"
        );
        kernel
            .fd_close("wasm", pid, regular_fd)
            .expect("close regular file");
        assert!(lock_or_recover(&kernel.fd_sockets).is_empty());
    }

    #[test]
    fn fd_socketpair_datagram_reads_one_truncated_message_at_a_time() {
        let (mut kernel, process) = kernel_with_process();
        let pid = process.pid();
        let (left, right) = kernel
            .fd_socketpair("wasm", pid, SocketType::Datagram, false, false)
            .expect("create datagram socketpair");
        kernel
            .fd_write("wasm", pid, left, b"abcd")
            .expect("write first datagram");
        kernel
            .fd_write("wasm", pid, left, b"ef")
            .expect("write second datagram");
        let truncated = kernel
            .fd_socket_recvmsg("wasm", pid, right, 2, 0, false, false, false, false)
            .unwrap()
            .unwrap();
        assert_eq!(truncated.payload, b"ab");
        assert!(truncated.payload_truncated);
        assert_eq!(truncated.full_length, 4);
        assert_eq!(kernel.fd_read("wasm", pid, right, 8).unwrap(), b"ef");
    }

    #[test]
    fn fd_socketpair_peek_duplicates_rights_without_consuming_message() {
        let (mut kernel, process) = kernel_with_process();
        let pid = process.pid();
        let (left, right) = kernel
            .fd_socketpair("wasm", pid, SocketType::Stream, false, false)
            .unwrap();
        let (pipe_read, pipe_write) = kernel.open_pipe("wasm", pid).unwrap();
        kernel
            .fd_socket_sendmsg("wasm", pid, left, b"hello", &[pipe_read])
            .unwrap();

        let peeked = kernel
            .fd_socket_recvmsg("wasm", pid, right, 2, 1, false, true, false, false)
            .unwrap()
            .unwrap();
        assert_eq!(peeked.payload, b"he");
        assert_eq!(peeked.full_length, 2);
        let peeked_fd = match peeked.rights[0] {
            ReceivedFdRight::Fd(fd) => fd,
            ReceivedFdRight::Opaque(_) => panic!("expected fd right"),
        };

        let consumed = kernel
            .fd_socket_recvmsg("wasm", pid, right, 5, 1, false, false, false, true)
            .unwrap()
            .unwrap();
        assert_eq!(consumed.payload, b"hello");
        let consumed_fd = match consumed.rights[0] {
            ReceivedFdRight::Fd(fd) => fd,
            ReceivedFdRight::Opaque(_) => panic!("expected fd right"),
        };
        kernel.fd_close("wasm", pid, pipe_read).unwrap();
        kernel.fd_write("wasm", pid, pipe_write, b"ab").unwrap();
        assert_eq!(kernel.fd_read("wasm", pid, peeked_fd, 1).unwrap(), b"a");
        assert_eq!(kernel.fd_read("wasm", pid, consumed_fd, 1).unwrap(), b"b");
        for fd in [peeked_fd, consumed_fd, pipe_write, left, right] {
            kernel.fd_close("wasm", pid, fd).unwrap();
        }
    }

    #[test]
    fn exact_fd_transfer_install_preserves_description_identity_and_offset() {
        let (mut kernel, parent) = kernel_with_process();
        let parent_pid = parent.pid();
        kernel.write_file("/shared", b"abc").unwrap();
        let source_fd = kernel
            .fd_open("wasm", parent_pid, "/shared", O_RDONLY, None)
            .unwrap();
        kernel.fd_seek("wasm", parent_pid, source_fd, 1, 0).unwrap();
        let transfer = kernel.fd_transfer("wasm", parent_pid, source_fd).unwrap();

        let child = kernel
            .spawn_process(
                "socket-test",
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from("wasm")),
                    parent_pid: Some(parent_pid),
                    ..SpawnOptions::default()
                },
            )
            .unwrap();
        kernel
            .fd_install_transfer_at("wasm", child.pid(), 9, 0, &transfer)
            .unwrap();
        assert_eq!(
            kernel
                .fd_transfer("wasm", child.pid(), 9)
                .unwrap()
                .description_id(),
            transfer.description_id()
        );
        assert_eq!(kernel.fd_read("wasm", child.pid(), 9, 1).unwrap(), b"b");
        assert_eq!(
            kernel.fd_read("wasm", parent_pid, source_fd, 1).unwrap(),
            b"c"
        );

        child.finish(0);
        parent.finish(0);
        kernel.waitpid(child.pid()).unwrap();
        kernel.waitpid(parent_pid).unwrap();
    }

    #[test]
    fn dev_fd_stat_reports_linux_pipe_and_socket_modes() {
        let (mut kernel, process) = kernel_with_process();
        let pid = process.pid();
        let (pipe_read, pipe_write) = kernel.open_pipe("wasm", pid).unwrap();
        let (socket_left, socket_right) = kernel
            .fd_socketpair("wasm", pid, SocketType::Stream, false, false)
            .unwrap();

        for fd in [pipe_read, pipe_write] {
            let stat = kernel.dev_fd_stat("wasm", pid, fd).unwrap();
            assert_eq!(stat.mode, 0o010600);
            assert_eq!(stat.nlink, 1);
            assert_eq!(stat.size, 0);
        }
        let read_stat = kernel.dev_fd_stat("wasm", pid, pipe_read).unwrap();
        let write_stat = kernel.dev_fd_stat("wasm", pid, pipe_write).unwrap();
        assert_eq!(read_stat.dev, write_stat.dev);
        assert_eq!(read_stat.ino, write_stat.ino);
        for fd in [socket_left, socket_right] {
            let stat = kernel.dev_fd_stat("wasm", pid, fd).unwrap();
            assert_eq!(stat.mode, 0o140777);
            assert_eq!(stat.nlink, 1);
            assert_eq!(stat.size, 0);
        }
    }

    #[test]
    fn adopted_kernel_socket_lives_while_queued_transfer_guard_exists() {
        let (mut kernel, process) = kernel_with_process();
        let pid = process.pid();
        let socket_id = kernel
            .socket_create("wasm", pid, SocketSpec::tcp())
            .expect("create transferable socket");
        let guard = kernel
            .fd_adopt_socket_transfer("wasm", pid, socket_id, O_NONBLOCK)
            .expect("retain socket description");
        assert_eq!(
            kernel.sockets.get(socket_id).unwrap().owner_pid(),
            0,
            "description-owned sockets must survive sender process cleanup"
        );

        let (left, right) = kernel
            .fd_socketpair("wasm", pid, SocketType::Stream, false, false)
            .expect("create rights channel");
        kernel
            .fd_socket_sendmsg_transfers(
                "wasm",
                pid,
                left,
                b"x",
                &[FdTransferRequest::Opaque(Arc::new(guard))],
            )
            .expect("queue socket guard");
        kernel.fd_close("wasm", pid, left).unwrap();
        kernel.fd_close("wasm", pid, right).unwrap();
        assert!(
            kernel.sockets.get(socket_id).is_none(),
            "discarding the queued right must release and prune the adopted socket"
        );
    }

    #[test]
    fn adopting_socket_for_transfer_does_not_require_a_free_sender_fd() {
        let mut config = KernelVmConfig::new("vm-full-fd-transfer-test");
        config.permissions = Permissions::allow_all();
        config.resources.max_open_fds = Some(3);
        let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
        kernel
            .register_driver(CommandDriver::new("wasm", ["socket-test"]))
            .unwrap();
        let process = kernel
            .spawn_process(
                "socket-test",
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from("wasm")),
                    ..SpawnOptions::default()
                },
            )
            .unwrap();
        let pid = process.pid();
        let socket_id = kernel
            .socket_create("wasm", pid, SocketSpec::tcp())
            .unwrap();

        let guard = kernel
            .fd_adopt_socket_transfer("wasm", pid, socket_id, 0)
            .expect("SCM_RIGHTS must not need a temporary fd in a full sender table");
        assert_eq!(kernel.fd_snapshot("wasm", pid).unwrap().len(), 3);
        assert_eq!(kernel.sockets.get(socket_id).unwrap().owner_pid(), 0);
        drop(guard);
    }

    struct RetainedKernelResources {
        process: KernelProcessHandle,
        fd_tables: Arc<Mutex<FdTableManager>>,
        pipes: PipeManager,
        ptys: PtyManager,
        sockets: SocketTable,
        driver_pids: Arc<Mutex<BTreeMap<String, BTreeSet<u32>>>>,
    }

    fn kernel_with_live_resources() -> (KernelVm<MemoryFileSystem>, RetainedKernelResources) {
        let mut config = KernelVmConfig::new("vm-drop-resources");
        config.permissions = Permissions::allow_all();
        let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
        kernel
            .register_driver(CommandDriver::new("shell", ["sh"]))
            .expect("register shell");

        let process = kernel
            .spawn_process(
                "sh",
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from("shell")),
                    ..SpawnOptions::default()
                },
            )
            .expect("spawn shell");
        let _ = kernel.open_pipe("shell", process.pid()).expect("open pipe");
        let _ = kernel.open_pty("shell", process.pid()).expect("open pty");
        let socket = kernel
            .socket_create("shell", process.pid(), SocketSpec::tcp())
            .expect("create socket");
        kernel
            .socket_set_state("shell", process.pid(), socket, SocketState::Listening)
            .expect("mark listener");

        let retained = RetainedKernelResources {
            process: process.clone(),
            fd_tables: Arc::clone(&kernel.fd_tables),
            pipes: kernel.pipes.clone(),
            ptys: kernel.ptys.clone(),
            sockets: kernel.sockets.clone(),
            driver_pids: Arc::clone(&kernel.driver_pids),
        };

        assert_eq!(lock_or_recover(retained.fd_tables.as_ref()).len(), 1);
        assert_eq!(retained.pipes.pipe_count(), 1);
        assert_eq!(retained.ptys.pty_count(), 1);
        assert_eq!(retained.sockets.snapshot().sockets, 1);

        (kernel, retained)
    }

    fn recursive_fs_kernel() -> KernelVm<MemoryFileSystem> {
        let mut config = KernelVmConfig::new("vm-recursive-fs");
        config.permissions = Permissions::allow_all();
        KernelVm::new(MemoryFileSystem::new(), config)
    }

    #[test]
    fn exec_process_replaces_image_without_replacing_linux_process_state() {
        let mut config = KernelVmConfig::new("vm-exec-process-state");
        config.permissions = Permissions::allow_all();
        config.env = BTreeMap::from([(String::from("INHERITED"), String::from("old"))]);
        let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
        kernel
            .register_driver(CommandDriver::new("runtime", ["old", "new"]))
            .expect("register runtime commands");

        let parent = kernel
            .spawn_process(
                "old",
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from("runtime")),
                    ..SpawnOptions::default()
                },
            )
            .expect("spawn parent");
        let child = kernel
            .spawn_process(
                "old",
                vec![String::from("old-argv")],
                SpawnOptions {
                    requester_driver: Some(String::from("runtime")),
                    parent_pid: Some(parent.pid()),
                    cwd: Some(String::from("/before-exec")),
                    env: BTreeMap::from([(String::from("STALE"), String::from("value"))]),
                },
            )
            .expect("spawn child");
        kernel
            .setpgid("runtime", child.pid(), child.pid())
            .expect("put child in its own process group");
        kernel
            .umask("runtime", child.pid(), Some(0o077))
            .expect("set process umask");
        let blocked = SignalSet::from_signal(SIGTERM).expect("signal set");
        kernel
            .sigprocmask("runtime", child.pid(), SigmaskHow::Block, blocked)
            .expect("block signal");
        kernel
            .kill_process("runtime", child.pid(), SIGTERM)
            .expect("queue blocked signal");

        let (preserved_fd, cloexec_fd) = kernel
            .open_pipe("runtime", child.pid())
            .expect("open process pipe");
        kernel
            .fd_fcntl("runtime", child.pid(), cloexec_fd, F_SETFD, FD_CLOEXEC)
            .expect("mark close-on-exec fd");
        let (forwarded_cloexec_fd, forwarded_peer_fd) = kernel
            .open_pipe("runtime", child.pid())
            .expect("open runner-forwarded process pipe");

        let before = kernel.processes.get(child.pid()).expect("child entry");
        let replacement_env = BTreeMap::from([(String::from("ONLY"), String::from("new"))]);
        kernel
            .mkdir("/literal", true)
            .expect("create literal executable directory");
        kernel
            .write_file("/literal/not-executable", b"wasm".to_vec())
            .expect("create non-executable replacement");
        kernel
            .chmod("/literal/not-executable", 0o644)
            .expect("clear replacement execute bits");
        let error = kernel
            .exec_process_retaining_internal_fds(
                "runtime",
                child.pid(),
                "new",
                vec![String::new(), String::from("argument")],
                replacement_env.clone(),
                String::from("/must-not-change-cwd"),
                &[],
                &[forwarded_cloexec_fd],
                Some("/literal/not-executable"),
            )
            .expect_err("pathname validation must fail before exec commits");
        assert_eq!(error.code(), "EACCES");
        assert_eq!(
            kernel.processes.get(child.pid()).expect("child entry"),
            before,
            "a pre-commit failure must not change process metadata"
        );
        kernel
            .fd_stat("runtime", child.pid(), cloexec_fd)
            .expect("a pre-commit failure must not close CLOEXEC descriptors");
        kernel
            .fd_stat("runtime", child.pid(), forwarded_cloexec_fd)
            .expect("a pre-commit failure must not close forwarded descriptors");
        kernel
            .write_file("/literal/new", b"wasm".to_vec())
            .expect("create executable replacement");
        kernel
            .chmod("/literal/new", 0o755)
            .expect("mark replacement executable");
        kernel
            .exec_process_retaining_internal_fds(
                "runtime",
                child.pid(),
                "new",
                vec![String::new(), String::from("argument")],
                replacement_env.clone(),
                String::from("/must-not-change-cwd"),
                &[],
                &[forwarded_cloexec_fd],
                Some("/literal/new"),
            )
            .expect("replace process image");

        let after = kernel.processes.get(child.pid()).expect("exec child entry");
        assert_eq!(after.pid, before.pid);
        assert_eq!(after.ppid, before.ppid);
        assert_eq!(after.pgid, before.pgid);
        assert_eq!(after.sid, before.sid);
        assert_eq!(after.identity, before.identity);
        assert_eq!(after.umask, before.umask);
        assert_eq!(after.cwd, before.cwd, "execve must preserve cwd");
        assert_eq!(after.command, "");
        assert_eq!(after.args, vec![String::from("argument")]);
        assert_eq!(
            kernel
                .read_file_for_process(
                    "runtime",
                    child.pid(),
                    &format!("/proc/{}/cmdline", child.pid()),
                )
                .expect("read post-exec cmdline"),
            b"\0argument\0".to_vec(),
            "procfs cmdline must contain argv exactly once, including empty argv0"
        );
        assert_eq!(after.env, replacement_env, "envp must replace, not overlay");
        assert_eq!(
            kernel
                .sigprocmask(
                    "runtime",
                    child.pid(),
                    SigmaskHow::Block,
                    SignalSet::empty(),
                )
                .expect("read signal mask"),
            blocked,
            "blocked signal mask must survive exec"
        );
        assert!(
            kernel
                .sigpending("runtime", child.pid())
                .expect("read pending signals")
                .contains(SIGTERM),
            "pending signals must survive exec"
        );
        kernel
            .fd_stat("runtime", child.pid(), preserved_fd)
            .expect("non-CLOEXEC fd must survive");
        assert_eq!(
            kernel
                .fd_stat("runtime", child.pid(), cloexec_fd)
                .expect_err("CLOEXEC fd must close")
                .code(),
            "EBADF"
        );
        assert_eq!(
            kernel
                .fd_stat("runtime", child.pid(), forwarded_cloexec_fd)
                .expect_err("runner-forwarded CLOEXEC fd must close")
                .code(),
            "EBADF"
        );
        kernel
            .fd_stat("runtime", child.pid(), forwarded_peer_fd)
            .expect("unmarked peer of runner-forwarded fd must survive");
    }

    #[test]
    fn validate_executable_path_matches_linux_path_errors_and_symlinks() {
        let mut config = KernelVmConfig::new("vm-exec-path-errors");
        config.permissions = Permissions::allow_all();
        let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);

        assert_eq!(
            kernel
                .validate_executable_path("/missing", "/")
                .expect_err("missing image must fail")
                .code(),
            "ENOENT"
        );

        kernel
            .write_file("/plain", b"data".to_vec())
            .expect("create plain file");
        assert_eq!(
            kernel
                .validate_executable_path("/plain/child", "/")
                .expect_err("non-directory path component must fail")
                .code(),
            "ENOTDIR"
        );

        kernel.mkdir("/directory", true).expect("create directory");
        assert_eq!(
            kernel
                .validate_executable_path("/directory", "/")
                .expect_err("directory image must fail")
                .code(),
            "EACCES"
        );
        assert_eq!(
            kernel
                .validate_executable_path("/plain", "/")
                .expect_err("non-executable image must fail")
                .code(),
            "EACCES"
        );

        kernel
            .chmod("/plain", 0o755)
            .expect("mark target executable");
        kernel
            .symlink("/plain", "/image-link")
            .expect("create executable symlink");
        assert_eq!(
            kernel
                .validate_executable_path("/image-link", "/")
                .expect("exec must follow final symlink"),
            "/plain"
        );

        kernel
            .symlink("/loop-b", "/loop-a")
            .expect("create first loop link");
        kernel
            .symlink("/loop-a", "/loop-b")
            .expect("create second loop link");
        assert_eq!(
            kernel
                .validate_executable_path("/loop-a", "/")
                .expect_err("symlink loop must fail")
                .code(),
            "ELOOP"
        );
    }

    #[test]
    fn validate_wasm_exec_image_follows_linux_shebang_chain() {
        let mut config = KernelVmConfig::new("vm-exec-shebang");
        config.permissions = Permissions::allow_all();
        let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);

        kernel
            .register_driver(CommandDriver::new("shell", ["sh"]))
            .expect("register projected shell command");
        kernel
            .mkdir("/bin", true)
            .expect("create command directory");
        kernel
            .write_file("/bin/sh", b"#!/bin/sh\n".to_vec())
            .expect("write self-referential registered command stub");
        kernel
            .write_file("/registered-script", b"#!/bin/sh\n".to_vec())
            .expect("write registered-interpreter script");
        kernel
            .chmod("/registered-script", 0o755)
            .expect("mark registered-interpreter script executable");
        kernel
            .validate_wasm_exec_image("/registered-script", "/")
            .expect("registered command stub must resolve as a runtime image");

        kernel
            .write_file("/interpreter.wasm", b"\0asm\x01\0\0\0".to_vec())
            .expect("write WASM interpreter");
        kernel
            .chmod("/interpreter.wasm", 0o755)
            .expect("mark interpreter executable");
        kernel
            .write_file(
                "/script",
                b"#!/interpreter.wasm one optional argument\necho ignored\n".to_vec(),
            )
            .expect("write executable script");
        kernel
            .chmod("/script", 0o755)
            .expect("mark script executable");
        kernel
            .validate_wasm_exec_image("/script", "/")
            .expect("WASM interpreter chain must validate");

        kernel
            .write_file("/missing-interpreter", b"#!/absent\n".to_vec())
            .expect("write missing-interpreter script");
        kernel
            .chmod("/missing-interpreter", 0o755)
            .expect("mark missing-interpreter script executable");
        assert_eq!(
            kernel
                .validate_wasm_exec_image("/missing-interpreter", "/")
                .expect_err("missing interpreter must fail")
                .code(),
            "ENOENT"
        );

        kernel
            .write_file("/not-executable", b"\0asm\x01\0\0\0".to_vec())
            .expect("write non-executable interpreter");
        kernel
            .write_file("/denied-script", b"#!/not-executable\n".to_vec())
            .expect("write denied-interpreter script");
        kernel
            .chmod("/denied-script", 0o755)
            .expect("mark denied-interpreter script executable");
        assert_eq!(
            kernel
                .validate_wasm_exec_image("/denied-script", "/")
                .expect_err("non-executable interpreter must fail")
                .code(),
            "EACCES"
        );

        for depth in 0..=MAX_EXEC_INTERPRETER_DEPTH {
            let path = format!("/recursive-{depth}");
            let next = format!("/recursive-{}", depth + 1);
            kernel
                .write_file(&path, format!("#!{next}\n").into_bytes())
                .expect("write recursive interpreter");
            kernel
                .chmod(&path, 0o755)
                .expect("mark recursive interpreter executable");
        }
        assert_eq!(
            kernel
                .validate_wasm_exec_image("/recursive-0", "/")
                .expect_err("interpreter recursion must be bounded")
                .code(),
            "ELOOP"
        );
    }

    #[test]
    fn recursive_copy_preserves_tree_metadata_and_symlinks() {
        let mut kernel = recursive_fs_kernel();
        kernel
            .mkdir("/src/nested", true)
            .expect("create source dirs");
        kernel
            .write_file("/src/nested/file.txt", b"hello".to_vec())
            .expect("write source file");
        kernel
            .chmod("/src/nested/file.txt", 0o640)
            .expect("chmod source file");
        kernel
            .chown("/src/nested/file.txt", 42, 43)
            .expect("chown source file");
        kernel
            .symlink("../nested/file.txt", "/src/link")
            .expect("create source symlink");

        kernel
            .copy_path("/src", "/dst", true)
            .expect("recursive copy");

        assert_eq!(
            kernel
                .read_file("/dst/nested/file.txt")
                .expect("read copied"),
            b"hello".to_vec()
        );
        let copied = kernel.lstat("/dst/nested/file.txt").expect("stat copied");
        assert_eq!(copied.mode & 0o777, 0o640);
        assert_eq!((copied.uid, copied.gid), (42, 43));
        let link = kernel.lstat("/dst/link").expect("lstat copied link");
        assert!(link.is_symbolic_link);
        assert_eq!(
            kernel.read_link("/dst/link").expect("read copied link"),
            "../nested/file.txt"
        );
    }

    #[test]
    fn recursive_remove_deletes_subtree_but_does_not_follow_symlinks() {
        let mut kernel = recursive_fs_kernel();
        kernel.mkdir("/tree/dir", true).expect("create tree");
        kernel
            .write_file("/tree/dir/file.txt", b"tree".to_vec())
            .expect("write tree file");
        kernel
            .write_file("/outside.txt", b"outside".to_vec())
            .expect("write outside file");
        kernel
            .symlink("/outside.txt", "/tree/link-out")
            .expect("create symlink out of tree");

        kernel.remove_path("/tree", true).expect("recursive remove");

        assert!(!kernel.exists("/tree").expect("tree existence"));
        assert_eq!(
            kernel.read_file("/outside.txt").expect("outside survives"),
            b"outside".to_vec()
        );
    }

    #[test]
    fn read_dir_recursive_respects_user_depth_and_reports_types() {
        let mut kernel = recursive_fs_kernel();
        kernel.mkdir("/root/a/b", true).expect("create deep tree");
        kernel
            .write_file("/root/a/file.txt", b"x".to_vec())
            .expect("write file");
        kernel
            .symlink("a/file.txt", "/root/link")
            .expect("create link");

        let entries = kernel
            .read_dir_recursive("/root", Some(0))
            .expect("recursive listing");
        assert_eq!(entries.len(), 2);
        assert!(entries
            .iter()
            .any(|entry| entry.path == "/root/a" && entry.is_directory));
        assert!(entries
            .iter()
            .any(|entry| entry.path == "/root/link" && entry.is_symbolic_link));
        assert!(!entries.iter().any(|entry| entry.path == "/root/a/file.txt"));
    }

    #[test]
    fn recursive_ops_enforce_depth_and_entry_bounds() {
        let mut depth_config = KernelVmConfig::new("vm-recursive-depth-limit");
        depth_config.permissions = Permissions::allow_all();
        depth_config.resources = ResourceLimits {
            max_recursive_fs_depth: Some(1),
            ..ResourceLimits::default()
        };
        let mut depth_kernel = KernelVm::new(MemoryFileSystem::new(), depth_config);
        depth_kernel
            .mkdir("/root/a/b", true)
            .expect("create deep tree");

        let error = depth_kernel
            .copy_path("/root", "/copy", true)
            .expect_err("copy should hit depth limit");
        assert_eq!(error.code(), "ENOMEM");
        assert!(error.to_string().contains("depth 2"));

        let mut entry_config = KernelVmConfig::new("vm-recursive-entry-limit");
        entry_config.permissions = Permissions::allow_all();
        entry_config.resources = ResourceLimits {
            max_recursive_fs_entries: Some(2),
            ..ResourceLimits::default()
        };
        let mut entry_kernel = KernelVm::new(MemoryFileSystem::new(), entry_config);
        entry_kernel.mkdir("/root", true).expect("create root");
        entry_kernel
            .write_file("/root/a.txt", b"a".to_vec())
            .expect("write a");
        entry_kernel
            .write_file("/root/b.txt", b"b".to_vec())
            .expect("write b");
        entry_kernel
            .write_file("/root/c.txt", b"c".to_vec())
            .expect("write c");

        let error = entry_kernel
            .read_dir_recursive("/root", None)
            .expect_err("listing should hit entry limit");
        assert_eq!(error.code(), "ENOMEM");
        assert!(error.to_string().contains("3 entries"));
    }

    fn assert_kernel_drop_released_resources(retained: &RetainedKernelResources) {
        assert_eq!(retained.process.wait(Duration::from_millis(50)), Some(143));
        assert_eq!(retained.process.kill_signals(), vec![15]);
        assert!(
            lock_or_recover(retained.fd_tables.as_ref()).is_empty(),
            "kernel drop should remove fd tables"
        );
        assert_eq!(
            retained.pipes.pipe_count(),
            0,
            "kernel drop should close pipes"
        );
        assert_eq!(
            retained.ptys.pty_count(),
            0,
            "kernel drop should close PTYs"
        );
        assert_eq!(
            retained.sockets.snapshot().sockets,
            0,
            "kernel drop should reclaim sockets"
        );
        assert!(
            lock_or_recover(retained.driver_pids.as_ref()).is_empty(),
            "kernel drop should clear driver-owned pid tracking"
        );
    }

    #[test]
    fn setpgid_rejects_joining_a_process_group_owned_by_another_driver() {
        let kernel = KernelVm::new(MemoryFileSystem::new(), KernelVmConfig::new("vm-setpgid"));

        let leader_pid = kernel.processes.allocate_pid().expect("allocate pid");
        kernel.processes.register(
            leader_pid,
            String::from("driver-a"),
            String::from("sh"),
            Vec::new(),
            ProcessContext {
                pid: leader_pid,
                ppid: 0,
                env: BTreeMap::new(),
                cwd: String::from("/"),
                umask: DEFAULT_PROCESS_UMASK,
                fds: Default::default(),
                identity: ProcessIdentity::default(),
                blocked_signals: SignalSet::empty(),
                pending_signals: SignalSet::empty(),
            },
            Arc::new(StubDriverProcess::default()),
        );

        let peer_pid = kernel.processes.allocate_pid().expect("allocate pid");
        kernel.processes.register(
            peer_pid,
            String::from("driver-b"),
            String::from("sh"),
            Vec::new(),
            ProcessContext {
                pid: peer_pid,
                ppid: leader_pid,
                env: BTreeMap::new(),
                cwd: String::from("/"),
                umask: DEFAULT_PROCESS_UMASK,
                fds: Default::default(),
                identity: ProcessIdentity::default(),
                blocked_signals: SignalSet::empty(),
                pending_signals: SignalSet::empty(),
            },
            Arc::new(StubDriverProcess::default()),
        );

        lock_or_recover(&kernel.driver_pids)
            .entry(String::from("driver-a"))
            .or_default()
            .insert(leader_pid);
        lock_or_recover(&kernel.driver_pids)
            .entry(String::from("driver-b"))
            .or_default()
            .insert(peer_pid);

        let error = kernel
            .setpgid("driver-b", peer_pid, leader_pid)
            .expect_err("cross-driver process-group join should be denied");
        assert_eq!(error.code(), "EPERM");
    }

    #[test]
    fn sigprocmask_and_sigpending_require_process_ownership() {
        let mut kernel = KernelVm::new(MemoryFileSystem::new(), KernelVmConfig::new("vm-sigmask"));
        let process = kernel
            .register_process(
                String::from("driver-a"),
                String::from("sleep"),
                Vec::new(),
                ProcessContext {
                    pid: 0,
                    ppid: 0,
                    env: BTreeMap::new(),
                    cwd: String::from("/"),
                    umask: DEFAULT_PROCESS_UMASK,
                    fds: Default::default(),
                    identity: ProcessIdentity::default(),
                    blocked_signals: SignalSet::empty(),
                    pending_signals: SignalSet::empty(),
                },
                None,
                None,
                false,
            )
            .expect("create virtual process");
        let mask =
            SignalSet::from_signal(crate::process_table::SIGCHLD).expect("SIGCHLD should be valid");

        let previous = kernel
            .sigprocmask("driver-a", process.pid(), SigmaskHow::Block, mask)
            .expect("owner should update signal mask");
        assert_eq!(previous, SignalSet::empty());
        assert_eq!(
            kernel
                .sigpending("driver-a", process.pid())
                .expect("owner should read pending signals"),
            SignalSet::empty()
        );

        let error = kernel
            .sigprocmask("driver-b", process.pid(), SigmaskHow::Block, mask)
            .expect_err("foreign driver should be rejected");
        assert_eq!(error.code(), "EPERM");
        let error = kernel
            .sigpending("driver-b", process.pid())
            .expect_err("foreign driver should be rejected");
        assert_eq!(error.code(), "EPERM");
    }

    #[test]
    fn cleanup_process_resources_blocks_concurrent_dup2_until_pipe_cleanup_finishes() {
        let fd_tables = Arc::new(Mutex::new(FdTableManager::new()));
        let file_locks = FileLockManager::new();
        let pipes = PipeManager::new();
        let ptys = PtyManager::new();
        let sockets = SocketTable::new();
        let fd_sockets = Arc::new(Mutex::new(BTreeMap::new()));
        let driver_pids = Arc::new(Mutex::new(BTreeMap::from([(
            String::from("driver"),
            BTreeSet::from([41]),
        )])));
        let pipe = pipes.create_pipe();

        {
            let mut tables = lock_or_recover(fd_tables.as_ref());
            let table = tables.create(41);
            table
                .open_with(
                    Arc::clone(&pipe.read.description),
                    pipe.read.filetype,
                    Some(10),
                )
                .expect("open pipe read end");
            table
                .open_with(
                    Arc::clone(&pipe.write.description),
                    pipe.write.filetype,
                    Some(11),
                )
                .expect("open pipe write end");
        }

        let hook_state = Arc::new((Mutex::new((false, false)), Condvar::new()));
        let hook_state_for_cleanup = Arc::clone(&hook_state);
        set_cleanup_process_resources_test_hook(Some(Arc::new(move || {
            let (state, wake) = &*hook_state_for_cleanup;
            let mut state = lock_or_recover(state);
            state.0 = true;
            wake.notify_all();
            while !state.1 {
                state = wake.wait(state).expect("wait for cleanup release");
            }
        })));

        let fd_tables_for_cleanup = Arc::clone(&fd_tables);
        let pipes_for_cleanup = pipes.clone();
        let driver_pids_for_cleanup = Arc::clone(&driver_pids);
        let cleanup_thread = thread::spawn(move || {
            cleanup_process_resources(
                fd_tables_for_cleanup.as_ref(),
                &file_locks,
                &pipes_for_cleanup,
                &ptys,
                &sockets,
                &fd_sockets,
                driver_pids_for_cleanup.as_ref(),
                41,
            );
        });

        {
            let (state, wake) = &*hook_state;
            let mut state = lock_or_recover(state);
            while !state.0 {
                state = wake.wait(state).expect("wait for cleanup hook");
            }
        }

        let fd_tables_for_dup = Arc::clone(&fd_tables);
        let dup_thread = thread::spawn(move || {
            let mut tables = lock_or_recover(fd_tables_for_dup.as_ref());
            let Some(table) = tables.get_mut(41) else {
                return Err(String::from("ESRCH"));
            };
            table.dup2(10, 12).map_err(|error| error.code().to_string())
        });

        {
            let (state, wake) = &*hook_state;
            let mut state = lock_or_recover(state);
            state.1 = true;
            wake.notify_all();
        }

        cleanup_thread.join().expect("cleanup thread should finish");
        let dup_result = dup_thread.join().expect("dup thread should finish");
        set_cleanup_process_resources_test_hook(None);

        assert_eq!(dup_result, Err(String::from("ESRCH")));
        assert!(
            lock_or_recover(fd_tables.as_ref()).get(41).is_none(),
            "cleanup should remove the process FD table"
        );
        assert_eq!(pipes.pipe_count(), 0, "pipe cleanup should not leak");
        assert!(
            lock_or_recover(driver_pids.as_ref())
                .get("driver")
                .is_none_or(|pids| pids.is_empty()),
            "driver ownership should be cleared"
        );
    }

    #[test]
    fn drop_disposes_live_kernel_vm_resources() {
        let (kernel, retained) = kernel_with_live_resources();
        drop(kernel);
        assert_kernel_drop_released_resources(&retained);
    }

    #[test]
    fn drop_during_panic_still_disposes_live_kernel_vm_resources() {
        let retained = Arc::new(Mutex::new(None::<RetainedKernelResources>));
        let retained_for_panic = Arc::clone(&retained);

        let panic_result = catch_unwind(AssertUnwindSafe(move || {
            let (kernel, resources) = kernel_with_live_resources();
            *lock_or_recover(retained_for_panic.as_ref()) = Some(resources);
            let _kernel = kernel;
            panic!("intentional panic to exercise KernelVm::drop");
        }));

        assert!(panic_result.is_err(), "panic should be observed");
        let retained = lock_or_recover(retained.as_ref())
            .take()
            .expect("panic path should retain resources for assertions");
        assert_kernel_drop_released_resources(&retained);
    }
}
