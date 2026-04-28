use agent_os_kernel::command_registry::CommandDriver;
use agent_os_kernel::kernel::{KernelProcessHandle, KernelVm, KernelVmConfig, SpawnOptions};
use agent_os_kernel::permissions::Permissions;
use agent_os_kernel::socket_table::{
    DatagramSocketOption, InetSocketAddress, SocketMulticastMembership, SocketSpec,
};
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
fn udp_datagrams_preserve_boundaries_and_truncate_per_receive() {
    let mut kernel = new_kernel("vm-udp-boundaries");
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
            InetSocketAddress::new("127.0.0.1", 54051),
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
            InetSocketAddress::new("127.0.0.1", 43151),
        )
        .expect("bind receiver");

    kernel
        .socket_send_to_inet_loopback(
            "shell",
            sender.pid(),
            sender_socket,
            InetSocketAddress::new("127.0.0.1", 43151),
            b"first-datagram",
        )
        .expect("send first datagram");
    kernel
        .socket_send_to_inet_loopback(
            "shell",
            sender.pid(),
            sender_socket,
            InetSocketAddress::new("localhost", 43151),
            b"second",
        )
        .expect("send second datagram");

    assert_eq!(
        kernel
            .socket_get(receiver_socket)
            .expect("receiver after sends")
            .queued_datagrams(),
        2
    );

    let first = kernel
        .socket_recv_datagram("shell", receiver.pid(), receiver_socket, 5)
        .expect("receive first datagram")
        .expect("first payload");
    assert_eq!(
        first.source_address(),
        Some(&InetSocketAddress::new("127.0.0.1", 54051))
    );
    assert_eq!(first.payload(), b"first");

    assert_eq!(
        kernel
            .socket_get(receiver_socket)
            .expect("receiver after first receive")
            .queued_datagrams(),
        1
    );

    let second = kernel
        .socket_recv_datagram("shell", receiver.pid(), receiver_socket, 64)
        .expect("receive second datagram")
        .expect("second payload");
    assert_eq!(second.payload(), b"second");

    let empty_error = kernel
        .socket_recv_datagram("shell", receiver.pid(), receiver_socket, 64)
        .expect_err("empty UDP queue should report would-block");
    assert_eq!(empty_error.code(), "EAGAIN");
}

#[test]
fn udp_send_and_receive_require_bound_sockets_and_bound_targets() {
    let mut kernel = new_kernel("vm-udp-errors");
    let sender = spawn_shell(&mut kernel);
    let receiver = spawn_shell(&mut kernel);

    let sender_socket = kernel
        .socket_create("shell", sender.pid(), SocketSpec::udp())
        .expect("create sender socket");
    let receiver_socket = kernel
        .socket_create("shell", receiver.pid(), SocketSpec::udp())
        .expect("create receiver socket");

    let unbound_send_error = kernel
        .socket_send_to_inet_loopback(
            "shell",
            sender.pid(),
            sender_socket,
            InetSocketAddress::new("127.0.0.1", 43152),
            b"payload",
        )
        .expect_err("unbound sender should fail");
    assert_eq!(unbound_send_error.code(), "EINVAL");

    kernel
        .socket_bind_inet(
            "shell",
            sender.pid(),
            sender_socket,
            InetSocketAddress::new("127.0.0.1", 54052),
        )
        .expect("bind sender");

    let missing_target_error = kernel
        .socket_send_to_inet_loopback(
            "shell",
            sender.pid(),
            sender_socket,
            InetSocketAddress::new("127.0.0.1", 43152),
            b"payload",
        )
        .expect_err("missing receiver should fail");
    assert_eq!(missing_target_error.code(), "ECONNREFUSED");

    let unbound_recv_error = kernel
        .socket_recv_datagram("shell", receiver.pid(), receiver_socket, 64)
        .expect_err("unbound receiver should fail");
    assert_eq!(unbound_recv_error.code(), "EINVAL");
}

#[test]
fn udp_reuseport_allows_two_sockets_to_bind_the_same_port() {
    let mut kernel = new_kernel("vm-udp-reuseport");
    let first = spawn_shell(&mut kernel);
    let second = spawn_shell(&mut kernel);

    let first_socket = kernel
        .socket_create("shell", first.pid(), SocketSpec::udp())
        .expect("create first UDP socket");
    kernel
        .socket_set_datagram_option(
            "shell",
            first.pid(),
            first_socket,
            DatagramSocketOption::ReusePort,
            true,
        )
        .expect("enable REUSEPORT on first socket");

    let second_socket = kernel
        .socket_create("shell", second.pid(), SocketSpec::udp())
        .expect("create second UDP socket");
    kernel
        .socket_set_datagram_option(
            "shell",
            second.pid(),
            second_socket,
            DatagramSocketOption::ReusePort,
            true,
        )
        .expect("enable REUSEPORT on second socket");

    let shared_address = InetSocketAddress::new("127.0.0.1", 43153);
    kernel
        .socket_bind_inet("shell", first.pid(), first_socket, shared_address.clone())
        .expect("bind first socket");
    kernel
        .socket_bind_inet("shell", second.pid(), second_socket, shared_address)
        .expect("bind second socket to the same port");

    assert!(kernel
        .socket_get(first_socket)
        .expect("first socket state")
        .reuse_port());
    assert!(kernel
        .socket_get(second_socket)
        .expect("second socket state")
        .reuse_port());
}

#[test]
fn udp_broadcast_option_is_tracked_in_kernel_socket_state() {
    let mut kernel = new_kernel("vm-udp-broadcast");
    let process = spawn_shell(&mut kernel);
    let socket_id = kernel
        .socket_create("shell", process.pid(), SocketSpec::udp())
        .expect("create UDP socket");

    kernel
        .socket_bind_inet(
            "shell",
            process.pid(),
            socket_id,
            InetSocketAddress::new("0.0.0.0", 43154),
        )
        .expect("bind UDP socket");
    kernel
        .socket_set_datagram_option(
            "shell",
            process.pid(),
            socket_id,
            DatagramSocketOption::Broadcast,
            true,
        )
        .expect("enable broadcast");

    assert!(kernel
        .socket_get(socket_id)
        .expect("socket state after broadcast enable")
        .broadcast_enabled());
}

#[test]
fn udp_multicast_memberships_are_added_and_removed_from_socket_state() {
    let mut kernel = new_kernel("vm-udp-multicast-membership");
    let process = spawn_shell(&mut kernel);
    let socket_id = kernel
        .socket_create("shell", process.pid(), SocketSpec::udp())
        .expect("create UDP socket");

    kernel
        .socket_bind_inet(
            "shell",
            process.pid(),
            socket_id,
            InetSocketAddress::new("0.0.0.0", 43155),
        )
        .expect("bind UDP socket");

    let membership = SocketMulticastMembership::new("239.1.2.3", None);
    kernel
        .socket_add_membership("shell", process.pid(), socket_id, membership.clone())
        .expect("join multicast group");

    let joined = kernel
        .socket_get(socket_id)
        .expect("socket state after join");
    assert_eq!(joined.multicast_membership_count(), 1);
    assert!(joined.has_multicast_membership(&membership));

    kernel
        .socket_drop_membership("shell", process.pid(), socket_id, membership.clone())
        .expect("leave multicast group");

    let left = kernel
        .socket_get(socket_id)
        .expect("socket state after leave");
    assert_eq!(left.multicast_membership_count(), 0);
    assert!(!left.has_multicast_membership(&membership));
}
