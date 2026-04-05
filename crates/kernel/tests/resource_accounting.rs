use agent_os_kernel::command_registry::CommandDriver;
use agent_os_kernel::kernel::{KernelVm, KernelVmConfig, SpawnOptions};
use agent_os_kernel::pty::LineDisciplineConfig;
use agent_os_kernel::resource_accounting::ResourceLimits;
use agent_os_kernel::vfs::{MemoryFileSystem, VirtualFileSystem};

#[test]
fn resource_snapshot_counts_processes_fds_pipes_and_ptys() {
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), KernelVmConfig::new("vm-resources"));
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
    let (read_fd, write_fd) = kernel.open_pipe("shell", process.pid()).expect("open pipe");
    let (master_fd, slave_fd, _) = kernel.open_pty("shell", process.pid()).expect("open pty");
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
        .expect("set raw pty");

    kernel
        .fd_write("shell", process.pid(), write_fd, b"pipe-data")
        .expect("write pipe");
    kernel
        .fd_write("shell", process.pid(), master_fd, b"term")
        .expect("write pty");

    let snapshot = kernel.resource_snapshot();
    assert_eq!(snapshot.running_processes, 1);
    assert_eq!(snapshot.fd_tables, 1);
    assert_eq!(snapshot.pipes, 1);
    assert_eq!(snapshot.ptys, 1);
    assert_eq!(snapshot.open_fds, 7);
    assert_eq!(snapshot.pipe_buffered_bytes, 9);
    assert_eq!(snapshot.pty_buffered_input_bytes, 4);
    assert_eq!(snapshot.pty_buffered_output_bytes, 0);

    let _ = kernel
        .fd_read("shell", process.pid(), read_fd, 16)
        .expect("drain pipe");
    let _ = kernel
        .fd_read("shell", process.pid(), slave_fd, 16)
        .expect("drain pty");
    process.finish(0);
    kernel.wait_and_reap(process.pid()).expect("reap process");
}

#[test]
fn resource_limits_reject_extra_processes_pipes_and_ptys() {
    let mut config = KernelVmConfig::new("vm-limits");
    config.resources = ResourceLimits {
        max_processes: Some(1),
        max_open_fds: Some(6),
        max_pipes: Some(1),
        max_ptys: Some(1),
        ..ResourceLimits::default()
    };

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
        .expect("spawn initial process");

    let error = kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                ..SpawnOptions::default()
            },
        )
        .expect_err("second process should exceed process limit");
    assert_eq!(error.code(), "EAGAIN");

    kernel
        .open_pipe("shell", process.pid())
        .expect("first pipe should succeed");
    let error = kernel
        .open_pipe("shell", process.pid())
        .expect_err("second pipe should exceed pipe limit");
    assert_eq!(error.code(), "EAGAIN");

    let error = kernel
        .open_pty("shell", process.pid())
        .expect_err("global FD limit should prevent PTY allocation");
    assert_eq!(error.code(), "EAGAIN");

    process.finish(0);
    kernel.wait_and_reap(process.pid()).expect("reap process");
}

#[test]
fn filesystem_limits_reject_inode_growth_and_file_expansion() {
    let mut config = KernelVmConfig::new("vm-filesystem-limits");
    config.resources = ResourceLimits {
        max_filesystem_bytes: Some(5),
        max_inode_count: Some(4),
        ..ResourceLimits::default()
    };

    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .write_file("/tmp/a.txt", b"hello".to_vec())
        .expect("seed file within byte limit");
    kernel
        .create_dir("/tmp/dir")
        .expect("create directory within inode limit");

    let write_error = kernel
        .write_file("/tmp/b.txt", b"!".to_vec())
        .expect_err("additional file should exceed inode limit");
    assert_eq!(write_error.code(), "ENOSPC");

    let truncate_error = kernel
        .truncate("/tmp/a.txt", 6)
        .expect_err("truncate should exceed filesystem byte limit");
    assert_eq!(truncate_error.code(), "ENOSPC");
    assert_eq!(
        kernel
            .read_file("/tmp/a.txt")
            .expect("file should stay unchanged"),
        b"hello".to_vec()
    );
}

#[test]
fn filesystem_limits_reject_fd_pwrite_before_resizing_file() {
    let mut config = KernelVmConfig::new("vm-fd-pwrite-limit");
    config.resources = ResourceLimits {
        max_filesystem_bytes: Some(16),
        ..ResourceLimits::default()
    };

    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");
    kernel
        .filesystem_mut()
        .write_file("/tmp/data.txt", b"abc".to_vec())
        .expect("seed file");

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
    let fd = kernel
        .fd_open("shell", process.pid(), "/tmp/data.txt", 0, None)
        .expect("open file");

    let error = kernel
        .fd_pwrite("shell", process.pid(), fd, b"z", 16)
        .expect_err("pwrite should exceed filesystem byte limit");
    assert_eq!(error.code(), "ENOSPC");
    assert_eq!(
        kernel
            .read_file("/tmp/data.txt")
            .expect("file should stay unchanged"),
        b"abc".to_vec()
    );

    process.finish(0);
    kernel.wait_and_reap(process.pid()).expect("reap shell");
}
