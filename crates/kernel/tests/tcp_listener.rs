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
fn tcp_listener_bind_listen_and_accept_preserve_listener_state() {
    let mut kernel = new_kernel("vm-tcp-listener-accept");
    let process = spawn_shell(&mut kernel);
    let listener = kernel
        .socket_create("shell", process.pid(), SocketSpec::tcp())
        .expect("create listener socket");

    kernel
        .socket_bind_inet(
            "shell",
            process.pid(),
            listener,
            InetSocketAddress::new("127.0.0.1", 43111),
        )
        .expect("bind listener");
    kernel
        .socket_listen("shell", process.pid(), listener, 2)
        .expect("listen");
    kernel
        .socket_queue_incoming_tcp_connection(
            "shell",
            process.pid(),
            listener,
            InetSocketAddress::new("127.0.0.1", 54001),
        )
        .expect("queue first connection");
    kernel
        .socket_queue_incoming_tcp_connection(
            "shell",
            process.pid(),
            listener,
            InetSocketAddress::new("127.0.0.1", 54002),
        )
        .expect("queue second connection");

    let listener_record = kernel.socket_get(listener).expect("listener record");
    assert_eq!(listener_record.state(), SocketState::Listening);
    assert_eq!(listener_record.listen_backlog(), Some(2));
    assert_eq!(listener_record.pending_accept_count(), 2);
    assert_eq!(
        listener_record.local_address(),
        Some(&InetSocketAddress::new("127.0.0.1", 43111))
    );

    let first_accepted = kernel
        .socket_accept("shell", process.pid(), listener)
        .expect("accept first connection");
    let first_record = kernel.socket_get(first_accepted).expect("first accepted");
    assert_eq!(first_record.state(), SocketState::Connected);
    assert_eq!(
        first_record.local_address(),
        Some(&InetSocketAddress::new("127.0.0.1", 43111))
    );
    assert_eq!(
        first_record.peer_address(),
        Some(&InetSocketAddress::new("127.0.0.1", 54001))
    );

    let listener_after_first_accept = kernel.socket_get(listener).expect("listener after accept");
    assert_eq!(listener_after_first_accept.state(), SocketState::Listening);
    assert_eq!(listener_after_first_accept.pending_accept_count(), 1);

    let second_accepted = kernel
        .socket_accept("shell", process.pid(), listener)
        .expect("accept second connection");
    let second_record = kernel.socket_get(second_accepted).expect("second accepted");
    assert_eq!(second_record.state(), SocketState::Connected);
    assert_eq!(
        second_record.peer_address(),
        Some(&InetSocketAddress::new("127.0.0.1", 54002))
    );

    let snapshot = kernel.resource_snapshot();
    assert_eq!(snapshot.socket_listeners, 1);
    assert_eq!(snapshot.socket_connections, 2);

    let accept_error = kernel
        .socket_accept("shell", process.pid(), listener)
        .expect_err("empty listener should not accept");
    assert_eq!(accept_error.code(), "EAGAIN");
}

#[test]
fn tcp_listener_requires_bind_and_enforces_backlog_capacity() {
    let mut kernel = new_kernel("vm-tcp-listener-backlog");
    let process = spawn_shell(&mut kernel);
    let listener = kernel
        .socket_create("shell", process.pid(), SocketSpec::tcp())
        .expect("create listener socket");

    let listen_error = kernel
        .socket_listen("shell", process.pid(), listener, 1)
        .expect_err("listen should require bind");
    assert_eq!(listen_error.code(), "EINVAL");

    kernel
        .socket_bind_inet(
            "shell",
            process.pid(),
            listener,
            InetSocketAddress::new("127.0.0.1", 43112),
        )
        .expect("bind listener");
    kernel
        .socket_listen("shell", process.pid(), listener, 1)
        .expect("listen");
    kernel
        .socket_queue_incoming_tcp_connection(
            "shell",
            process.pid(),
            listener,
            InetSocketAddress::new("127.0.0.1", 55001),
        )
        .expect("queue first connection");

    let backlog_error = kernel
        .socket_queue_incoming_tcp_connection(
            "shell",
            process.pid(),
            listener,
            InetSocketAddress::new("127.0.0.1", 55002),
        )
        .expect_err("second queued connection should exceed backlog");
    assert_eq!(backlog_error.code(), "EAGAIN");
}
