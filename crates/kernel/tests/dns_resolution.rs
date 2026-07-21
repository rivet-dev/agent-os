use agentos_kernel::dns::{
    DnsConfig, DnsLookupPolicy, DnsLookupRequest, DnsRecordLookupRequest, DnsResolver,
    DnsResolverError,
};
use agentos_kernel::kernel::{KernelVm, KernelVmConfig};
use agentos_kernel::permissions::{
    NetworkAccessRequest, NetworkOperation, PermissionDecision, Permissions,
};
use agentos_kernel::vfs::MemoryFileSystem;
use hickory_proto::rr::{Record, RecordType};
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

#[derive(Debug, Clone)]
struct MockDnsResolver {
    requests: Arc<Mutex<Vec<DnsLookupRequest>>>,
    record_requests: Arc<Mutex<Vec<DnsRecordLookupRequest>>>,
    response: Vec<IpAddr>,
    record_error: Option<DnsResolverError>,
}

impl MockDnsResolver {
    fn new(response: Vec<IpAddr>) -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
            record_requests: Arc::new(Mutex::new(Vec::new())),
            response,
            record_error: None,
        }
    }

    fn with_record_error(mut self, error: DnsResolverError) -> Self {
        self.record_error = Some(error);
        self
    }

    fn requests(&self) -> Vec<DnsLookupRequest> {
        self.requests.lock().expect("mock requests").clone()
    }

    fn record_requests(&self) -> Vec<DnsRecordLookupRequest> {
        self.record_requests
            .lock()
            .expect("mock record requests")
            .clone()
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

    fn lookup_records(
        &self,
        request: &DnsRecordLookupRequest,
    ) -> Result<Vec<Record>, DnsResolverError> {
        self.record_requests
            .lock()
            .expect("mock record requests")
            .push(request.clone());
        self.record_error
            .clone()
            .map_or_else(|| Ok(Vec::new()), Err)
    }
}

struct AsyncOnlyDnsResolver;

impl DnsResolver for AsyncOnlyDnsResolver {
    fn lookup_ip(&self, _: &DnsLookupRequest) -> Result<Vec<IpAddr>, DnsResolverError> {
        panic!("reactor DNS path called the synchronous resolver")
    }

    fn lookup_records(&self, _: &DnsRecordLookupRequest) -> Result<Vec<Record>, DnsResolverError> {
        panic!("reactor DNS path called the synchronous record resolver")
    }

    fn lookup_ip_async<'a>(
        &'a self,
        _: DnsLookupRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<IpAddr>, DnsResolverError>> + Send + 'a>> {
        Box::pin(async { Ok(vec![IpAddr::V4(Ipv4Addr::new(198, 51, 100, 91))]) })
    }

    fn lookup_records_async<'a>(
        &'a self,
        _: DnsRecordLookupRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Record>, DnsResolverError>> + Send + 'a>> {
        Box::pin(async { Ok(Vec::new()) })
    }
}

fn new_kernel(config: KernelVmConfig) -> KernelVm<MemoryFileSystem> {
    KernelVm::new(MemoryFileSystem::new(), config)
}

fn poll_ready<F: Future>(future: F) -> F::Output {
    let mut context = Context::from_waker(Waker::noop());
    let mut future = std::pin::pin!(future);
    match future.as_mut().poll(&mut context) {
        Poll::Ready(output) => output,
        Poll::Pending => panic!("transport-neutral test resolver unexpectedly returned Pending"),
    }
}

#[test]
fn kernel_async_dns_path_never_calls_the_synchronous_resolver_on_a_runtime_worker() {
    let mut config = KernelVmConfig::new("vm-async-dns");
    config.permissions = Permissions::allow_all();
    config.dns_resolver = Arc::new(AsyncOnlyDnsResolver);
    let kernel = new_kernel(config);
    let resolution = poll_ready(
        kernel.resolve_dns_async("async.example.test", DnsLookupPolicy::CheckPermissions),
    )
    .expect("async resolver path");
    assert_eq!(
        resolution.addresses(),
        &[IpAddr::V4(Ipv4Addr::new(198, 51, 100, 91))]
    );
}

#[test]
fn kernel_default_resolver_is_transport_neutral_and_unavailable() {
    let mut config = KernelVmConfig::new("vm-no-host-dns");
    config.permissions = Permissions::allow_all();
    let kernel = new_kernel(config);

    let error = kernel
        .resolve_dns("example.test", DnsLookupPolicy::CheckPermissions)
        .expect_err("kernel must not perform ambient host DNS without injection");
    assert_eq!(error.code(), "EHOSTUNREACH");
    assert!(error
        .to_string()
        .contains("host DNS resolver is unavailable"));

    let literal = kernel
        .resolve_dns("192.0.2.1", DnsLookupPolicy::CheckPermissions)
        .expect("literal resolution remains kernel-owned");
    assert_eq!(
        literal.addresses(),
        &["192.0.2.1".parse::<IpAddr>().expect("IP")]
    );
}

#[test]
fn kernel_dns_resolution_prefers_overrides_before_the_resolver() {
    let resolver = MockDnsResolver::new(vec![IpAddr::V4(Ipv4Addr::new(198, 51, 100, 44))]);
    let mut config = KernelVmConfig::new("vm-dns-override");
    config.permissions = Permissions::allow_all();
    config.dns = DnsConfig {
        name_servers: vec!["203.0.113.53:5353"
            .parse::<SocketAddr>()
            .expect("nameserver")],
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
    assert_eq!(
        resolution.addresses(),
        &[IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))]
    );
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
        name_servers: vec!["203.0.113.53:5353"
            .parse::<SocketAddr>()
            .expect("nameserver")],
        overrides: Default::default(),
    };
    config.dns_resolver = Arc::new(resolver.clone());
    let kernel = new_kernel(config);

    let resolution = kernel
        .resolve_dns("resolver.example.test", DnsLookupPolicy::CheckPermissions)
        .expect("resolve via mock resolver");

    assert_eq!(resolution.source().as_str(), "resolver");
    assert_eq!(
        resolution.addresses(),
        &[IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7))]
    );

    let requests = resolver.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].hostname(), "resolver.example.test");
    assert_eq!(
        requests[0].name_servers(),
        &["203.0.113.53:5353"
            .parse::<SocketAddr>()
            .expect("nameserver")]
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

#[test]
fn kernel_dns_resolution_denies_by_default_before_resolver_lookup() {
    let resolver = MockDnsResolver::new(vec![IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))]);
    let mut config = KernelVmConfig::new("vm-dns-default-deny");
    config.dns_resolver = Arc::new(resolver.clone());
    let kernel = new_kernel(config);

    let lookup_error = kernel
        .resolve_dns("example.test", DnsLookupPolicy::CheckPermissions)
        .expect_err("missing network hook should deny address lookup");
    assert_eq!(lookup_error.code(), "EACCES");
    assert!(
        lookup_error.to_string().contains("dns://example.test"),
        "unexpected error: {lookup_error}"
    );

    let record_error = kernel
        .resolve_dns_records(
            "example.test",
            RecordType::A,
            DnsLookupPolicy::CheckPermissions,
        )
        .expect_err("missing network hook should deny record lookup");
    assert_eq!(record_error.code(), "EACCES");
    assert!(
        record_error.to_string().contains("dns://example.test"),
        "unexpected error: {record_error}"
    );

    assert!(
        resolver.requests().is_empty(),
        "permission denial should happen before address resolver lookup"
    );
    assert!(
        resolver.record_requests().is_empty(),
        "permission denial should happen before record resolver lookup"
    );
}

#[test]
fn kernel_dns_record_resolution_preserves_nxdomain_and_nodata() {
    for (resolver, expected_code) in [
        (MockDnsResolver::new(Vec::new()), "ENODATA"),
        (
            MockDnsResolver::new(Vec::new())
                .with_record_error(DnsResolverError::nx_domain("name does not exist")),
            "ENOENT",
        ),
    ] {
        let mut config = KernelVmConfig::new(format!("vm-dns-{expected_code}"));
        config.permissions = Permissions::allow_all();
        config.dns_resolver = Arc::new(resolver);
        let kernel = new_kernel(config);

        let error = kernel
            .resolve_dns_records(
                "missing.example.test",
                RecordType::SSHFP,
                DnsLookupPolicy::CheckPermissions,
            )
            .expect_err("empty DNS record answer must retain its negative status");
        assert_eq!(error.code(), expected_code);
    }
}
