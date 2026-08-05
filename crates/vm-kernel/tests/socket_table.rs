use agentos_vm_kernel::command_registry::CommandDriver;
use agentos_vm_kernel::kernel::{KernelProcessHandle, KernelVm, KernelVmConfig, SpawnOptions};
use agentos_vm_kernel::permissions::Permissions;
use agentos_vm_kernel::resource_accounting::ResourceLimits;
use agentos_vm_kernel::socket_table::{InetSocketAddress, SocketSpec, SocketState};
use agentos_vm_kernel::vfs::MemoryFileSystem;

fn spawn_shell(kernel: &mut KernelVm<MemoryFileSystem>) -> KernelProcessHandle {
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

fn new_kernel(vm_id: &str) -> KernelVm<MemoryFileSystem> {
    let mut config = KernelVmConfig::new(vm_id);
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");
    kernel
}

#[test]
fn socket_resources_appear_in_kernel_resource_snapshot_and_cleanup_with_process_exit() {
    let mut config = KernelVmConfig::new("vm-socket-resources");
    config.permissions = Permissions::allow_all();
    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");

    let process = spawn_shell(&mut kernel);
    let listener = kernel
        .socket_create("shell", process.pid(), SocketSpec::tcp())
        .expect("create listener socket");
    kernel
        .socket_set_state("shell", process.pid(), listener, SocketState::Listening)
        .expect("mark listener");

    let connected = kernel
        .socket_create("shell", process.pid(), SocketSpec::tcp())
        .expect("create connected socket");
    kernel
        .socket_set_state("shell", process.pid(), connected, SocketState::Connected)
        .expect("mark connected");

    let snapshot = kernel.resource_snapshot();
    assert_eq!(snapshot.sockets, 2);
    assert_eq!(snapshot.socket_listeners, 1);
    assert_eq!(snapshot.socket_connections, 1);

    process.finish(0);

    let snapshot_after_exit = kernel.resource_snapshot();
    assert_eq!(snapshot_after_exit.sockets, 0);
    assert_eq!(snapshot_after_exit.socket_listeners, 0);
    assert_eq!(snapshot_after_exit.socket_connections, 0);
}

#[test]
fn socket_resource_limits_reject_extra_sockets_and_connections() {
    let mut config = KernelVmConfig::new("vm-socket-limits");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_sockets: Some(2),
        max_connections: Some(1),
        ..ResourceLimits::default()
    };

    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");

    let process = spawn_shell(&mut kernel);
    let listener = kernel
        .socket_create("shell", process.pid(), SocketSpec::tcp())
        .expect("create listener socket");
    kernel
        .socket_set_state("shell", process.pid(), listener, SocketState::Listening)
        .expect("mark listener");

    let first_connection = kernel
        .socket_create("shell", process.pid(), SocketSpec::tcp())
        .expect("create first connection socket");
    kernel
        .socket_set_state(
            "shell",
            process.pid(),
            first_connection,
            SocketState::Connected,
        )
        .expect("mark first connection");

    let socket_error = kernel
        .socket_create("shell", process.pid(), SocketSpec::tcp())
        .expect_err("third socket should exceed max_sockets");
    assert_eq!(socket_error.code(), "EAGAIN");

    kernel
        .socket_close("shell", process.pid(), listener)
        .expect("close listener");
    let second_connection = kernel
        .socket_create("shell", process.pid(), SocketSpec::tcp())
        .expect("create replacement socket");
    let connection_error = kernel
        .socket_set_state(
            "shell",
            process.pid(),
            second_connection,
            SocketState::Connected,
        )
        .expect_err("second connection should exceed max_connections");
    assert_eq!(connection_error.code(), "EAGAIN");
}

#[test]
fn socket_resource_snapshot_counts_stream_bytes_and_udp_queue_pressure() {
    let mut kernel = new_kernel("vm-socket-buffer-snapshot");
    let sender = spawn_shell(&mut kernel);
    let receiver = spawn_shell(&mut kernel);

    let stream_sender = kernel
        .socket_create("shell", sender.pid(), SocketSpec::tcp())
        .expect("create stream sender");
    let stream_receiver = kernel
        .socket_create("shell", receiver.pid(), SocketSpec::tcp())
        .expect("create stream receiver");
    kernel
        .socket_connect_pair("shell", sender.pid(), stream_sender, stream_receiver)
        .expect("connect stream pair");
    kernel
        .socket_write("shell", sender.pid(), stream_sender, b"hello")
        .expect("write stream payload");

    let datagram_sender = kernel
        .socket_create("shell", sender.pid(), SocketSpec::udp())
        .expect("create datagram sender");
    kernel
        .socket_bind_inet(
            "shell",
            sender.pid(),
            datagram_sender,
            InetSocketAddress::new("127.0.0.1", 54071),
        )
        .expect("bind datagram sender");
    let datagram_receiver = kernel
        .socket_create("shell", receiver.pid(), SocketSpec::udp())
        .expect("create datagram receiver");
    kernel
        .socket_bind_inet(
            "shell",
            receiver.pid(),
            datagram_receiver,
            InetSocketAddress::new("127.0.0.1", 43171),
        )
        .expect("bind datagram receiver");
    kernel
        .socket_send_to_inet_loopback(
            "shell",
            sender.pid(),
            datagram_sender,
            InetSocketAddress::new("127.0.0.1", 43171),
            b"abc",
        )
        .expect("send first datagram");
    kernel
        .socket_send_to_inet_loopback(
            "shell",
            sender.pid(),
            datagram_sender,
            InetSocketAddress::new("127.0.0.1", 43171),
            b"defg",
        )
        .expect("send second datagram");

    let snapshot = kernel.resource_snapshot();
    assert_eq!(snapshot.socket_buffered_bytes, 12);
    assert_eq!(snapshot.socket_datagram_queue_len, 2);

    let _ = kernel
        .socket_read("shell", receiver.pid(), stream_receiver, 5)
        .expect("read stream payload");
    let _ = kernel
        .socket_recv_datagram("shell", receiver.pid(), datagram_receiver, 16)
        .expect("receive datagram");

    let snapshot = kernel.resource_snapshot();
    assert_eq!(snapshot.socket_buffered_bytes, 4);
    assert_eq!(snapshot.socket_datagram_queue_len, 1);
}

#[test]
fn socket_resource_limits_reject_buffer_and_datagram_queue_growth() {
    let mut config = KernelVmConfig::new("vm-socket-buffer-limits");
    config.permissions = Permissions::allow_all();
    config.resources = ResourceLimits {
        max_socket_buffered_bytes: Some(5),
        max_socket_datagram_queue_len: Some(1),
        ..ResourceLimits::default()
    };

    let mut kernel = KernelVm::new(MemoryFileSystem::new(), config);
    kernel
        .register_driver(CommandDriver::new("shell", ["sh"]))
        .expect("register shell");
    let sender = spawn_shell(&mut kernel);
    let receiver = spawn_shell(&mut kernel);

    let stream_sender = kernel
        .socket_create("shell", sender.pid(), SocketSpec::tcp())
        .expect("create stream sender");
    let stream_receiver = kernel
        .socket_create("shell", receiver.pid(), SocketSpec::tcp())
        .expect("create stream receiver");
    kernel
        .socket_connect_pair("shell", sender.pid(), stream_sender, stream_receiver)
        .expect("connect stream pair");

    kernel
        .socket_write("shell", sender.pid(), stream_sender, b"12345")
        .expect("write up to stream buffer limit");
    let stream_error = kernel
        .socket_write("shell", sender.pid(), stream_sender, b"6")
        .expect_err("extra stream byte should exceed socket buffer limit");
    assert_eq!(stream_error.code(), "EAGAIN");
    assert_eq!(
        kernel
            .socket_get(stream_receiver)
            .expect("stream receiver")
            .buffered_read_bytes(),
        5
    );
    let _ = kernel
        .socket_read("shell", receiver.pid(), stream_receiver, 5)
        .expect("drain stream buffer");
    kernel
        .socket_write("shell", sender.pid(), stream_sender, b"6")
        .expect("write should succeed after draining stream buffer");
    let _ = kernel
        .socket_read("shell", receiver.pid(), stream_receiver, 1)
        .expect("drain second stream write");

    let datagram_sender = kernel
        .socket_create("shell", sender.pid(), SocketSpec::udp())
        .expect("create datagram sender");
    kernel
        .socket_bind_inet(
            "shell",
            sender.pid(),
            datagram_sender,
            InetSocketAddress::new("127.0.0.1", 54072),
        )
        .expect("bind datagram sender");
    let datagram_receiver = kernel
        .socket_create("shell", receiver.pid(), SocketSpec::udp())
        .expect("create datagram receiver");
    kernel
        .socket_bind_inet(
            "shell",
            receiver.pid(),
            datagram_receiver,
            InetSocketAddress::new("127.0.0.1", 43172),
        )
        .expect("bind datagram receiver");

    kernel
        .socket_send_to_inet_loopback(
            "shell",
            sender.pid(),
            datagram_sender,
            InetSocketAddress::new("127.0.0.1", 43172),
            b"abc",
        )
        .expect("send first datagram");
    let queue_error = kernel
        .socket_send_to_inet_loopback(
            "shell",
            sender.pid(),
            datagram_sender,
            InetSocketAddress::new("127.0.0.1", 43172),
            b"d",
        )
        .expect_err("second datagram should exceed queue length limit");
    assert_eq!(queue_error.code(), "EAGAIN");
    assert_eq!(
        kernel
            .socket_get(datagram_receiver)
            .expect("datagram receiver")
            .queued_datagrams(),
        1
    );
    let _ = kernel
        .socket_recv_datagram("shell", receiver.pid(), datagram_receiver, 16)
        .expect("drain datagram queue");

    let byte_error = kernel
        .socket_send_to_inet_loopback(
            "shell",
            sender.pid(),
            datagram_sender,
            InetSocketAddress::new("127.0.0.1", 43172),
            b"123456",
        )
        .expect_err("oversized datagram should exceed socket buffer byte limit");
    assert_eq!(byte_error.code(), "EAGAIN");
    assert_eq!(
        kernel
            .socket_get(datagram_receiver)
            .expect("datagram receiver after oversized send")
            .queued_datagrams(),
        0
    );
}
