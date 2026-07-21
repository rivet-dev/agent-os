use super::super::*;

pub(in crate::execution) fn emit_dns_resolution_event<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    hostname: &str,
    source: KernelDnsResolutionSource,
    addresses: &[IpAddr],
    dns: &VmDnsConfig,
) where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    emit_structured_event_or_stderr(
        bridge,
        vm_id,
        "network.dns.resolved",
        audit_fields([
            ("hostname", hostname.to_owned()),
            ("source", source.as_str().to_owned()),
            (
                "addresses",
                addresses
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
            ),
            ("address_count", addresses.len().to_string()),
            ("resolver_count", dns.name_servers.len().to_string()),
            (
                "resolvers",
                dns.name_servers
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
            ),
        ]),
    );
}

pub(in crate::execution) fn emit_dns_record_resolution_event<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    hostname: &str,
    resolution: &DnsRecordResolution,
    dns: &VmDnsConfig,
) where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    if let Some(addresses) = dns_resolution_ip_addrs(resolution.records()) {
        emit_dns_resolution_event(
            bridge,
            vm_id,
            hostname,
            resolution.source(),
            &addresses,
            dns,
        );
        return;
    }

    emit_structured_event_or_stderr(
        bridge,
        vm_id,
        "network.dns.resolved",
        audit_fields([
            ("hostname", hostname.to_owned()),
            ("source", resolution.source().as_str().to_owned()),
            (
                "addresses",
                resolution
                    .records()
                    .iter()
                    .map(summarize_dns_record)
                    .collect::<Vec<_>>()
                    .join(","),
            ),
            ("address_count", resolution.records().len().to_string()),
            ("resolver_count", dns.name_servers.len().to_string()),
            (
                "resolvers",
                dns.name_servers
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
            ),
        ]),
    );
}

pub(in crate::execution) fn emit_dns_resolution_failure_event<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    hostname: &str,
    dns: &VmDnsConfig,
    error: &SidecarError,
) where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    emit_structured_event_or_stderr(
        bridge,
        vm_id,
        "network.dns.resolve_failed",
        audit_fields([
            ("hostname", hostname.to_owned()),
            ("reason", error.to_string()),
            ("resolver_count", dns.name_servers.len().to_string()),
            (
                "resolvers",
                dns.name_servers
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
            ),
        ]),
    );
}

fn parse_dns_record_type(rrtype: &str) -> Result<RecordType, SidecarError> {
    match rrtype {
        "A" => Ok(RecordType::A),
        "AAAA" => Ok(RecordType::AAAA),
        "MX" => Ok(RecordType::MX),
        "TXT" => Ok(RecordType::TXT),
        "SRV" => Ok(RecordType::SRV),
        "CNAME" => Ok(RecordType::CNAME),
        "PTR" => Ok(RecordType::PTR),
        "SSHFP" => Ok(RecordType::SSHFP),
        "NS" => Ok(RecordType::NS),
        "SOA" => Ok(RecordType::SOA),
        "NAPTR" => Ok(RecordType::NAPTR),
        "CAA" => Ok(RecordType::CAA),
        "ANY" => Ok(RecordType::ANY),
        other => Err(SidecarError::Execution(format!(
            "ERR_NOT_IMPLEMENTED: dns rrtype {other} is not supported by the agentos dns bridge"
        ))),
    }
}

fn dns_resolution_to_node_value(
    resolution: &DnsRecordResolution,
    requested_type: &str,
) -> Result<Value, SidecarError> {
    let safe_ips = dns_resolution_safe_ip_set(resolution.records(), resolution.hostname())?;
    match requested_type {
        "A" | "AAAA" => Ok(Value::Array(
            resolution
                .records()
                .iter()
                .filter_map(|record| dns_record_ip_string(record, &safe_ips))
                .map(Value::String)
                .collect(),
        )),
        "MX" => Ok(Value::Array(
            resolution
                .records()
                .iter()
                .filter_map(|record| match record.data() {
                    RData::MX(mx) => Some(json!({
                        "priority": mx.preference,
                        "exchange": normalize_dns_name_for_node(&mx.exchange),
                        "type": "MX",
                    })),
                    _ => None,
                })
                .collect(),
        )),
        "TXT" => Ok(Value::Array(
            resolution
                .records()
                .iter()
                .filter_map(|record| match record.data() {
                    RData::TXT(txt) => Some(Value::Array(
                        txt.txt_data
                            .iter()
                            .map(|entry| Value::String(String::from_utf8_lossy(entry).into_owned()))
                            .collect(),
                    )),
                    _ => None,
                })
                .collect(),
        )),
        "SRV" => Ok(Value::Array(
            resolution
                .records()
                .iter()
                .filter_map(|record| match record.data() {
                    RData::SRV(srv) => Some(json!({
                        "priority": srv.priority,
                        "weight": srv.weight,
                        "port": srv.port,
                        "name": normalize_dns_name_for_node(&srv.target),
                        "type": "SRV",
                    })),
                    _ => None,
                })
                .collect(),
        )),
        "CNAME" => Ok(Value::Array(
            resolution
                .records()
                .iter()
                .filter_map(|record| match record.data() {
                    RData::CNAME(name) => Some(Value::String(normalize_dns_name_for_node(&name.0))),
                    _ => None,
                })
                .collect(),
        )),
        "PTR" => Ok(Value::Array(
            resolution
                .records()
                .iter()
                .filter_map(|record| match record.data() {
                    RData::PTR(name) => Some(Value::String(normalize_dns_name_for_node(&name.0))),
                    _ => None,
                })
                .collect(),
        )),
        "NS" => Ok(Value::Array(
            resolution
                .records()
                .iter()
                .filter_map(|record| match record.data() {
                    RData::NS(name) => Some(Value::String(normalize_dns_name_for_node(&name.0))),
                    _ => None,
                })
                .collect(),
        )),
        "SOA" => resolution
            .records()
            .iter()
            .find_map(|record| match record.data() {
                RData::SOA(soa) => Some(json!({
                    "nsname": normalize_dns_name_for_node(&soa.mname),
                    "hostmaster": normalize_dns_name_for_node(&soa.rname),
                    "serial": soa.serial,
                    "refresh": soa.refresh,
                    "retry": soa.retry,
                    "expire": soa.expire,
                    "minttl": soa.minimum,
                })),
                _ => None,
            })
            .ok_or_else(|| {
                SidecarError::Execution(String::from("failed to resolve DNS SOA record"))
            }),
        "NAPTR" => Ok(Value::Array(
            resolution
                .records()
                .iter()
                .filter_map(|record| match record.data() {
                    RData::NAPTR(naptr) => Some(json!({
                        "flags": String::from_utf8_lossy(&naptr.flags).into_owned(),
                        "service": String::from_utf8_lossy(&naptr.services).into_owned(),
                        "regexp": String::from_utf8_lossy(&naptr.regexp).into_owned(),
                        "replacement": normalize_dns_name_for_node(&naptr.replacement),
                        "order": naptr.order,
                        "preference": naptr.preference,
                    })),
                    _ => None,
                })
                .collect(),
        )),
        "CAA" => Ok(Value::Array(
            resolution
                .records()
                .iter()
                .filter_map(|record| match record.data() {
                    RData::CAA(caa) => {
                        let mut value = serde_json::Map::new();
                        value.insert(
                            "critical".to_owned(),
                            Value::from(u8::from(caa.issuer_critical)),
                        );
                        value.insert("type".to_owned(), Value::String(String::from("CAA")));
                        if caa.tag.eq_ignore_ascii_case("iodef") {
                            value.insert(
                                "iodef".to_owned(),
                                Value::String(
                                    caa.value_as_iodef()
                                        .map(|url| url.to_string())
                                        .unwrap_or_else(|_| {
                                            String::from_utf8_lossy(&caa.value).into_owned()
                                        }),
                                ),
                            );
                        } else if let Ok((issuer, _params)) = caa.value_as_issue() {
                            let field = if caa.tag.eq_ignore_ascii_case("issuewild") {
                                "issuewild"
                            } else {
                                "issue"
                            };
                            value.insert(
                                field.to_owned(),
                                Value::String(
                                    issuer.as_ref().map(ToString::to_string).unwrap_or_else(|| {
                                        String::from_utf8_lossy(&caa.value).into_owned()
                                    }),
                                ),
                            );
                        } else {
                            value.insert(
                                caa.tag.to_ascii_lowercase(),
                                Value::String(String::from_utf8_lossy(&caa.value).into_owned()),
                            );
                        }
                        Some(Value::Object(value))
                    }
                    _ => None,
                })
                .collect(),
        )),
        "ANY" => Ok(Value::Array(
            resolution
                .records()
                .iter()
                .filter_map(|record| dns_any_record_to_value(record, &safe_ips))
                .collect(),
        )),
        other => Err(SidecarError::Execution(format!(
            "ERR_NOT_IMPLEMENTED: dns rrtype {other} is not supported by the agentos dns bridge"
        ))),
    }
}

fn dns_resolution_safe_ip_set(
    records: &[Record],
    hostname: &str,
) -> Result<BTreeSet<IpAddr>, SidecarError> {
    let ips = records
        .iter()
        .filter_map(dns_record_ip_addr)
        .collect::<Vec<_>>();
    if ips.is_empty() {
        return Ok(BTreeSet::new());
    }
    Ok(filter_dns_safe_ip_addrs(ips, hostname)?
        .into_iter()
        .collect())
}

fn dns_resolution_ip_addrs(records: &[Record]) -> Option<Vec<IpAddr>> {
    let ips = records
        .iter()
        .filter_map(dns_record_ip_addr)
        .collect::<Vec<_>>();
    if ips.is_empty() {
        return None;
    }
    Some(ips)
}

fn dns_record_ip_addr(record: &Record) -> Option<IpAddr> {
    match record.data() {
        RData::A(address) => Some(IpAddr::V4(**address)),
        RData::AAAA(address) => Some(IpAddr::V6(**address)),
        _ => None,
    }
}

fn dns_record_ip_string(record: &Record, safe_ips: &BTreeSet<IpAddr>) -> Option<String> {
    let ip = dns_record_ip_addr(record)?;
    safe_ips.contains(&ip).then(|| ip.to_string())
}

fn dns_any_record_to_value(record: &Record, safe_ips: &BTreeSet<IpAddr>) -> Option<Value> {
    let value = match record.data() {
        RData::A(_) | RData::AAAA(_) => json!({
            "address": dns_record_ip_string(record, safe_ips)?,
            "ttl": record.ttl(),
            "type": record.record_type().to_string(),
        }),
        RData::MX(mx) => json!({
            "exchange": normalize_dns_name_for_node(&mx.exchange),
            "priority": mx.preference,
            "type": "MX",
        }),
        RData::TXT(txt) => json!({
            "entries": txt
                .txt_data
                .iter()
                .map(|entry| String::from_utf8_lossy(entry).into_owned())
                .collect::<Vec<_>>(),
            "type": "TXT",
        }),
        RData::SRV(srv) => json!({
            "name": normalize_dns_name_for_node(&srv.target),
            "port": srv.port,
            "priority": srv.priority,
            "weight": srv.weight,
            "type": "SRV",
        }),
        RData::CNAME(name) => json!({
            "value": normalize_dns_name_for_node(&name.0),
            "type": "CNAME",
        }),
        RData::PTR(name) => json!({
            "value": normalize_dns_name_for_node(&name.0),
            "type": "PTR",
        }),
        RData::NS(name) => json!({
            "value": normalize_dns_name_for_node(&name.0),
            "type": "NS",
        }),
        RData::SOA(soa) => json!({
            "nsname": normalize_dns_name_for_node(&soa.mname),
            "hostmaster": normalize_dns_name_for_node(&soa.rname),
            "serial": soa.serial,
            "refresh": soa.refresh,
            "retry": soa.retry,
            "expire": soa.expire,
            "minttl": soa.minimum,
            "type": "SOA",
        }),
        RData::NAPTR(naptr) => json!({
            "flags": String::from_utf8_lossy(&naptr.flags).into_owned(),
            "service": String::from_utf8_lossy(&naptr.services).into_owned(),
            "regexp": String::from_utf8_lossy(&naptr.regexp).into_owned(),
            "replacement": normalize_dns_name_for_node(&naptr.replacement),
            "order": naptr.order,
            "preference": naptr.preference,
            "type": "NAPTR",
        }),
        RData::CAA(caa) => {
            let mut value = serde_json::Map::new();
            value.insert(
                "critical".to_owned(),
                Value::from(u8::from(caa.issuer_critical)),
            );
            value.insert("type".to_owned(), Value::String(String::from("CAA")));
            if caa.tag.eq_ignore_ascii_case("iodef") {
                value.insert(
                    "iodef".to_owned(),
                    Value::String(
                        caa.value_as_iodef()
                            .map(|url| url.to_string())
                            .unwrap_or_else(|_| String::from_utf8_lossy(&caa.value).into_owned()),
                    ),
                );
            } else if let Ok((issuer, _params)) = caa.value_as_issue() {
                let field = if caa.tag.eq_ignore_ascii_case("issuewild") {
                    "issuewild"
                } else {
                    "issue"
                };
                value.insert(
                    field.to_owned(),
                    Value::String(
                        issuer
                            .as_ref()
                            .map(ToString::to_string)
                            .unwrap_or_else(|| String::from_utf8_lossy(&caa.value).into_owned()),
                    ),
                );
            }
            Value::Object(value)
        }
        _ => return None,
    };
    Some(value)
}

fn normalize_dns_name_for_node(name: &impl ToString) -> String {
    name.to_string().trim_end_matches('.').to_owned()
}

fn summarize_dns_record(record: &Record) -> String {
    match record.data() {
        RData::A(_) | RData::AAAA(_) => record.data().to_string(),
        _ => format!("{} {}", record.record_type(), record.data()),
    }
}

fn dns_raw_rr_negative_response(error_code: &str) -> Option<Value> {
    let status = match error_code {
        "ENOENT" => "nxdomain",
        "ENODATA" => "nodata",
        _ => return None,
    };
    Some(json!({
        "status": status,
        "records": [],
    }))
}

fn dns_raw_rr_response(resolution: &DnsRecordResolution, requested_type: &str) -> Value {
    let records = resolution
        .records()
        .iter()
        .filter_map(|record| {
            let data = match record.data() {
                RData::PTR(name) if requested_type == "PTR" => {
                    normalize_dns_name_for_node(&name.0).into_bytes()
                }
                RData::SSHFP(sshfp) if requested_type == "SSHFP" => {
                    let mut data = Vec::with_capacity(sshfp.fingerprint.len() + 2);
                    data.push(sshfp.algorithm.into());
                    data.push(sshfp.fingerprint_type.into());
                    data.extend_from_slice(&sshfp.fingerprint);
                    data
                }
                _ => return None,
            };
            Some(json!({
                "data": base64::engine::general_purpose::STANDARD.encode(data),
                "ttl": record.ttl(),
            }))
        })
        .collect::<Vec<_>>();
    json!({
        "status": "ok",
        "records": records,
    })
}

// build_root_filesystem, convert_root_lower_descriptor, convert_root_filesystem_entry,
// root_snapshot_entry moved to crate::bootstrap

// apply_root_filesystem_entry, ensure_parent_directories moved to crate::bootstrap

// ProcNetEntry moved to crate::state

pub(crate) fn format_dns_resource(hostname: &str) -> String {
    format!("dns://{hostname}")
}

// --- Guest Python socket bridge helpers ------------------------------------

pub(in crate::execution) fn service_javascript_dns_sync_rpc<B>(
    bridge: &SharedBridge<B>,
    kernel: &SidecarKernel,
    vm_id: &str,
    dns: &VmDnsConfig,
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    match request.method.as_str() {
        "dns.lookup" => {
            let payload = request
                .args
                .first()
                .cloned()
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "dns.lookup requires a request payload",
                    ))
                })
                .and_then(|value| {
                    serde_json::from_value::<JavascriptDnsLookupRequest>(value).map_err(|error| {
                        SidecarError::InvalidState(format!("invalid dns.lookup payload: {error}"))
                    })
                })?;
            let addresses = filter_dns_ip_addrs(
                resolve_dns_ip_addrs(
                    bridge,
                    kernel,
                    vm_id,
                    dns,
                    &payload.hostname,
                    DnsLookupPolicy::CheckPermissions,
                )?,
                payload.family,
            )?;
            let addresses = filter_dns_safe_ip_addrs(addresses, &payload.hostname)?;
            Ok(Value::Array(
                addresses
                    .into_iter()
                    .map(|ip| {
                        json!({
                            "address": ip.to_string(),
                            "family": if ip.is_ipv6() { 6 } else { 4 },
                        })
                    })
                    .collect(),
            ))
        }
        "dns.resolve" | "dns.resolve4" | "dns.resolve6" | "dns.resolveRawRr" => {
            let payload = request
                .args
                .first()
                .cloned()
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "dns.resolve requires a request payload",
                    ))
                })
                .and_then(|value| {
                    serde_json::from_value::<JavascriptDnsResolveRequest>(value).map_err(|error| {
                        SidecarError::InvalidState(format!("invalid dns.resolve payload: {error}"))
                    })
                })?;
            let requested_type = match request.method.as_str() {
                "dns.resolve4" => String::from("A"),
                "dns.resolve6" => String::from("AAAA"),
                _ => payload
                    .rrtype
                    .as_deref()
                    .unwrap_or("A")
                    .to_ascii_uppercase(),
            };
            let record_type = parse_dns_record_type(&requested_type)?;
            if request.method == "dns.resolveRawRr" {
                if !matches!(requested_type.as_str(), "PTR" | "SSHFP") {
                    return Err(SidecarError::InvalidState(format!(
                        "EINVAL: raw DNS RR bridge does not support {requested_type}"
                    )));
                }
                let resolution = match kernel.resolve_dns_records(
                    &payload.hostname,
                    record_type,
                    DnsLookupPolicy::CheckPermissions,
                ) {
                    Ok(resolution) => {
                        emit_dns_record_resolution_event(
                            bridge,
                            vm_id,
                            &payload.hostname,
                            &resolution,
                            dns,
                        );
                        resolution
                    }
                    Err(error) => {
                        if let Some(response) = dns_raw_rr_negative_response(error.code()) {
                            return Ok(response);
                        }
                        let sidecar_error = kernel_error(error.clone());
                        if error.code() != "EACCES" {
                            emit_dns_resolution_failure_event(
                                bridge,
                                vm_id,
                                &payload.hostname,
                                dns,
                                &sidecar_error,
                            );
                        }
                        return Err(sidecar_error);
                    }
                };
                Ok(dns_raw_rr_response(&resolution, &requested_type))
            } else {
                let resolution = resolve_dns_records(
                    bridge,
                    kernel,
                    vm_id,
                    dns,
                    &payload.hostname,
                    record_type,
                    DnsLookupPolicy::CheckPermissions,
                )?;
                dns_resolution_to_node_value(&resolution, &requested_type)
            }
        }
        other => Err(SidecarError::InvalidState(format!(
            "unsupported JavaScript dns sync RPC method {other}"
        ))),
    }
}

#[cfg(test)]
mod raw_rr_tests {
    use super::*;
    use hickory_resolver::proto::rr::{
        rdata::sshfp::{Algorithm, FingerprintType},
        rdata::{PTR, SSHFP},
        Name,
    };

    #[test]
    fn raw_rr_record_type_accepts_sshfp() {
        assert_eq!(
            parse_dns_record_type("SSHFP").expect("SSHFP record type"),
            RecordType::SSHFP
        );
    }

    #[test]
    fn raw_rr_response_preserves_sshfp_wire_bytes_and_ttl() {
        let owner = Name::from_ascii("host.example.test.").expect("owner name");
        let record = Record::from_rdata(
            owner,
            3_600,
            RData::SSHFP(SSHFP::new(
                Algorithm::Ed25519,
                FingerprintType::SHA256,
                vec![0xde, 0xad, 0xbe, 0xef],
            )),
        );
        let resolution = DnsRecordResolution::new(
            "host.example.test",
            KernelDnsResolutionSource::Resolver,
            vec![record],
        );

        assert_eq!(
            dns_raw_rr_response(&resolution, "SSHFP"),
            json!({
                "status": "ok",
                "records": [{
                    "data": base64::engine::general_purpose::STANDARD.encode([
                        4, 2, 0xde, 0xad, 0xbe, 0xef,
                    ]),
                    "ttl": 3_600,
                }],
            })
        );
    }

    #[test]
    fn raw_rr_response_encodes_normalized_ptr_target_bytes() {
        let owner = Name::from_ascii("4.3.2.1.in-addr.arpa.").expect("owner name");
        let target = Name::from_ascii("host.example.test.").expect("target name");
        let record = Record::from_rdata(owner, 90, RData::PTR(PTR(target)));
        let resolution = DnsRecordResolution::new(
            "4.3.2.1.in-addr.arpa",
            KernelDnsResolutionSource::Resolver,
            vec![record],
        );

        assert_eq!(
            dns_raw_rr_response(&resolution, "PTR"),
            json!({
                "status": "ok",
                "records": [{
                    "data": base64::engine::general_purpose::STANDARD
                        .encode(b"host.example.test"),
                    "ttl": 90,
                }],
            })
        );
    }

    #[test]
    fn raw_rr_negative_status_distinguishes_nxdomain_and_nodata() {
        assert_eq!(
            dns_raw_rr_negative_response("ENOENT"),
            Some(json!({ "status": "nxdomain", "records": [] }))
        );
        assert_eq!(
            dns_raw_rr_negative_response("ENODATA"),
            Some(json!({ "status": "nodata", "records": [] }))
        );
        assert_eq!(dns_raw_rr_negative_response("EACCES"), None);
    }
}
