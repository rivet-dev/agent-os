use agent_os_kernel::command_registry::CommandDriver;
use agent_os_kernel::kernel::{KernelProcessHandle, KernelVm, KernelVmConfig, SpawnOptions};
use agent_os_kernel::permissions::Permissions;
use agent_os_kernel::socket_table::{SocketShutdown, SocketSpec, SocketState};
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
fn unix_domain_sockets_bind_connect_accept_and_transfer_data() {
    let mut kernel = new_kernel("vm-unix-domain-flow");
    let server = spawn_shell(&mut kernel);
    let client = spawn_shell(&mut kernel);

    let listener = kernel
        .socket_create("shell", server.pid(), SocketSpec::unix_stream())
        .expect("create unix listener");
    kernel
        .socket_bind_unix("shell", server.pid(), listener, "/tmp/kernel/server.sock")
        .expect("bind unix listener");
    kernel
        .socket_listen("shell", server.pid(), listener, 1)
        .expect("listen on unix listener");

    let client_socket = kernel
        .socket_create("shell", client.pid(), SocketSpec::unix_stream())
        .expect("create unix client");
    kernel
        .socket_bind_unix(
            "shell",
            client.pid(),
            client_socket,
            "/tmp/kernel/client.sock",
        )
        .expect("bind unix client");
    kernel
        .socket_connect_unix(
            "shell",
            client.pid(),
            client_socket,
            "/tmp/kernel/server.sock",
        )
        .expect("connect unix client");

    let listener_record = kernel.socket_get(listener).expect("listener record");
    assert_eq!(listener_record.state(), SocketState::Listening);
    assert_eq!(
        listener_record.local_unix_path(),
        Some("/tmp/kernel/server.sock")
    );
    assert_eq!(listener_record.pending_accept_count(), 1);

    let client_record = kernel.socket_get(client_socket).expect("client record");
    assert_eq!(client_record.state(), SocketState::Connected);
    assert_eq!(
        client_record.local_unix_path(),
        Some("/tmp/kernel/client.sock")
    );
    assert_eq!(
        client_record.peer_unix_path(),
        Some("/tmp/kernel/server.sock")
    );

    let accepted = kernel
        .socket_accept("shell", server.pid(), listener)
        .expect("accept unix client");
    let accepted_record = kernel.socket_get(accepted).expect("accepted record");
    assert_eq!(accepted_record.state(), SocketState::Connected);
    assert_eq!(
        accepted_record.local_unix_path(),
        Some("/tmp/kernel/server.sock")
    );
    assert_eq!(
        accepted_record.peer_unix_path(),
        Some("/tmp/kernel/client.sock")
    );
    assert_eq!(accepted_record.peer_socket_id(), Some(client_socket));

    kernel
        .socket_write("shell", client.pid(), client_socket, b"ping-unix")
        .expect("write unix payload");
    let payload = kernel
        .socket_read("shell", server.pid(), accepted, 64)
        .expect("read unix payload")
        .expect("unix payload");
    assert_eq!(payload, b"ping-unix");

    kernel
        .socket_shutdown("shell", server.pid(), accepted, SocketShutdown::Write)
        .expect("shutdown accepted write side");
    assert_eq!(
        kernel
            .socket_read("shell", client.pid(), client_socket, 16)
            .expect("read unix eof"),
        None
    );

    let snapshot = kernel.resource_snapshot();
    assert_eq!(snapshot.socket_listeners, 1);
    assert_eq!(snapshot.socket_connections, 2);
}

#[test]
fn unix_domain_sockets_require_bound_listener_paths_and_backlog_capacity() {
    let mut kernel = new_kernel("vm-unix-domain-errors");
    let server = spawn_shell(&mut kernel);
    let client = spawn_shell(&mut kernel);

    let listener = kernel
        .socket_create("shell", server.pid(), SocketSpec::unix_stream())
        .expect("create unix listener");
    let listen_error = kernel
        .socket_listen("shell", server.pid(), listener, 1)
        .expect_err("unix listen should require bind");
    assert_eq!(listen_error.code(), "EINVAL");

    let missing_target = kernel
        .socket_create("shell", client.pid(), SocketSpec::unix_stream())
        .expect("create missing-target client");
    let connect_error = kernel
        .socket_connect_unix(
            "shell",
            client.pid(),
            missing_target,
            "/tmp/kernel/missing.sock",
        )
        .expect_err("missing unix listener should fail");
    assert_eq!(connect_error.code(), "ECONNREFUSED");

    kernel
        .socket_bind_unix("shell", server.pid(), listener, "/tmp/kernel/listener.sock")
        .expect("bind unix listener");
    kernel
        .socket_listen("shell", server.pid(), listener, 1)
        .expect("listen on bound unix listener");

    let duplicate_listener = kernel
        .socket_create("shell", server.pid(), SocketSpec::unix_stream())
        .expect("create duplicate listener");
    let duplicate_bind_error = kernel
        .socket_bind_unix(
            "shell",
            server.pid(),
            duplicate_listener,
            "/tmp/kernel/listener.sock",
        )
        .expect_err("duplicate unix path should fail");
    assert_eq!(duplicate_bind_error.code(), "EADDRINUSE");

    let first_client = kernel
        .socket_create("shell", client.pid(), SocketSpec::unix_stream())
        .expect("create first client");
    kernel
        .socket_connect_unix(
            "shell",
            client.pid(),
            first_client,
            "/tmp/kernel/listener.sock",
        )
        .expect("connect first client");

    let second_client = kernel
        .socket_create("shell", client.pid(), SocketSpec::unix_stream())
        .expect("create second client");
    let backlog_error = kernel
        .socket_connect_unix(
            "shell",
            client.pid(),
            second_client,
            "/tmp/kernel/listener.sock",
        )
        .expect_err("second connect should exceed backlog");
    assert_eq!(backlog_error.code(), "EAGAIN");
}
