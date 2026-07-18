use agentos_kernel::bridge::LifecycleState;
use agentos_kernel::command_registry::CommandDriver;
use agentos_kernel::kernel::{KernelVm, KernelVmConfig, SpawnOptions};
use agentos_kernel::permissions::Permissions;
use agentos_kernel::process_runtime::ProcessExit;
use agentos_kernel::process_table::SIGPIPE;
use agentos_kernel::pty::LineDisciplineConfig;
use agentos_kernel::vfs::MemoryFileSystem;
use std::sync::Arc;
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
                ..Default::default()
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
fn isatty_recognizes_both_kernel_pty_ends() {
    let mut config = KernelVmConfig::new("vm-kernel-pty-isatty");
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
    let (master_fd, slave_fd, _) = kernel
        .open_pty("shell", process.pid())
        .expect("open kernel PTY");

    assert!(
        kernel
            .isatty("shell", process.pid(), master_fd)
            .expect("classify PTY master"),
        "Linux PTY masters are terminal descriptors"
    );
    assert!(
        kernel
            .isatty("shell", process.pid(), slave_fd)
            .expect("classify PTY slave"),
        "PTY slaves are terminal descriptors"
    );

    process.finish(0);
    kernel.wait_and_reap(process.pid()).expect("reap shell");
}

#[test]
fn raw_mode_recovery_lease_is_limited_to_foreground_process_group() {
    let mut config = KernelVmConfig::new("vm-pty-raw-owner");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");

    let shell = kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                ..SpawnOptions::default()
            },
        )
        .expect("spawn shell");
    let (master_fd, slave_fd, _) = kernel
        .open_pty("shell", shell.pid())
        .expect("open controlling pty");
    kernel
        .fd_dup2("shell", shell.pid(), slave_fd, 0)
        .expect("install shell stdin");
    kernel
        .pty_set_foreground_pgid("shell", shell.pid(), master_fd, shell.pid())
        .expect("make shell group foreground");

    let foreground_child = kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                parent_pid: Some(shell.pid()),
                ..SpawnOptions::default()
            },
        )
        .expect("spawn foreground child");
    assert!(kernel
        .pty_set_raw_mode("shell", foreground_child.pid(), 0, true)
        .expect("foreground raw mode")
        .is_some());

    let background_child = kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                parent_pid: Some(shell.pid()),
                ..SpawnOptions::default()
            },
        )
        .expect("spawn background child");
    kernel
        .setpgid("shell", background_child.pid(), background_child.pid())
        .expect("move child into background process group");
    assert_eq!(
        kernel
            .pty_set_raw_mode("shell", background_child.pid(), 0, true)
            .expect("background raw mode"),
        None,
        "background process must not own foreground recovery"
    );
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

    let exit_reporter = process.exit_reporter();
    let _controls = process
        .attach_runtime_control(Arc::new(move || {
            exit_reporter
                .report_exit(ProcessExit::Signaled {
                    signal: 15,
                    core_dumped: false,
                })
                .expect("report SIGTERM exit");
        }))
        .expect("attach runtime controls");

    kernel.dispose().expect("dispose kernel");
    assert_eq!(kernel.state(), LifecycleState::Terminated);
    assert_eq!(process.wait(Duration::from_millis(50)), Some(143));

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

    let exit_reporter = writer.exit_reporter();
    let _controls = writer
        .attach_runtime_control(Arc::new(move || {
            exit_reporter
                .report_exit(ProcessExit::Signaled {
                    signal: SIGPIPE,
                    core_dumped: false,
                })
                .expect("report SIGPIPE exit");
        }))
        .expect("attach runtime controls");

    let error = kernel
        .fd_write("shell", writer.pid(), write_fd, b"fail")
        .expect_err("broken pipe writes should fail");
    assert_eq!(error.code(), "EPIPE");
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

#[test]
fn spawn_process_executes_shebang_scripts_with_registered_interpreters() {
    let mut config = KernelVmConfig::new("vm-shebang");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");
    kernel
        .register_driver(CommandDriver::new("node", ["node"]))
        .expect("register node");

    kernel
        .write_file("/tmp/script.sh", b"#!/bin/sh -eu\necho shell\n".to_vec())
        .expect("write shell script");
    kernel
        .chmod("/tmp/script.sh", 0o755)
        .expect("chmod shell script");
    let shell_process = kernel
        .spawn_process(
            "/tmp/script.sh",
            vec![String::from("arg")],
            SpawnOptions::default(),
        )
        .expect("spawn shell script");
    assert_eq!(
        kernel
            .read_file(&format!("/proc/{}/cmdline", shell_process.pid()))
            .expect("read shell cmdline"),
        b"sh\0-eu\0/tmp/script.sh\0arg\0".to_vec()
    );

    kernel
        .write_file(
            "/tmp/script.mjs",
            b"#!/usr/bin/env node --trace-warnings\nconsole.log('node');\n".to_vec(),
        )
        .expect("write node script");
    kernel
        .chmod("/tmp/script.mjs", 0o755)
        .expect("chmod node script");
    let node_process = kernel
        .spawn_process("/tmp/script.mjs", Vec::new(), SpawnOptions::default())
        .expect("spawn node script");
    assert_eq!(
        kernel
            .read_file(&format!("/proc/{}/cmdline", node_process.pid()))
            .expect("read node cmdline"),
        b"node\0--trace-warnings\0/tmp/script.mjs\0".to_vec()
    );
}

#[test]
fn spawn_process_rejects_invalid_shebang_scripts() {
    let mut config = KernelVmConfig::new("vm-shebang-errors");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");

    kernel
        .write_file("/tmp/missing.sh", b"#!/missing/interpreter\n".to_vec())
        .expect("write missing-interpreter script");
    kernel
        .chmod("/tmp/missing.sh", 0o755)
        .expect("chmod missing-interpreter script");
    let missing = kernel
        .spawn_process("/tmp/missing.sh", Vec::new(), SpawnOptions::default())
        .expect_err("missing interpreter should fail");
    assert_eq!(missing.code(), "ENOENT");

    let long_shebang = format!("#!/{0}\n", "a".repeat(256));
    kernel
        .write_file("/tmp/long.sh", long_shebang.into_bytes())
        .expect("write long-shebang script");
    kernel
        .chmod("/tmp/long.sh", 0o755)
        .expect("chmod long-shebang script");
    let long_error = kernel
        .spawn_process("/tmp/long.sh", Vec::new(), SpawnOptions::default())
        .expect_err("overlong shebang should fail");
    assert_eq!(long_error.code(), "ENOEXEC");
}

#[test]
fn driver_registration_rejects_command_names_that_escape_bin_stubs() {
    let mut config = KernelVmConfig::new("vm-command-registry-traversal");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);

    let error = kernel
        .register_driver(CommandDriver::new("malicious", ["safe", "../escape"]))
        .expect_err("path-like command names should be rejected");

    assert_eq!(error.code(), "EINVAL");
    assert!(
        error.to_string().contains("invalid command name"),
        "unexpected error: {error}"
    );
    assert!(!kernel.exists("/bin").expect("check /bin"));
    assert!(!kernel.exists("/bin/safe").expect("check safe stub"));
    assert!(!kernel.exists("/escape").expect("check escaped stub"));
}
