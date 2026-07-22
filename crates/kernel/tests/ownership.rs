use agentos_kernel::fd_table::{O_DIRECTORY, O_RDONLY};
use agentos_kernel::kernel::{KernelVm, KernelVmConfig, VirtualProcessOptions};
use agentos_kernel::mount_table::MountTable;
use agentos_kernel::permissions::Permissions;
use agentos_kernel::root_fs::{RootFileSystem, RootFilesystemDescriptor, RootFilesystemMode};
use agentos_kernel::socket_table::SocketType;
use agentos_kernel::user::UserConfig;
use agentos_kernel::vfs::MemoryFileSystem;

const DRIVER: &str = "ownership-driver";
const UNCHANGED: u32 = u32::MAX;

fn kernel_and_pid() -> (KernelVm<MemoryFileSystem>, u32) {
    let mut config = KernelVmConfig::new("vm-ownership");
    config.permissions = Permissions::allow_all();
    config.user = UserConfig {
        supplementary_gids: vec![44],
        ..UserConfig::default()
    };
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    let process = kernel
        .create_virtual_process(
            DRIVER,
            DRIVER,
            "ownership-check",
            Vec::new(),
            VirtualProcessOptions::default(),
        )
        .expect("create process");
    (kernel, process.pid())
}

#[test]
fn unprivileged_chown_matches_linux_owner_and_group_rules() {
    let (mut kernel, pid) = kernel_and_pid();
    kernel
        .write_file_for_process(DRIVER, pid, "/file", b"data", Some(0o6755))
        .expect("create owned file");

    kernel
        .chown_for_process(DRIVER, pid, "/file", UNCHANGED, 44, true)
        .expect("owner may select supplementary group");
    let stat = kernel.stat("/file").expect("stat changed file");
    assert_eq!((stat.uid, stat.gid), (1000, 44));
    assert_eq!(stat.mode & 0o7777, 0o755, "chown clears set-id bits");

    let error = kernel
        .chown_for_process(DRIVER, pid, "/file", 1001, UNCHANGED, true)
        .expect_err("unprivileged owner change must fail");
    assert_eq!(error.code(), "EPERM");
    let error = kernel
        .chown_for_process(DRIVER, pid, "/file", UNCHANGED, 55, true)
        .expect_err("unprivileged foreign group change must fail");
    assert_eq!(error.code(), "EPERM");

    kernel
        .chown("/file", 2000, 2000)
        .expect("seed foreign owner");
    kernel
        .chown_for_process(DRIVER, pid, "/file", UNCHANGED, UNCHANGED, true)
        .expect("Linux accepts an all-unchanged request on a foreign inode");
    let error = kernel
        .chown_for_process(DRIVER, pid, "/file", 2000, UNCHANGED, true)
        .expect_err("specifying even the existing uid requires ownership");
    assert_eq!(error.code(), "EPERM");
}

#[test]
fn chown_preserves_non_executable_setgid_like_linux() {
    let (mut kernel, pid) = kernel_and_pid();
    kernel
        .write_file_for_process(DRIVER, pid, "/mandatory-lock", b"data", Some(0o6745))
        .expect("create set-id file without group execute");

    kernel
        .chown_for_process(DRIVER, pid, "/mandatory-lock", UNCHANGED, 44, true)
        .expect("change file group");
    let stat = kernel.stat("/mandatory-lock").expect("stat changed file");
    assert_eq!((stat.uid, stat.gid), (1000, 44));
    assert_eq!(
        stat.mode & 0o7777,
        0o2745,
        "Linux clears setuid but preserves setgid without group execute"
    );
}

#[test]
fn unprivileged_truncate_preserves_mandatory_locking_sgid_only() {
    let (mut kernel, pid) = kernel_and_pid();
    kernel
        .write_file_for_process(DRIVER, pid, "/mandatory-lock-write", b"data", None)
        .expect("create mandatory-locking file");
    kernel
        .chmod("/mandatory-lock-write", 0o6744)
        .expect("set non-group-executable set-id mode");

    kernel
        .truncate_for_process(DRIVER, pid, "/mandatory-lock-write", 0)
        .expect("truncate mandatory-locking file");
    assert_eq!(
        kernel
            .stat("/mandatory-lock-write")
            .expect("stat mandatory-locking file")
            .mode
            & 0o7777,
        0o2744,
        "truncate clears setuid but preserves mandatory-locking setgid"
    );

    kernel
        .chmod("/mandatory-lock-write", 0o6754)
        .expect("set group-executable set-id mode");
    kernel
        .truncate_for_process(DRIVER, pid, "/mandatory-lock-write", 1)
        .expect("truncate group-executable file");
    assert_eq!(
        kernel
            .stat("/mandatory-lock-write")
            .expect("stat group-executable file")
            .mode
            & 0o7777,
        0o754,
        "truncate clears executable setuid and setgid bits"
    );
}

#[test]
fn unprivileged_foreign_group_write_clears_non_executable_setgid() {
    let (mut kernel, pid) = kernel_and_pid();
    kernel
        .write_file("/foreign-group-write", b"data".to_vec())
        .expect("create foreign-group file");
    kernel
        .chown("/foreign-group-write", 2000, 2000)
        .expect("set foreign owner and group");
    kernel
        .chmod("/foreign-group-write", 0o6666)
        .expect("make foreign set-id file writable");

    kernel
        .truncate_for_process(DRIVER, pid, "/foreign-group-write", 0)
        .expect("other-writable foreign file may be truncated");
    assert_eq!(
        kernel
            .stat("/foreign-group-write")
            .expect("stat foreign-group file")
            .mode
            & 0o7777,
        0o666,
        "Linux clears setuid and non-executable setgid when the writer is not in the file group"
    );
}

#[test]
fn chown_follows_but_lchown_mutates_the_symlink_inode() {
    let (mut kernel, pid) = kernel_and_pid();
    kernel
        .write_file_for_process(DRIVER, pid, "/target", b"data", None)
        .expect("create target");
    kernel.symlink("/target", "/link").expect("create link");

    kernel
        .chown_for_process(DRIVER, pid, "/link", UNCHANGED, 44, false)
        .expect("lchown link");
    assert_eq!(kernel.lstat("/link").expect("lstat link").gid, 44);
    assert_eq!(kernel.stat("/target").expect("stat target").gid, 1000);

    kernel
        .chown_for_process(DRIVER, pid, "/link", UNCHANGED, 44, true)
        .expect("chown target through link");
    assert_eq!(kernel.stat("/target").expect("stat target").gid, 44);
}

#[test]
fn fchown_updates_an_open_file_after_unlink() {
    let (mut kernel, pid) = kernel_and_pid();
    kernel
        .write_file_for_process(DRIVER, pid, "/open", b"data", Some(0o6755))
        .expect("create file");
    let fd = kernel
        .fd_open(DRIVER, pid, "/open", O_RDONLY, None)
        .expect("open file");
    kernel.remove_file("/open").expect("unlink open file");

    kernel
        .fd_chown_for_process(DRIVER, pid, fd, UNCHANGED, 44)
        .expect("fchown detached file");
    let stat = kernel
        .dev_fd_stat(DRIVER, pid, fd)
        .expect("fstat detached file");
    assert_eq!((stat.uid, stat.gid), (1000, 44));
    assert_eq!(stat.mode & 0o7777, 0o755);
}

#[test]
fn fchown_after_unlink_updates_the_surviving_hard_link_inode() {
    let (mut kernel, pid) = kernel_and_pid();
    kernel
        .write_file_for_process(DRIVER, pid, "/original", b"data", Some(0o6745))
        .expect("create hard-link source");
    kernel
        .link("/original", "/alias")
        .expect("create surviving hard link");
    let fd = kernel
        .fd_open(DRIVER, pid, "/original", O_RDONLY, None)
        .expect("open source");
    let original = kernel.stat("/original").expect("stat source");

    kernel.remove_file("/original").expect("unlink source name");
    kernel
        .fd_chown_for_process(DRIVER, pid, fd, UNCHANGED, 44)
        .expect("fchown through deleted name");

    let descriptor = kernel
        .dev_fd_stat(DRIVER, pid, fd)
        .expect("fstat open hard link");
    let alias = kernel.stat("/alias").expect("stat surviving alias");
    assert_eq!((descriptor.dev, descriptor.ino), (alias.dev, alias.ino));
    assert_eq!((alias.dev, alias.ino), (original.dev, original.ino));
    assert_eq!((descriptor.uid, descriptor.gid), (1000, 44));
    assert_eq!((alias.uid, alias.gid), (1000, 44));
    assert_eq!(descriptor.mode & 0o7777, 0o2745);
    assert_eq!(alias.mode & 0o7777, 0o2745);
}

#[test]
fn fchmod_mutates_detached_directories_pipes_and_sockets() {
    let (mut kernel, pid) = kernel_and_pid();
    kernel.mkdir("/detached", false).expect("create directory");
    let directory_fd = kernel
        .fd_open(DRIVER, pid, "/detached", O_RDONLY | O_DIRECTORY, None)
        .expect("open directory");
    kernel
        .remove_dir("/detached")
        .expect("remove open directory");
    kernel
        .fd_chmod_for_process(DRIVER, pid, directory_fd, 0o701)
        .expect("fchmod detached directory");
    assert_eq!(
        kernel
            .dev_fd_stat(DRIVER, pid, directory_fd)
            .expect("fstat detached directory")
            .mode
            & 0o7777,
        0o701
    );

    let (pipe_read, pipe_write) = kernel.open_pipe(DRIVER, pid).expect("open pipe");
    kernel
        .fd_chmod_for_process(DRIVER, pid, pipe_read, 0o640)
        .expect("fchmod pipe");
    for fd in [pipe_read, pipe_write] {
        let stat = kernel.dev_fd_stat(DRIVER, pid, fd).expect("fstat pipe");
        assert_eq!(stat.mode, 0o010640);
        assert_eq!((stat.uid, stat.gid), (1000, 1000));
    }

    let (socket_left, socket_right) = kernel
        .fd_socketpair(DRIVER, pid, SocketType::Stream, false, false)
        .expect("open socketpair");
    kernel
        .fd_chmod_for_process(DRIVER, pid, socket_left, 0o601)
        .expect("fchmod socket");
    let left = kernel
        .dev_fd_stat(DRIVER, pid, socket_left)
        .expect("fstat changed socket");
    let right = kernel
        .dev_fd_stat(DRIVER, pid, socket_right)
        .expect("fstat peer socket");
    assert_eq!(left.mode, 0o140601);
    assert_eq!(right.mode, 0o140777);
    assert_eq!((left.uid, left.gid), (1000, 1000));
}

#[test]
fn lchown_reaches_the_ephemeral_root_overlay_without_following() {
    let mut config = KernelVmConfig::new("vm-root-ownership");
    config.permissions = Permissions::allow_all();
    let root = RootFileSystem::from_descriptor(RootFilesystemDescriptor {
        mode: RootFilesystemMode::Ephemeral,
        disable_default_base_layer: true,
        lowers: Vec::new(),
        bootstrap_entries: Vec::new(),
    })
    .expect("build root filesystem");
    let mut kernel = KernelVm::new(MountTable::new(root), config);
    let process = kernel
        .create_virtual_process(
            DRIVER,
            DRIVER,
            "root-ownership-check",
            Vec::new(),
            VirtualProcessOptions::default(),
        )
        .expect("create process");
    let pid = process.pid();
    kernel
        .write_file("/target", b"data")
        .expect("create target");
    kernel.symlink("/target", "/link").expect("create link");

    kernel
        .chown_for_process(DRIVER, pid, "/link", UNCHANGED, UNCHANGED, false)
        .expect("lchown through mounted root overlay");
    assert!(kernel.lstat("/link").expect("lstat link").is_symbolic_link);
}
