//! Native DNS transport backed by Hickory and the process-owned Tokio runtime.

use agentos_kernel::dns::{
    DnsLookupRequest, DnsRecordLookupRequest, DnsResolver, DnsResolverError,
};
use agentos_runtime::{BlockingJobError, RuntimeContext};
use hickory_resolver::config::{NameServerConfig, ResolverConfig};
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::proto::rr::{Record, RecordType};
use hickory_resolver::TokioResolver;
use std::collections::BTreeSet;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;

/// Native resolver implementation injected into each kernel VM by the sidecar.
///
/// The kernel owns DNS policy, overrides, and result semantics. This adapter is
/// only the host transport: it creates Hickory resolvers on the one injected
/// process runtime and admits synchronous compatibility lookups through that
/// runtime's bounded blocking executor.
pub(crate) struct HickoryDnsResolver {
    runtime: RuntimeContext,
}

impl HickoryDnsResolver {
    pub(crate) fn new(runtime: RuntimeContext) -> Self {
        Self { runtime }
    }

    fn send_lookup_ip(
        &self,
        hostname: String,
        name_servers: Vec<SocketAddr>,
    ) -> Result<Vec<IpAddr>, DnsResolverError> {
        let resolver = {
            let _entered = self.runtime.handle().enter();
            resolver_for(&name_servers)?
        };
        let reserved_bytes = dns_lookup_input_bytes(&hostname, &name_servers);
        let handle = self.runtime.handle().clone();
        let timeout = self.runtime.blocking_job_timeout();
        self.runtime
            .blocking()
            .run_sync(reserved_bytes, timeout, move || {
                handle.block_on(async move {
                    tokio::time::timeout(timeout, lookup_ip_with_resolver(resolver, hostname))
                        .await
                        .unwrap_or_else(|_| Err(dns_lookup_timeout_error(timeout)))
                })
            })
            .map_err(map_blocking_lookup_error)?
    }

    fn send_lookup_records(
        &self,
        hostname: String,
        name_servers: Vec<SocketAddr>,
        record_type: RecordType,
    ) -> Result<Vec<Record>, DnsResolverError> {
        let resolver = {
            let _entered = self.runtime.handle().enter();
            resolver_for(&name_servers)?
        };
        let reserved_bytes = dns_lookup_input_bytes(&hostname, &name_servers);
        let handle = self.runtime.handle().clone();
        let timeout = self.runtime.blocking_job_timeout();
        self.runtime
            .blocking()
            .run_sync(reserved_bytes, timeout, move || {
                handle.block_on(async move {
                    tokio::time::timeout(
                        timeout,
                        lookup_records_with_resolver(resolver, hostname, record_type),
                    )
                    .await
                    .unwrap_or_else(|_| Err(dns_lookup_timeout_error(timeout)))
                })
            })
            .map_err(map_blocking_lookup_error)?
    }
}

impl DnsResolver for HickoryDnsResolver {
    fn lookup_ip(&self, request: &DnsLookupRequest) -> Result<Vec<IpAddr>, DnsResolverError> {
        self.send_lookup_ip(
            request.hostname().to_owned(),
            request.name_servers().to_vec(),
        )
    }

    fn lookup_records(
        &self,
        request: &DnsRecordLookupRequest,
    ) -> Result<Vec<Record>, DnsResolverError> {
        self.send_lookup_records(
            request.hostname().to_owned(),
            request.name_servers().to_vec(),
            request.record_type(),
        )
    }

    fn lookup_ip_async<'a>(
        &'a self,
        request: DnsLookupRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<IpAddr>, DnsResolverError>> + Send + 'a>> {
        Box::pin(async move {
            let resolver = {
                let _entered = self.runtime.handle().enter();
                resolver_for(request.name_servers())?
            };
            let timeout = self.runtime.blocking_job_timeout();
            tokio::time::timeout(
                timeout,
                lookup_ip_with_resolver(resolver, request.hostname().to_owned()),
            )
            .await
            .unwrap_or_else(|_| Err(dns_lookup_timeout_error(timeout)))
        })
    }

    fn lookup_records_async<'a>(
        &'a self,
        request: DnsRecordLookupRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Record>, DnsResolverError>> + Send + 'a>> {
        Box::pin(async move {
            let resolver = {
                let _entered = self.runtime.handle().enter();
                resolver_for(request.name_servers())?
            };
            let timeout = self.runtime.blocking_job_timeout();
            tokio::time::timeout(
                timeout,
                lookup_records_with_resolver(
                    resolver,
                    request.hostname().to_owned(),
                    request.record_type(),
                ),
            )
            .await
            .unwrap_or_else(|_| Err(dns_lookup_timeout_error(timeout)))
        })
    }
}

fn resolver_for(name_servers: &[SocketAddr]) -> Result<TokioResolver, DnsResolverError> {
    let resolver_config = resolver_config_from_name_servers(name_servers);
    let builder = if let Some(config) = resolver_config {
        TokioResolver::builder_with_config(config, TokioRuntimeProvider::default())
    } else {
        TokioResolver::builder_tokio().map_err(|error| {
            DnsResolverError::lookup_failed(format!(
                "failed to initialize DNS resolver from system configuration: {error}"
            ))
        })?
    };
    builder.build().map_err(|error| {
        DnsResolverError::lookup_failed(format!("failed to build DNS resolver: {error}"))
    })
}

fn dns_lookup_input_bytes(hostname: &str, name_servers: &[SocketAddr]) -> usize {
    hostname.len().saturating_add(
        name_servers
            .len()
            .saturating_mul(std::mem::size_of::<SocketAddr>()),
    )
}

fn map_blocking_lookup_error(error: BlockingJobError) -> DnsResolverError {
    DnsResolverError::lookup_failed(format!("ERR_AGENTOS_DNS_LOOKUP_EXECUTOR: {error}"))
}

fn dns_lookup_timeout_error(timeout: std::time::Duration) -> DnsResolverError {
    DnsResolverError::lookup_failed(format!(
        "ERR_AGENTOS_DNS_LOOKUP_TIMEOUT: DNS lookup exceeded {}ms; raise runtime.blocking.jobTimeoutMs",
        timeout.as_millis()
    ))
}

async fn lookup_ip_with_resolver(
    resolver: TokioResolver,
    hostname: String,
) -> Result<Vec<IpAddr>, DnsResolverError> {
    let lookup = resolver.lookup_ip(&hostname).await.map_err(|error| {
        DnsResolverError::lookup_failed(format!(
            "failed to resolve DNS address {hostname}: {error}"
        ))
    })?;

    let mut addresses = Vec::new();
    let mut seen = BTreeSet::new();
    for ip in lookup.iter() {
        if seen.insert(ip) {
            addresses.push(ip);
        }
    }

    if addresses.is_empty() {
        return Err(DnsResolverError::lookup_failed(format!(
            "failed to resolve DNS address {hostname}"
        )));
    }

    Ok(addresses)
}

async fn lookup_records_with_resolver(
    resolver: TokioResolver,
    hostname: String,
    record_type: RecordType,
) -> Result<Vec<Record>, DnsResolverError> {
    let lookup = resolver
        .lookup(&hostname, record_type)
        .await
        .map_err(|error| {
            let message = format!("failed to resolve DNS {record_type} record {hostname}: {error}");
            if error.is_nx_domain() {
                DnsResolverError::nx_domain(message)
            } else if error.is_no_records_found() {
                DnsResolverError::no_data(message)
            } else {
                DnsResolverError::lookup_failed(message)
            }
        })?;
    let records = lookup.answers().to_vec();
    if records.is_empty() {
        return Err(DnsResolverError::no_data(format!(
            "failed to resolve DNS {record_type} record {hostname}"
        )));
    }
    Ok(records)
}

fn resolver_config_from_name_servers(name_servers: &[SocketAddr]) -> Option<ResolverConfig> {
    if name_servers.is_empty() {
        return None;
    }

    let name_servers = name_servers
        .iter()
        .map(|server| {
            let mut config = NameServerConfig::udp_and_tcp(server.ip());
            for connection in &mut config.connections {
                connection.port = server.port();
                connection.bind_addr = Some(SocketAddr::new(
                    if server.is_ipv6() {
                        IpAddr::V6(Ipv6Addr::UNSPECIFIED)
                    } else {
                        IpAddr::V4(Ipv4Addr::UNSPECIFIED)
                    },
                    0,
                ));
            }
            config
        })
        .collect();

    Some(ResolverConfig::from_parts(None, vec![], name_servers))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_nameservers_preserve_family_port_and_unspecified_bind_address() {
        let requested = [
            "203.0.113.53:5353".parse::<SocketAddr>().expect("IPv4 DNS"),
            "[2001:db8::53]:5454"
                .parse::<SocketAddr>()
                .expect("IPv6 DNS"),
        ];
        let config = resolver_config_from_name_servers(&requested).expect("explicit config");
        let configured = config.name_servers();

        assert_eq!(configured.len(), requested.len());
        for (expected, server) in requested.iter().zip(configured.iter()) {
            assert_eq!(server.ip, expected.ip());
            assert_eq!(server.connections.len(), 2, "UDP and TCP per nameserver");
            for connection in &server.connections {
                assert_eq!(connection.port, expected.port());
                assert_eq!(
                    connection.bind_addr,
                    Some(SocketAddr::new(
                        if expected.is_ipv6() {
                            IpAddr::V6(Ipv6Addr::UNSPECIFIED)
                        } else {
                            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
                        },
                        0
                    ))
                );
            }
        }
    }

    #[test]
    fn empty_nameserver_list_defers_to_host_resolver_configuration() {
        assert!(resolver_config_from_name_servers(&[]).is_none());
    }
}
