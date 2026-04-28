use agent_os_kernel::command_registry::CommandDriver;
use agent_os_kernel::kernel::{KernelVm, KernelVmConfig, SpawnOptions};
use agent_os_kernel::mount_table::{MountOptions, MountTable};
use agent_os_kernel::permissions::Permissions;
use agent_os_kernel::pty::LineDisciplineConfig;
use agent_os_kernel::resource_accounting::{
    ResourceLimits, DEFAULT_MAX_CONNECTIONS, DEFAULT_MAX_OPEN_FDS, DEFAULT_MAX_PIPES,
    DEFAULT_MAX_PROCESSES, DEFAULT_MAX_PTYS, DEFAULT_MAX_SOCKETS, DEFAULT_VIRTUAL_CPU_COUNT,
};
use agent_os_kernel::vfs::{MemoryFileSystem, VirtualFileSystem};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

#[test]
fn resource_snapshot_counts_processes_fds_pipes_and_ptys() {
    let mut config = KernelVmConfig::new("vm-resources");
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
fn resource_limits_default_to_bounded_values() {
    let limits = ResourceLimits::default();

    assert_eq!(limits.virtual_cpu_count, Some(DEFAULT_VIRTUAL_CPU_COUNT));
    assert_eq!(limits.max_processes, Some(DEFAULT_MAX_PROCESSES));
    assert_eq!(limits.max_open_fds, Some(DEFAULT_MAX_OPEN_FDS));
    assert_eq!(limits.max_pipes, Some(DEFAULT_MAX_PIPES));
    assert_eq!(limits.max_ptys, Some(DEFAULT_MAX_PTYS));
    assert_eq!(limits.max_sockets, Some(DEFAULT_MAX_SOCKETS));
    assert_eq!(limits.max_connections, Some(DEFAULT_MAX_CONNECTIONS));
}

#[test]
fn resource_limits_reject_extra_processes_pipes_and_ptys() {
    let mut config = KernelVmConfig::new("vm-limits");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_processes: Some(1),
        max_open_fds: Some(16),
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

    kernel
        .open_pty("shell", process.pid())
        .expect("first PTY should fit within the configured caps");
    let error = kernel
        .open_pty("shell", process.pid())
        .expect_err("second PTY should exceed PTY limit");
    assert_eq!(error.code(), "EAGAIN");

    process.finish(0);
    kernel.wait_and_reap(process.pid()).expect("reap process");
}

#[test]
fn resource_limits_reject_global_fd_growth_with_enfile() {
    let mut config = KernelVmConfig::new("vm-open-fd-limit");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_open_fds: Some(8),
        ..ResourceLimits::default()
    };

    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");

    kernel
        .write_file("/tmp/a.txt", b"a".to_vec())
        .expect("seed first file");
    kernel
        .write_file("/tmp/b.txt", b"b".to_vec())
        .expect("seed second file");
    kernel
        .write_file("/tmp/c.txt", b"c".to_vec())
        .expect("seed third file");

    let process_a = kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                ..SpawnOptions::default()
            },
        )
        .expect("spawn first process");
    kernel
        .fd_open("shell", process_a.pid(), "/tmp/a.txt", 0, None)
        .expect("first extra FD should fit");
    kernel
        .fd_open("shell", process_a.pid(), "/tmp/b.txt", 0, None)
        .expect("second extra FD should fit");

    let process_b = kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                ..SpawnOptions::default()
            },
        )
        .expect("spawn second process at the global FD ceiling");

    let error = kernel
        .fd_open("shell", process_b.pid(), "/tmp/c.txt", 0, None)
        .expect_err("extra open should exceed the VM-wide FD limit");
    assert_eq!(error.code(), "ENFILE");

    process_a.finish(0);
    kernel
        .wait_and_reap(process_a.pid())
        .expect("reap first process");
    process_b.finish(0);
    kernel
        .wait_and_reap(process_b.pid())
        .expect("reap second process");
}

#[test]
fn zombie_processes_count_against_process_limits_until_reaped() {
    let mut config = KernelVmConfig::new("vm-zombie-process-limit");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_processes: Some(1),
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
    process.finish(0);

    let error = kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                ..SpawnOptions::default()
            },
        )
        .expect_err("zombie should still count against process limit");
    assert_eq!(error.code(), "EAGAIN");

    kernel.wait_and_reap(process.pid()).expect("reap zombie");
    kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                ..SpawnOptions::default()
            },
        )
        .expect("spawn should succeed after zombie is reaped");
}

#[test]
fn filesystem_limits_reject_inode_growth_and_file_expansion() {
    let mut config = KernelVmConfig::new("vm-filesystem-limits");
    config.permissions = Permissions::allow_all();
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
    config.permissions = Permissions::allow_all();
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

#[test]
fn filesystem_limits_ignore_read_only_mount_usage() {
    let mut config = KernelVmConfig::new("vm-mounted-filesystem-limits");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_filesystem_bytes: Some(16),
        ..ResourceLimits::default()
    };

    let mut mounted = MemoryFileSystem::new();
    mounted
        .write_file("/big.bin", vec![b'x'; 1024])
        .expect("seed mounted file");

    let mut kernel = KernelVm::new(MountTable::new(MemoryFileSystem::new()), config);
    kernel
        .filesystem_mut()
        .inner_mut()
        .inner_mut()
        .mount("/mnt", mounted, MountOptions::new("memory").read_only(true))
        .expect("mount read-only filesystem");

    kernel
        .write_file("/tmp/a.txt", b"ok".to_vec())
        .expect("mounted files should not count against root filesystem byte limits");
}

#[test]
fn blocking_pipe_and_pty_reads_time_out_instead_of_hanging_forever() {
    let mut config = KernelVmConfig::new("vm-read-timeouts");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_blocking_read_ms: Some(25),
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
        .expect("spawn shell");

    let (read_fd, _write_fd) = kernel.open_pipe("shell", process.pid()).expect("open pipe");
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

    let started = Instant::now();
    let pipe_error = kernel
        .fd_read("shell", process.pid(), read_fd, 16)
        .expect_err("empty pipe read should time out");
    assert_eq!(pipe_error.code(), "EAGAIN");
    assert!(
        started.elapsed() >= Duration::from_millis(20),
        "pipe read timed out too early: {:?}",
        started.elapsed()
    );

    let started = Instant::now();
    let pty_error = kernel
        .fd_read("shell", process.pid(), slave_fd, 16)
        .expect_err("empty PTY read should time out");
    assert_eq!(pty_error.code(), "EAGAIN");
    assert!(
        started.elapsed() >= Duration::from_millis(20),
        "PTY read timed out too early: {:?}",
        started.elapsed()
    );

    process.finish(0);
    kernel.wait_and_reap(process.pid()).expect("reap shell");
}

#[test]
fn resource_limits_reject_oversized_spawn_payloads() {
    let mut config = KernelVmConfig::new("vm-spawn-payload-limits");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_process_argv_bytes: Some(13),
        max_process_env_bytes: Some(15),
        ..ResourceLimits::default()
    };

    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");

    let argv_error = kernel
        .spawn_process(
            "sh",
            vec![String::from("1234567890")],
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                ..SpawnOptions::default()
            },
        )
        .expect_err("oversized argv should be rejected");
    assert_eq!(argv_error.code(), "EINVAL");

    let env_error = kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                env: BTreeMap::from([(String::from("LONG"), String::from("1234567890"))]),
                ..SpawnOptions::default()
            },
        )
        .expect_err("oversized environment should be rejected");
    assert_eq!(env_error.code(), "EINVAL");
}

#[test]
fn resource_limits_reject_oversized_pread_and_write_operations() {
    let mut config = KernelVmConfig::new("vm-io-op-limits");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_pread_bytes: Some(4),
        max_fd_write_bytes: Some(3),
        ..ResourceLimits::default()
    };

    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");
    kernel
        .write_file("/tmp/data.txt", b"hello".to_vec())
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

    let pread_error = kernel
        .fd_pread("shell", process.pid(), fd, 5, 0)
        .expect_err("oversized pread should be rejected");
    assert_eq!(pread_error.code(), "EINVAL");

    let write_error = kernel
        .fd_write("shell", process.pid(), fd, b"four")
        .expect_err("oversized fd_write should be rejected");
    assert_eq!(write_error.code(), "EINVAL");

    let pwrite_error = kernel
        .fd_pwrite("shell", process.pid(), fd, b"four", 0)
        .expect_err("oversized fd_pwrite should be rejected");
    assert_eq!(pwrite_error.code(), "EINVAL");

    assert_eq!(
        kernel
            .read_file("/tmp/data.txt")
            .expect("file should remain unchanged"),
        b"hello".to_vec()
    );

    process.finish(0);
    kernel.wait_and_reap(process.pid()).expect("reap shell");
}

#[test]
fn resource_limits_reject_oversized_readdir_batches() {
    let mut config = KernelVmConfig::new("vm-readdir-limit");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_readdir_entries: Some(2),
        ..ResourceLimits::default()
    };

    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel.create_dir("/tmp").expect("create tmp");
    kernel
        .write_file("/tmp/a.txt", b"a".to_vec())
        .expect("write first entry");
    kernel
        .write_file("/tmp/b.txt", b"b".to_vec())
        .expect("write second entry");
    kernel
        .write_file("/tmp/c.txt", b"c".to_vec())
        .expect("write third entry");

    let error = kernel
        .read_dir("/tmp")
        .expect_err("oversized readdir batch should be rejected");
    assert_eq!(error.code(), "ENOMEM");
}
