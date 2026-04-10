use agent_os_kernel::command_registry::CommandDriver;
use agent_os_kernel::kernel::{KernelProcessHandle, KernelVm, KernelVmConfig, SpawnOptions};
use agent_os_kernel::permissions::Permissions;
use agent_os_kernel::socket_table::{InetSocketAddress, SocketSpec, SocketState};
use agent_os_kernel::vfs::MemoryFileSystem;

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
fn kernel_loopback_connect_routes_into_guest_listener_and_accepts_connected_socket() {
    let mut kernel = new_kernel("vm-loopback-tcp");
    let server = spawn_shell(&mut kernel);
    let client = spawn_shell(&mut kernel);

    let listener = kernel
        .socket_create("shell", server.pid(), SocketSpec::tcp())
        .expect("create listener");
    kernel
        .socket_bind_inet(
            "shell",
            server.pid(),
            listener,
            InetSocketAddress::new("127.0.0.1", 43131),
        )
        .expect("bind listener");
    kernel
        .socket_listen("shell", server.pid(), listener, 1)
        .expect("listen");

    let client_socket = kernel
        .socket_create("shell", client.pid(), SocketSpec::tcp())
        .expect("create client socket");
    kernel
        .socket_bind_inet(
            "shell",
            client.pid(),
            client_socket,
            InetSocketAddress::new("127.0.0.1", 54031),
        )
        .expect("bind client");

    kernel
        .socket_connect_inet_loopback(
            "shell",
            client.pid(),
            client_socket,
            InetSocketAddress::new("localhost", 43131),
        )
        .expect("route loopback connect");

    let listener_record = kernel.socket_get(listener).expect("listener record");
    assert_eq!(listener_record.state(), SocketState::Listening);
    assert_eq!(listener_record.pending_accept_count(), 1);

    let client_record = kernel.socket_get(client_socket).expect("client record");
    assert_eq!(client_record.state(), SocketState::Connected);
    assert_eq!(
        client_record.peer_address(),
        Some(&InetSocketAddress::new("127.0.0.1", 43131))
    );

    let accepted = kernel
        .socket_accept("shell", server.pid(), listener)
        .expect("accept loopback connection");
    let accepted_record = kernel.socket_get(accepted).expect("accepted record");
    assert_eq!(accepted_record.state(), SocketState::Connected);
    assert_eq!(accepted_record.peer_socket_id(), Some(client_socket));
    assert_eq!(
        accepted_record.peer_address(),
        Some(&InetSocketAddress::new("127.0.0.1", 54031))
    );

    let client_after_accept = kernel
        .socket_get(client_socket)
        .expect("client after accept");
    assert_eq!(client_after_accept.peer_socket_id(), Some(accepted));

    kernel
        .socket_write("shell", client.pid(), client_socket, b"ping")
        .expect("client write");
    let payload = kernel
        .socket_read("shell", server.pid(), accepted, 16)
        .expect("accepted read")
        .expect("accepted payload");
    assert_eq!(payload, b"ping");

    let snapshot = kernel.resource_snapshot();
    assert_eq!(snapshot.socket_listeners, 1);
    assert_eq!(snapshot.socket_connections, 2);
}

#[test]
fn kernel_loopback_udp_delivery_stays_within_socket_table() {
    let mut kernel = new_kernel("vm-loopback-udp");
    let sender = spawn_shell(&mut kernel);
    let receiver = spawn_shell(&mut kernel);

    let sender_socket = kernel
        .socket_create("shell", sender.pid(), SocketSpec::udp())
        .expect("create sender socket");
    kernel
        .socket_bind_inet(
            "shell",
            sender.pid(),
            sender_socket,
            InetSocketAddress::new("127.0.0.1", 54041),
        )
        .expect("bind sender");

    let receiver_socket = kernel
        .socket_create("shell", receiver.pid(), SocketSpec::udp())
        .expect("create receiver socket");
    kernel
        .socket_bind_inet(
            "shell",
            receiver.pid(),
            receiver_socket,
            InetSocketAddress::new("127.0.0.1", 43141),
        )
        .expect("bind receiver");

    let written = kernel
        .socket_send_to_inet_loopback(
            "shell",
            sender.pid(),
            sender_socket,
            InetSocketAddress::new("localhost", 43141),
            b"ping-udp",
        )
        .expect("send udp payload");
    assert_eq!(written, b"ping-udp".len());
    assert_eq!(
        kernel
            .socket_get(receiver_socket)
            .expect("receiver record")
            .queued_datagrams(),
        1
    );

    let datagram = kernel
        .socket_recv_datagram("shell", receiver.pid(), receiver_socket, 64)
        .expect("receive datagram")
        .expect("datagram payload");
    assert_eq!(
        datagram.source_address(),
        Some(&InetSocketAddress::new("127.0.0.1", 54041))
    );
    assert_eq!(datagram.payload(), b"ping-udp");
    assert_eq!(
        kernel
            .socket_get(receiver_socket)
            .expect("receiver after read")
            .queued_datagrams(),
        0
    );
}
