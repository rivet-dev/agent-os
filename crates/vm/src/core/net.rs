use serde_json::{json, Value};
use std::net::SocketAddr;

pub fn socket_addr_family(addr: &SocketAddr) -> &'static str {
    match addr {
        SocketAddr::V4(_) => "IPv4",
        SocketAddr::V6(_) => "IPv6",
    }
}

pub fn socket_address_value(addr: &SocketAddr) -> Value {
    json!({
        "address": addr.ip().to_string(),
        "family": socket_addr_family(addr),
        "port": addr.port(),
    })
}

pub fn local_endpoint_value(addr: &SocketAddr) -> Value {
    json!({
        "localAddress": addr.ip().to_string(),
        "localPort": addr.port(),
        "family": socket_addr_family(addr),
    })
}

pub fn remote_endpoint_value(addr: &SocketAddr, port: u16) -> Value {
    json!({
        "remoteAddress": addr.ip().to_string(),
        "remotePort": port,
        "remoteFamily": socket_addr_family(addr),
    })
}

pub fn tcp_socket_info_value(local: &SocketAddr, remote: &SocketAddr) -> Value {
    json!({
        "localAddress": local.ip().to_string(),
        "localPort": local.port(),
        "localFamily": socket_addr_family(local),
        "remoteAddress": remote.ip().to_string(),
        "remotePort": remote.port(),
        "remoteFamily": socket_addr_family(remote),
    })
}

pub fn unix_socket_info_value(local_path: Option<&str>, remote_path: Option<&str>) -> Value {
    json!({
        "localPath": local_path,
        "remotePath": remote_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    #[test]
    fn formats_socket_families() {
        assert_eq!(
            socket_addr_family(&SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1)),
            "IPv4"
        );
        assert_eq!(
            socket_addr_family(&SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 1)),
            "IPv6"
        );
    }

    #[test]
    fn formats_tcp_socket_info() {
        let local = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1234);
        let remote = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), 80);

        assert_eq!(
            tcp_socket_info_value(&local, &remote),
            json!({
                "localAddress": "127.0.0.1",
                "localPort": 1234,
                "localFamily": "IPv4",
                "remoteAddress": "10.0.0.1",
                "remotePort": 80,
                "remoteFamily": "IPv4",
            })
        );
    }

    #[test]
    fn formats_unix_socket_info() {
        assert_eq!(
            unix_socket_info_value(Some("/tmp/server.sock"), Some("/tmp/client.sock")),
            json!({
                "localPath": "/tmp/server.sock",
                "remotePath": "/tmp/client.sock",
            })
        );
    }
}
