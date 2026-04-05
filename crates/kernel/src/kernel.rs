use crate::bridge::LifecycleState;
use crate::command_registry::{CommandDriver, CommandRegistry};
use crate::device_layer::{create_device_layer, DeviceLayer};
use crate::fd_table::{
    FdStat, FdTableError, FdTableManager, FileDescription, ProcessFdTable,
    FILETYPE_CHARACTER_DEVICE, FILETYPE_DIRECTORY, FILETYPE_PIPE, FILETYPE_REGULAR_FILE,
    FILETYPE_SYMBOLIC_LINK, O_APPEND, O_CREAT, O_EXCL, O_TRUNC,
};
use crate::mount_table::{MountEntry, MountOptions, MountTable, MountedFileSystem};
use crate::permissions::{
    check_command_execution, FsOperation, PermissionError, PermissionedFileSystem, Permissions,
};
use crate::pipe_manager::{PipeError, PipeManager};
use crate::process_table::{
    DriverProcess, ProcessContext, ProcessExitCallback, ProcessInfo, ProcessStatus, ProcessTable,
    ProcessTableError, ProcessWaitResult, SIGPIPE,
};
use crate::pty::{LineDisciplineConfig, PartialTermios, PtyError, PtyManager, Termios};
use crate::resource_accounting::{
    measure_filesystem_usage, FileSystemUsage, ResourceAccountant, ResourceError, ResourceLimits,
    ResourceSnapshot,
};
use crate::root_fs::{RootFileSystem, RootFilesystemError, RootFilesystemSnapshot};
use crate::user::UserManager;
use crate::vfs::{normalize_path, VfsError, VfsResult, VirtualFileSystem, VirtualStat};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, WaitTimeoutResult};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub type KernelResult<T> = Result<T, KernelError>;
pub use crate::process_table::{ProcessWaitEvent as WaitPidEvent, WaitPidFlags};

pub const SEEK_SET: u8 = 0;
pub const SEEK_CUR: u8 = 1;
pub const SEEK_END: u8 = 2;

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
    pub permissions: Permissions,
    pub resources: ResourceLimits,
    pub zombie_ttl: Duration,
}

impl KernelVmConfig {
    pub fn new(vm_id: impl Into<String>) -> Self {
        Self {
            vm_id: vm_id.into(),
            env: BTreeMap::new(),
            cwd: String::from("/home/user"),
            permissions: Permissions::default(),
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
    env: BTreeMap<String, String>,
    cwd: String,
    commands: CommandRegistry,
    fd_tables: Arc<Mutex<FdTableManager>>,
    processes: ProcessTable,
    pipes: PipeManager,
    ptys: PtyManager,
    users: UserManager,
    resources: ResourceAccountant,
    driver_pids: Arc<Mutex<BTreeMap<String, BTreeSet<u32>>>>,
    terminated: bool,
}

fn cleanup_process_resources(
    fd_tables: &Mutex<FdTableManager>,
    pipes: &PipeManager,
    ptys: &PtyManager,
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
        close_special_resource_if_needed(pipes, ptys, &description, filetype);
    }

    let mut owners = lock_or_recover(driver_pids);
    for pids in owners.values_mut() {
        pids.remove(&pid);
    }
}

fn close_special_resource_if_needed(
    pipes: &PipeManager,
    ptys: &PtyManager,
    description: &Arc<FileDescription>,
    filetype: u8,
) {
    if description.ref_count() != 0 {
        return;
    }

    if filetype == FILETYPE_PIPE && pipes.is_pipe(description.id()) {
        pipes.close(description.id());
    }

    if ptys.is_pty(description.id()) {
        ptys.close(description.id());
    }
}

impl<F: VirtualFileSystem> KernelVm<F> {
    pub fn new(filesystem: F, config: KernelVmConfig) -> Self {
        let vm_id = config.vm_id;
        let permissions = config.permissions.clone();
        let process_table = ProcessTable::with_zombie_ttl(config.zombie_ttl);
        let process_table_for_pty = process_table.clone();
        let fd_tables = Arc::new(Mutex::new(FdTableManager::new()));
        let driver_pids = Arc::new(Mutex::new(BTreeMap::new()));
        let pipes = PipeManager::new();
        let ptys = PtyManager::with_signal_handler(Arc::new(move |pgid, signal| {
            let _ = process_table_for_pty.kill(-(pgid as i32), signal);
        }));

        let fd_tables_for_exit = Arc::clone(&fd_tables);
        let driver_pids_for_exit = Arc::clone(&driver_pids);
        let pipes_for_exit = pipes.clone();
        let ptys_for_exit = ptys.clone();
        process_table.set_on_process_exit(Some(Arc::new(move |pid| {
            cleanup_process_resources(
                fd_tables_for_exit.as_ref(),
                &pipes_for_exit,
                &ptys_for_exit,
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
            env: config.env,
            cwd: config.cwd,
            commands: CommandRegistry::new(),
            fd_tables,
            processes: process_table,
            pipes,
            ptys,
            users: UserManager::new(),
            resources: ResourceAccountant::new(config.resources),
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

    pub fn resource_snapshot(&self) -> ResourceSnapshot {
        let fd_tables = lock_or_recover(&self.fd_tables);
        self.resources
            .snapshot(&self.processes, &fd_tables, &self.pipes, &self.ptys)
    }

    pub fn resource_limits(&self) -> &ResourceLimits {
        self.resources.limits()
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
        Ok(self.filesystem.read_file(path)?)
    }

    pub fn write_file(&mut self, path: &str, content: impl Into<Vec<u8>>) -> KernelResult<()> {
        self.assert_not_terminated()?;
        let content = content.into();
        self.check_write_file_limits(path, content.len() as u64)?;
        Ok(self.filesystem.write_file(path, content)?)
    }

    pub fn create_dir(&mut self, path: &str) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.check_create_dir_limits(path)?;
        Ok(self.filesystem.create_dir(path)?)
    }

    pub fn mkdir(&mut self, path: &str, recursive: bool) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.check_mkdir_limits(path, recursive)?;
        Ok(self.filesystem.mkdir(path, recursive)?)
    }

    pub fn exists(&self, path: &str) -> KernelResult<bool> {
        self.assert_not_terminated()?;
        Ok(self.filesystem.exists(path)?)
    }

    pub fn stat(&mut self, path: &str) -> KernelResult<VirtualStat> {
        self.assert_not_terminated()?;
        Ok(self.filesystem.stat(path)?)
    }

    pub fn lstat(&self, path: &str) -> KernelResult<VirtualStat> {
        self.assert_not_terminated()?;
        Ok(self.filesystem.lstat(path)?)
    }

    pub fn read_link(&self, path: &str) -> KernelResult<String> {
        self.assert_not_terminated()?;
        Ok(self.filesystem.read_link(path)?)
    }

    pub fn read_dir(&mut self, path: &str) -> KernelResult<Vec<String>> {
        self.assert_not_terminated()?;
        let entries = if let Some(limit) = self.resources.max_readdir_entries() {
            self.filesystem.read_dir_limited(path, limit)?
        } else {
            self.filesystem.read_dir(path)?
        };
        self.resources.check_readdir_entries(entries.len())?;
        Ok(entries)
    }

    pub fn remove_file(&mut self, path: &str) -> KernelResult<()> {
        self.assert_not_terminated()?;
        Ok(self.filesystem.remove_file(path)?)
    }

    pub fn remove_dir(&mut self, path: &str) -> KernelResult<()> {
        self.assert_not_terminated()?;
        Ok(self.filesystem.remove_dir(path)?)
    }

    pub fn rename(&mut self, old_path: &str, new_path: &str) -> KernelResult<()> {
        self.assert_not_terminated()?;
        Ok(self.filesystem.rename(old_path, new_path)?)
    }

    pub fn realpath(&self, path: &str) -> KernelResult<String> {
        self.assert_not_terminated()?;
        Ok(self.filesystem.realpath(path)?)
    }

    pub fn symlink(&mut self, target: &str, link_path: &str) -> KernelResult<()> {
        self.assert_not_terminated()?;
        self.check_symlink_limits(target, link_path)?;
        Ok(self.filesystem.symlink(target, link_path)?)
    }

    pub fn chmod(&mut self, path: &str, mode: u32) -> KernelResult<()> {
        self.assert_not_terminated()?;
        Ok(self.filesystem.chmod(path, mode)?)
    }

    pub fn link(&mut self, old_path: &str, new_path: &str) -> KernelResult<()> {
        self.assert_not_terminated()?;
        Ok(self.filesystem.link(old_path, new_path)?)
    }

    pub fn chown(&mut self, path: &str, uid: u32, gid: u32) -> KernelResult<()> {
        self.assert_not_terminated()?;
        Ok(self.filesystem.chown(path, uid, gid)?)
    }

    pub fn utimes(&mut self, path: &str, atime_ms: u64, mtime_ms: u64) -> KernelResult<()> {
        self.assert_not_terminated()?;
        Ok(self.filesystem.utimes(path, atime_ms, mtime_ms)?)
    }

    pub fn truncate(&mut self, path: &str, length: u64) -> KernelResult<()> {
        self.assert_not_terminated()?;
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
        let driver = self
            .commands
            .resolve(command)
            .cloned()
            .ok_or_else(|| KernelError::command_not_found(command))?;

        if let (Some(requester), Some(parent_pid)) =
            (options.requester_driver.as_deref(), options.parent_pid)
        {
            self.assert_driver_owns(requester, parent_pid)?;
        }

        self.resources.check_process_argv_bytes(command, &args)?;
        self.resources
            .check_process_env_bytes(&self.env, &options.env)?;

        let mut env = self.env.clone();
        env.extend(options.env.clone());
        let cwd = options.cwd.clone().unwrap_or_else(|| self.cwd.clone());
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

        let pid = self.processes.allocate_pid();
        {
            let mut tables = lock_or_recover(&self.fd_tables);
            if let Some(parent_pid) = options.parent_pid {
                tables.fork(parent_pid, pid);
            } else {
                tables.create(pid);
            }
        }

        let process = Arc::new(StubDriverProcess::default());
        let driver_name = driver.name().to_owned();
        self.processes.register(
            pid,
            driver_name.clone(),
            command.to_owned(),
            args,
            ProcessContext {
                pid,
                ppid: options.parent_pid.unwrap_or(0),
                env,
                cwd,
                fds: Default::default(),
            },
            process.clone(),
        );

        let mut owners = lock_or_recover(&self.driver_pids);
        owners.entry(driver_name.clone()).or_default().insert(pid);
        if let Some(requester) = options.requester_driver {
            owners.entry(requester).or_default().insert(pid);
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

    pub fn fd_open(
        &mut self,
        requester_driver: &str,
        pid: u32,
        path: &str,
        flags: u32,
        _mode: Option<u32>,
    ) -> KernelResult<u32> {
        self.assert_not_terminated()?;
        self.assert_driver_owns(requester_driver, pid)?;
        if let Some(existing_fd) = parse_dev_fd_path(path)? {
            let mut tables = lock_or_recover(&self.fd_tables);
            let table = tables
                .get_mut(pid)
                .ok_or_else(|| KernelError::no_such_process(pid))?;
            return Ok(table.dup(existing_fd)?);
        }

        let filetype = self.prepare_fd_open(path, flags)?;
        let mut tables = lock_or_recover(&self.fd_tables);
        let table = tables
            .get_mut(pid)
            .ok_or_else(|| KernelError::no_such_process(pid))?;
        Ok(table.open_with_filetype(path, flags, filetype)?)
    }

    pub fn fd_read(
        &mut self,
        requester_driver: &str,
        pid: u32,
        fd: u32,
        length: usize,
    ) -> KernelResult<Vec<u8>> {
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
            return Ok(self
                .pipes
                .read_with_timeout(entry.description.id(), length, self.blocking_read_timeout())?
                .unwrap_or_default());
        }

        if self.ptys.is_pty(entry.description.id()) {
            return Ok(self
                .ptys
                .read_with_timeout(entry.description.id(), length, self.blocking_read_timeout())?
                .unwrap_or_default());
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
        Ok(bytes)
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
            return match self.pipes.write(entry.description.id(), data) {
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

        let path = entry.description.path().to_owned();
        let current_size = self.current_storage_file_size(&path)?;
        let mut cursor = entry.description.cursor() as usize;
        if entry.description.flags() & O_APPEND != 0 {
            cursor = current_size as usize;
        }
        let required_size = current_size.max(checked_write_end(cursor as u64, data.len())?);
        self.check_path_resize_limits(&path, required_size)?;

        let mut existing = if VirtualFileSystem::exists(&self.filesystem, &path) {
            VirtualFileSystem::read_file(&mut self.filesystem, &path)?
        } else {
            Vec::new()
        };
        if entry.description.flags() & O_APPEND != 0 {
            cursor = existing.len();
        }
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
            SEEK_END => i128::from(self.filesystem.stat(entry.description.path())?.size),
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

    fn prepare_fd_open(&mut self, path: &str, flags: u32) -> KernelResult<u8> {
        let exists = self.filesystem.exists(path)?;
        if exists {
            if flags & O_CREAT != 0 && flags & O_EXCL != 0 {
                return Err(KernelError::new(
                    "EEXIST",
                    format!("file already exists: {path}"),
                ));
            }
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
        Ok(filetype_for_path(path, &stat))
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
            &self.pipes,
            &self.ptys,
            self.driver_pids.as_ref(),
            pid,
        );
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

    fn filesystem_usage(&mut self) -> KernelResult<FileSystemUsage> {
        Ok(measure_filesystem_usage(self.raw_filesystem_mut())?)
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
        close_special_resource_if_needed(&self.pipes, &self.ptys, description, filetype);
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
        if signal == crate::process_table::SIGCHLD {
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

impl From<RootFilesystemError> for KernelError {
    fn from(error: RootFilesystemError) -> Self {
        map_error("EINVAL", error.to_string())
    }
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
                fds: Default::default(),
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
                fds: Default::default(),
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
