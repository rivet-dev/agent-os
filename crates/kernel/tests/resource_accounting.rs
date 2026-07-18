use agentos_bridge::queue_tracker::{set_limit_warning_handler, LimitWarning, TrackedLimit};
use agentos_kernel::command_registry::CommandDriver;
use agentos_kernel::fd_table::O_RDWR;
use agentos_kernel::kernel::{KernelVm, KernelVmConfig, SpawnOptions, SEEK_SET};
use agentos_kernel::mount_table::{MountOptions, MountTable};
use agentos_kernel::permissions::Permissions;
use agentos_kernel::process_table::SIGSTOP;
use agentos_kernel::pty::LineDisciplineConfig;
use agentos_kernel::resource_accounting::{
    ResourceLimits, DEFAULT_BLOCKING_READ_TIMEOUT_MS, DEFAULT_MAX_CONNECTIONS,
    DEFAULT_MAX_OPEN_FDS, DEFAULT_MAX_PIPES, DEFAULT_MAX_PROCESSES, DEFAULT_MAX_PTYS,
    DEFAULT_MAX_SOCKETS, DEFAULT_MAX_SOCKET_BUFFERED_BYTES, DEFAULT_MAX_SOCKET_DATAGRAM_QUEUE_LEN,
    DEFAULT_VIRTUAL_CPU_COUNT,
};
use agentos_kernel::root_fs::{
    FilesystemEntry, RootFileSystem, RootFilesystemDescriptor, RootFilesystemMode,
    RootFilesystemSnapshot,
};
use agentos_kernel::socket_table::{InetSocketAddress, SocketSpec};
use agentos_kernel::vfs::{MemoryFileSystem, VirtualFileSystem};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
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
                ..Default::default()
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
    assert_eq!(
        limits.max_socket_buffered_bytes,
        Some(DEFAULT_MAX_SOCKET_BUFFERED_BYTES)
    );
    assert_eq!(
        limits.max_socket_datagram_queue_len,
        Some(DEFAULT_MAX_SOCKET_DATAGRAM_QUEUE_LEN)
    );
    assert_eq!(
        limits.max_blocking_read_ms,
        Some(DEFAULT_BLOCKING_READ_TIMEOUT_MS)
    );
    assert_eq!(DEFAULT_BLOCKING_READ_TIMEOUT_MS, 30_000);
}

#[test]
fn socket_stream_buffered_bytes_count_against_resource_limits() {
    let mut config = KernelVmConfig::new("vm-socket-buffer-limit");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_socket_buffered_bytes: Some(5),
        ..ResourceLimits::default()
    };

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
    let reader = kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                ..SpawnOptions::default()
            },
        )
        .expect("spawn reader");
    let writer_socket = kernel
        .socket_create("shell", writer.pid(), SocketSpec::tcp())
        .expect("create writer socket");
    let reader_socket = kernel
        .socket_create("shell", reader.pid(), SocketSpec::tcp())
        .expect("create reader socket");
    kernel
        .socket_connect_pair("shell", writer.pid(), writer_socket, reader_socket)
        .expect("connect socket pair");

    kernel
        .socket_write("shell", writer.pid(), writer_socket, b"12345")
        .expect("fill stream receive buffer budget");
    assert_eq!(kernel.resource_snapshot().socket_buffered_bytes, 5);

    let error = kernel
        .socket_write("shell", writer.pid(), writer_socket, b"!")
        .expect_err("extra byte should exceed buffered byte limit");
    assert_eq!(error.code(), "EAGAIN");
    assert_eq!(kernel.resource_snapshot().socket_buffered_bytes, 5);

    let drained = kernel
        .socket_read("shell", reader.pid(), reader_socket, 5)
        .expect("drain stream receive buffer")
        .expect("stream payload");
    assert_eq!(drained, b"12345");
    assert_eq!(kernel.resource_snapshot().socket_buffered_bytes, 0);

    kernel
        .socket_write("shell", writer.pid(), writer_socket, b"!")
        .expect("write should succeed after draining stream buffer");
    assert_eq!(kernel.resource_snapshot().socket_buffered_bytes, 1);
}

#[test]
fn udp_datagram_queue_counts_against_resource_limits() {
    let mut config = KernelVmConfig::new("vm-socket-datagram-limit");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_socket_datagram_queue_len: Some(1),
        ..ResourceLimits::default()
    };

    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");
    let sender = kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                ..SpawnOptions::default()
            },
        )
        .expect("spawn sender");
    let receiver = kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                ..SpawnOptions::default()
            },
        )
        .expect("spawn receiver");
    let sender_socket = kernel
        .socket_create("shell", sender.pid(), SocketSpec::udp())
        .expect("create sender socket");
    kernel
        .socket_bind_inet(
            "shell",
            sender.pid(),
            sender_socket,
            InetSocketAddress::new("127.0.0.1", 54196),
        )
        .expect("bind sender socket");
    let receiver_socket = kernel
        .socket_create("shell", receiver.pid(), SocketSpec::udp())
        .expect("create receiver socket");
    kernel
        .socket_bind_inet(
            "shell",
            receiver.pid(),
            receiver_socket,
            InetSocketAddress::new("127.0.0.1", 43196),
        )
        .expect("bind receiver socket");

    kernel
        .socket_send_to_inet_loopback(
            "shell",
            sender.pid(),
            sender_socket,
            InetSocketAddress::new("127.0.0.1", 43196),
            b"one",
        )
        .expect("enqueue first datagram");
    let snapshot = kernel.resource_snapshot();
    assert_eq!(snapshot.socket_datagram_queue_len, 1);
    assert_eq!(snapshot.socket_buffered_bytes, 3);

    let error = kernel
        .socket_send_to_inet_loopback(
            "shell",
            sender.pid(),
            sender_socket,
            InetSocketAddress::new("127.0.0.1", 43196),
            b"two",
        )
        .expect_err("second datagram should exceed queue length limit");
    assert_eq!(error.code(), "EAGAIN");
    let snapshot = kernel.resource_snapshot();
    assert_eq!(snapshot.socket_datagram_queue_len, 1);
    assert_eq!(snapshot.socket_buffered_bytes, 3);

    let datagram = kernel
        .socket_recv_datagram("shell", receiver.pid(), receiver_socket, 16)
        .expect("receive datagram")
        .expect("datagram payload");
    assert_eq!(datagram.payload(), b"one");
    let snapshot = kernel.resource_snapshot();
    assert_eq!(snapshot.socket_datagram_queue_len, 0);
    assert_eq!(snapshot.socket_buffered_bytes, 0);

    kernel
        .socket_send_to_inet_loopback(
            "shell",
            sender.pid(),
            sender_socket,
            InetSocketAddress::new("127.0.0.1", 43196),
            b"two",
        )
        .expect("send should succeed after draining datagram queue");
    let snapshot = kernel.resource_snapshot();
    assert_eq!(snapshot.socket_datagram_queue_len, 1);
    assert_eq!(snapshot.socket_buffered_bytes, 3);
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
fn stopped_processes_remain_visible_to_the_process_limit() {
    let mut config = KernelVmConfig::new("vm-stopped-process-limit");
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
    let runtime_control = process
        .attach_runtime_control(Arc::new(|| {}))
        .expect("attach runtime control");

    kernel
        .signal_process("shell", process.pid() as i32, SIGSTOP)
        .expect("stop process");
    let stop = runtime_control.pending();
    assert_eq!(stop.stopped, Some(true));
    runtime_control
        .acknowledge(stop)
        .expect("acknowledge stopped runtime state");
    let snapshot = kernel.resource_snapshot();
    assert_eq!(snapshot.running_processes, 0);
    assert_eq!(snapshot.stopped_processes, 1);
    assert_eq!(snapshot.exited_processes, 0);

    let error = kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                ..SpawnOptions::default()
            },
        )
        .expect_err("stopped process should still consume the process slot");
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
        .fd_open("shell", process.pid(), "/tmp/data.txt", O_RDWR, None)
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
fn filesystem_limits_reject_fd_allocate_before_resizing_file() {
    let mut config = KernelVmConfig::new("vm-fd-allocate-limit");
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
        .fd_open("shell", process.pid(), "/tmp/data.txt", O_RDWR, None)
        .expect("open file");

    kernel
        .fd_allocate("shell", process.pid(), fd, u64::MAX, 0)
        .expect("zero-length allocation is a no-op");
    assert_eq!(
        kernel
            .read_file("/tmp/data.txt")
            .expect("file is unchanged"),
        b"abc".to_vec()
    );

    let error = kernel
        .fd_allocate("shell", process.pid(), fd, 8, 9)
        .expect_err("allocation should exceed filesystem byte limit");
    assert_eq!(error.code(), "ENOSPC");
    assert_eq!(
        kernel
            .read_file("/tmp/data.txt")
            .expect("file is unchanged"),
        b"abc".to_vec()
    );

    process.finish(0);
    kernel.wait_and_reap(process.pid()).expect("reap shell");
}

#[test]
fn filesystem_limits_charge_rename_over_open_files_until_the_last_fd_closes() {
    let mut config = KernelVmConfig::new("vm-open-file-rename-accounting");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        // The registered `sh` command contributes a 32-byte executable stub;
        // the three 4-byte data files fill the remaining quota exactly.
        max_filesystem_bytes: Some(44),
        ..ResourceLimits::default()
    };

    let mut filesystem = MemoryFileSystem::new();
    filesystem
        .write_file("/tmp/dst.bin", b"dest".to_vec())
        .expect("seed original destination");
    filesystem
        .write_file("/tmp/src-one.bin", b"one!".to_vec())
        .expect("seed first source");
    filesystem
        .write_file("/tmp/src-two.bin", b"two!".to_vec())
        .expect("seed second source");
    let mut kernel = KernelVm::new(filesystem, config);
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
    let original_destination = kernel
        .fd_open("shell", process.pid(), "/tmp/dst.bin", O_RDWR, None)
        .expect("open original destination");
    kernel
        .rename("/tmp/src-one.bin", "/tmp/dst.bin")
        .expect("replace first open destination");
    let first_replacement = kernel
        .fd_open("shell", process.pid(), "/tmp/dst.bin", O_RDWR, None)
        .expect("open first replacement");
    let first_replacement_dup = kernel
        .fd_dup("shell", process.pid(), first_replacement)
        .expect("duplicate first replacement fd");
    kernel
        .rename("/tmp/src-two.bin", "/tmp/dst.bin")
        .expect("replace second open destination");

    let full_error = kernel
        .write_file("/tmp/extra.bin", b"x".to_vec())
        .expect_err("both anonymous destinations must remain charged");
    assert_eq!(full_error.code(), "ENOSPC");
    assert!(full_error
        .to_string()
        .contains("limits.resources.maxFilesystemBytes"));

    kernel
        .fd_close("shell", process.pid(), original_destination)
        .expect("close original destination");
    let duplicate_still_charged = kernel
        .write_file("/tmp/extra.bin", b"12345".to_vec())
        .expect_err("open duplicate must retain the anonymous destination charge");
    assert_eq!(duplicate_still_charged.code(), "ENOSPC");

    kernel
        .fd_close("shell", process.pid(), first_replacement)
        .expect("close first replacement fd");
    let last_duplicate_still_charged = kernel
        .write_file("/tmp/extra.bin", b"12345".to_vec())
        .expect_err("charge must remain until the last duplicate closes");
    assert_eq!(last_duplicate_still_charged.code(), "ENOSPC");

    kernel
        .fd_close("shell", process.pid(), first_replacement_dup)
        .expect("close last replacement duplicate");
    kernel
        .write_file("/tmp/extra.bin", b"12345678".to_vec())
        .expect("last close must release anonymous-file quota");

    process.finish(0);
    kernel.wait_and_reap(process.pid()).expect("reap shell");
}

#[test]
fn filesystem_limits_reject_anonymous_fd_growth_with_typed_limit_errors() {
    let mut config = KernelVmConfig::new("vm-anonymous-fd-growth-limit");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        // The registered `sh` stub (32 bytes) plus the 3-byte file leave two
        // bytes of headroom before anonymous growth must fail.
        max_filesystem_bytes: Some(37),
        ..ResourceLimits::default()
    };

    let mut filesystem = MemoryFileSystem::new();
    filesystem
        .write_file("/tmp/data.bin", b"abc".to_vec())
        .expect("seed file");
    let mut kernel = KernelVm::new(filesystem, config);
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
    let fd = kernel
        .fd_open("shell", process.pid(), "/tmp/data.bin", O_RDWR, None)
        .expect("open data file");
    kernel
        .remove_file("/tmp/data.bin")
        .expect("unlink data file");

    let pwrite_error = kernel
        .fd_pwrite("shell", process.pid(), fd, b"z", 5)
        .expect_err("anonymous pwrite growth must respect byte limit");
    assert_eq!(pwrite_error.code(), "ENOSPC");
    assert!(pwrite_error
        .to_string()
        .contains("limits.resources.maxFilesystemBytes"));

    let truncate_error = kernel
        .fd_truncate("shell", process.pid(), fd, 6)
        .expect_err("anonymous truncate growth must respect byte limit");
    assert_eq!(truncate_error.code(), "ENOSPC");
    assert!(truncate_error
        .to_string()
        .contains("limits.resources.maxFilesystemBytes"));
    assert_eq!(
        kernel
            .fd_pread("shell", process.pid(), fd, 3, 0)
            .expect("rejected growth must leave anonymous file unchanged"),
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
fn writable_mount_mutations_do_not_pollute_root_filesystem_usage_cache() {
    let mut config = KernelVmConfig::new("vm-writable-mount-accounting");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_filesystem_bytes: Some(4),
        ..ResourceLimits::default()
    };

    let root = RootFileSystem::from_descriptor(RootFilesystemDescriptor {
        mode: RootFilesystemMode::Ephemeral,
        disable_default_base_layer: true,
        lowers: Vec::new(),
        bootstrap_entries: Vec::new(),
    })
    .expect("build empty root filesystem");
    let mut kernel = KernelVm::new(MountTable::new(root), config);
    kernel
        .mount_filesystem("/mnt", MemoryFileSystem::new(), MountOptions::new("memory"))
        .expect("mount writable filesystem");

    kernel
        .snapshot_root_filesystem()
        .expect("prime root filesystem usage cache");
    for index in 0..8 {
        kernel
            .write_file(&format!("/mnt/file-{index}"), b"x".to_vec())
            .expect("mounted writes must not consume root filesystem quota");
    }

    let snapshot = kernel
        .snapshot_root_filesystem()
        .expect("mounted writes must not make root snapshot exceed its quota");
    assert!(
        snapshot
            .entries
            .iter()
            .all(|entry| !entry.path.starts_with("/mnt/")),
        "root snapshot must exclude mounted filesystem entries"
    );
}

#[test]
fn mount_topology_changes_invalidate_root_filesystem_usage_cache() {
    let mut config = KernelVmConfig::new("vm-mount-accounting-invalidation");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_filesystem_bytes: Some(4),
        ..ResourceLimits::default()
    };

    let root = RootFileSystem::from_descriptor(RootFilesystemDescriptor {
        mode: RootFilesystemMode::Ephemeral,
        disable_default_base_layer: true,
        lowers: Vec::new(),
        bootstrap_entries: Vec::new(),
    })
    .expect("build empty root filesystem");
    let mut kernel = KernelVm::new(MountTable::new(root), config);
    kernel
        .snapshot_root_filesystem()
        .expect("prime root filesystem usage cache");
    kernel
        .root_filesystem_mut()
        .expect("native root filesystem")
        .write_file("/oversized", vec![b'x'; 8])
        .expect("mutate root beneath kernel accounting");

    kernel
        .mount_filesystem("/mnt", MemoryFileSystem::new(), MountOptions::new("memory"))
        .expect("mount writable filesystem");
    let error = kernel
        .snapshot_root_filesystem()
        .expect_err("mount must invalidate stale root usage");
    assert_eq!(error.code(), "ENOSPC");
    assert!(
        error
            .to_string()
            .contains("limits.resources.maxFilesystemBytes"),
        "{error}"
    );

    kernel
        .root_filesystem_mut()
        .expect("native root filesystem")
        .remove_file("/oversized")
        .expect("restore root beneath kernel accounting");
    kernel
        .unmount_filesystem("/mnt")
        .expect("unmount writable filesystem");
    kernel
        .snapshot_root_filesystem()
        .expect("unmount must invalidate stale root usage");
}

#[test]
fn filesystem_limits_reject_overlay_rename_copy_up_before_materializing_lower_tree() {
    let mut config = KernelVmConfig::new("vm-overlay-rename-copy-up-limit");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_filesystem_bytes: Some(8),
        ..ResourceLimits::default()
    };

    let root = RootFileSystem::from_descriptor(RootFilesystemDescriptor {
        mode: RootFilesystemMode::Ephemeral,
        disable_default_base_layer: true,
        lowers: vec![RootFilesystemSnapshot {
            entries: vec![
                FilesystemEntry::directory("/lower"),
                FilesystemEntry::file("/lower/big.bin", vec![b'x'; 32]),
            ],
        }],
        bootstrap_entries: Vec::new(),
    })
    .expect("build root filesystem");
    let mut kernel = KernelVm::new(MountTable::new(root), config);

    let error = kernel
        .rename("/lower", "/moved")
        .expect_err("copying up lower tree should exceed byte limit");
    assert_eq!(error.code(), "ENOSPC");
    assert_eq!(
        kernel
            .read_file("/lower/big.bin")
            .expect("source tree should remain readable"),
        vec![b'x'; 32]
    );
    assert!(!kernel.exists("/moved").expect("check destination"));
}

#[test]
fn filesystem_limits_preserve_read_only_error_before_overlay_rename_copy_up_limit() {
    let mut config = KernelVmConfig::new("vm-overlay-rename-copy-up-read-only");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_filesystem_bytes: Some(8),
        ..ResourceLimits::default()
    };

    let mut root = RootFileSystem::from_descriptor(RootFilesystemDescriptor {
        mode: RootFilesystemMode::ReadOnly,
        disable_default_base_layer: true,
        lowers: vec![RootFilesystemSnapshot {
            entries: vec![
                FilesystemEntry::directory("/lower"),
                FilesystemEntry::file("/lower/big.bin", vec![b'x'; 32]),
            ],
        }],
        bootstrap_entries: Vec::new(),
    })
    .expect("build root filesystem");
    root.finish_bootstrap();
    let mut kernel = KernelVm::new(MountTable::new(root), config);

    let error = kernel
        .rename("/lower", "/moved")
        .expect_err("read-only root should reject before copy-up accounting");
    assert_eq!(error.code(), "EROFS");
}

#[test]
fn filesystem_limits_preserve_missing_destination_parent_before_overlay_rename_copy_up_limit() {
    let mut config = KernelVmConfig::new("vm-overlay-rename-copy-up-missing-parent");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_filesystem_bytes: Some(8),
        ..ResourceLimits::default()
    };

    let root = RootFileSystem::from_descriptor(RootFilesystemDescriptor {
        mode: RootFilesystemMode::Ephemeral,
        disable_default_base_layer: true,
        lowers: vec![RootFilesystemSnapshot {
            entries: vec![
                FilesystemEntry::directory("/lower"),
                FilesystemEntry::file("/lower/big.bin", vec![b'x'; 32]),
            ],
        }],
        bootstrap_entries: Vec::new(),
    })
    .expect("build root filesystem");
    let mut kernel = KernelVm::new(MountTable::new(root), config);

    let error = kernel
        .rename("/lower", "/missing/moved")
        .expect_err("missing destination parent should reject before copy-up accounting");
    assert_eq!(error.code(), "ENOENT");
}

#[test]
fn filesystem_limits_allow_overlay_rename_into_lower_only_destination_parent() {
    let mut config = KernelVmConfig::new("vm-overlay-rename-lower-destination-parent");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_inode_count: Some(3),
        ..ResourceLimits::default()
    };

    let root = RootFileSystem::from_descriptor(RootFilesystemDescriptor {
        mode: RootFilesystemMode::Ephemeral,
        disable_default_base_layer: true,
        lowers: vec![RootFilesystemSnapshot {
            entries: vec![
                FilesystemEntry::directory("/dest"),
                FilesystemEntry::file("/dest/keep.txt", b"keep".to_vec()),
                FilesystemEntry::file("/src.bin", b"src".to_vec()),
            ],
        }],
        bootstrap_entries: Vec::new(),
    })
    .expect("build root filesystem");
    let mut kernel = KernelVm::new(MountTable::new(root), config);

    kernel
        .rename("/src.bin", "/dest/src.bin")
        .expect("lower-only destination parent should be materialized first");
    assert_eq!(
        kernel
            .read_file("/dest/src.bin")
            .expect("renamed file should be readable"),
        b"src".to_vec()
    );
    assert_eq!(
        kernel
            .read_file("/dest/keep.txt")
            .expect("lower sibling should remain visible"),
        b"keep".to_vec()
    );
    assert!(!kernel.exists("/src.bin").expect("source should be hidden"));
}

#[test]
fn filesystem_limits_allow_overlay_rename_through_lower_symlink_destination_parent() {
    let mut config = KernelVmConfig::new("vm-overlay-rename-symlink-destination-parent");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_inode_count: Some(5),
        ..ResourceLimits::default()
    };

    let root = RootFileSystem::from_descriptor(RootFilesystemDescriptor {
        mode: RootFilesystemMode::Ephemeral,
        disable_default_base_layer: true,
        lowers: vec![RootFilesystemSnapshot {
            entries: vec![
                FilesystemEntry::directory("/real"),
                FilesystemEntry::symlink("/link", "/real"),
                FilesystemEntry::file("/src.bin", b"src".to_vec()),
            ],
        }],
        bootstrap_entries: Vec::new(),
    })
    .expect("build root filesystem");
    let mut kernel = KernelVm::new(MountTable::new(root), config);

    kernel
        .rename("/src.bin", "/link/src.bin")
        .expect("symlink destination parent should resolve to materialized target");
    assert_eq!(
        kernel
            .read_file("/real/src.bin")
            .expect("renamed file should be readable through target"),
        b"src".to_vec()
    );
    assert!(!kernel.exists("/src.bin").expect("source should be hidden"));
}

#[test]
fn filesystem_limits_allow_overlay_rename_through_lower_symlink_ancestor() {
    let mut config = KernelVmConfig::new("vm-overlay-rename-symlink-destination-ancestor");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_inode_count: Some(5),
        ..ResourceLimits::default()
    };

    let root = RootFileSystem::from_descriptor(RootFilesystemDescriptor {
        mode: RootFilesystemMode::Ephemeral,
        disable_default_base_layer: true,
        lowers: vec![RootFilesystemSnapshot {
            entries: vec![
                FilesystemEntry::directory("/real"),
                FilesystemEntry::directory("/real/subdir"),
                FilesystemEntry::symlink("/link", "/real"),
                FilesystemEntry::file("/src.bin", b"src".to_vec()),
            ],
        }],
        bootstrap_entries: Vec::new(),
    })
    .expect("build root filesystem");
    let mut kernel = KernelVm::new(MountTable::new(root), config);

    kernel
        .rename("/src.bin", "/link/subdir/src.bin")
        .expect("symlink ancestor should resolve to materialized target");
    assert_eq!(
        kernel
            .read_file("/real/subdir/src.bin")
            .expect("renamed file should be readable through target"),
        b"src".to_vec()
    );
    assert_eq!(
        kernel
            .read_file("/link/subdir/src.bin")
            .expect("renamed file should be readable through symlink"),
        b"src".to_vec()
    );
    assert!(!kernel.exists("/src.bin").expect("source should be hidden"));
}

#[test]
fn filesystem_limits_allow_overlay_rename_through_chained_lower_symlink_destination_parent() {
    let mut config = KernelVmConfig::new("vm-overlay-rename-chained-symlink-destination-parent");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_inode_count: Some(7),
        ..ResourceLimits::default()
    };

    let root = RootFileSystem::from_descriptor(RootFilesystemDescriptor {
        mode: RootFilesystemMode::Ephemeral,
        disable_default_base_layer: true,
        lowers: vec![RootFilesystemSnapshot {
            entries: vec![
                FilesystemEntry::directory("/a"),
                FilesystemEntry::directory("/real"),
                FilesystemEntry::directory("/other"),
                FilesystemEntry::symlink("/a/link", "/real"),
                FilesystemEntry::symlink("/real/subdir", "/other"),
                FilesystemEntry::file("/src.bin", b"src".to_vec()),
            ],
        }],
        bootstrap_entries: Vec::new(),
    })
    .expect("build root filesystem");
    let mut kernel = KernelVm::new(MountTable::new(root), config);

    kernel
        .rename("/src.bin", "/a/link/subdir/src.bin")
        .expect("chained symlink destination parent should resolve to materialized target");
    assert_eq!(
        kernel
            .read_file("/other/src.bin")
            .expect("renamed file should be readable through final target"),
        b"src".to_vec()
    );
    assert_eq!(
        kernel
            .read_file("/a/link/subdir/src.bin")
            .expect("renamed file should be readable through symlink chain"),
        b"src".to_vec()
    );
    assert!(!kernel.exists("/src.bin").expect("source should be hidden"));
}

#[test]
fn filesystem_limits_allow_overlay_rename_through_upper_symlink_to_lower_destination_parent() {
    let mut config = KernelVmConfig::new("vm-overlay-rename-upper-symlink-to-lower-parent");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_inode_count: Some(5),
        ..ResourceLimits::default()
    };

    let root = RootFileSystem::from_descriptor(RootFilesystemDescriptor {
        mode: RootFilesystemMode::Ephemeral,
        disable_default_base_layer: true,
        lowers: vec![RootFilesystemSnapshot {
            entries: vec![
                FilesystemEntry::directory("/real"),
                FilesystemEntry::directory("/real/subdir"),
                FilesystemEntry::file("/src.bin", b"src".to_vec()),
            ],
        }],
        bootstrap_entries: vec![FilesystemEntry::symlink("/link", "/real")],
    })
    .expect("build root filesystem");
    let mut kernel = KernelVm::new(MountTable::new(root), config);

    kernel
        .rename("/src.bin", "/link/subdir/src.bin")
        .expect("upper symlink should resolve to lower destination parent");
    assert_eq!(
        kernel
            .read_file("/real/subdir/src.bin")
            .expect("renamed file should be readable through target"),
        b"src".to_vec()
    );
    assert_eq!(
        kernel
            .read_file("/link/subdir/src.bin")
            .expect("renamed file should be readable through symlink"),
        b"src".to_vec()
    );
    assert!(!kernel.exists("/src.bin").expect("source should be hidden"));
}

#[test]
fn filesystem_limits_reject_overlay_rename_copy_up_against_existing_upper_usage() {
    let mut config = KernelVmConfig::new("vm-overlay-rename-copy-up-existing-usage-limit");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_filesystem_bytes: Some(8),
        ..ResourceLimits::default()
    };

    let root = RootFileSystem::from_descriptor(RootFilesystemDescriptor {
        mode: RootFilesystemMode::Ephemeral,
        disable_default_base_layer: true,
        lowers: vec![RootFilesystemSnapshot {
            entries: vec![
                FilesystemEntry::directory("/lower"),
                FilesystemEntry::file("/lower/small.bin", vec![b'x'; 7]),
            ],
        }],
        bootstrap_entries: vec![FilesystemEntry::file("/existing.bin", vec![b'y'; 7])],
    })
    .expect("build root filesystem");
    let mut kernel = KernelVm::new(MountTable::new(root), config);

    let error = kernel
        .rename("/lower", "/moved")
        .expect_err("copy-up should include current upper usage");
    assert_eq!(error.code(), "ENOSPC");
    assert_eq!(
        kernel
            .read_file("/lower/small.bin")
            .expect("source tree should remain readable"),
        vec![b'x'; 7]
    );
    assert_eq!(
        kernel
            .read_file("/existing.bin")
            .expect("existing upper file should remain readable"),
        vec![b'y'; 7]
    );
    assert!(!kernel.exists("/moved").expect("check destination"));
}

#[test]
fn filesystem_limits_allow_overlay_rename_copy_up_when_replacing_upper_destination_within_limit() {
    let mut config = KernelVmConfig::new("vm-overlay-rename-copy-up-replace-destination");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_filesystem_bytes: Some(13),
        ..ResourceLimits::default()
    };

    let root = RootFileSystem::from_descriptor(RootFilesystemDescriptor {
        mode: RootFilesystemMode::Ephemeral,
        disable_default_base_layer: true,
        lowers: vec![RootFilesystemSnapshot {
            entries: vec![FilesystemEntry::file("/src.bin", vec![b'x'; 7])],
        }],
        bootstrap_entries: vec![FilesystemEntry::file("/dst.bin", vec![b'y'; 7])],
    })
    .expect("build root filesystem");
    let mut kernel = KernelVm::new(MountTable::new(root), config);

    kernel
        .rename("/src.bin", "/dst.bin")
        .expect("destination replacement should subtract removed upper usage");
    assert_eq!(
        kernel
            .read_file("/dst.bin")
            .expect("destination should contain renamed source"),
        vec![b'x'; 7]
    );
    assert!(!kernel.exists("/src.bin").expect("source should be hidden"));
}

#[test]
fn filesystem_limits_reject_overlay_rename_copy_up_when_replaced_destination_hardlink_remains() {
    let mut config = KernelVmConfig::new("vm-overlay-rename-copy-up-hardlink-destination");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_filesystem_bytes: Some(8),
        ..ResourceLimits::default()
    };

    let root = RootFileSystem::from_descriptor(RootFilesystemDescriptor {
        mode: RootFilesystemMode::Ephemeral,
        disable_default_base_layer: true,
        lowers: vec![RootFilesystemSnapshot {
            entries: vec![FilesystemEntry::file("/src.bin", vec![b'x'; 7])],
        }],
        bootstrap_entries: vec![FilesystemEntry::file("/dst.bin", vec![b'y'; 7])],
    })
    .expect("build root filesystem");
    let mut kernel = KernelVm::new(MountTable::new(root), config);
    kernel
        .link("/dst.bin", "/alias.bin")
        .expect("create destination hardlink");

    let error = kernel
        .rename("/src.bin", "/dst.bin")
        .expect_err("destination alias should keep old inode usage live");
    assert_eq!(error.code(), "ENOSPC");
    assert_eq!(
        kernel
            .read_file("/dst.bin")
            .expect("destination should remain unchanged"),
        vec![b'y'; 7]
    );
    assert_eq!(
        kernel
            .read_file("/alias.bin")
            .expect("alias should remain readable"),
        vec![b'y'; 7]
    );
}

#[test]
fn filesystem_limits_reject_overlay_rename_copy_up_against_inode_limit() {
    let mut config = KernelVmConfig::new("vm-overlay-rename-copy-up-inode-limit");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_inode_count: Some(2),
        ..ResourceLimits::default()
    };

    let root = RootFileSystem::from_descriptor(RootFilesystemDescriptor {
        mode: RootFilesystemMode::Ephemeral,
        disable_default_base_layer: true,
        lowers: vec![RootFilesystemSnapshot {
            entries: vec![
                FilesystemEntry::directory("/lower"),
                FilesystemEntry::directory("/lower/child"),
            ],
        }],
        bootstrap_entries: Vec::new(),
    })
    .expect("build root filesystem");
    let mut kernel = KernelVm::new(MountTable::new(root), config);

    let error = kernel
        .rename("/lower", "/moved")
        .expect_err("copy-up should include current upper inode usage");
    assert_eq!(error.code(), "ENOSPC");
    assert!(kernel.exists("/lower/child").expect("source child remains"));
    assert!(!kernel.exists("/moved").expect("check destination"));
}

#[test]
fn filesystem_limits_allow_upper_only_overlay_directory_rename_at_inode_limit() {
    let mut config = KernelVmConfig::new("vm-overlay-upper-only-rename-at-inode-limit");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_inode_count: Some(3),
        ..ResourceLimits::default()
    };

    let root = RootFileSystem::from_descriptor(RootFilesystemDescriptor {
        mode: RootFilesystemMode::Ephemeral,
        disable_default_base_layer: true,
        lowers: Vec::new(),
        bootstrap_entries: vec![
            FilesystemEntry::directory("/dir"),
            FilesystemEntry::file("/dir/file.txt", b"upper".to_vec()),
        ],
    })
    .expect("build root filesystem");
    let mut kernel = KernelVm::new(MountTable::new(root), config);

    kernel
        .rename("/dir", "/renamed")
        .expect("upper-only rename should not allocate inodes");
    assert_eq!(
        kernel
            .read_file("/renamed/file.txt")
            .expect("renamed file should remain readable"),
        b"upper".to_vec()
    );
    assert!(!kernel.exists("/dir").expect("old directory should be gone"));
}

#[test]
fn filesystem_limits_do_not_double_count_upper_hardlinks_during_overlay_rename_preflight() {
    let mut config = KernelVmConfig::new("vm-overlay-rename-hardlink-accounting");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_filesystem_bytes: Some(8),
        ..ResourceLimits::default()
    };

    let root = RootFileSystem::from_descriptor(RootFilesystemDescriptor {
        mode: RootFilesystemMode::Ephemeral,
        disable_default_base_layer: true,
        lowers: Vec::new(),
        bootstrap_entries: vec![FilesystemEntry::file("/existing.bin", vec![b'x'; 7])],
    })
    .expect("build root filesystem");
    let mut kernel = KernelVm::new(MountTable::new(root), config);
    kernel
        .link("/existing.bin", "/alias.bin")
        .expect("create hardlink");

    kernel
        .rename("/existing.bin", "/renamed.bin")
        .expect("hardlinked upper inode should be counted once");
    assert_eq!(
        kernel
            .read_file("/renamed.bin")
            .expect("renamed hardlink source should remain readable"),
        vec![b'x'; 7]
    );
    assert_eq!(
        kernel
            .read_file("/alias.bin")
            .expect("alias should remain readable"),
        vec![b'x'; 7]
    );
}

#[test]
fn filesystem_limits_preserve_not_directory_errors_for_upper_files() {
    let mut config = KernelVmConfig::new("vm-overlay-read-dir-upper-file");
    config.permissions = Permissions::allow_all();

    let root = RootFileSystem::from_descriptor(RootFilesystemDescriptor {
        mode: RootFilesystemMode::Ephemeral,
        disable_default_base_layer: true,
        lowers: Vec::new(),
        bootstrap_entries: vec![FilesystemEntry::file("/file.txt", b"upper".to_vec())],
    })
    .expect("build root filesystem");
    let mut kernel = KernelVm::new(MountTable::new(root), config);

    let error = kernel
        .read_dir("/file.txt")
        .expect_err("upper file should not read as an empty directory");
    assert_eq!(error.code(), "ENOTDIR");
}

#[test]
fn filesystem_limits_reject_overlay_rename_copy_up_in_nested_root_mount() {
    let mut config = KernelVmConfig::new("vm-overlay-rename-copy-up-nested-mount-limit");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_filesystem_bytes: Some(8),
        ..ResourceLimits::default()
    };

    let mounted_root = RootFileSystem::from_descriptor(RootFilesystemDescriptor {
        mode: RootFilesystemMode::Ephemeral,
        disable_default_base_layer: true,
        lowers: vec![RootFilesystemSnapshot {
            entries: vec![
                FilesystemEntry::directory("/lower"),
                FilesystemEntry::file("/lower/big.bin", vec![b'x'; 32]),
            ],
        }],
        bootstrap_entries: Vec::new(),
    })
    .expect("build mounted root filesystem");
    let mut kernel = KernelVm::new(MountTable::new(MemoryFileSystem::new()), config);
    kernel
        .mount_filesystem("/mnt", mounted_root, MountOptions::new("root"))
        .expect("mount root filesystem");

    let error = kernel
        .rename("/mnt/lower", "/mnt/moved")
        .expect_err("nested mount copy-up should exceed byte limit");
    assert_eq!(error.code(), "ENOSPC");
    assert_eq!(
        kernel
            .read_file("/mnt/lower/big.bin")
            .expect("source tree should remain readable"),
        vec![b'x'; 32]
    );
    assert!(!kernel.exists("/mnt/moved").expect("check destination"));
}

#[test]
fn blocking_pipe_and_pty_reads_time_out_instead_of_hanging_forever() {
    let captured: Arc<Mutex<Vec<LimitWarning>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&captured);
    set_limit_warning_handler(Box::new(move |warning| {
        if warning.name == TrackedLimit::VmBlockingReadMs {
            sink.lock()
                .expect("blocking-read warning sink")
                .push(warning.clone());
        }
    }));

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
                ..Default::default()
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

    let warnings = captured.lock().expect("blocking-read warnings");
    assert_eq!(warnings.len(), 2, "pipe and PTY must each warn once");
    for warning in warnings.iter() {
        assert_eq!(warning.capacity, 25);
        assert!(
            warning.observed >= 20 && warning.observed < 25,
            "warning must precede the hard deadline: {warning:?}"
        );
        assert!(warning.fill_percent >= 80 && warning.fill_percent < 100);
    }

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

    let exact_argv = kernel
        .spawn_process(
            "sh",
            vec![String::from("123456789")],
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                ..SpawnOptions::default()
            },
        )
        .expect("exact argv byte boundary should be admitted");
    exact_argv.finish(0);
    kernel
        .wait_and_reap(exact_argv.pid())
        .expect("reap exact argv process");

    let exact_env = kernel
        .spawn_process(
            "sh",
            Vec::new(),
            SpawnOptions {
                requester_driver: Some(String::from("shell")),
                env: BTreeMap::from([(String::from("LONG"), String::from("123456789"))]),
                ..SpawnOptions::default()
            },
        )
        .expect("exact environment byte boundary should be admitted");
    exact_env.finish(0);
    kernel
        .wait_and_reap(exact_env.pid())
        .expect("reap exact environment process");

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
    assert!(argv_error
        .to_string()
        .contains("limits.resources.maxProcessArgvBytes"));
    assert_eq!(
        kernel.resource_snapshot().running_processes,
        0,
        "rejected argv must not create a process"
    );

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
    assert!(env_error
        .to_string()
        .contains("limits.resources.maxProcessEnvBytes"));
    assert_eq!(
        kernel.resource_snapshot().running_processes,
        0,
        "rejected environment must not create a process"
    );
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
        .fd_open("shell", process.pid(), "/tmp/data.txt", O_RDWR, None)
        .expect("open file");

    assert_eq!(
        kernel
            .fd_pread("shell", process.pid(), fd, 4, 0)
            .expect("exact pread byte boundary"),
        b"hell"
    );
    assert_eq!(
        kernel
            .fd_pwrite("shell", process.pid(), fd, b"xyz", 0)
            .expect("exact write byte boundary"),
        3
    );
    kernel
        .write_file("/tmp/data.txt", b"hello".to_vec())
        .expect("restore rollback sentinel");

    let pread_error = kernel
        .fd_pread("shell", process.pid(), fd, 5, 0)
        .expect_err("oversized pread should be rejected");
    assert_eq!(pread_error.code(), "EINVAL");
    assert!(pread_error
        .to_string()
        .contains("limits.resources.maxPreadBytes"));

    let write_error = kernel
        .fd_write("shell", process.pid(), fd, b"four")
        .expect_err("oversized fd_write should be rejected");
    assert_eq!(write_error.code(), "EINVAL");
    assert!(write_error
        .to_string()
        .contains("limits.resources.maxFdWriteBytes"));

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
fn fd_write_rejects_unaddressable_sparse_offsets_without_mutating_file() {
    let mut config = KernelVmConfig::new("vm-fd-write-huge-offset");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_filesystem_bytes: None,
        max_fd_write_bytes: Some(8),
        ..ResourceLimits::default()
    };

    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");
    kernel
        .write_file("/tmp/data.txt", b"safe".to_vec())
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
        .fd_open("shell", process.pid(), "/tmp/data.txt", O_RDWR, None)
        .expect("open file");
    kernel
        .fd_seek("shell", process.pid(), fd, i64::MAX, SEEK_SET)
        .expect("seek to unaddressable offset");

    let error = kernel
        .fd_write("shell", process.pid(), fd, b"x")
        .expect_err("huge sparse fd_write should be rejected");
    assert_eq!(error.code(), "ENOMEM");
    assert_eq!(
        kernel
            .read_file("/tmp/data.txt")
            .expect("file should remain unchanged"),
        b"safe".to_vec()
    );
}

#[test]
fn snapshot_root_filesystem_rejects_current_usage_over_configured_limit() {
    let mut root = RootFileSystem::from_descriptor(RootFilesystemDescriptor {
        mode: RootFilesystemMode::Ephemeral,
        disable_default_base_layer: true,
        lowers: vec![RootFilesystemSnapshot {
            entries: vec![
                FilesystemEntry::directory("/workspace"),
                FilesystemEntry::file("/workspace/data.txt", b"large".to_vec()),
            ],
        }],
        bootstrap_entries: Vec::new(),
    })
    .expect("create root filesystem");
    root.write_file("/workspace/extra.txt", b"extra".to_vec())
        .expect("write extra data before applying kernel limit");

    let mut config = KernelVmConfig::new("vm-snapshot-limit");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_filesystem_bytes: Some(4),
        ..ResourceLimits::default()
    };
    let mut kernel = KernelVm::new(MountTable::new(root), config);

    let error = kernel
        .snapshot_root_filesystem()
        .expect_err("snapshot should be rejected before cloning root contents");
    assert_eq!(error.code(), "ENOSPC");
}

#[test]
fn bounded_root_export_rejects_before_materializing_content_over_max_bytes() {
    let root = RootFileSystem::from_descriptor(RootFilesystemDescriptor {
        mode: RootFilesystemMode::Ephemeral,
        disable_default_base_layer: true,
        lowers: vec![RootFilesystemSnapshot {
            entries: vec![
                FilesystemEntry::directory("/workspace"),
                FilesystemEntry::file("/workspace/data.txt", b"too large".to_vec()),
            ],
        }],
        bootstrap_entries: Vec::new(),
    })
    .expect("create root filesystem");
    let mut config = KernelVmConfig::new("vm-explicit-export-limit");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MountTable::new(root), config);

    let error = kernel
        .snapshot_root_filesystem_bounded(4)
        .expect_err("caller export bound should reject oversized content");
    assert_eq!(error.code(), "EFBIG");
    assert!(error.to_string().contains("maxBytes"));
    assert!(error.to_string().contains("raise maxBytes"));
}

#[test]
fn resource_limits_reject_oversized_direct_pread_before_device_allocation() {
    let mut config = KernelVmConfig::new("vm-direct-pread-limit");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_pread_bytes: Some(4),
        ..ResourceLimits::default()
    };

    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);

    let error = kernel
        .pread_file("/dev/zero", 0, 5)
        .expect_err("oversized direct pread should be rejected");
    assert_eq!(error.code(), "EINVAL");
    assert!(
        error.to_string().contains("pread length 5"),
        "unexpected error: {error}"
    );

    assert_eq!(
        kernel
            .pread_file("/dev/zero", 0, 4)
            .expect("bounded direct pread should succeed"),
        vec![0; 4]
    );
}

#[test]
fn resource_limits_reject_oversized_fd_read_before_device_allocation() {
    let mut config = KernelVmConfig::new("vm-fd-read-device-limit");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_pread_bytes: Some(4),
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
    let fd = kernel
        .fd_open("shell", process.pid(), "/dev/zero", 0, None)
        .expect("open device");

    let error = kernel
        .fd_read("shell", process.pid(), fd, 5)
        .expect_err("oversized fd read should be rejected");
    assert_eq!(error.code(), "EINVAL");
    assert!(
        error.to_string().contains("pread length 5"),
        "unexpected error: {error}"
    );

    assert_eq!(
        kernel
            .fd_read("shell", process.pid(), fd, 4)
            .expect("bounded fd read should succeed"),
        vec![0; 4]
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
    kernel.create_dir("/exact").expect("create exact directory");
    kernel
        .write_file("/exact/a.txt", b"a".to_vec())
        .expect("write exact first entry");
    kernel
        .write_file("/exact/b.txt", b"b".to_vec())
        .expect("write exact second entry");
    assert_eq!(
        kernel
            .read_dir("/exact")
            .expect("exact readdir entry boundary")
            .len(),
        2
    );
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
    assert!(error
        .to_string()
        .contains("limits.resources.maxReaddirEntries"));
}
