use agent_os_kernel::resource_accounting::ResourceLimits;
use agent_os_kernel::socket_table::{DatagramPacket, SocketTable};
use std::fmt::Debug;

fn assert_error_code<T: Debug>(
    result: Result<T, agent_os_kernel::socket_table::SocketError>,
    expected: &str,
) {
    let error = result.expect_err("expected operation to fail");
    assert_eq!(error.code(), expected);
}

#[test]
fn tcp_listener_accepts_loopback_connection_and_round_trips_data() {
    let mut sockets = SocketTable::default();
    let listener_id = sockets
        .create_tcp_listener(100, "127.0.0.1", 43_111, 4)
        .expect("create listener");
    let client_id = sockets
        .connect_tcp(200, "127.0.0.1", 43_111)
        .expect("connect client");
    let server_id = sockets
        .accept(listener_id)
        .expect("accept queued connection")
        .expect("pending accepted socket");

    assert_eq!(sockets.connection_count(), 1);
    assert_eq!(
        sockets
            .send(client_id, b"ping".to_vec())
            .expect("client send"),
        4
    );
    assert_eq!(
        sockets.recv(server_id).expect("server recv"),
        Some(b"ping".to_vec())
    );

    assert_eq!(
        sockets
            .send(server_id, b"pong".to_vec())
            .expect("server send"),
        4
    );
    assert_eq!(
        sockets.recv(client_id).expect("client recv"),
        Some(b"pong".to_vec())
    );

    sockets.close(client_id).expect("close client");
    assert_eq!(sockets.recv(server_id).expect("server eof"), None);
    assert_error_code(sockets.send(server_id, b"late".to_vec()), "EPIPE");

    sockets.close(server_id).expect("close server");
    sockets.close(listener_id).expect("close listener");
    assert_eq!(sockets.socket_count(), 0);
}

#[test]
fn udp_sockets_bind_send_and_receive_datagrams() {
    let mut sockets = SocketTable::default();
    let sender_id = sockets.create_udp_socket(100).expect("create sender");
    let receiver_id = sockets.create_udp_socket(200).expect("create receiver");

    sockets
        .bind_udp(sender_id, "127.0.0.1", 43_112)
        .expect("bind sender");
    sockets
        .bind_udp(receiver_id, "127.0.0.1", 43_113)
        .expect("bind receiver");

    assert_eq!(
        sockets
            .send_to(sender_id, "127.0.0.1", 43_113, b"ping".to_vec())
            .expect("send udp datagram"),
        4
    );

    let packet = sockets
        .recv_from(receiver_id)
        .expect("receive datagram")
        .expect("packet should be queued");
    assert_eq!(
        packet,
        DatagramPacket {
            from: agent_os_kernel::socket_table::SocketAddress::Inet {
                host: String::from("127.0.0.1"),
                port: 43_112,
            },
            data: b"ping".to_vec(),
        }
    );
    assert_eq!(
        sockets.recv_from(receiver_id).expect("drain udp queue"),
        None
    );
}

#[test]
fn unix_domain_listener_accepts_connection_and_round_trips_data() {
    let mut sockets = SocketTable::default();
    let listener_id = sockets
        .create_unix_listener(300, "/tmp/agent-os.sock", 2)
        .expect("create unix listener");
    let client_id = sockets
        .connect_unix(400, "/tmp/agent-os.sock")
        .expect("connect unix client");
    let server_id = sockets
        .accept(listener_id)
        .expect("accept unix client")
        .expect("server-side unix socket");

    sockets
        .send(client_id, b"hello".to_vec())
        .expect("write unix client payload");
    assert_eq!(
        sockets.recv(server_id).expect("read unix server payload"),
        Some(b"hello".to_vec())
    );

    sockets
        .send(server_id, b"world".to_vec())
        .expect("write unix server payload");
    assert_eq!(
        sockets.recv(client_id).expect("read unix client payload"),
        Some(b"world".to_vec())
    );
}

#[test]
fn socket_table_enforces_max_socket_limit() {
    let mut sockets = SocketTable::new(ResourceLimits {
        max_sockets: Some(2),
        ..ResourceLimits::default()
    });

    sockets
        .create_tcp_listener(100, "127.0.0.1", 43_114, 1)
        .expect("first socket");
    sockets.create_udp_socket(200).expect("second socket");

    assert_error_code(
        sockets.create_unix_listener(300, "/tmp/too-many.sock", 1),
        "EAGAIN",
    );
}

#[test]
fn cleanup_process_releases_owned_sockets_and_disconnects_peers() {
    let mut sockets = SocketTable::default();
    let listener_id = sockets
        .create_tcp_listener(10, "127.0.0.1", 43_115, 4)
        .expect("create listener");
    let udp_id = sockets.create_udp_socket(10).expect("create udp socket");
    sockets
        .bind_udp(udp_id, "127.0.0.1", 43_116)
        .expect("bind udp socket");

    let client_id = sockets
        .connect_tcp(20, "127.0.0.1", 43_115)
        .expect("connect client");
    let server_id = sockets
        .accept(listener_id)
        .expect("accept client")
        .expect("accepted server socket");

    sockets.cleanup_process(10);

    assert!(!sockets.contains(listener_id));
    assert!(!sockets.contains(server_id));
    assert!(!sockets.contains(udp_id));
    assert!(sockets.contains(client_id));
    assert_eq!(sockets.recv(client_id).expect("peer sees eof"), None);
    assert_error_code(sockets.send(client_id, b"ping".to_vec()), "EPIPE");
    assert_eq!(sockets.socket_count(), 1);
}
