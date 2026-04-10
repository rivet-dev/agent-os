use crate::bridge::LifecycleState;
use crate::command_registry::{CommandDriver, CommandRegistry};
use crate::device_layer::{create_device_layer, DeviceLayer};
use crate::dns::{
    format_dns_resource, resolve_dns, DnsConfig, DnsLookupPolicy, DnsResolution,
    DnsResolverErrorKind, HickoryDnsResolver, SharedDnsResolver,
};
use crate::fd_table::{
    FdEntry, FdStat, FdTableError, FdTableManager, FileDescription, FileLockManager,
    FileLockTarget, FlockOperation, ProcessFdTable, FILETYPE_CHARACTER_DEVICE, FILETYPE_DIRECTORY,
    FILETYPE_PIPE, FILETYPE_REGULAR_FILE, FILETYPE_SYMBOLIC_LINK, O_APPEND, O_CREAT, O_EXCL,
    O_NONBLOCK, O_TRUNC,
};
use crate::mount_table::{MountEntry, MountOptions, MountTable, MountedFileSystem};
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
    ProcessTableError, ProcessWaitResult, DEFAULT_PROCESS_UMASK, SIGCONT, SIGPIPE, SIGSTOP,
    SIGTSTP, SIGWINCH,
};
use crate::pty::{LineDisciplineConfig, PartialTermios, PtyError, PtyManager, Termios};
use crate::resource_accounting::{
    measure_filesystem_usage, FileSystemUsage, ResourceAccountant, ResourceError, ResourceLimits,
    ResourceSnapshot,
};
use crate::root_fs::{RootFileSystem, RootFilesystemError, RootFilesystemSnapshot};
use crate::socket_table::{
    InetSocketAddress, ReceivedDatagram, SocketId, SocketRecord, SocketShutdown, SocketSpec,
    SocketState, SocketTable, SocketTableError,
};
use crate::user::{ProcessIdentity, UserConfig, UserManager};
use crate::vfs::{normalize_path, VfsError, VfsResult, VirtualFileSystem, VirtualStat};
use std::any::Any;
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, WaitTimeoutResult};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub type KernelResult<T> = Result<T, KernelError>;
pub use crate::process_table::{ProcessWaitEvent as WaitPidEvent, WaitPidFlags};

pub const SEEK_SET: u8 = 0;
pub const SEEK_CUR: u8 = 1;
pub const SEEK_END: u8 = 2;
const EXECUTABLE_PERMISSION_BITS: u32 = 0o111;
const SHEBANG_LINE_MAX_BYTES: usize = 256;

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

#[derive(Clone)]
pub struct KernelVmConfig {
    pub vm_id: String,
    pub env: BTreeMap<String, String>,
    pub cwd: String,
    pub user: UserConfig,
    pub permissions: Permissions,
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
            cwd: String::from("/home/user"),
            user: UserConfig::default(),
            permissions: Permissions::default(),
            dns: DnsConfig::default(),
            dns_resolver: Arc::new(HickoryDnsResolver),
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
    filesystem: PermissionedFileSystem<DeviceLayer<F>>,
    permissions: Permissions,
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
    poll_notifier: PollNotifier,
    users: UserManager,
    resources: ResourceAccountant,
    file_locks: FileLockManager,
    driver_pids: Arc<Mutex<BTreeMap<String, BTreeSet<u32>>>>,
    terminated: bool,
}

fn cleanup_process_resources(
    fd_tables: &Mutex<FdTableManager>,
    file_locks: &FileLockManager,
    pipes: &PipeManager,
    ptys: &PtyManager,
    sockets: &SocketTable,
    driver_pids: &Mutex<BTreeMap<String, BTreeSet<u32>>>,
    pid: u32,
) {
    let descriptors = {
        let tables = lock_or_recover(fd_tables);
        tables
            .get(pid)
            .map(|table| {
                table
                    .iter()
                    .map(|entry| (entry.fd, Arc::clone(&entry.description), entry.filetype))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };

    let mut cleanup = Vec::new();
    {
        let mut tables = lock_or_recover(fd_tables);
        if let Some(table) = tables.get_mut(pid) {
            for (fd, description, filetype) in &descriptors {
                table.close(*fd);
                cleanup.push((Arc::clone(description), *filetype));
            }
        }
        tables.remove(pid);
    }

    for (description, filetype) in cleanup {
        close_special_resource_if_needed(file_locks, pipes, ptys, &description, filetype);
    }

    sockets.remove_all_for_pid(pid);

    let mut owners = lock_or_recover(driver_pids);
    for pids in owners.values_mut() {
        pids.remove(&pid);
    }
}

fn close_special_resource_if_needed(
    file_locks: &FileLockManager,
    pipes: &PipeManager,
    ptys: &PtyManager,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProcNode {
    RootDir,
    MountsFile,
    SelfLink { pid: u32 },
    PidDir { pid: u32 },
    PidFdDir { pid: u32 },
    PidCmdline { pid: u32 },
    PidEnviron { pid: u32 },
    PidCwdLink { pid: u32 },
    PidStatFile { pid: u32 },
    PidFdLink { pid: u32, fd: u32 },
}

impl<F: VirtualFileSystem + 'static> KernelVm<F> {
    pub fn new(filesystem: F, config: KernelVmConfig) -> Self {
        let vm_id = config.vm_id;
        let permissions = config.permissions.clone();
        let users = UserManager::from_config(config.user);
        let process_table = ProcessTable::with_zombie_ttl(config.zombie_ttl);
        let process_table_for_pty = process_table.clone();
        let fd_tables = Arc::new(Mutex::new(FdTableManager::new()));
        let file_locks = FileLockManager::new();
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

        let fd_tables_for_exit = Arc::clone(&fd_tables);
        let file_locks_for_exit = file_locks.clone();
        let driver_pids_for_exit = Arc::clone(&driver_pids);
        let pipes_for_exit = pipes.clone();
        let ptys_for_exit = ptys.clone();
        let sockets_for_exit = sockets.clone();
        process_table.set_on_process_exit(Some(Arc::new(move |pid| {
            cleanup_process_resources(
                fd_tables_for_exit.as_ref(),
                &file_locks_for_exit,
                &pipes_for_exit,
                &ptys_for_exit,
                &sockets_for_exit,
                driver_pids_for_exit.as_ref(),
                pid,
            );
        })));

        Self {
            vm_id: vm_id.clone(),
            filesystem: PermissionedFileSystem::new(
                create_device_layer(filesystem),
                vm_id,
                permissions.clone(),
            ),
            permissions,
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
            poll_notifier,
            users,
            resources: ResourceAccountant::new(config.resources),
            file_locks,
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
        Ok(self.process_identity(requester_driver, pid)?.supplementary_gids)
    }

    pub fn getpwuid(&self, uid: u32) -> String {
        self.users.getpwuid(uid)
    }

    pub fn getgrgid(&self, gid: u32) -> String {
        self.users.getgrgid(gid)
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

    pub fn register_driver(&mut self, driver: CommandDriver) -> KernelResult<()> {
        self.assert_not_terminated()?;
        lock_or_recover(&self.driver_pids)
            .entry(driver.name().to_owned())
            .or_default();
        self.commands.register(driver);
        self.commands.populate_bin(&mut self.filesystem)?;
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

    pub fn read_file_for_process(
        &mut self,
        requester_driver: &str,
        pid: u32,
        path: &str,
    ) -> KernelResult<Vec<u8>> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        self.read_file_internal(Some(pid), path)
    }

    pub fn write_file(&mut self, path: &str, content: impl Into<Vec<u8>>) -> KernelResult<()> {
        self.assert_not_terminated()?;
        if is_proc_path(path) {
            self.filesystem
                .check_virtual_path(FsOperation::Write, path)
                .map_err(KernelError::from)?;
            return Err(read_only_filesystem_error(path));
        }
        let content = content.into();
        self.check_write_file_limits(path, content.len() as u64)?;
        Ok(self.filesystem.write_file(path, content)?)
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
        let content = content.into();
        self.write_file(path, content)?;
        if !existed {
            let umask = self.processes.get_umask(pid)?;
            self.apply_creation_mode(path, mode.unwrap_or(0o666), umask)?;
        }
        Ok(())
    }

    pub fn create_dir(&mut self, path: &str) -> KernelResult<()> {
        self.assert_not_terminated()?;
        if is_proc_path(path) {
            self.filesystem
                .check_virtual_path(FsOperation::Write, path)
                .map_err(KernelError::from)?;
            return Err(read_only_filesystem_error(path));
        }
        self.check_create_dir_limits(path)?;
        Ok(self.filesystem.create_dir(path)?)
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
        self.create_dir(path)?;
        if !existed {
            let umask = self.processes.get_umask(pid)?;
            self.apply_creation_mode(path, mode.unwrap_or(0o777), umask)?;
        }
        Ok(())
    }

    pub fn mkdir(&mut self, path: &str, recursive: bool) -> KernelResult<()> {
        self.assert_not_terminated()?;
        if is_proc_path(path) {
            self.filesystem
                .check_virtual_path(FsOperation::Write, path)
                .map_err(KernelError::from)?;
            return Err(read_only_filesystem_error(path));
        }
        self.check_mkdir_limits(path, recursive)?;
        Ok(self.filesystem.mkdir(path, recursive)?)
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
        self.mkdir(path, recursive)?;
        if !created_paths.is_empty() {
            let umask = self.processes.get_umask(pid)?;
            let mode = mode.unwrap_or(0o777);
            for created_path in created_paths {
                self.apply_creation_mode(&created_path, mode, umask)?;
            }
        }
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
        &self,
        requester_driver: &str,
        pid: u32,
        path: &str,
    ) -> KernelResult<bool> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
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
        self.stat_internal(Some(pid), path)
    }

    pub fn lstat(&self, path: &str) -> KernelResult<VirtualStat> {
        self.assert_not_terminated()?;
        self.lstat_internal(None, path)
    }

    pub fn lstat_for_process(
        &self,
        requester_driver: &str,
        pid: u32,
        path: &str,
    ) -> KernelResult<VirtualStat> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        self.lstat_internal(Some(pid), path)
    }

    pub fn read_link(&self, path: &str) -> KernelResult<String> {
        self.assert_not_terminated()?;
        self.read_link_internal(None, path)
    }

    pub fn read_link_for_process(
        &self,
        requester_driver: &str,
        pid: u32,
        path: &str,
    ) -> KernelResult<String> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
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
        let entries = self.read_dir_internal(Some(pid), path)?;
        self.resources.check_readdir_entries(entries.len())?;
        Ok(entries)
    }

    pub fn remove_file(&mut self, path: &str) -> KernelResult<()> {
        self.assert_not_terminated()?;
        if is_proc_path(path) {
            self.filesystem
                .check_virtual_path(FsOperation::Write, path)
                .map_err(KernelError::from)?;
            return Err(read_only_filesystem_error(path));
        }
        Ok(self.filesystem.remove_file(path)?)
    }

    pub fn remove_dir(&mut self, path: &str) -> KernelResult<()> {
        self.assert_not_terminated()?;
        if is_proc_path(path) {
            self.filesystem
                .check_virtual_path(FsOperation::Write, path)
                .map_err(KernelError::from)?;
            return Err(read_only_filesystem_error(path));
        }
        Ok(self.filesystem.remove_dir(path)?)
    }

    pub fn rename(&mut self, old_path: &str, new_path: &str) -> KernelResult<()> {
        self.assert_not_terminated()?;
        if is_proc_path(old_path) || is_proc_path(new_path) {
            self.filesystem
                .check_virtual_path(FsOperation::Write, old_path)
                .map_err(KernelError::from)?;
            self.filesystem
                .check_virtual_path(FsOperation::Write, new_path)
                .map_err(KernelError::from)?;
            return Err(read_only_filesystem_error(if is_proc_path(new_path) {
                new_path
            } else {
                old_path
            }));
        }
        Ok(self.filesystem.rename(old_path, new_path)?)
    }

    pub fn realpath(&self, path: &str) -> KernelResult<String> {
        self.assert_not_terminated()?;
        self.realpath_internal(None, path)
    }

    pub fn realpath_for_process(
        &self,
        requester_driver: &str,
        pid: u32,
        path: &str,
    ) -> KernelResult<String> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        self.realpath_internal(Some(pid), path)
    }

    pub fn symlink(&mut self, target: &str, link_path: &str) -> KernelResult<()> {
        self.assert_not_terminated()?;
        if is_proc_path(target) || is_proc_path(link_path) {
            self.filesystem
                .check_virtual_path(FsOperation::Write, link_path)
                .map_err(KernelError::from)?;
            return Err(read_only_filesystem_error(link_path));
        }
        self.check_symlink_limits(target, link_path)?;
        Ok(self.filesystem.symlink(target, link_path)?)
    }

    pub fn chmod(&mut self, path: &str, mode: u32) -> KernelResult<()> {
        self.assert_not_terminated()?;
        if is_proc_path(path) {
            self.filesystem
                .check_virtual_path(FsOperation::Write, path)
                .map_err(KernelError::from)?;
            return Err(read_only_filesystem_error(path));
        }
        Ok(self.filesystem.chmod(path, mode)?)
    }

    pub fn link(&mut self, old_path: &str, new_path: &str) -> KernelResult<()> {
        self.assert_not_terminated()?;
        if is_proc_path(old_path) || is_proc_path(new_path) {
            self.filesystem
                .check_virtual_path(FsOperation::Write, new_path)
                .map_err(KernelError::from)?;
            return Err(read_only_filesystem_error(new_path));
        }
        Ok(self.filesystem.link(old_path, new_path)?)
    }

    pub fn chown(&mut self, path: &str, uid: u32, gid: u32) -> KernelResult<()> {
        self.assert_not_terminated()?;
        if is_proc_path(path) {
            self.filesystem
                .check_virtual_path(FsOperation::Write, path)
                .map_err(KernelError::from)?;
            return Err(read_only_filesystem_error(path));
        }
        Ok(self.filesystem.chown(path, uid, gid)?)
    }

    pub fn utimes(&mut self, path: &str, atime_ms: u64, mtime_ms: u64) -> KernelResult<()> {
        self.assert_not_terminated()?;
        if is_proc_path(path) {
            self.filesystem
                .check_virtual_path(FsOperation::Write, path)
                .map_err(KernelError::from)?;
            return Err(read_only_filesystem_error(path));
        }
        Ok(self.filesystem.utimes(path, atime_ms, mtime_ms)?)
    }

    pub fn truncate(&mut self, path: &str, length: u64) -> KernelResult<()> {
        self.assert_not_terminated()?;
        if is_proc_path(path) {
            self.filesystem
                .check_virtual_path(FsOperation::Write, path)
                .map_err(KernelError::from)?;
            return Err(read_only_filesystem_error(path));
        }
        self.check_truncate_limits(path, length)?;
        Ok(self.filesystem.truncate(path, length)?)
    }

    pub fn list_processes(&self) -> BTreeMap<u32, ProcessInfo> {
        self.processes.list_processes()
    }

    pub fn zombie_timer_count(&self) -> usize {
        self.processes.zombie_timer_count()
    }

    pub fn spawn_process(
        &mut self,
        command: &str,
        args: Vec<String>,
        options: SpawnOptions,
    ) -> KernelResult<KernelProcessHandle> {
        self.assert_not_terminated()?;
        if let (Some(requester), Some(parent_pid)) =
            (options.requester_driver.as_deref(), options.parent_pid)
        {
            self.assert_driver_owns(requester, parent_pid)?;
        }

        let cwd = options.cwd.clone().unwrap_or_else(|| self.cwd.clone());
        let resolved = self.resolve_spawn_command(command, &args, &cwd)?;

        self.resources
            .check_process_argv_bytes(&resolved.command, &resolved.args)?;
        self.resources
            .check_process_env_bytes(&self.env, &options.env)?;

        let mut env = self.env.clone();
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
                .and_then(|pid| tables.get(pid).map(ProcessFdTable::len))
                .unwrap_or(3)
        };
        self.resources
            .check_process_spawn(&self.resource_snapshot(), inherited_fds)?;

        self.register_process(
            resolved.driver.name().to_owned(),
            resolved.command,
            resolved.args,
            ProcessContext {
                pid: 0,
                ppid: options.parent_pid.unwrap_or(0),
                env,
                cwd,
                umask: DEFAULT_PROCESS_UMASK,
                fds: Default::default(),
                identity: self.users.identity(),
            },
            options.requester_driver.as_deref(),
        )
    }

    pub fn create_virtual_process(
        &mut self,
        requester_driver: &str,
        driver: &str,
        command: &str,
        args: Vec<String>,
        options: VirtualProcessOptions,
    ) -> KernelResult<KernelProcessHandle> {
        self.assert_not_terminated()?;
        if let Some(parent_pid) = options.parent_pid {
            self.assert_driver_owns(requester_driver, parent_pid)?;
        }

        let cwd = options.cwd.clone().unwrap_or_else(|| self.cwd.clone());
        self.resources.check_process_argv_bytes(command, &args)?;
        self.resources
            .check_process_env_bytes(&self.env, &options.env)?;

        let mut env = self.env.clone();
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

        self.register_process(
            String::from(driver),
            String::from(command),
            args,
            ProcessContext {
                pid: 0,
                ppid: options.parent_pid.unwrap_or(0),
                env,
                cwd,
                umask: DEFAULT_PROCESS_UMASK,
                fds: Default::default(),
                identity: self.users.identity(),
            },
            Some(requester_driver),
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

    fn register_process(
        &mut self,
        driver_name: String,
        command: String,
        args: Vec<String>,
        mut ctx: ProcessContext,
        requester_driver: Option<&str>,
    ) -> KernelResult<KernelProcessHandle> {
        let pid = self.processes.allocate_pid();
        ctx.pid = pid;

        {
            let mut tables = lock_or_recover(&self.fd_tables);
            if ctx.ppid != 0 {
                let parent_pid = ctx.ppid;
                tables.fork(parent_pid, pid);
            } else {
                tables.create(pid);
            }
        }

        let process = Arc::new(StubDriverProcess::default());
        self.processes.register(
            pid,
            driver_name.clone(),
            command,
            args,
            ctx,
            process.clone(),
        );

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

    pub fn wait_and_reap(&mut self, pid: u32) -> KernelResult<(u32, i32)> {
        let result = self.waitpid(pid)?;
        Ok((result.pid, result.status))
    }

    pub fn open_pipe(&mut self, requester_driver: &str, pid: u32) -> KernelResult<(u32, u32)> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        self.resources
            .check_pipe_allocation(&self.resource_snapshot())?;
        let mut tables = lock_or_recover(&self.fd_tables);
        let table = tables
            .get_mut(pid)
            .ok_or_else(|| KernelError::no_such_process(pid))?;
        Ok(self.pipes.create_pipe_fds(table)?)
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
        self.resources
            .check_socket_allocation(&self.resource_snapshot())?;
        Ok(self.sockets.allocate(pid, spec).id())
    }

    pub fn socket_get(&self, socket_id: SocketId) -> Option<SocketRecord> {
        self.sockets.get(socket_id)
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
        if existing.owner_pid() != pid {
            return Err(KernelError::permission_denied(format!(
                "process {pid} does not own socket {socket_id}"
            )));
        }

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
        if existing.owner_pid() != pid {
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
        if existing.owner_pid() != pid {
            return Err(KernelError::permission_denied(format!(
                "process {pid} does not own socket {socket_id}"
            )));
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
        if existing.owner_pid() != pid {
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
        if existing.owner_pid() != pid {
            return Err(KernelError::permission_denied(format!(
                "process {pid} does not own socket {listener_socket_id}"
            )));
        }

        let snapshot = self.resource_snapshot();
        self.resources.check_socket_allocation(&snapshot)?;
        self.resources.check_socket_state_transition(
            &snapshot,
            SocketState::Created,
            SocketState::Connected,
        )?;

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
        if existing.owner_pid() != pid {
            return Err(KernelError::permission_denied(format!(
                "process {pid} does not own socket {socket_id}"
            )));
        }

        let peer = self.sockets.get(peer_socket_id).ok_or_else(|| {
            KernelError::new("ENOENT", format!("no such socket {peer_socket_id}"))
        })?;
        self.assert_driver_owns(requester_driver, peer.owner_pid())?;

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
        if existing.owner_pid() != pid {
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
        if existing.owner_pid() != pid {
            return Err(KernelError::permission_denied(format!(
                "process {pid} does not own socket {socket_id}"
            )));
        }

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
        if existing.owner_pid() != pid {
            return Err(KernelError::permission_denied(format!(
                "process {pid} does not own socket {socket_id}"
            )));
        }

        let written = self
            .sockets
            .send_to_bound_udp_socket(socket_id, target_address, data)?;
        if written > 0 {
            self.poll_notifier.notify();
        }
        Ok(written)
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
        if existing.owner_pid() != pid {
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
        if existing.owner_pid() != pid {
            return Err(KernelError::permission_denied(format!(
                "process {pid} does not own socket {socket_id}"
            )));
        }

        self.resources.check_socket_state_transition(
            &self.resource_snapshot(),
            existing.state(),
            state,
        )?;
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
        if existing.owner_pid() != pid {
            return Err(KernelError::permission_denied(format!(
                "process {pid} does not own socket {socket_id}"
            )));
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
        if existing.owner_pid() != pid {
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
        if existing.owner_pid() != pid {
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
        if existing.owner_pid() != pid {
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
        if let Some(existing_fd) = parse_dev_fd_path(path)? {
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
            if flags & (O_CREAT | O_EXCL | O_TRUNC) != 0
                || (flags & 0b11) != crate::fd_table::O_RDONLY
            {
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

        let existed = if flags & O_CREAT != 0 {
            self.exists_internal(Some(pid), path)?
        } else {
            false
        };
        let (filetype, lock_target) = self.prepare_fd_open(path, flags)?;
        if flags & O_CREAT != 0 && !existed {
            let umask = self.processes.get_umask(pid)?;
            self.apply_creation_mode(path, mode.unwrap_or(0o666), umask)?;
        }
        let mut tables = lock_or_recover(&self.fd_tables);
        let table = tables
            .get_mut(pid)
            .ok_or_else(|| KernelError::no_such_process(pid))?;
        Ok(table.open_with_details(path, flags, filetype, lock_target)?)
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

        if self.pipes.is_pipe(entry.description.id()) {
            return Ok(self.pipes.read_with_timeout(
                entry.description.id(),
                length,
                if entry.status_flags & O_NONBLOCK != 0 {
                    Some(Duration::ZERO)
                } else {
                    timeout.or_else(|| self.blocking_read_timeout())
                },
            )?);
        }

        if self.ptys.is_pty(entry.description.id()) {
            return Ok(self.ptys.read_with_timeout(
                entry.description.id(),
                length,
                if entry.status_flags & O_NONBLOCK != 0 {
                    Some(Duration::ZERO)
                } else {
                    timeout.or_else(|| self.blocking_read_timeout())
                },
            )?);
        }

        if is_proc_path(entry.description.path()) {
            let bytes = self.proc_read_file_from_open_path(Some(pid), entry.description.path())?;
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
        let bytes = VirtualFileSystem::pread(
            &mut self.filesystem,
            entry.description.path(),
            cursor,
            length,
        )?;
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

        if self.pipes.is_pipe(entry.description.id()) {
            return match self.pipes.write_with_mode(
                entry.description.id(),
                data,
                entry.status_flags & O_NONBLOCK != 0,
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

        if is_proc_path(entry.description.path()) {
            return Err(read_only_filesystem_error(entry.description.path()));
        }

        let path = entry.description.path().to_owned();
        let current_size = self.current_storage_file_size(&path)?;
        let cursor = entry.description.cursor() as usize;
        if entry.description.flags() & O_APPEND != 0 {
            let required_size = current_size.max(checked_write_end(current_size, data.len())?);
            self.check_path_resize_limits(&path, required_size)?;
            let new_len = VirtualFileSystem::append_file(&mut self.filesystem, &path, data)?;
            entry.description.set_cursor(new_len);
            return Ok(data.len());
        }

        let required_size = current_size.max(checked_write_end(cursor as u64, data.len())?);
        self.check_path_resize_limits(&path, required_size)?;

        let mut existing = if VirtualFileSystem::exists(&self.filesystem, &path) {
            VirtualFileSystem::read_file(&mut self.filesystem, &path)?
        } else {
            Vec::new()
        };
        if cursor > existing.len() {
            existing.resize(cursor, 0);
        }

        let new_len = cursor.saturating_add(data.len());
        if new_len > existing.len() {
            existing.resize(new_len, 0);
        }
        existing[cursor..new_len].copy_from_slice(data);
        VirtualFileSystem::write_file(&mut self.filesystem, &path, existing)?;
        entry.description.set_cursor(new_len as u64);
        Ok(data.len())
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

        if self.pipes.is_pipe(entry.description.id()) || self.ptys.is_pty(entry.description.id()) {
            return Err(KernelError::new("ESPIPE", "illegal seek"));
        }

        let base = match whence {
            SEEK_SET => 0_i128,
            SEEK_CUR => i128::from(entry.description.cursor()),
            SEEK_END => {
                let size = if is_proc_path(entry.description.path()) {
                    self.proc_stat_from_open_path(Some(pid), entry.description.path())?
                        .size
                } else {
                    self.filesystem.stat(entry.description.path())?.size
                };
                i128::from(size)
            }
            _ => {
                return Err(KernelError::new(
                    "EINVAL",
                    format!("invalid whence {whence}"),
                ))
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

        if self.pipes.is_pipe(entry.description.id()) || self.ptys.is_pty(entry.description.id()) {
            return Err(KernelError::new("ESPIPE", "illegal seek"));
        }

        if is_proc_path(entry.description.path()) {
            let bytes = self.proc_read_file_from_open_path(Some(pid), entry.description.path())?;
            let start = usize::try_from(offset)
                .map_err(|_| KernelError::new("EINVAL", "pread offset out of range"))?;
            let end = start.saturating_add(length).min(bytes.len());
            return Ok(if start >= bytes.len() {
                Vec::new()
            } else {
                bytes[start..end].to_vec()
            });
        }

        Ok(VirtualFileSystem::pread(
            &mut self.filesystem,
            entry.description.path(),
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

        if self.pipes.is_pipe(entry.description.id()) || self.ptys.is_pty(entry.description.id()) {
            return Err(KernelError::new("ESPIPE", "illegal seek"));
        }

        if is_proc_path(entry.description.path()) {
            return Err(read_only_filesystem_error(entry.description.path()));
        }

        let required_size = self
            .current_storage_file_size(entry.description.path())?
            .max(checked_write_end(offset, data.len())?);
        self.check_path_resize_limits(entry.description.path(), required_size)?;
        VirtualFileSystem::pwrite(
            &mut self.filesystem,
            entry.description.path(),
            data.to_vec(),
            offset,
        )?;
        Ok(data.len())
    }

    pub fn fd_dup(&mut self, requester_driver: &str, pid: u32, fd: u32) -> KernelResult<u32> {
        self.assert_driver_owns(requester_driver, pid)?;
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
        let replaced = {
            let mut tables = lock_or_recover(&self.fd_tables);
            let table = tables
                .get_mut(pid)
                .ok_or_else(|| KernelError::no_such_process(pid))?;
            let replaced = if old_fd == new_fd {
                None
            } else {
                table.get(new_fd).cloned()
            };
            table.dup2(old_fd, new_fd)?;
            replaced
        };

        if let Some(entry) = replaced {
            self.close_special_resource_if_needed(&entry.description, entry.filetype);
        }
        Ok(())
    }

    pub fn fd_close(&mut self, requester_driver: &str, pid: u32, fd: u32) -> KernelResult<()> {
        self.assert_driver_owns(requester_driver, pid)?;
        let (description, filetype) = {
            let mut tables = lock_or_recover(&self.fd_tables);
            let table = tables
                .get_mut(pid)
                .ok_or_else(|| KernelError::no_such_process(pid))?;
            let entry = table
                .get(fd)
                .cloned()
                .ok_or_else(|| KernelError::bad_file_descriptor(fd))?;
            table.close(fd);
            (entry.description, entry.filetype)
        };
        self.close_special_resource_if_needed(&description, filetype);
        Ok(())
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

        if entry.filetype != FILETYPE_REGULAR_FILE {
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

    pub fn fd_stat(&self, requester_driver: &str, pid: u32, fd: u32) -> KernelResult<FdStat> {
        self.assert_driver_owns(requester_driver, pid)?;
        let tables = lock_or_recover(&self.fd_tables);
        Ok(tables
            .get(pid)
            .ok_or_else(|| KernelError::no_such_process(pid))?
            .stat(fd)?)
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

    pub fn kill_process(&self, requester_driver: &str, pid: u32, signal: i32) -> KernelResult<()> {
        self.assert_driver_owns(requester_driver, pid)?;
        self.processes.kill(pid as i32, signal)?;
        Ok(())
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

        if self.pipes.is_pipe(entry.description.id()) || self.ptys.is_pty(entry.description.id()) {
            return Ok(synthetic_character_device_stat(entry.description.id()));
        }

        if is_proc_path(entry.description.path()) {
            return self.proc_stat_from_open_path(Some(pid), entry.description.path());
        }

        Ok(self.filesystem.stat(entry.description.path())?)
    }

    pub fn dispose(&mut self) -> KernelResult<()> {
        if self.terminated {
            return Ok(());
        }

        self.processes.terminate_all();
        let pids = lock_or_recover(&self.fd_tables).pids();
        for pid in pids {
            self.cleanup_process_resources(pid);
        }
        lock_or_recover(&self.driver_pids).clear();
        self.terminated = true;
        Ok(())
    }

    fn prepare_fd_open(
        &mut self,
        path: &str,
        flags: u32,
    ) -> KernelResult<(u8, Option<FileLockTarget>)> {
        if flags & O_CREAT != 0 && flags & O_EXCL != 0 {
            self.check_write_file_limits(path, 0)?;
            VirtualFileSystem::create_file_exclusive(&mut self.filesystem, path, Vec::new())?;
            let stat = VirtualFileSystem::stat(&mut self.filesystem, path)?;
            return Ok((
                filetype_for_path(path, &stat),
                Some(FileLockTarget::new(stat.ino)),
            ));
        }

        let exists = self.filesystem.exists(path)?;
        if exists {
            if flags & O_TRUNC != 0 {
                self.check_truncate_limits(path, 0)?;
                VirtualFileSystem::truncate(&mut self.filesystem, path, 0)?;
            }
        } else if flags & O_CREAT != 0 {
            self.check_write_file_limits(path, 0)?;
            VirtualFileSystem::write_file(&mut self.filesystem, path, Vec::new())?;
        } else {
            let _ = VirtualFileSystem::stat(&mut self.filesystem, path)?;
            unreachable!("stat should return an error when opening a missing path");
        }

        let stat = VirtualFileSystem::stat(&mut self.filesystem, path)?;
        Ok((
            filetype_for_path(path, &stat),
            Some(FileLockTarget::new(stat.ino)),
        ))
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
                    if socket.owner_pid() != pid {
                        return Err(KernelError::permission_denied(format!(
                            "process {pid} does not own socket {socket_id}"
                        )));
                    }
                    Ok(self.sockets.poll(socket_id, requested)?)
                } else {
                    Ok(POLLNVAL)
                }
            }
        }
    }

    fn poll_entry(
        &self,
        entry: &crate::fd_table::FdEntry,
        requested: PollEvents,
    ) -> KernelResult<PollEvents> {
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
            self.driver_pids.as_ref(),
            pid,
        );
    }

    fn resolve_spawn_command(
        &mut self,
        command: &str,
        args: &[String],
        cwd: &str,
    ) -> KernelResult<ResolvedSpawnCommand> {
        if let Some(driver) = self.commands.resolve(command).cloned() {
            return Ok(ResolvedSpawnCommand {
                command: command.to_owned(),
                args: args.to_vec(),
                driver,
            });
        }

        let Some(path) = self.resolve_executable_path(command, cwd)? else {
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
    ) -> KernelResult<Option<String>> {
        if !command.contains('/') {
            return Ok(None);
        }

        let path = if command.starts_with('/') {
            normalize_path(command)
        } else {
            normalize_path(&format!("{cwd}/{command}"))
        };
        let stat = self.filesystem.stat(&path)?;
        if stat.is_directory {
            return Err(KernelError::new(
                "EACCES",
                format!("permission denied, execute '{path}'"),
            ));
        }
        if stat.mode & EXECUTABLE_PERMISSION_BITS == 0 {
            return Err(KernelError::new(
                "EACCES",
                format!("permission denied, execute '{path}'"),
            ));
        }
        Ok(Some(path))
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
                ))
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

        Ok(self.filesystem.read_file(path)?)
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

        if parts == ["mounts"] {
            return Ok(Some(ProcNode::MountsFile));
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
            ProcNode::PidCmdline { pid } => Ok(self.proc_cmdline_bytes(*pid)),
            ProcNode::PidEnviron { pid } => Ok(self.proc_environ_bytes(*pid)),
            ProcNode::PidStatFile { pid } => Ok(self.proc_stat_bytes(*pid)),
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
                entries.push(String::from("mounts"));
                entries.push(String::from("self"));
                entries.sort();
                Ok(entries)
            }
            ProcNode::PidDir { .. } => Ok(vec![
                String::from("cmdline"),
                String::from("cwd"),
                String::from("environ"),
                String::from("fd"),
                String::from("stat"),
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
            ProcNode::PidFdLink { pid, fd } => {
                Ok(self.proc_fd_entry(*pid, *fd)?.description.path().to_owned())
            }
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
            ProcNode::SelfLink { pid } => format!("/proc/{pid}"),
            ProcNode::PidDir { pid } => format!("/proc/{pid}"),
            ProcNode::PidFdDir { pid } => format!("/proc/{pid}/fd"),
            ProcNode::PidCmdline { pid } => format!("/proc/{pid}/cmdline"),
            ProcNode::PidEnviron { pid } => format!("/proc/{pid}/environ"),
            ProcNode::PidCwdLink { pid } => format!("/proc/{pid}/cwd"),
            ProcNode::PidStatFile { pid } => format!("/proc/{pid}/stat"),
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
                read_only: false,
            }]
        };

        mounts
            .into_iter()
            .map(|mount| {
                let options = if mount.read_only { "ro" } else { "rw" };
                format!(
                    "{source} {target} {fstype} {options} 0 0\n",
                    source = mount.plugin_id,
                    target = mount.path,
                    fstype = mount.plugin_id,
                )
            })
            .collect::<String>()
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
        let filesystem = self.raw_filesystem_mut();
        let filesystem_any = filesystem as &mut dyn Any;
        if let Some(mount_table) = filesystem_any.downcast_mut::<MountTable>() {
            return Ok(mount_table.root_usage()?);
        }
        Ok(measure_filesystem_usage(filesystem)?)
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

    fn apply_creation_mode(&mut self, path: &str, mode: u32, umask: u32) -> KernelResult<()> {
        let masked_mode = (mode & !0o777) | ((mode & 0o777) & !(umask & 0o777));
        Ok(self.filesystem.chmod(path, masked_mode)?)
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
        if is_virtual_device_storage_path(path) {
            return Ok(());
        }

        let usage = self.filesystem_usage()?;
        if let Some(existing) = self.storage_stat(path)? {
            if existing.is_directory {
                return Ok(());
            }

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

        let new_inodes =
            count_missing_directory_components(self.raw_filesystem_mut(), path, false)?
                .saturating_add(1);
        self.resources.check_filesystem_usage(
            &usage,
            usage.total_bytes.saturating_add(new_size),
            usage.inode_count.saturating_add(new_inodes),
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

    fn check_truncate_limits(&mut self, path: &str, length: u64) -> KernelResult<()> {
        self.check_path_resize_limits(path, length)
    }

    fn check_path_resize_limits(&mut self, path: &str, new_size: u64) -> KernelResult<()> {
        if is_virtual_device_storage_path(path) {
            return Ok(());
        }

        let Some(existing) = self.storage_stat(path)? else {
            return Ok(());
        };
        if existing.is_directory {
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
            description,
            filetype,
        );
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
            .map_err(KernelError::from)
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
            .map_err(KernelError::from)
    }

    pub fn unmount_filesystem(&mut self, path: &str) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.check_mount_permissions(path)?;
        self.filesystem
            .inner_mut()
            .inner_mut()
            .unmount(path)
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
        let root = self
            .root_filesystem_mut()
            .ok_or_else(|| KernelError::new("EINVAL", "native root filesystem is not available"))?;
        root.snapshot().map_err(KernelError::from)
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

fn is_proc_path(path: &str) -> bool {
    let normalized = normalize_path(path);
    normalized == "/proc" || normalized.starts_with("/proc/")
}

fn checked_write_end(offset: u64, len: usize) -> KernelResult<u64> {
    offset
        .checked_add(len as u64)
        .ok_or_else(|| KernelError::new("EINVAL", "write offset out of range"))
}

fn filetype_for_path(path: &str, stat: &VirtualStat) -> u8 {
    if stat.is_directory {
        FILETYPE_DIRECTORY
    } else if path.starts_with("/dev/") {
        FILETYPE_CHARACTER_DEVICE
    } else if stat.is_symbolic_link {
        FILETYPE_SYMBOLIC_LINK
    } else {
        FILETYPE_REGULAR_FILE
    }
}

fn synthetic_character_device_stat(ino: u64) -> VirtualStat {
    let now = now_ms();
    VirtualStat {
        mode: 0o666,
        size: 0,
        blocks: 0,
        dev: 2,
        rdev: 0,
        is_directory: false,
        is_symbolic_link: false,
        atime_ms: now,
        mtime_ms: now,
        ctime_ms: now,
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
        mode: 0o555,
        size: 0,
        blocks: 0,
        dev: 3,
        rdev: 0,
        is_directory: true,
        is_symbolic_link: false,
        atime_ms: now,
        mtime_ms: now,
        ctime_ms: now,
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
        mode: 0o444,
        size,
        blocks: if size == 0 { 0 } else { size.div_ceil(512) },
        dev: 3,
        rdev: 0,
        is_directory: false,
        is_symbolic_link: false,
        atime_ms: now,
        mtime_ms: now,
        ctime_ms: now,
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
        mode: 0o777,
        size,
        blocks: if size == 0 { 0 } else { size.div_ceil(512) },
        dev: 3,
        rdev: 0,
        is_directory: false,
        is_symbolic_link: true,
        atime_ms: now,
        mtime_ms: now,
        ctime_ms: now,
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
        | ProcNode::PidCmdline { .. }
        | ProcNode::PidEnviron { .. }
        | ProcNode::PidStatFile { .. } => FILETYPE_REGULAR_FILE,
    }
}

fn proc_inode(node: &ProcNode) -> u64 {
    match node {
        ProcNode::RootDir => 0xfffe_0001,
        ProcNode::MountsFile => 0xfffe_0002,
        ProcNode::SelfLink { pid } => 0xfffe_1000 + u64::from(*pid),
        ProcNode::PidDir { pid } => 0xfffe_2000 + u64::from(*pid),
        ProcNode::PidFdDir { pid } => 0xfffe_3000 + u64::from(*pid),
        ProcNode::PidCmdline { pid } => 0xfffe_4000 + u64::from(*pid),
        ProcNode::PidEnviron { pid } => 0xfffe_5000 + u64::from(*pid),
        ProcNode::PidCwdLink { pid } => 0xfffe_6000 + u64::from(*pid),
        ProcNode::PidStatFile { pid } => 0xfffe_7000 + u64::from(*pid),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::MemoryFileSystem;

    #[test]
    fn setpgid_rejects_joining_a_process_group_owned_by_another_driver() {
        let kernel = KernelVm::new(MemoryFileSystem::new(), KernelVmConfig::new("vm-setpgid"));

        let leader_pid = kernel.processes.allocate_pid();
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
            },
            Arc::new(StubDriverProcess::default()),
        );

        let peer_pid = kernel.processes.allocate_pid();
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
}
