use agent_os_kernel::command_registry::CommandDriver;
use agent_os_kernel::kernel::{KernelProcessHandle, KernelVm, KernelVmConfig, SpawnOptions};
use agent_os_kernel::permissions::Permissions;
use agent_os_kernel::socket_table::{InetSocketAddress, SocketShutdown, SocketSpec, SocketState};
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
fn tcp_connected_sockets_transfer_data_bidirectionally() {
    let mut kernel = new_kernel("vm-tcp-data-plane");
    let client = spawn_shell(&mut kernel);
    let server = spawn_shell(&mut kernel);

    let client_socket = kernel
        .socket_create("shell", client.pid(), SocketSpec::tcp())
        .expect("create client socket");
    kernel
        .socket_bind_inet(
            "shell",
            client.pid(),
            client_socket,
            InetSocketAddress::new("127.0.0.1", 54011),
        )
        .expect("bind client socket");

    let server_socket = kernel
        .socket_create("shell", server.pid(), SocketSpec::tcp())
        .expect("create server socket");
    kernel
        .socket_bind_inet(
            "shell",
            server.pid(),
            server_socket,
            InetSocketAddress::new("127.0.0.1", 43121),
        )
        .expect("bind server socket");

    kernel
        .socket_connect_pair("shell", client.pid(), client_socket, server_socket)
        .expect("connect pair");

    let client_record = kernel.socket_get(client_socket).expect("client record");
    assert_eq!(client_record.state(), SocketState::Connected);
    assert_eq!(client_record.peer_socket_id(), Some(server_socket));
    assert_eq!(
        client_record.peer_address(),
        Some(&InetSocketAddress::new("127.0.0.1", 43121))
    );

    let server_record = kernel.socket_get(server_socket).expect("server record");
    assert_eq!(server_record.state(), SocketState::Connected);
    assert_eq!(server_record.peer_socket_id(), Some(client_socket));
    assert_eq!(
        server_record.peer_address(),
        Some(&InetSocketAddress::new("127.0.0.1", 54011))
    );

    let written = kernel
        .socket_write("shell", client.pid(), client_socket, b"hello server")
        .expect("write client payload");
    assert_eq!(written, b"hello server".len());
    assert_eq!(
        kernel
            .socket_get(server_socket)
            .expect("server socket after write")
            .buffered_read_bytes(),
        b"hello server".len()
    );

    let first_read = kernel
        .socket_read("shell", server.pid(), server_socket, 5)
        .expect("read first chunk")
        .expect("first chunk should be available");
    assert_eq!(first_read, b"hello");

    let second_read = kernel
        .socket_read("shell", server.pid(), server_socket, 64)
        .expect("read remaining bytes")
        .expect("remaining bytes should be available");
    assert_eq!(second_read, b" server");

    kernel
        .socket_write("shell", server.pid(), server_socket, b"ack")
        .expect("write server reply");
    let reply = kernel
        .socket_read("shell", client.pid(), client_socket, 16)
        .expect("read server reply")
        .expect("reply should be available");
    assert_eq!(reply, b"ack");

    let snapshot = kernel.resource_snapshot();
    assert_eq!(snapshot.socket_connections, 2);
}

#[test]
fn tcp_shutdown_and_close_propagate_eof_and_broken_pipe() {
    let mut kernel = new_kernel("vm-tcp-shutdown-close");
    let client = spawn_shell(&mut kernel);
    let server = spawn_shell(&mut kernel);

    let client_socket = kernel
        .socket_create("shell", client.pid(), SocketSpec::tcp())
        .expect("create client socket");
    let server_socket = kernel
        .socket_create("shell", server.pid(), SocketSpec::tcp())
        .expect("create server socket");
    kernel
        .socket_connect_pair("shell", client.pid(), client_socket, server_socket)
        .expect("connect pair");

    kernel
        .socket_shutdown("shell", server.pid(), server_socket, SocketShutdown::Write)
        .expect("shutdown server write side");
    assert_eq!(
        kernel
            .socket_read("shell", client.pid(), client_socket, 32)
            .expect("read after peer write shutdown"),
        None
    );
    assert!(kernel
        .socket_get(client_socket)
        .expect("client after peer write shutdown")
        .peer_write_shutdown());

    kernel
        .socket_write("shell", client.pid(), client_socket, b"still-open")
        .expect("client write after peer write shutdown");
    let payload = kernel
        .socket_read("shell", server.pid(), server_socket, 64)
        .expect("server read after peer write shutdown")
        .expect("server should still read");
    assert_eq!(payload, b"still-open");

    kernel
        .socket_shutdown("shell", server.pid(), server_socket, SocketShutdown::Read)
        .expect("shutdown server read side");
    let write_error = kernel
        .socket_write("shell", client.pid(), client_socket, b"no-reader")
        .expect_err("peer read shutdown should reject writes");
    assert_eq!(write_error.code(), "EPIPE");

    kernel
        .socket_close("shell", client.pid(), client_socket)
        .expect("close client socket");
    assert_eq!(
        kernel
            .socket_read("shell", server.pid(), server_socket, 32)
            .expect("read after peer close"),
        None
    );
    let closed_write_error = kernel
        .socket_write("shell", server.pid(), server_socket, b"peer-gone")
        .expect_err("peer close should reject writes");
    assert_eq!(closed_write_error.code(), "EPIPE");

    kernel
        .socket_close("shell", server.pid(), server_socket)
        .expect("close server socket");
    let snapshot = kernel.resource_snapshot();
    assert_eq!(snapshot.sockets, 0);
    assert_eq!(snapshot.socket_connections, 0);
}
