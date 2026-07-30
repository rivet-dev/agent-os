use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

pub fn is_loopback_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_loopback(),
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip
                    .to_ipv4_mapped()
                    .is_some_and(|mapped| mapped.is_loopback())
        }
    }
}

pub fn loopback_cidr(ip: IpAddr) -> &'static str {
    match ip {
        IpAddr::V4(ip) if ip.is_loopback() => "127.0.0.0/8",
        IpAddr::V6(ip)
            if ip
                .to_ipv4_mapped()
                .is_some_and(|mapped| mapped.is_loopback()) =>
        {
            "127.0.0.0/8"
        }
        IpAddr::V6(_) => "::1/128",
        IpAddr::V4(_) => "127.0.0.0/8",
    }
}

pub fn format_tcp_resource(host: &str, port: u16) -> String {
    format!("tcp://{host}:{port}")
}

pub fn restricted_non_loopback_ip_range(ip: IpAddr) -> Option<(&'static str, &'static str)> {
    match ip {
        IpAddr::V4(ip) => {
            if ip.is_unspecified() {
                return Some(("0.0.0.0/32", "unspecified"));
            }
            let [first, second, ..] = ip.octets();
            match (first, second) {
                (10, _) => Some(("10.0.0.0/8", "private")),
                (100, 64..=127) => Some(("100.64.0.0/10", "carrier-grade-nat")),
                (172, 16..=31) => Some(("172.16.0.0/12", "private")),
                (192, 168) => Some(("192.168.0.0/16", "private")),
                (169, 254) => Some(("169.254.0.0/16", "link-local")),
                (224..=239, _) => Some(("224.0.0.0/4", "multicast")),
                (240..=255, _) => Some(("240.0.0.0/4", "reserved")),
                _ => None,
            }
        }
        IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return restricted_non_loopback_ip_range(IpAddr::V4(mapped));
            }
            if let Some(compat) = ipv4_compatible_embedded(ip) {
                return restricted_non_loopback_ip_range(IpAddr::V4(compat));
            }
            if ip.is_unspecified() {
                return Some(("::/128", "unspecified"));
            }

            let segments = ip.segments();
            if (segments[0] & 0xfe00) == 0xfc00 {
                return Some(("fc00::/7", "unique-local"));
            }
            if (segments[0] & 0xffc0) == 0xfe80 {
                return Some(("fe80::/10", "link-local"));
            }
            None
        }
    }
}

fn ipv4_compatible_embedded(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    let segments = ip.segments();
    if segments[0..6].iter().any(|&s| s != 0) {
        return None;
    }
    let embedded = (u32::from(segments[6]) << 16) | u32::from(segments[7]);
    if embedded == 0 || embedded == 1 {
        return None;
    }
    Some(Ipv4Addr::from(embedded))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_restricted(ip: IpAddr, expected_label: &str) {
        let classification = restricted_non_loopback_ip_range(ip);
        assert!(
            classification.is_some(),
            "{ip} must be classified as a restricted egress target"
        );
        let (_cidr, label) = classification.unwrap();
        assert_eq!(
            label, expected_label,
            "{ip} should be labelled {expected_label}, got {label}"
        );
    }

    #[test]
    fn classifier_denies_unspecified_and_cgnat_targets() {
        assert_restricted(IpAddr::V4(Ipv4Addr::UNSPECIFIED), "unspecified");
        assert_restricted(IpAddr::V6(Ipv6Addr::UNSPECIFIED), "unspecified");
        assert_restricted(
            IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1)),
            "carrier-grade-nat",
        );
        assert_restricted(
            IpAddr::V4(Ipv4Addr::new(100, 127, 255, 254)),
            "carrier-grade-nat",
        );
        assert!(
            restricted_non_loopback_ip_range(IpAddr::V4(Ipv4Addr::new(100, 63, 255, 255,)))
                .is_none()
        );
        assert!(
            restricted_non_loopback_ip_range(IpAddr::V4(Ipv4Addr::new(100, 128, 0, 0))).is_none()
        );
    }

    #[test]
    fn classifier_denies_ipv6_spelled_metadata_addresses() {
        assert_restricted(
            IpAddr::V6("::ffff:169.254.169.254".parse::<Ipv6Addr>().unwrap()),
            "link-local",
        );
        assert_restricted(
            IpAddr::V6("::169.254.169.254".parse::<Ipv6Addr>().unwrap()),
            "link-local",
        );
        assert_restricted(
            IpAddr::V6("::10.0.0.1".parse::<Ipv6Addr>().unwrap()),
            "private",
        );
        assert_restricted(
            IpAddr::V6("::100.64.0.1".parse::<Ipv6Addr>().unwrap()),
            "carrier-grade-nat",
        );
        assert_eq!(
            restricted_non_loopback_ip_range(IpAddr::V6(Ipv6Addr::UNSPECIFIED)),
            Some(("::/128", "unspecified"))
        );
        assert!(
            restricted_non_loopback_ip_range(IpAddr::V6(Ipv6Addr::LOCALHOST)).is_none()
                || is_loopback_ip(IpAddr::V6(Ipv6Addr::LOCALHOST))
        );
        assert!(restricted_non_loopback_ip_range(IpAddr::V6(
            "::8.8.8.8".parse::<Ipv6Addr>().unwrap()
        ))
        .is_none());
    }

    #[test]
    fn classifier_denies_reserved_and_multicast_targets() {
        assert_restricted(IpAddr::V4(Ipv4Addr::new(224, 0, 0, 1)), "multicast");
        assert_restricted(IpAddr::V4(Ipv4Addr::new(239, 255, 255, 255)), "multicast");
        assert_restricted(IpAddr::V4(Ipv4Addr::new(240, 0, 0, 1)), "reserved");
        assert_restricted(IpAddr::V4(Ipv4Addr::BROADCAST), "reserved");
        assert_restricted(
            IpAddr::V6("::224.0.0.1".parse::<Ipv6Addr>().unwrap()),
            "multicast",
        );
        assert_restricted(
            IpAddr::V6("::240.0.0.1".parse::<Ipv6Addr>().unwrap()),
            "reserved",
        );
        assert!(
            restricted_non_loopback_ip_range(IpAddr::V4(Ipv4Addr::new(223, 255, 255, 255)))
                .is_none()
        );
    }

    #[test]
    fn tcp_resource_format_matches_permission_resources() {
        assert_eq!(
            format_tcp_resource("127.0.0.1", 8080),
            "tcp://127.0.0.1:8080"
        );
        assert_eq!(
            format_tcp_resource("example.test", 443),
            "tcp://example.test:443"
        );
    }
}
