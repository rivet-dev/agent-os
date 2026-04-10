use agent_os_kernel::dns::{
    DnsConfig, DnsLookupPolicy, DnsLookupRequest, DnsResolver, DnsResolverError,
};
use agent_os_kernel::kernel::{KernelVm, KernelVmConfig};
use agent_os_kernel::permissions::{
    NetworkAccessRequest, NetworkOperation, PermissionDecision, Permissions,
};
use agent_os_kernel::vfs::MemoryFileSystem;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
struct MockDnsResolver {
    requests: Arc<Mutex<Vec<DnsLookupRequest>>>,
    response: Vec<IpAddr>,
}

impl MockDnsResolver {
    fn new(response: Vec<IpAddr>) -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
            response,
        }
    }

    fn requests(&self) -> Vec<DnsLookupRequest> {
        self.requests.lock().expect("mock requests").clone()
    }
}

impl DnsResolver for MockDnsResolver {
    fn lookup_ip(&self, request: &DnsLookupRequest) -> Result<Vec<IpAddr>, DnsResolverError> {
        self.requests
            .lock()
            .expect("mock requests")
            .push(request.clone());
        Ok(self.response.clone())
    }
}

fn new_kernel(config: KernelVmConfig) -> KernelVm<MemoryFileSystem> {
    KernelVm::new(MemoryFileSystem::new(), config)
}

#[test]
fn kernel_dns_resolution_prefers_overrides_before_the_resolver() {
    let resolver = MockDnsResolver::new(vec![IpAddr::V4(Ipv4Addr::new(198, 51, 100, 44))]);
    let mut config = KernelVmConfig::new("vm-dns-override");
    config.permissions = Permissions::allow_all();
    config.dns = DnsConfig {
        name_servers: vec!["203.0.113.53:5353".parse::<SocketAddr>().expect("nameserver")],
        overrides: std::iter::once((
            String::from("example.test"),
            vec![IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))],
        ))
        .collect(),
    };
    config.dns_resolver = Arc::new(resolver.clone());
    let kernel = new_kernel(config);

    let resolution = kernel
        .resolve_dns(" Example.Test. ", DnsLookupPolicy::CheckPermissions)
        .expect("resolve override hostname");

    assert_eq!(resolution.hostname(), "example.test");
    assert_eq!(resolution.source().as_str(), "override");
    assert_eq!(resolution.addresses(), &[IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))]);
    assert!(
        resolver.requests().is_empty(),
        "override lookup should not reach the resolver"
    );
}

#[test]
fn kernel_dns_resolution_passes_vm_nameservers_into_the_resolver() {
    let resolver = MockDnsResolver::new(vec![
        IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7)),
        IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7)),
    ]);
    let mut config = KernelVmConfig::new("vm-dns-resolver");
    config.permissions = Permissions::allow_all();
    config.dns = DnsConfig {
        name_servers: vec!["203.0.113.53:5353".parse::<SocketAddr>().expect("nameserver")],
        overrides: Default::default(),
    };
    config.dns_resolver = Arc::new(resolver.clone());
    let kernel = new_kernel(config);

    let resolution = kernel
        .resolve_dns("resolver.example.test", DnsLookupPolicy::CheckPermissions)
        .expect("resolve via mock resolver");

    assert_eq!(resolution.source().as_str(), "resolver");
    assert_eq!(resolution.addresses(), &[IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7))]);

    let requests = resolver.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].hostname(), "resolver.example.test");
    assert_eq!(
        requests[0].name_servers(),
        &["203.0.113.53:5353".parse::<SocketAddr>().expect("nameserver")]
    );
}

#[test]
fn kernel_dns_resolution_checks_network_permissions_when_requested() {
    let permission_requests = Arc::new(Mutex::new(Vec::<NetworkAccessRequest>::new()));
    let permission_requests_for_check = Arc::clone(&permission_requests);
    let resolver = MockDnsResolver::new(vec![IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))]);
    let mut config = KernelVmConfig::new("vm-dns-permissions");
    config.permissions = Permissions {
        network: Some(Arc::new(move |request: &NetworkAccessRequest| {
            permission_requests_for_check
                .lock()
                .expect("permission requests")
                .push(request.clone());
            PermissionDecision::deny("dns denied")
        })),
        ..Permissions::allow_all()
    };
    config.dns_resolver = Arc::new(resolver);
    let kernel = new_kernel(config);

    let error = kernel
        .resolve_dns("example.test", DnsLookupPolicy::CheckPermissions)
        .expect_err("dns permission should deny lookup");
    assert_eq!(error.code(), "EACCES");

    let requests = permission_requests.lock().expect("permission requests");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].vm_id, "vm-dns-permissions");
    assert_eq!(requests[0].op, NetworkOperation::Dns);
    assert_eq!(requests[0].resource, "dns://example.test");
}
