use agent_os_kernel::command_registry::CommandDriver;
use agent_os_kernel::fd_table::{
    LOCK_EX, LOCK_NB, LOCK_SH, LOCK_UN, O_APPEND, O_CREAT, O_EXCL, O_NONBLOCK, O_RDWR,
};
use agent_os_kernel::kernel::{
    ExecOptions, KernelVm, KernelVmConfig, OpenShellOptions, SpawnOptions, WaitPidFlags,
    WaitPidResult, SEEK_SET,
};
use agent_os_kernel::mount_table::{MountOptions, MountTable};
use agent_os_kernel::permissions::Permissions;
use agent_os_kernel::pipe_manager::MAX_PIPE_BUFFER_BYTES;
use agent_os_kernel::process_table::{ProcessWaitEvent, SIGWINCH};
use agent_os_kernel::vfs::{
    MemoryFileSystem, VfsResult, VirtualDirEntry, VirtualFileSystem, VirtualStat,
};
use std::cell::{Cell, RefCell};

fn assert_kernel_error_code<T: std::fmt::Debug>(
    result: agent_os_kernel::kernel::KernelResult<T>,
    expected: &str,
) {
    let error = result.expect_err("operation should fail");
    assert_eq!(error.code(), expected);
}

fn spawn_shell(
    kernel: &mut KernelVm<MemoryFileSystem>,
) -> agent_os_kernel::kernel::KernelProcessHandle {
    kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                ..SpawnOptions::default()
            },
        )
        .expect("spawn shell")
}

fn spawn_shell_in<F: VirtualFileSystem + 'static>(
    kernel: &mut KernelVm<F>,
) -> agent_os_kernel::kernel::KernelProcessHandle {
    kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                ..SpawnOptions::default()
            },
        )
        .expect("spawn shell")
}

fn assert_not_trivial_pattern(bytes: &[u8]) {
    assert!(bytes.iter().any(|byte| *byte != 0));
    assert!(
        bytes.windows(2).any(|window| window[0] != window[1]),
        "random data should not collapse to a repeated byte"
    );
}

struct AtomicityProbeFileSystem {
    inner: RefCell<MemoryFileSystem>,
    exclusive_race_pending: Cell<bool>,
    append_race_pending: Cell<bool>,
    target_path: &'static str,
}

impl AtomicityProbeFileSystem {
    fn new(target_path: &'static str) -> Self {
        let mut inner = MemoryFileSystem::new();
        inner
            .write_file(target_path, Vec::new())
            .expect("seed append target");
        Self {
            inner: RefCell::new(inner),
            exclusive_race_pending: Cell::new(false),
            append_race_pending: Cell::new(false),
            target_path,
        }
    }

    fn trigger_exclusive_race(&self) {
        self.inner
            .borrow_mut()
            .remove_file(self.target_path)
            .expect("clear target before exclusive race");
        self.exclusive_race_pending.set(true);
    }

    fn trigger_append_race(&self) {
        self.inner
            .borrow_mut()
            .write_file(self.target_path, Vec::new())
            .expect("reset target before append race");
        self.append_race_pending.set(true);
    }
}

impl VirtualFileSystem for AtomicityProbeFileSystem {
    fn read_file(&mut self, path: &str) -> VfsResult<Vec<u8>> {
        self.inner.borrow_mut().read_file(path)
    }

    fn read_dir(&mut self, path: &str) -> VfsResult<Vec<String>> {
        self.inner.borrow_mut().read_dir(path)
    }

    fn read_dir_limited(&mut self, path: &str, max_entries: usize) -> VfsResult<Vec<String>> {
        self.inner.borrow_mut().read_dir_limited(path, max_entries)
    }

    fn read_dir_with_types(&mut self, path: &str) -> VfsResult<Vec<VirtualDirEntry>> {
        self.inner.borrow_mut().read_dir_with_types(path)
    }

    fn write_file(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<()> {
        let content = content.into();
        if path == self.target_path {
            if self.exclusive_race_pending.replace(false) {
                self.inner
                    .borrow_mut()
                    .write_file(path, b"winner".to_vec())
                    .expect("inject competing exclusive creator");
            }
            if self.append_race_pending.replace(false) {
                self.inner
                    .borrow_mut()
                    .write_file(path, b"RACE".to_vec())
                    .expect("inject competing append writer");
            }
        }
        self.inner.borrow_mut().write_file(path, content)
    }

    fn create_file_exclusive(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<()> {
        if path == self.target_path && self.exclusive_race_pending.replace(false) {
            self.inner
                .borrow_mut()
                .write_file(path, b"winner".to_vec())
                .expect("inject competing exclusive creator");
            return Err(agent_os_kernel::vfs::VfsError::new(
                "EEXIST",
                format!("file already exists, open '{path}'"),
            ));
        }
        self.inner.borrow_mut().create_file_exclusive(path, content)
    }

    fn append_file(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<u64> {
        if path == self.target_path && self.append_race_pending.replace(false) {
            self.inner
                .borrow_mut()
                .append_file(path, b"RACE".to_vec())
                .expect("inject competing append writer");
        }
        self.inner.borrow_mut().append_file(path, content)
    }

    fn create_dir(&mut self, path: &str) -> VfsResult<()> {
        self.inner.borrow_mut().create_dir(path)
    }

    fn mkdir(&mut self, path: &str, recursive: bool) -> VfsResult<()> {
        self.inner.borrow_mut().mkdir(path, recursive)
    }

    fn exists(&self, path: &str) -> bool {
        if path == self.target_path && self.exclusive_race_pending.get() {
            return false;
        }
        self.inner.borrow().exists(path)
    }

    fn stat(&mut self, path: &str) -> VfsResult<VirtualStat> {
        self.inner.borrow_mut().stat(path)
    }

    fn remove_file(&mut self, path: &str) -> VfsResult<()> {
        self.inner.borrow_mut().remove_file(path)
    }

    fn remove_dir(&mut self, path: &str) -> VfsResult<()> {
        self.inner.borrow_mut().remove_dir(path)
    }

    fn rename(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        self.inner.borrow_mut().rename(old_path, new_path)
    }

    fn realpath(&self, path: &str) -> VfsResult<String> {
        self.inner.borrow().realpath(path)
    }

    fn symlink(&mut self, target: &str, link_path: &str) -> VfsResult<()> {
        self.inner.borrow_mut().symlink(target, link_path)
    }

    fn read_link(&self, path: &str) -> VfsResult<String> {
        self.inner.borrow().read_link(path)
    }

    fn lstat(&self, path: &str) -> VfsResult<VirtualStat> {
        self.inner.borrow().lstat(path)
    }

    fn link(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        self.inner.borrow_mut().link(old_path, new_path)
    }

    fn chmod(&mut self, path: &str, mode: u32) -> VfsResult<()> {
        self.inner.borrow_mut().chmod(path, mode)
    }

    fn chown(&mut self, path: &str, uid: u32, gid: u32) -> VfsResult<()> {
        self.inner.borrow_mut().chown(path, uid, gid)
    }

    fn utimes(&mut self, path: &str, atime_ms: u64, mtime_ms: u64) -> VfsResult<()> {
        self.inner.borrow_mut().utimes(path, atime_ms, mtime_ms)
    }

    fn truncate(&mut self, path: &str, length: u64) -> VfsResult<()> {
        self.inner.borrow_mut().truncate(path, length)
    }

    fn pread(&mut self, path: &str, offset: u64, length: usize) -> VfsResult<Vec<u8>> {
        self.inner.borrow_mut().pread(path, offset, length)
    }
}

#[test]
fn kernel_fd_surface_supports_open_seek_positional_io_dup_and_dev_fd_views() {
    let mut config = KernelVmConfig::new("vm-api-fd");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");
    kernel
        .filesystem_mut()
        .write_file("/tmp/data.txt", b"hello".to_vec())
        .expect("seed file");

    let process = spawn_shell(&mut kernel);
    let fd = kernel
        .fd_open("shell", process.pid(), "/tmp/data.txt", O_RDWR, None)
        .expect("open existing file");
    let created_fd = kernel
        .fd_open(
            "shell",
            process.pid(),
            "/tmp/created.txt",
            O_CREAT | O_RDWR,
            None,
        )
        .expect("open created file");
    kernel
        .fd_write("shell", process.pid(), created_fd, b"created")
        .expect("write created file");
    assert_eq!(
        kernel
            .filesystem_mut()
            .read_file("/tmp/created.txt")
            .expect("read created file"),
        b"created".to_vec()
    );

    let entries = kernel
        .dev_fd_read_dir("shell", process.pid())
        .expect("list /dev/fd");
    assert!(entries.contains(&String::from("0")));
    assert!(entries.contains(&String::from("1")));
    assert!(entries.contains(&fd.to_string()));
    assert!(entries.contains(&created_fd.to_string()));

    let pread = kernel
        .fd_pread("shell", process.pid(), fd, 2, 1)
        .expect("pread from offset");
    assert_eq!(pread, b"el".to_vec());
    assert_eq!(
        kernel
            .fd_seek("shell", process.pid(), fd, 4, SEEK_SET)
            .expect("seek to byte 4"),
        4
    );

    let dup_fd = kernel
        .fd_dup("shell", process.pid(), fd)
        .expect("duplicate fd");
    let dup_read = kernel
        .fd_read("shell", process.pid(), dup_fd, 1)
        .expect("read through dup");
    assert_eq!(dup_read, b"o".to_vec());

    kernel
        .fd_dup2("shell", process.pid(), fd, 20)
        .expect("dup2 onto target fd");
    kernel
        .fd_seek("shell", process.pid(), 20, 0, SEEK_SET)
        .expect("seek dup2 target to start");
    let full = kernel
        .fd_read("shell", process.pid(), fd, 5)
        .expect("read full file");
    assert_eq!(full, b"hello".to_vec());

    kernel
        .fd_pwrite("shell", process.pid(), fd, b"X", 1)
        .expect("pwrite at offset");
    assert_eq!(
        kernel
            .filesystem_mut()
            .read_file("/tmp/data.txt")
            .expect("read updated file"),
        b"hXllo".to_vec()
    );

    let file_stat = kernel
        .dev_fd_stat("shell", process.pid(), fd)
        .expect("stat regular file fd");
    assert_eq!(file_stat.size, 5);
    assert!(!file_stat.is_directory);

    let (read_fd, write_fd) = kernel.open_pipe("shell", process.pid()).expect("open pipe");
    kernel
        .fd_write("shell", process.pid(), write_fd, b"pipe-data")
        .expect("write pipe");
    let dev_dup = kernel
        .fd_open(
            "shell",
            process.pid(),
            &format!("/dev/fd/{read_fd}"),
            0,
            None,
        )
        .expect("duplicate through /dev/fd");
    let pipe_bytes = kernel
        .fd_read("shell", process.pid(), dev_dup, 32)
        .expect("read duplicated pipe fd");
    assert_eq!(pipe_bytes, b"pipe-data".to_vec());

    let pipe_stat = kernel
        .dev_fd_stat("shell", process.pid(), read_fd)
        .expect("stat pipe fd");
    assert_eq!(pipe_stat.mode, 0o666);
    assert_eq!(pipe_stat.size, 0);
    assert!(!pipe_stat.is_directory);

    process.finish(0);
    kernel.waitpid(process.pid()).expect("wait for shell");
}

#[test]
fn kernel_fd_surface_reads_exact_byte_counts_from_device_nodes() {
    let mut config = KernelVmConfig::new("vm-api-fd-devices");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");

    let process = spawn_shell(&mut kernel);

    let zero_fd = kernel
        .fd_open("shell", process.pid(), "/dev/zero", O_RDWR, None)
        .expect("open /dev/zero");
    let zeroes = kernel
        .fd_read("shell", process.pid(), zero_fd, 5)
        .expect("read 5 bytes from /dev/zero");
    assert_eq!(zeroes.len(), 5);
    assert!(zeroes.iter().all(|byte| *byte == 0));

    let random_fd = kernel
        .fd_open("shell", process.pid(), "/dev/urandom", O_RDWR, None)
        .expect("open /dev/urandom");
    let random = kernel
        .fd_read("shell", process.pid(), random_fd, 1024 * 1024)
        .expect("read 1MiB from /dev/urandom");
    assert_eq!(random.len(), 1024 * 1024);
    assert_not_trivial_pattern(&random[..1024]);

    process.finish(0);
    kernel.waitpid(process.pid()).expect("wait for shell");
}

#[test]
fn kernel_fd_surface_supports_nonblocking_pipe_duplicates_via_dev_fd() {
    let mut config = KernelVmConfig::new("vm-api-fd-nonblock");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");

    let process = spawn_shell(&mut kernel);
    let (read_fd, write_fd) = kernel.open_pipe("shell", process.pid()).expect("open pipe");
    let nonblocking_read_fd = kernel
        .fd_open(
            "shell",
            process.pid(),
            &format!("/dev/fd/{read_fd}"),
            O_NONBLOCK,
            None,
        )
        .expect("duplicate read end with O_NONBLOCK");
    let nonblocking_write_fd = kernel
        .fd_open(
            "shell",
            process.pid(),
            &format!("/dev/fd/{write_fd}"),
            O_NONBLOCK,
            None,
        )
        .expect("duplicate write end with O_NONBLOCK");

    assert_eq!(
        kernel
            .fd_stat("shell", process.pid(), read_fd)
            .expect("stat blocking read fd")
            .flags
            & O_NONBLOCK,
        0
    );
    assert_ne!(
        kernel
            .fd_stat("shell", process.pid(), nonblocking_read_fd)
            .expect("stat nonblocking read fd")
            .flags
            & O_NONBLOCK,
        0
    );
    assert_ne!(
        kernel
            .fd_stat("shell", process.pid(), nonblocking_write_fd)
            .expect("stat nonblocking write fd")
            .flags
            & O_NONBLOCK,
        0
    );

    assert_kernel_error_code(
        kernel.fd_read("shell", process.pid(), nonblocking_read_fd, 1),
        "EAGAIN",
    );

    kernel
        .fd_write(
            "shell",
            process.pid(),
            write_fd,
            &vec![7; MAX_PIPE_BUFFER_BYTES],
        )
        .expect("fill pipe buffer");
    assert_kernel_error_code(
        kernel.fd_write("shell", process.pid(), nonblocking_write_fd, &[8]),
        "EAGAIN",
    );

    process.finish(0);
    kernel.waitpid(process.pid()).expect("wait for shell");
}

#[test]
fn kernel_fd_surface_uses_atomic_exclusive_create() {
    let target = "/tmp/race.txt";
    let filesystem = AtomicityProbeFileSystem::new(target);
    filesystem.trigger_exclusive_race();

    let mut config = KernelVmConfig::new("vm-api-exclusive-create");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(filesystem, config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");

    let process = spawn_shell_in(&mut kernel);
    assert_kernel_error_code(
        kernel.fd_open(
            "shell",
            process.pid(),
            target,
            O_CREAT | O_EXCL | O_RDWR,
            None,
        ),
        "EEXIST",
    );
    assert_eq!(
        kernel
            .filesystem_mut()
            .read_file(target)
            .expect("winner should remain visible"),
        b"winner".to_vec()
    );

    process.finish(0);
    kernel.waitpid(process.pid()).expect("wait shell");
}

#[test]
fn kernel_fd_surface_uses_atomic_append_writes() {
    let target = "/tmp/race.txt";
    let filesystem = AtomicityProbeFileSystem::new(target);
    filesystem.trigger_append_race();

    let mut config = KernelVmConfig::new("vm-api-append-write");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(filesystem, config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");

    let process = spawn_shell_in(&mut kernel);
    let fd = kernel
        .fd_open("shell", process.pid(), target, O_APPEND | O_RDWR, None)
        .expect("open append target");
    assert_eq!(
        kernel
            .fd_write("shell", process.pid(), fd, b"mine")
            .expect("append write"),
        4
    );
    assert_eq!(
        kernel
            .filesystem_mut()
            .read_file(target)
            .expect("read appended file"),
        b"RACEmine".to_vec()
    );

    process.finish(0);
    kernel.waitpid(process.pid()).expect("wait shell");
}

#[test]
fn kernel_fd_surface_supports_advisory_locks_and_releases_on_last_close() {
    let mut config = KernelVmConfig::new("vm-api-flock-close");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");
    kernel
        .filesystem_mut()
        .write_file("/tmp/lock.txt", b"lock".to_vec())
        .expect("seed file");

    let owner = spawn_shell(&mut kernel);
    let contender = spawn_shell(&mut kernel);
    let owner_fd = kernel
        .fd_open("shell", owner.pid(), "/tmp/lock.txt", O_RDWR, None)
        .expect("owner opens lock file");
    let owner_dup = kernel
        .fd_dup("shell", owner.pid(), owner_fd)
        .expect("duplicate owner fd");
    let contender_fd = kernel
        .fd_open("shell", contender.pid(), "/tmp/lock.txt", O_RDWR, None)
        .expect("contender opens lock file");

    kernel
        .fd_flock("shell", owner.pid(), owner_fd, LOCK_EX)
        .expect("owner acquires exclusive lock");
    kernel
        .fd_flock("shell", owner.pid(), owner_dup, LOCK_EX | LOCK_NB)
        .expect("duplicate shares exclusive lock");
    assert_kernel_error_code(
        kernel.fd_flock("shell", contender.pid(), contender_fd, LOCK_SH | LOCK_NB),
        "EWOULDBLOCK",
    );

    kernel
        .fd_close("shell", owner.pid(), owner_fd)
        .expect("close original owner fd");
    assert_kernel_error_code(
        kernel.fd_flock("shell", contender.pid(), contender_fd, LOCK_SH | LOCK_NB),
        "EWOULDBLOCK",
    );

    kernel
        .fd_close("shell", owner.pid(), owner_dup)
        .expect("close duplicate owner fd");
    kernel
        .fd_flock("shell", contender.pid(), contender_fd, LOCK_SH | LOCK_NB)
        .expect("lock released on last close");
    kernel
        .fd_flock("shell", contender.pid(), contender_fd, LOCK_UN)
        .expect("unlock contender");

    owner.finish(0);
    contender.finish(0);
    kernel.waitpid(owner.pid()).expect("wait owner");
    kernel.waitpid(contender.pid()).expect("wait contender");
}

#[test]
fn kernel_fd_surface_supports_shared_locks_and_nonblocking_upgrade_conflicts() {
    let mut config = KernelVmConfig::new("vm-api-flock-shared");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");
    kernel
        .filesystem_mut()
        .write_file("/tmp/shared-lock.txt", b"shared".to_vec())
        .expect("seed file");

    let first = spawn_shell(&mut kernel);
    let second = spawn_shell(&mut kernel);
    let first_fd = kernel
        .fd_open("shell", first.pid(), "/tmp/shared-lock.txt", O_RDWR, None)
        .expect("first opens file");
    let second_fd = kernel
        .fd_open("shell", second.pid(), "/tmp/shared-lock.txt", O_RDWR, None)
        .expect("second opens file");

    kernel
        .fd_flock("shell", first.pid(), first_fd, LOCK_SH)
        .expect("first shared lock");
    kernel
        .fd_flock("shell", second.pid(), second_fd, LOCK_SH)
        .expect("second shared lock");
    assert_kernel_error_code(
        kernel.fd_flock("shell", first.pid(), first_fd, LOCK_EX | LOCK_NB),
        "EWOULDBLOCK",
    );

    kernel
        .fd_flock("shell", second.pid(), second_fd, LOCK_UN)
        .expect("unlock second shared lock");
    kernel
        .fd_flock("shell", first.pid(), first_fd, LOCK_EX | LOCK_NB)
        .expect("first upgrades to exclusive once peer unlocks");

    first.finish(0);
    second.finish(0);
    kernel.waitpid(first.pid()).expect("wait first");
    kernel.waitpid(second.pid()).expect("wait second");
}

#[test]
fn kernel_fd_surface_shares_advisory_locks_across_fork_inherited_fds() {
    let mut config = KernelVmConfig::new("vm-api-flock-fork");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");
    kernel
        .filesystem_mut()
        .write_file("/tmp/fork-lock.txt", b"fork".to_vec())
        .expect("seed file");

    let parent = spawn_shell(&mut kernel);
    let inherited_fd = kernel
        .fd_open("shell", parent.pid(), "/tmp/fork-lock.txt", O_RDWR, None)
        .expect("parent opens file");
    kernel
        .fd_flock("shell", parent.pid(), inherited_fd, LOCK_EX)
        .expect("parent acquires exclusive lock");

    let child = kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                parent_pid: Some(parent.pid()),
                ..SpawnOptions::default()
            },
        )
        .expect("spawn child with inherited fds");
    let contender = spawn_shell(&mut kernel);
    let contender_fd = kernel
        .fd_open("shell", contender.pid(), "/tmp/fork-lock.txt", O_RDWR, None)
        .expect("contender opens file");

    kernel
        .fd_flock("shell", child.pid(), inherited_fd, LOCK_EX | LOCK_NB)
        .expect("child sees the inherited open-file-description lock");
    assert_kernel_error_code(
        kernel.fd_flock("shell", contender.pid(), contender_fd, LOCK_SH | LOCK_NB),
        "EWOULDBLOCK",
    );

    parent.finish(0);
    child.finish(0);
    contender.finish(0);
    kernel.waitpid(parent.pid()).expect("wait parent");
    kernel.waitpid(child.pid()).expect("wait child");
    kernel.waitpid(contender.pid()).expect("wait contender");
}

#[test]
fn waitpid_returns_structured_result_and_process_introspection_works() {
    let mut config = KernelVmConfig::new("vm-api-proc");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");

    let parent = spawn_shell(&mut kernel);
    let child = kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                parent_pid: Some(parent.pid()),
                ..SpawnOptions::default()
            },
        )
        .expect("spawn child");

    assert_eq!(
        kernel.getpid("shell", child.pid()).expect("getpid"),
        child.pid()
    );
    assert_eq!(
        kernel.getppid("shell", child.pid()).expect("getppid"),
        parent.pid()
    );
    assert_eq!(
        kernel.getsid("shell", child.pid()).expect("inherited sid"),
        parent.pid()
    );
    assert_eq!(
        kernel.setsid("shell", child.pid()).expect("setsid"),
        child.pid()
    );
    assert_eq!(
        kernel.getsid("shell", child.pid()).expect("new sid"),
        child.pid()
    );

    let processes = kernel.list_processes();
    assert_eq!(
        processes.get(&parent.pid()).expect("parent info").command,
        "sh"
    );
    assert_eq!(
        processes.get(&child.pid()).expect("child info").ppid,
        parent.pid()
    );

    child.finish(23);
    assert_eq!(
        kernel.waitpid(child.pid()).expect("wait child"),
        WaitPidResult {
            pid: child.pid(),
            status: 23,
        }
    );

    parent.finish(0);
    kernel.waitpid(parent.pid()).expect("wait parent");
}

#[test]
fn waitpid_with_options_supports_wnohang_and_any_child_waits() {
    let mut config = KernelVmConfig::new("vm-api-waitpid-flags");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");

    let parent = spawn_shell(&mut kernel);
    let child = kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                parent_pid: Some(parent.pid()),
                ..SpawnOptions::default()
            },
        )
        .expect("spawn child");

    assert_eq!(
        kernel
            .waitpid_with_options("shell", parent.pid(), -1, WaitPidFlags::WNOHANG)
            .expect("wnohang wait should succeed"),
        None
    );

    child.finish(9);
    let waited = kernel
        .waitpid_with_options("shell", parent.pid(), -1, WaitPidFlags::empty())
        .expect("wait for any child should succeed")
        .expect("child exit should be reported");
    assert_eq!(waited.pid, child.pid());
    assert_eq!(waited.status, 9);
    assert_eq!(waited.event, ProcessWaitEvent::Exited);
    assert_eq!(
        kernel.list_processes().get(&child.pid()),
        None,
        "exited child should be reaped after wait"
    );

    parent.finish(0);
    kernel.waitpid(parent.pid()).expect("wait parent");
}

#[test]
fn proc_filesystem_exposes_live_process_metadata_and_fd_symlinks() {
    let mut config = KernelVmConfig::new("vm-api-procfs");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");
    kernel
        .filesystem_mut()
        .write_file("/tmp/data.txt", b"hello".to_vec())
        .expect("seed procfs data file");

    let process = kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                cwd: Some(String::from("/tmp")),
                env: std::collections::BTreeMap::from([(
                    String::from("VISIBLE_MARKER"),
                    String::from("present"),
                )]),
                ..SpawnOptions::default()
            },
        )
        .expect("spawn procfs shell");
    let fd = kernel
        .fd_open("shell", process.pid(), "/tmp/data.txt", O_RDWR, None)
        .expect("open procfs data file");

    let proc_entries = kernel
        .read_dir_for_process("shell", process.pid(), "/proc")
        .expect("read /proc");
    assert!(proc_entries.contains(&String::from("self")));
    assert!(proc_entries.contains(&String::from("mounts")));
    assert!(proc_entries.contains(&process.pid().to_string()));

    assert_eq!(
        kernel
            .read_link_for_process("shell", process.pid(), "/proc/self")
            .expect("read /proc/self link"),
        format!("/proc/{}", process.pid())
    );
    assert_eq!(
        kernel
            .realpath_for_process("shell", process.pid(), "/proc/self")
            .expect("realpath /proc/self"),
        format!("/proc/{}", process.pid())
    );

    let self_lstat = kernel
        .lstat_for_process("shell", process.pid(), "/proc/self")
        .expect("lstat /proc/self");
    assert!(self_lstat.is_symbolic_link);
    let self_stat = kernel
        .stat_for_process("shell", process.pid(), "/proc/self")
        .expect("stat /proc/self");
    assert!(self_stat.is_directory);

    let fd_entries = kernel
        .read_dir_for_process("shell", process.pid(), "/proc/self/fd")
        .expect("read /proc/self/fd");
    assert!(fd_entries.contains(&String::from("0")));
    assert!(fd_entries.contains(&fd.to_string()));
    assert_eq!(
        kernel
            .read_link_for_process("shell", process.pid(), &format!("/proc/self/fd/{fd}"),)
            .expect("read proc fd link"),
        String::from("/tmp/data.txt")
    );

    assert_eq!(
        kernel
            .read_link_for_process("shell", process.pid(), "/proc/self/cwd")
            .expect("read cwd link"),
        String::from("/tmp")
    );
    assert_eq!(
        kernel
            .read_file_for_process("shell", process.pid(), "/proc/self/cmdline")
            .expect("read cmdline"),
        b"sh\0".to_vec()
    );

    let environ = String::from_utf8(
        kernel
            .read_file_for_process("shell", process.pid(), "/proc/self/environ")
            .expect("read environ"),
    )
    .expect("proc environ should be utf8");
    assert!(environ.contains("VISIBLE_MARKER=present"));

    let stat_text = String::from_utf8(
        kernel
            .read_file_for_process("shell", process.pid(), "/proc/self/stat")
            .expect("read stat"),
    )
    .expect("proc stat should be utf8");
    assert!(stat_text.starts_with(&format!("{} (sh) R ", process.pid())));

    let error = kernel
        .write_file("/proc/mounts", b"blocked".to_vec())
        .expect_err("procfs should be read-only");
    assert_eq!(error.code(), "EROFS");

    process.finish(0);
    kernel.waitpid(process.pid()).expect("wait procfs shell");
}

#[test]
fn proc_mounts_lists_root_and_active_mounts() {
    let mut config = KernelVmConfig::new("vm-api-proc-mounts");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MountTable::new(MemoryFileSystem::new()), config);
    kernel
        .mount_filesystem(
            "/data",
            MemoryFileSystem::new(),
            MountOptions::new("memory").read_only(true),
        )
        .expect("mount memory filesystem");

    let mounts = String::from_utf8(kernel.read_file("/proc/mounts").expect("read proc mounts"))
        .expect("proc mounts should be utf8");
    assert!(mounts.contains("root / root rw 0 0"));
    assert!(mounts.contains("memory /data memory ro 0 0"));
}

#[test]
fn open_shell_configures_pty_and_exec_uses_shell_driver() {
    let mut config = KernelVmConfig::new("vm-api-shell");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");

    let shell = kernel
        .open_shell(OpenShellOptions {
            requester_driver: Some(String::from("shell")),
            ..OpenShellOptions::default()
        })
        .expect("open shell");
    assert!(shell.pty_path().starts_with("/dev/pts/"));
    assert_eq!(
        kernel.getpgid("shell", shell.pid()).expect("shell pgid"),
        shell.pid()
    );
    assert_eq!(
        kernel
            .tcgetpgrp("shell", shell.pid(), shell.master_fd())
            .expect("foreground pgid"),
        shell.pid()
    );

    shell.process().finish(0);
    kernel.waitpid(shell.pid()).expect("wait shell");

    let exec = kernel
        .exec(
            "echo hello",
            ExecOptions {
                requester_driver: Some(String::from("shell")),
                ..ExecOptions::default()
            },
        )
        .expect("exec through shell");
    assert_eq!(exec.driver(), "shell");
    assert_eq!(
        kernel
            .list_processes()
            .get(&exec.pid())
            .expect("exec process")
            .command,
        "sh"
    );

    exec.finish(0);
    kernel.waitpid(exec.pid()).expect("wait exec");
}

#[test]
fn pty_resize_delivers_sigwinch_to_the_foreground_process_group() {
    let mut config = KernelVmConfig::new("vm-api-shell");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");

    let shell = kernel
        .open_shell(OpenShellOptions {
            requester_driver: Some(String::from("shell")),
            ..OpenShellOptions::default()
        })
        .expect("open shell");

    kernel
        .pty_resize("shell", shell.pid(), shell.master_fd(), 120, 40)
        .expect("resize shell pty");
    kernel
        .pty_resize("shell", shell.pid(), shell.master_fd(), 120, 40)
        .expect("repeat shell pty resize");

    assert_eq!(shell.process().kill_signals(), vec![SIGWINCH]);

    shell.process().finish(0);
    kernel.waitpid(shell.pid()).expect("wait shell");
}

#[test]
fn shell_foreground_process_group_must_stay_in_the_same_session() {
    let mut config = KernelVmConfig::new("vm-api-shell");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");

    let shell = kernel
        .open_shell(OpenShellOptions {
            requester_driver: Some(String::from("shell")),
            ..OpenShellOptions::default()
        })
        .expect("open shell");
    let peer = kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                parent_pid: Some(shell.pid()),
                ..SpawnOptions::default()
            },
        )
        .expect("spawn peer");

    assert_eq!(
        kernel.getsid("shell", peer.pid()).expect("peer sid"),
        shell.pid()
    );
    assert_eq!(
        kernel.setsid("shell", peer.pid()).expect("setsid"),
        peer.pid()
    );

    let error = kernel
        .pty_set_foreground_pgid("shell", shell.pid(), shell.master_fd(), peer.pid())
        .expect_err("different-session process group should be rejected");
    assert_eq!(error.code(), "EPERM");
    assert!(error.to_string().contains("different session"));

    peer.finish(0);
    kernel.waitpid(peer.pid()).expect("wait peer");
    shell.process().finish(0);
    kernel.waitpid(shell.pid()).expect("wait shell");
}

#[test]
fn virtual_filesystem_default_pwrite_zero_fills_missing_bytes() {
    let mut filesystem = MemoryFileSystem::new();
    filesystem
        .write_file("/tmp/pwrite.txt", b"AB".to_vec())
        .expect("seed file");

    VirtualFileSystem::pwrite(&mut filesystem, "/tmp/pwrite.txt", b"CD".to_vec(), 5)
        .expect("default pwrite");

    assert_eq!(
        filesystem
            .read_file("/tmp/pwrite.txt")
            .expect("read back pwrite result"),
        vec![b'A', b'B', 0, 0, 0, b'C', b'D']
    );
}
