use agent_os_kernel::bridge::LifecycleState;
use agent_os_kernel::command_registry::CommandDriver;
use agent_os_kernel::kernel::{KernelVm, KernelVmConfig, SpawnOptions};
use agent_os_kernel::permissions::Permissions;
use agent_os_kernel::process_table::SIGPIPE;
use agent_os_kernel::pty::LineDisciplineConfig;
use agent_os_kernel::vfs::MemoryFileSystem;
use std::time::Duration;

#[test]
fn minimal_vm_lifecycle_transitions_between_ready_busy_and_terminated() {
    let mut config = KernelVmConfig::new("vm-kernel");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");

    assert_eq!(kernel.state(), LifecycleState::Ready);

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
    assert_eq!(kernel.state(), LifecycleState::Busy);

    let (master_fd, slave_fd, path) = kernel.open_pty("shell", process.pid()).expect("open pty");
    assert!(path.starts_with("/dev/pts/"));
    kernel
        .pty_set_discipline(
            "shell",
            process.pid(),
            master_fd,
            LineDisciplineConfig {
                canonical: Some(false),
                echo: Some(false),
                isig: Some(false),
            },
        )
        .expect("set raw mode");

    kernel
        .fd_write("shell", process.pid(), master_fd, b"kernel-ready")
        .expect("write PTY input");
    let data = kernel
        .fd_read("shell", process.pid(), slave_fd, 64)
        .expect("read PTY slave");
    assert_eq!(String::from_utf8(data).expect("valid utf8"), "kernel-ready");

    process.finish(0);
    let (_, exit_code) = kernel.wait_and_reap(process.pid()).expect("reap shell");
    assert_eq!(exit_code, 0);
    assert_eq!(kernel.state(), LifecycleState::Ready);
    assert_eq!(kernel.resource_snapshot().fd_tables, 0);
    assert_eq!(kernel.resource_snapshot().open_fds, 0);

    kernel.dispose().expect("dispose kernel");
    assert_eq!(kernel.state(), LifecycleState::Terminated);
}

#[test]
fn dispose_kills_running_processes_and_cleans_special_resources() {
    let mut config = KernelVmConfig::new("vm-dispose");
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

    kernel.dispose().expect("dispose kernel");
    assert_eq!(kernel.state(), LifecycleState::Terminated);
    assert_eq!(process.wait(Duration::from_millis(50)), Some(143));
    assert_eq!(process.kill_signals(), vec![15]);

    let snapshot = kernel.resource_snapshot();
    assert_eq!(snapshot.fd_tables, 0);
    assert_eq!(snapshot.open_fds, 0);
    assert_eq!(snapshot.pipes, 0);
    assert_eq!(snapshot.ptys, 0);
}

#[test]
fn process_exit_cleanup_closes_pipe_writers_and_returns_eof_to_readers() {
    let mut config = KernelVmConfig::new("vm-process-exit-pipe");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");

    let writer = kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                ..SpawnOptions::default()
            },
        )
        .expect("spawn writer");
    let (read_fd, write_fd) = kernel
        .open_pipe("shell", writer.pid())
        .expect("open writer pipe");
    let reader = kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                parent_pid: Some(writer.pid()),
                ..SpawnOptions::default()
            },
        )
        .expect("spawn reader");

    kernel
        .fd_close("shell", reader.pid(), write_fd)
        .expect("close inherited write end");
    kernel
        .fd_write("shell", writer.pid(), write_fd, b"before-exit")
        .expect("write pipe contents");
    let bytes = kernel
        .fd_read("shell", reader.pid(), read_fd, 64)
        .expect("read pipe contents");
    assert_eq!(String::from_utf8(bytes).expect("valid utf8"), "before-exit");

    writer.finish(0);
    assert_eq!(
        kernel
            .open_pipe("shell", writer.pid())
            .expect_err("exited writer should lose PID ownership")
            .code(),
        "ESRCH"
    );

    let eof = kernel
        .fd_read("shell", reader.pid(), read_fd, 64)
        .expect("read EOF after writer exit");
    assert!(eof.is_empty());
}

#[test]
fn broken_pipe_writes_deliver_sigpipe_and_return_epipe() {
    let mut config = KernelVmConfig::new("vm-broken-pipe-sigpipe");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");

    let writer = kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                ..SpawnOptions::default()
            },
        )
        .expect("spawn writer");
    let (read_fd, write_fd) = kernel
        .open_pipe("shell", writer.pid())
        .expect("open writer pipe");

    kernel
        .fd_close("shell", writer.pid(), read_fd)
        .expect("close inherited read end");

    let error = kernel
        .fd_write("shell", writer.pid(), write_fd, b"fail")
        .expect_err("broken pipe writes should fail");
    assert_eq!(error.code(), "EPIPE");
    assert_eq!(writer.kill_signals(), vec![SIGPIPE]);
    assert_eq!(writer.wait(Duration::from_millis(50)), Some(128 + SIGPIPE));
}

#[test]
fn process_exit_cleanup_removes_fd_tables_before_and_after_reap() {
    let mut config = KernelVmConfig::new("vm-process-exit-fds");
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
        .expect("spawn process");
    let _ = kernel.open_pipe("shell", process.pid()).expect("open pipe");
    let _ = kernel.open_pty("shell", process.pid()).expect("open pty");

    process.finish(0);

    let snapshot_after_exit = kernel.resource_snapshot();
    assert_eq!(snapshot_after_exit.fd_tables, 0);
    assert_eq!(snapshot_after_exit.open_fds, 0);
    assert_eq!(snapshot_after_exit.pipes, 0);
    assert_eq!(snapshot_after_exit.ptys, 0);

    let (_, exit_code) = kernel
        .wait_and_reap(process.pid())
        .expect("wait and reap exited process");
    assert_eq!(exit_code, 0);

    let snapshot_after_reap = kernel.resource_snapshot();
    assert_eq!(snapshot_after_reap.fd_tables, 0);
    assert_eq!(snapshot_after_reap.open_fds, 0);
    assert_eq!(
        kernel
            .fd_stat("shell", process.pid(), 0)
            .expect_err("reaped process should not keep FD entries")
            .code(),
        "ESRCH"
    );
}
