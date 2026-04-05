use agent_os_kernel::command_registry::CommandDriver;
use agent_os_kernel::fd_table::{O_CREAT, O_RDWR};
use agent_os_kernel::kernel::{
    ExecOptions, KernelVm, KernelVmConfig, OpenShellOptions, SpawnOptions, WaitPidFlags,
    WaitPidResult, SEEK_SET,
};
use agent_os_kernel::permissions::Permissions;
use agent_os_kernel::process_table::ProcessWaitEvent;
use agent_os_kernel::vfs::{MemoryFileSystem, VirtualFileSystem};

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
