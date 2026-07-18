use agentos_kernel::command_registry::CommandDriver;
use agentos_kernel::fd_table::{
    RecordLockType, FD_CLOEXEC, F_DUPFD, F_GETFD, F_GETFL, F_SETFD, F_SETFL, LOCK_EX, LOCK_NB,
    LOCK_SH, LOCK_UN, O_APPEND, O_CREAT, O_DIRECTORY, O_EXCL, O_NOFOLLOW, O_NONBLOCK, O_RDONLY,
    O_RDWR, O_TRUNC,
};
use agentos_kernel::kernel::{
    ExecOptions, KernelVm, KernelVmConfig, OpenShellOptions, SpawnOptions, WaitPidFlags,
    WaitPidResult, SEEK_SET,
};
use agentos_kernel::mount_table::{MountOptions, MountTable};
use agentos_kernel::permissions::Permissions;
use agentos_kernel::pipe_manager::MAX_PIPE_BUFFER_BYTES;
use agentos_kernel::process_table::{ProcessWaitEvent, SignalAction, SignalDisposition, SIGWINCH};
use agentos_kernel::socket_table::SocketType;
use agentos_kernel::vfs::{
    MemoryFileSystem, VfsResult, VirtualDirEntry, VirtualFileSystem, VirtualStat, MAX_PATH_LENGTH,
};
use std::cell::{Cell, RefCell};

fn assert_kernel_error_code<T: std::fmt::Debug>(
    result: agentos_kernel::kernel::KernelResult<T>,
    expected: &str,
) {
    let error = result.expect_err("operation should fail");
    assert_eq!(error.code(), expected);
}

fn spawn_shell(
    kernel: &mut KernelVm<MemoryFileSystem>,
) -> agentos_kernel::kernel::KernelProcessHandle {
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
) -> agentos_kernel::kernel::KernelProcessHandle {
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
            return Err(agentos_kernel::vfs::VfsError::new(
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
    kernel
        .fd_sync("shell", process.pid(), created_fd)
        .expect("sync regular file");
    let directory_fd = kernel
        .fd_open("shell", process.pid(), "/tmp", 0, None)
        .expect("open directory for sync");
    kernel
        .fd_sync("shell", process.pid(), directory_fd)
        .expect("sync directory");
    let directory_entries = kernel
        .fd_read_dir_with_types("shell", process.pid(), directory_fd)
        .expect("read directory through fd");
    assert!(directory_entries.iter().any(|entry| {
        entry.name == "data.txt" && !entry.is_directory && !entry.is_symbolic_link
    }));
    assert!(directory_entries.iter().any(|entry| {
        entry.name == "created.txt" && !entry.is_directory && !entry.is_symbolic_link
    }));
    assert_kernel_error_code(
        kernel.fd_read_dir_with_types("shell", process.pid(), created_fd),
        "ENOTDIR",
    );
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
    assert_eq!(file_stat.blocks, 1);
    // Device ids are unique per filesystem instance; assert the fd stat
    // reports the same device as a direct path stat on the same filesystem.
    assert_eq!(
        file_stat.dev,
        kernel
            .filesystem_mut()
            .stat("/tmp/data.txt")
            .expect("stat updated file")
            .dev
    );
    assert_eq!(file_stat.rdev, 0);
    assert!(!file_stat.is_directory);

    let (read_fd, write_fd) = kernel.open_pipe("shell", process.pid()).expect("open pipe");
    assert_kernel_error_code(kernel.fd_sync("shell", process.pid(), read_fd), "EINVAL");
    let (socket_fd, peer_fd) = kernel
        .fd_socketpair("shell", process.pid(), SocketType::Stream, false, false)
        .expect("open socketpair for sync validation");
    assert_kernel_error_code(kernel.fd_sync("shell", process.pid(), socket_fd), "EINVAL");
    kernel
        .fd_close("shell", process.pid(), socket_fd)
        .expect("close first socket");
    kernel
        .fd_close("shell", process.pid(), peer_fd)
        .expect("close second socket");
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
    let write_pipe_stat = kernel
        .dev_fd_stat("shell", process.pid(), write_fd)
        .expect("stat pipe write fd");
    assert_eq!(pipe_stat.mode, 0o010600);
    assert_eq!(pipe_stat.size, 0);
    assert_eq!(pipe_stat.blocks, 0);
    assert_ne!(pipe_stat.dev, 0);
    assert_eq!(pipe_stat.nlink, 1);
    assert_eq!(write_pipe_stat.dev, pipe_stat.dev);
    assert_eq!(write_pipe_stat.ino, pipe_stat.ino);
    assert!(!pipe_stat.is_directory);

    kernel
        .fd_close("shell", process.pid(), directory_fd)
        .expect("close directory fd");
    assert_kernel_error_code(
        kernel.fd_read_dir_with_types("shell", process.pid(), directory_fd),
        "EBADF",
    );
    kernel
        .fd_close("shell", process.pid(), created_fd)
        .expect("close synced file fd");
    assert_kernel_error_code(kernel.fd_sync("shell", process.pid(), created_fd), "EBADF");

    process.finish(0);
    kernel.waitpid(process.pid()).expect("wait for shell");
}

#[test]
fn fd_open_directory_truncate_rejects_regular_file_without_truncating_it() {
    let mut config = KernelVmConfig::new("vm-open-directory-truncate");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");
    kernel
        .write_file("/regular", b"payload")
        .expect("seed regular file");
    let process = spawn_shell(&mut kernel);

    assert_kernel_error_code(
        kernel.fd_open(
            "shell",
            process.pid(),
            "/regular",
            O_RDONLY | O_DIRECTORY | O_TRUNC,
            None,
        ),
        "ENOTDIR",
    );
    assert_eq!(
        kernel.read_file("/regular").expect("read regular file"),
        b"payload"
    );
}

#[test]
fn fd_open_nofollow_truncate_rejects_symlink_without_truncating_target() {
    let mut config = KernelVmConfig::new("vm-open-nofollow-truncate");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");
    kernel
        .write_file("/target", b"payload")
        .expect("seed target file");
    kernel
        .symlink("/target", "/target-link")
        .expect("create target symlink");
    let process = spawn_shell(&mut kernel);

    assert_kernel_error_code(
        kernel.fd_open(
            "shell",
            process.pid(),
            "/target-link",
            O_RDONLY | O_NOFOLLOW | O_TRUNC,
            None,
        ),
        "ELOOP",
    );
    assert_eq!(
        kernel.read_file("/target").expect("read target file"),
        b"payload"
    );
}

#[test]
fn fd_open_directory_create_rejects_missing_path_without_creating_it() {
    let mut config = KernelVmConfig::new("vm-open-directory-create");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");
    let process = spawn_shell(&mut kernel);

    assert_kernel_error_code(
        kernel.fd_open(
            "shell",
            process.pid(),
            "/missing",
            O_RDONLY | O_DIRECTORY | O_CREAT,
            None,
        ),
        "EINVAL",
    );
    assert!(!kernel.exists("/missing").expect("query missing path"));
}

#[test]
fn fd_open_nofollow_rejects_proc_and_dev_fd_symlink_aliases() {
    let mut config = KernelVmConfig::new("vm-open-nofollow-special-links");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");
    kernel
        .write_file("/target", b"payload")
        .expect("seed target file");
    let process = spawn_shell(&mut kernel);
    let target_fd = kernel
        .fd_open("shell", process.pid(), "/target", O_RDONLY, None)
        .expect("open target file");

    for path in [
        String::from("/proc/self"),
        String::from("/proc/self/cwd"),
        format!("/proc/self/fd/{target_fd}"),
        format!("/dev/fd/{target_fd}"),
    ] {
        assert_kernel_error_code(
            kernel.fd_open("shell", process.pid(), &path, O_RDONLY | O_NOFOLLOW, None),
            "ELOOP",
        );
    }

    for path in ["/proc/self/fd/999999", "/dev/fd/999999"] {
        assert_kernel_error_code(
            kernel.fd_open("shell", process.pid(), path, O_RDONLY | O_NOFOLLOW, None),
            "ENOENT",
        );
    }
}

#[test]
fn open_file_descriptions_survive_unlink_and_follow_rename() {
    let mut config = KernelVmConfig::new("vm-api-open-file-description");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");
    kernel
        .write_file("/tmp/unlinked.txt", b"hello".to_vec())
        .expect("seed unlink target");
    kernel
        .write_file("/tmp/renamed.txt", b"rename".to_vec())
        .expect("seed rename target");

    let process = spawn_shell(&mut kernel);
    let unlinked_fd = kernel
        .fd_open("shell", process.pid(), "/tmp/unlinked.txt", O_RDWR, None)
        .expect("open unlink target");
    kernel
        .remove_file("/tmp/unlinked.txt")
        .expect("unlink open file");
    assert_eq!(
        kernel
            .read_link_for_process(
                "shell",
                process.pid(),
                &format!("/proc/self/fd/{unlinked_fd}"),
            )
            .expect("read unlinked file proc fd target"),
        "/tmp/unlinked.txt (deleted)"
    );

    assert!(!kernel
        .exists("/tmp/unlinked.txt")
        .expect("check removed path"));
    assert_eq!(
        kernel
            .fd_pread("shell", process.pid(), unlinked_fd, 5, 0)
            .expect("read unlinked file description"),
        b"hello".to_vec()
    );
    kernel
        .fd_pwrite("shell", process.pid(), unlinked_fd, b"X", 1)
        .expect("write unlinked file description");
    kernel
        .fd_chmod("shell", process.pid(), unlinked_fd, 0o600)
        .expect("chmod unlinked file description");
    let unlinked_stat = kernel
        .dev_fd_stat("shell", process.pid(), unlinked_fd)
        .expect("stat unlinked file description");
    assert_eq!(unlinked_stat.size, 5);
    assert_eq!(unlinked_stat.mode & 0o777, 0o600);
    assert_eq!(
        kernel
            .fd_pread("shell", process.pid(), unlinked_fd, 5, 0)
            .expect("read updated unlinked file description"),
        b"hXllo".to_vec()
    );

    let renamed_fd = kernel
        .fd_open("shell", process.pid(), "/tmp/renamed.txt", O_RDWR, None)
        .expect("open rename target");
    kernel
        .rename("/tmp/renamed.txt", "/tmp/moved.txt")
        .expect("rename open file");
    assert_eq!(
        kernel
            .fd_path("shell", process.pid(), renamed_fd)
            .expect("resolve renamed fd path"),
        "/tmp/moved.txt"
    );
    assert_eq!(
        kernel
            .fd_pread("shell", process.pid(), renamed_fd, 6, 0)
            .expect("read renamed file description"),
        b"rename".to_vec()
    );

    process.finish(0);
    kernel.waitpid(process.pid()).expect("wait for shell");
}

#[test]
fn overwritten_open_destinations_report_zero_links() {
    let mut config = KernelVmConfig::new("vm-api-open-rename-overwrite");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");
    kernel
        .write_file("/tmp/source.txt", b"source".to_vec())
        .expect("seed rename source");
    kernel
        .write_file("/tmp/destination.txt", b"destination".to_vec())
        .expect("seed rename destination");
    kernel.mkdir("/tmp/source-dir", false).expect("source dir");
    kernel
        .mkdir("/tmp/destination-dir", false)
        .expect("destination dir");

    let process = spawn_shell(&mut kernel);
    let file_fd = kernel
        .fd_open(
            "shell",
            process.pid(),
            "/tmp/destination.txt",
            O_RDONLY,
            None,
        )
        .expect("open file destination");
    let directory_fd = kernel
        .fd_open(
            "shell",
            process.pid(),
            "/tmp/destination-dir",
            O_RDONLY | O_DIRECTORY,
            None,
        )
        .expect("open directory destination");

    kernel
        .rename("/tmp/source.txt", "/tmp/destination.txt")
        .expect("overwrite file destination");
    kernel
        .rename("/tmp/source-dir", "/tmp/destination-dir")
        .expect("overwrite directory destination");

    assert_eq!(
        kernel
            .dev_fd_stat("shell", process.pid(), file_fd)
            .expect("fstat overwritten file")
            .nlink,
        0
    );
    assert_eq!(
        kernel
            .dev_fd_stat("shell", process.pid(), directory_fd)
            .expect("fstat overwritten directory")
            .nlink,
        0
    );

    process.finish(0);
    kernel.waitpid(process.pid()).expect("wait for shell");
}

#[test]
fn kernel_process_umask_applies_to_created_files_and_directories() {
    let mut config = KernelVmConfig::new("vm-api-umask");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");

    let process = spawn_shell(&mut kernel);
    assert_eq!(
        kernel
            .umask("shell", process.pid(), None)
            .expect("read default umask"),
        0o022
    );
    assert_eq!(
        kernel
            .umask("shell", process.pid(), Some(0o027))
            .expect("set umask"),
        0o022
    );

    let created_fd = kernel
        .fd_open(
            "shell",
            process.pid(),
            "/tmp/umask-file.txt",
            O_CREAT | O_RDWR,
            Some(0o666),
        )
        .expect("create file with umask");
    kernel
        .fd_close("shell", process.pid(), created_fd)
        .expect("close created fd");
    let file_stat = kernel.stat("/tmp/umask-file.txt").expect("stat umask file");
    assert_eq!(file_stat.mode & 0o777, 0o640);

    kernel
        .mkdir_for_process(
            "shell",
            process.pid(),
            "/tmp/private-dir",
            false,
            Some(0o777),
        )
        .expect("create directory with umask");
    let dir_stat = kernel.stat("/tmp/private-dir").expect("stat private dir");
    assert_eq!(dir_stat.mode & 0o777, 0o750);

    let child = kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                parent_pid: Some(process.pid()),
                ..SpawnOptions::default()
            },
        )
        .expect("spawn child with inherited umask");
    assert_eq!(
        kernel
            .umask("shell", child.pid(), None)
            .expect("read child umask"),
        0o027
    );
    let child_fd = kernel
        .fd_open(
            "shell",
            child.pid(),
            "/tmp/child-umask-file.txt",
            O_CREAT | O_RDWR,
            Some(0o666),
        )
        .expect("create file with inherited child umask");
    kernel
        .fd_close("shell", child.pid(), child_fd)
        .expect("close child-created file");
    let child_stat = kernel
        .stat("/tmp/child-umask-file.txt")
        .expect("stat child umask file");
    assert_eq!(child_stat.mode & 0o777, 0o640);

    child.finish(0);
    kernel.waitpid(child.pid()).expect("wait for child");

    process.finish(0);
    kernel.waitpid(process.pid()).expect("wait for shell");
}

#[test]
fn read_dir_with_types_for_process_reports_entries_and_enforces_driver_ownership() {
    let mut config = KernelVmConfig::new("vm-api-readdir-types");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");
    kernel
        .register_driver(CommandDriver::new("other", ["other"]))
        .expect("register other driver");
    kernel
        .filesystem_mut()
        .write_file("/tmp/typed-file.txt", b"hello".to_vec())
        .expect("seed typed file");
    kernel
        .filesystem_mut()
        .mkdir("/tmp/typed-dir", true)
        .expect("seed typed dir");

    let process = spawn_shell(&mut kernel);
    let entries = kernel
        .read_dir_with_types_for_process("shell", process.pid(), "/tmp")
        .expect("read typed entries");

    let file_entry = entries
        .iter()
        .find(|entry| entry.name == "typed-file.txt")
        .expect("typed file entry");
    assert!(!file_entry.is_directory);
    assert!(!file_entry.is_symbolic_link);

    let dir_entry = entries
        .iter()
        .find(|entry| entry.name == "typed-dir")
        .expect("typed dir entry");
    assert!(dir_entry.is_directory);
    assert!(!dir_entry.is_symbolic_link);

    assert_kernel_error_code(
        kernel.read_dir_with_types_for_process("other", process.pid(), "/tmp"),
        "EPERM",
    );

    process.finish(0);
    kernel.waitpid(process.pid()).expect("wait for shell");
}

#[test]
fn descriptor_readdir_reports_linux_dot_order_and_inode_identity() {
    let mut config = KernelVmConfig::new("vm-api-fd-readdir-inodes");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");
    kernel.mkdir("/real", true).expect("create real root");
    kernel
        .mkdir("/real/target", true)
        .expect("create target directory");
    kernel.mkdir("/links", true).expect("create link root");
    kernel
        .write_file("/real/target/child", b"child")
        .expect("create child");
    kernel
        .symlink("/real/target", "/links/target-link")
        .expect("create directory symlink");

    let process = spawn_shell(&mut kernel);
    let directory_fd = kernel
        .fd_open("shell", process.pid(), "/links/target-link", 0, None)
        .expect("open directory through symlink");
    let entries = kernel
        .fd_read_dir_with_types("shell", process.pid(), directory_fd)
        .expect("read descriptor directory");
    assert_eq!(entries[0].name, ".");
    assert_eq!(entries[1].name, "..");
    assert!(entries.iter().all(|entry| entry.ino != 0));
    assert_eq!(entries[0].ino, kernel.stat("/real/target").unwrap().ino);
    assert_eq!(entries[1].ino, kernel.stat("/real").unwrap().ino);
    assert_ne!(entries[1].ino, kernel.stat("/links").unwrap().ino);
    assert_eq!(
        entries
            .iter()
            .find(|entry| entry.name == "child")
            .unwrap()
            .ino,
        kernel.lstat("/real/target/child").unwrap().ino
    );
    assert_eq!(
        entries,
        kernel
            .fd_read_dir_with_types("shell", process.pid(), directory_fd)
            .expect("repeat descriptor directory read")
    );

    let root_fd = kernel
        .fd_open("shell", process.pid(), "/", 0, None)
        .expect("open root directory");
    let root_entries = kernel
        .fd_read_dir_with_types("shell", process.pid(), root_fd)
        .expect("read root directory");
    assert_eq!(root_entries[0].name, ".");
    assert_eq!(root_entries[1].name, "..");
    assert_ne!(root_entries[0].ino, 0);
    assert_eq!(root_entries[0].ino, root_entries[1].ino);
    assert_eq!(root_entries[0].ino, kernel.stat("/").unwrap().ino);

    kernel
        .mkdir("/renamed-before", true)
        .expect("create directory before rename");
    kernel
        .write_file("/renamed-before/child", b"child")
        .expect("create renamed child");
    let renamed_fd = kernel
        .fd_open("shell", process.pid(), "/renamed-before", 0, None)
        .expect("open directory before rename");
    let renamed_ino = kernel.stat("/renamed-before").unwrap().ino;
    kernel
        .rename("/renamed-before", "/renamed-after")
        .expect("rename open directory");
    assert_eq!(
        kernel
            .fd_path("shell", process.pid(), renamed_fd)
            .expect("descriptor path follows rename"),
        "/renamed-after"
    );
    let renamed_entries = kernel
        .fd_read_dir_with_types("shell", process.pid(), renamed_fd)
        .expect("read renamed directory descriptor");
    assert_eq!(renamed_entries[0].ino, renamed_ino);
    assert!(renamed_entries.iter().any(|entry| entry.name == "child"));

    kernel
        .mkdir("/removed-open", true)
        .expect("create directory to remove while open");
    let removed_fd = kernel
        .fd_open("shell", process.pid(), "/removed-open", 0, None)
        .expect("open directory before removal");
    let removed_stat = kernel
        .dev_fd_stat("shell", process.pid(), removed_fd)
        .expect("stat open directory before removal");
    kernel
        .remove_dir("/removed-open")
        .expect("remove open empty directory");
    assert_eq!(
        kernel
            .read_link_for_process(
                "shell",
                process.pid(),
                &format!("/proc/self/fd/{removed_fd}"),
            )
            .expect("read detached directory proc fd target"),
        "/removed-open (deleted)"
    );
    let detached_stat = kernel
        .dev_fd_stat("shell", process.pid(), removed_fd)
        .expect("fstat detached directory");
    assert_eq!(detached_stat.ino, removed_stat.ino);
    assert_eq!(detached_stat.mode, removed_stat.mode);
    assert!(
        kernel
            .fd_read_dir_with_types("shell", process.pid(), removed_fd)
            .expect("read detached directory")
            .is_empty(),
        "Linux getdents returns EOF for an unlinked open directory"
    );

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
        kernel.fd_write_nonblocking("shell", process.pid(), write_fd, &[8]),
        "EAGAIN",
    );
    assert_eq!(
        kernel
            .fd_stat("shell", process.pid(), write_fd)
            .expect("stat logically blocking writer after nonblocking attempt")
            .flags
            & O_NONBLOCK,
        0,
        "host-side cooperative writes must not change guest fd flags"
    );
    assert_kernel_error_code(
        kernel.fd_write("shell", process.pid(), nonblocking_write_fd, &[8]),
        "EAGAIN",
    );

    process.finish(0);
    kernel.waitpid(process.pid()).expect("wait for shell");
}

#[test]
fn trusted_nonblocking_pipe_write_does_not_change_guest_flags() {
    let mut config = KernelVmConfig::new("vm-api-trusted-nonblock");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");

    let process = spawn_shell(&mut kernel);
    let (_read_fd, write_fd) = kernel.open_pipe("shell", process.pid()).expect("open pipe");
    kernel
        .fd_write(
            "shell",
            process.pid(),
            write_fd,
            &vec![7; MAX_PIPE_BUFFER_BYTES],
        )
        .expect("fill pipe buffer");

    assert_eq!(
        kernel
            .fd_fcntl("shell", process.pid(), write_fd, F_GETFL, 0)
            .expect("read guest-visible flags")
            & O_NONBLOCK,
        0
    );
    assert_kernel_error_code(
        kernel.fd_write_nonblocking("shell", process.pid(), write_fd, &[8]),
        "EAGAIN",
    );
    assert_eq!(
        kernel
            .fd_fcntl("shell", process.pid(), write_fd, F_GETFL, 0)
            .expect("read flags after trusted write")
            & O_NONBLOCK,
        0
    );

    process.finish(0);
    kernel.waitpid(process.pid()).expect("wait for shell");
}

#[test]
fn nonblocking_pipe_write_probe_preserves_descriptor_flags() {
    let mut config = KernelVmConfig::new("vm-api-pipe-write-probe");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");
    let process = spawn_shell(&mut kernel);
    let (read_fd, write_fd) = kernel.open_pipe("shell", process.pid()).expect("open pipe");
    let payload = vec![0x5a; MAX_PIPE_BUFFER_BYTES];

    assert_eq!(
        kernel
            .fd_write_nonblocking_pipe("shell", process.pid(), write_fd, &payload)
            .expect("fill pipe without blocking"),
        payload.len()
    );
    assert_kernel_error_code(
        kernel.fd_write_nonblocking_pipe("shell", process.pid(), write_fd, b"x"),
        "EAGAIN",
    );
    assert_eq!(
        kernel
            .fd_stat("shell", process.pid(), write_fd)
            .expect("stat pipe writer")
            .flags
            & O_NONBLOCK,
        0,
        "the internal probe must not change guest-visible descriptor flags"
    );

    assert_eq!(
        kernel
            .fd_read("shell", process.pid(), read_fd, 1024)
            .expect("drain pipe")
            .len(),
        1024
    );
    assert_eq!(
        kernel
            .fd_write_nonblocking_pipe("shell", process.pid(), write_fd, &payload[..1024])
            .expect("write after reader frees capacity"),
        1024
    );
}

#[test]
fn kernel_fd_surface_supports_fcntl_status_and_descriptor_flags() {
    let mut config = KernelVmConfig::new("vm-api-fcntl");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");

    let process = spawn_shell(&mut kernel);
    let (read_fd, _write_fd) = kernel.open_pipe("shell", process.pid()).expect("open pipe");

    assert_eq!(
        kernel
            .fd_fcntl("shell", process.pid(), read_fd, F_GETFL, 0)
            .expect("initial F_GETFL"),
        0
    );
    kernel
        .fd_fcntl("shell", process.pid(), read_fd, F_SETFL, O_NONBLOCK)
        .expect("set O_NONBLOCK");
    assert_eq!(
        kernel
            .fd_fcntl("shell", process.pid(), read_fd, F_GETFL, 0)
            .expect("updated F_GETFL")
            & O_NONBLOCK,
        O_NONBLOCK
    );
    assert_kernel_error_code(kernel.fd_read("shell", process.pid(), read_fd, 1), "EAGAIN");

    kernel
        .fd_fcntl("shell", process.pid(), read_fd, F_SETFD, FD_CLOEXEC)
        .expect("set cloexec");
    assert_eq!(
        kernel
            .fd_fcntl("shell", process.pid(), read_fd, F_GETFD, 0)
            .expect("read cloexec"),
        FD_CLOEXEC
    );

    let dup_fd = kernel
        .fd_fcntl("shell", process.pid(), read_fd, F_DUPFD, 10)
        .expect("duplicate with minimum fd");
    assert_eq!(dup_fd, 10);
    assert_eq!(
        kernel
            .fd_fcntl("shell", process.pid(), dup_fd, F_GETFD, 0)
            .expect("dup cloexec should be clear"),
        0
    );
    assert_eq!(
        kernel
            .fd_fcntl("shell", process.pid(), dup_fd, F_GETFL, 0)
            .expect("dup status flags")
            & O_NONBLOCK,
        O_NONBLOCK
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
fn kernel_fd_surface_enforces_posix_record_lock_ranges_and_close_semantics() {
    let mut config = KernelVmConfig::new("vm-api-record-locks");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");
    kernel
        .filesystem_mut()
        .write_file("/tmp/record-lock.txt", vec![0; 64])
        .expect("seed record-lock file");

    let owner = spawn_shell(&mut kernel);
    let contender = spawn_shell(&mut kernel);
    let owner_fd = kernel
        .fd_open("shell", owner.pid(), "/tmp/record-lock.txt", O_RDWR, None)
        .expect("owner opens file");
    let owner_dup = kernel
        .fd_dup("shell", owner.pid(), owner_fd)
        .expect("duplicate owner fd");
    let contender_fd = kernel
        .fd_open(
            "shell",
            contender.pid(),
            "/tmp/record-lock.txt",
            O_RDWR,
            None,
        )
        .expect("contender opens file");

    kernel
        .fd_record_lock(
            "shell",
            owner.pid(),
            owner_fd,
            RecordLockType::Write,
            10,
            20,
            false,
        )
        .expect("owner locks [10, 30)");
    let conflict = kernel
        .fd_record_lock(
            "shell",
            contender.pid(),
            contender_fd,
            RecordLockType::Read,
            0,
            64,
            true,
        )
        .expect("query conflicting lock")
        .expect("write lock should conflict");
    assert_eq!(conflict.pid, owner.pid());
    assert_eq!(conflict.lock_type, RecordLockType::Write);
    assert_eq!((conflict.start, conflict.length()), (10, 20));
    assert_kernel_error_code(
        kernel
            .fd_record_lock(
                "shell",
                contender.pid(),
                contender_fd,
                RecordLockType::Write,
                20,
                2,
                false,
            )
            .map(|_| ()),
        "EWOULDBLOCK",
    );
    kernel
        .fd_record_lock(
            "shell",
            contender.pid(),
            contender_fd,
            RecordLockType::Write,
            30,
            10,
            false,
        )
        .expect("adjacent range does not conflict");

    kernel
        .fd_record_lock(
            "shell",
            owner.pid(),
            owner_fd,
            RecordLockType::Unlock,
            15,
            10,
            false,
        )
        .expect("split owner range");
    kernel
        .fd_record_lock(
            "shell",
            contender.pid(),
            contender_fd,
            RecordLockType::Write,
            15,
            10,
            false,
        )
        .expect("unlocked middle range is available");

    // Linux process locks are released when any descriptor for the inode is
    // closed, not only when the last duplicate closes.
    kernel
        .fd_close("shell", owner.pid(), owner_dup)
        .expect("close owner duplicate");
    assert!(kernel
        .fd_record_lock(
            "shell",
            contender.pid(),
            contender_fd,
            RecordLockType::Write,
            0,
            15,
            false,
        )
        .is_ok());

    owner.finish(0);
    contender.finish(0);
    kernel.waitpid(owner.pid()).expect("wait owner");
    kernel.waitpid(contender.pid()).expect("wait contender");
}

#[test]
fn kernel_fd_record_lock_wait_detects_deadlock_and_cleans_up_waiters() {
    let mut config = KernelVmConfig::new("vm-api-record-lock-deadlock");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");
    kernel
        .filesystem_mut()
        .write_file("/tmp/record-lock-a", vec![0; 8])
        .expect("seed first record-lock file");
    kernel
        .filesystem_mut()
        .write_file("/tmp/record-lock-b", vec![0; 8])
        .expect("seed second record-lock file");

    let first = spawn_shell(&mut kernel);
    let second = spawn_shell(&mut kernel);
    let first_a = kernel
        .fd_open("shell", first.pid(), "/tmp/record-lock-a", O_RDWR, None)
        .expect("first process opens first file");
    let first_b = kernel
        .fd_open("shell", first.pid(), "/tmp/record-lock-b", O_RDWR, None)
        .expect("first process opens second file");
    let second_a = kernel
        .fd_open("shell", second.pid(), "/tmp/record-lock-a", O_RDWR, None)
        .expect("second process opens first file");
    let second_b = kernel
        .fd_open("shell", second.pid(), "/tmp/record-lock-b", O_RDWR, None)
        .expect("second process opens second file");

    kernel
        .fd_record_lock(
            "shell",
            first.pid(),
            first_a,
            RecordLockType::Write,
            0,
            8,
            false,
        )
        .expect("first process locks first file");
    kernel
        .fd_record_lock(
            "shell",
            second.pid(),
            second_b,
            RecordLockType::Write,
            0,
            8,
            false,
        )
        .expect("second process locks second file");

    assert_kernel_error_code(
        kernel.fd_record_lock_wait("shell", first.pid(), first_b, RecordLockType::Write, 0, 8),
        "EWOULDBLOCK",
    );
    assert_kernel_error_code(
        kernel.fd_record_lock_wait("shell", second.pid(), second_a, RecordLockType::Write, 0, 8),
        "EDEADLK",
    );

    kernel
        .fd_record_lock_cancel("shell", first.pid())
        .expect("cancel first process wait");
    assert_kernel_error_code(
        kernel.fd_record_lock_wait("shell", second.pid(), second_a, RecordLockType::Write, 0, 8),
        "EWOULDBLOCK",
    );
    kernel
        .fd_close("shell", second.pid(), second_a)
        .expect("close cancels second process wait");

    second.finish(0);
    kernel.waitpid(second.pid()).expect("wait second process");
    kernel
        .fd_record_lock_wait("shell", first.pid(), first_b, RecordLockType::Write, 0, 8)
        .expect("process exit releases lock and wait edge");

    first.finish(0);
    kernel.waitpid(first.pid()).expect("wait first process");
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
    assert!(mounts.contains("root / root rw,relatime 0 0"));
    assert!(mounts.contains("memory /data memory ro,relatime 0 0"));
}

#[test]
fn filesystem_operations_return_linux_errno_values_for_common_failures() {
    let mut config = KernelVmConfig::new("vm-api-errno");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MountTable::new(MemoryFileSystem::new()), config);

    kernel.create_dir("/dir").expect("create dir");
    assert_kernel_error_code(kernel.write_file("/dir", b"blocked".to_vec()), "EISDIR");

    kernel
        .write_file("/file", b"parent".to_vec())
        .expect("write file parent");
    assert_kernel_error_code(kernel.stat("/file/child"), "ENOTDIR");

    let long_path = format!("/{}", "a".repeat(MAX_PATH_LENGTH));
    assert_kernel_error_code(kernel.stat(&long_path), "ENAMETOOLONG");

    kernel
        .mount_filesystem(
            "/readonly",
            MemoryFileSystem::new(),
            MountOptions::new("memory").read_only(true),
        )
        .expect("mount readonly fs");
    assert_kernel_error_code(
        kernel.write_file("/readonly/blocked.txt", b"blocked".to_vec()),
        "EROFS",
    );
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
    shell
        .process()
        .signal_action(
            SIGWINCH,
            Some(SignalAction {
                disposition: SignalDisposition::User,
                ..SignalAction::DEFAULT
            }),
        )
        .expect("catch SIGWINCH");
    let controls = shell
        .process()
        .attach_runtime_control(std::sync::Arc::new(|| {}))
        .expect("attach runtime controls");

    kernel
        .pty_resize("shell", shell.pid(), shell.master_fd(), 120, 40)
        .expect("resize shell pty");
    kernel
        .pty_resize("shell", shell.pid(), shell.master_fd(), 120, 40)
        .expect("repeat shell pty resize");

    let pending = controls.pending();
    assert!(pending.checkpoint);
    controls
        .acknowledge(pending)
        .expect("acknowledge resize signal checkpoint");
    assert!(shell
        .process()
        .sigpending()
        .expect("pending signals")
        .contains(SIGWINCH));

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
