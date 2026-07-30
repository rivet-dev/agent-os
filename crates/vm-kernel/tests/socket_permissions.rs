use agentos_vm_kernel::kernel::{KernelVm, KernelVmConfig, VirtualProcessOptions};
use agentos_vm_kernel::permissions::{
    NetworkAccessRequest, NetworkOperation, PermissionDecision, Permissions,
};
use agentos_vm_kernel::socket_table::{InetSocketAddress, SocketSpec, SocketState};
use agentos_vm_kernel::vfs::MemoryFileSystem;
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

fn spawn_shell(kernel: &mut KernelVm<MemoryFileSystem>) -> u32 {
    kernel
        .create_virtual_process(
            "shell",
            "shell",
            "sh",
            Vec::new(),
            VirtualProcessOptions::default(),
        )
        .expect("spawn shell")
        .pid()
}

fn kernel_with_network_permissions(
    callback: impl Fn(&NetworkAccessRequest) -> PermissionDecision + Send + Sync + 'static,
) -> KernelVm<MemoryFileSystem> {
    let mut config = KernelVmConfig::new("vm-socket-permissions");
    config.permissions = Permissions {
        network: Some(Arc::new(callback)),
        ..Permissions::allow_all()
    };
    KernelVm::new(MemoryFileSystem::new(), config)
}

fn kernel_with_loopback_exempt_ports(
    ports: impl IntoIterator<Item = u16>,
) -> KernelVm<MemoryFileSystem> {
    let mut config = KernelVmConfig::new("vm-socket-loopback");
    config.permissions = Permissions::allow_all();
    config.loopback_exempt_ports = ports.into_iter().collect::<BTreeSet<_>>();
    KernelVm::new(MemoryFileSystem::new(), config)
}

#[test]
fn socket_bind_inet_checks_network_listen_permission_before_mutation() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let requests_for_callback = Arc::clone(&requests);
    let mut kernel = kernel_with_network_permissions(move |request| {
        requests_for_callback
            .lock()
            .expect("request log lock")
            .push(request.clone());
        PermissionDecision::deny("network disabled")
    });
    let pid = spawn_shell(&mut kernel);
    let socket = kernel
        .socket_create("shell", pid, SocketSpec::tcp())
        .expect("create socket");

    let error = kernel
        .socket_bind_inet(
            "shell",
            pid,
            socket,
            InetSocketAddress::new("127.0.0.1", 8080),
        )
        .expect_err("bind should be denied by network policy");

    assert_eq!(error.code(), "EACCES");
    assert_eq!(
        kernel.socket_get(socket).expect("socket exists").state(),
        SocketState::Created
    );
    assert_eq!(
        *requests.lock().expect("request log lock"),
        vec![NetworkAccessRequest {
            vm_id: String::from("vm-socket-permissions"),
            op: NetworkOperation::Listen,
            resource: String::from("tcp://127.0.0.1:8080"),
        }]
    );
}

#[test]
fn socket_connect_inet_loopback_checks_network_http_permission_before_mutation() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let requests_for_callback = Arc::clone(&requests);
    let mut kernel = kernel_with_network_permissions(move |request| {
        requests_for_callback
            .lock()
            .expect("request log lock")
            .push(request.clone());
        match request.op {
            NetworkOperation::Listen => PermissionDecision::allow(),
            NetworkOperation::Http => PermissionDecision::deny("connect disabled"),
            NetworkOperation::Fetch | NetworkOperation::Dns => PermissionDecision::allow(),
        }
    });

    let server_pid = spawn_shell(&mut kernel);
    let listener = kernel
        .socket_create("shell", server_pid, SocketSpec::tcp())
        .expect("create listener socket");
    let target = InetSocketAddress::new("127.0.0.1", 9090);
    kernel
        .socket_bind_inet("shell", server_pid, listener, target.clone())
        .expect("bind listener");
    kernel
        .socket_listen("shell", server_pid, listener, 1)
        .expect("listen");

    let client_pid = spawn_shell(&mut kernel);
    let client = kernel
        .socket_create("shell", client_pid, SocketSpec::tcp())
        .expect("create client socket");
    let error = kernel
        .socket_connect_inet_loopback("shell", client_pid, client, target)
        .expect_err("connect should be denied by network policy");

    assert_eq!(error.code(), "EACCES");
    assert_eq!(
        kernel
            .socket_get(client)
            .expect("client socket exists")
            .state(),
        SocketState::Created
    );
    assert!(requests
        .lock()
        .expect("request log lock")
        .iter()
        .any(|request| request.op == NetworkOperation::Http
            && request.resource == "tcp://127.0.0.1:9090"));
    assert_eq!(kernel.resource_snapshot().socket_connections, 0);
}

#[test]
fn socket_send_to_inet_loopback_checks_network_http_permission_before_mutation() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let requests_for_callback = Arc::clone(&requests);
    let mut kernel = kernel_with_network_permissions(move |request| {
        requests_for_callback
            .lock()
            .expect("request log lock")
            .push(request.clone());
        match request.op {
            NetworkOperation::Listen => PermissionDecision::allow(),
            NetworkOperation::Http => PermissionDecision::deny("udp send disabled"),
            NetworkOperation::Fetch | NetworkOperation::Dns => PermissionDecision::allow(),
        }
    });

    let receiver_pid = spawn_shell(&mut kernel);
    let receiver = kernel
        .socket_create("shell", receiver_pid, SocketSpec::udp())
        .expect("create UDP receiver");
    let target = InetSocketAddress::new("127.0.0.1", 9091);
    kernel
        .socket_bind_inet("shell", receiver_pid, receiver, target.clone())
        .expect("bind UDP receiver");

    let sender_pid = spawn_shell(&mut kernel);
    let sender = kernel
        .socket_create("shell", sender_pid, SocketSpec::udp())
        .expect("create UDP sender");
    kernel
        .socket_bind_inet(
            "shell",
            sender_pid,
            sender,
            InetSocketAddress::new("127.0.0.1", 0),
        )
        .expect("bind UDP sender");

    let error = kernel
        .socket_send_to_inet_loopback("shell", sender_pid, sender, target, b"blocked")
        .expect_err("UDP send should be denied by network policy");

    assert_eq!(error.code(), "EACCES");
    assert!(requests
        .lock()
        .expect("request log lock")
        .iter()
        .any(|request| request.op == NetworkOperation::Http
            && request.resource == "tcp://127.0.0.1:9091"));
    let empty_queue = kernel
        .socket_recv_datagram("shell", receiver_pid, receiver, 64)
        .expect_err("denied UDP send must not enqueue a datagram");
    assert_eq!(empty_queue.code(), "EAGAIN");
}

#[test]
fn socket_connect_inet_loopback_requires_owned_or_exempt_port() {
    let mut kernel = kernel_with_loopback_exempt_ports([]);
    let pid = spawn_shell(&mut kernel);
    let client = kernel
        .socket_create("shell", pid, SocketSpec::tcp())
        .expect("create client socket");
    let error = kernel
        .socket_connect_inet_loopback(
            "shell",
            pid,
            client,
            InetSocketAddress::new("127.0.0.1", 7777),
        )
        .expect_err("unowned non-exempt loopback port should be denied");

    assert_eq!(error.code(), "EPERM");
    assert!(error.to_string().contains("not loopback-exempt"));
    assert_eq!(kernel.resource_snapshot().socket_connections, 0);
}

#[test]
fn socket_connect_inet_loopback_allows_exempt_port_to_reach_connect_path() {
    let mut kernel = kernel_with_loopback_exempt_ports([7777]);
    let pid = spawn_shell(&mut kernel);
    let client = kernel
        .socket_create("shell", pid, SocketSpec::tcp())
        .expect("create client socket");
    let error = kernel
        .socket_connect_inet_loopback(
            "shell",
            pid,
            client,
            InetSocketAddress::new("127.0.0.1", 7777),
        )
        .expect_err("exempt but unbound loopback port should fail as not listening");

    assert_eq!(error.code(), "ECONNREFUSED");
    assert_eq!(kernel.resource_snapshot().socket_connections, 0);
}

#[test]
fn socket_send_to_inet_loopback_requires_owned_or_exempt_port() {
    let mut kernel = kernel_with_loopback_exempt_ports([]);
    let pid = spawn_shell(&mut kernel);
    let socket = kernel
        .socket_create("shell", pid, SocketSpec::udp())
        .expect("create UDP socket");
    kernel
        .socket_bind_inet("shell", pid, socket, InetSocketAddress::new("127.0.0.1", 0))
        .expect("bind UDP socket");

    let error = kernel
        .socket_send_to_inet_loopback(
            "shell",
            pid,
            socket,
            InetSocketAddress::new("127.0.0.1", 7777),
            b"hello",
        )
        .expect_err("unowned non-exempt loopback UDP port should be denied");

    assert_eq!(error.code(), "EPERM");
    assert!(error.to_string().contains("not loopback-exempt"));
}

#[test]
fn socket_send_to_inet_loopback_allows_exempt_port_to_reach_udp_path() {
    let mut kernel = kernel_with_loopback_exempt_ports([7777]);
    let pid = spawn_shell(&mut kernel);
    let socket = kernel
        .socket_create("shell", pid, SocketSpec::udp())
        .expect("create UDP socket");
    kernel
        .socket_bind_inet("shell", pid, socket, InetSocketAddress::new("127.0.0.1", 0))
        .expect("bind UDP socket");

    let error = kernel
        .socket_send_to_inet_loopback(
            "shell",
            pid,
            socket,
            InetSocketAddress::new("127.0.0.1", 7777),
            b"hello",
        )
        .expect_err("exempt but unbound loopback UDP port should fail as not bound");

    assert_eq!(error.code(), "ECONNREFUSED");
}
