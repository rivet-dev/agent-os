use super::*;
use crate::state::ManagedHostNetRoute;
use agentos_execution::host::{SocketDomain as HostSocketDomain, SocketKind as HostSocketKind};

const SYNTHETIC_V8_TERMINATION_STDERR: &[u8] = b"Error: Execution terminated\n";

fn finish_kernel_child_from_runtime_exit(
    kernel_handle: &KernelProcessHandle,
    exit_code: i32,
    exit_signal: Option<i32>,
    core_dumped: bool,
) {
    if let Some(signal) = exit_signal {
        kernel_handle.finish_signaled(signal, core_dumped);
    } else {
        kernel_handle.finish(exit_code);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InheritedOutputStream {
    Stdout,
    Stderr,
}

fn classify_inherited_output_stream(
    _child_description: u64,
    child_description_path: &str,
) -> Option<InheritedOutputStream> {
    match child_description_path {
        "/dev/stdout" => Some(InheritedOutputStream::Stdout),
        "/dev/stderr" => Some(InheritedOutputStream::Stderr),
        _ => None,
    }
}

fn cancel_host_call_completion(
    completion: &crate::state::HostCallCompletion,
    message: impl Into<String>,
) -> Result<(), SidecarError> {
    completion
        .reply
        .fail(HostServiceError::new("ECANCELED", message))
        .map_err(SidecarError::from)
}

fn cancel_direct_host_reply(
    reply: &DirectHostReplyHandle,
    message: impl Into<String>,
) -> Result<(), SidecarError> {
    reply
        .fail(HostServiceError::new("ECANCELED", message))
        .map_err(SidecarError::from)
}

#[derive(Debug)]
pub(super) enum TransferredHostNetSocket {
    Tcp {
        socket: Box<ActiveTcpSocket>,
        metadata: TransferredHostNetMetadata,
    },
    TcpListener {
        listener: ActiveTcpListener,
        metadata: TransferredHostNetMetadata,
    },
    Udp {
        socket: ActiveUdpSocket,
        metadata: TransferredHostNetMetadata,
    },
    Unix {
        socket: ActiveUnixSocket,
        metadata: TransferredHostNetMetadata,
    },
    UnixListener {
        listener: ActiveUnixListener,
        metadata: TransferredHostNetMetadata,
    },
    Pending {
        metadata: TransferredHostNetMetadata,
        description_handles: Arc<()>,
        tcp_reservation: Option<(SocketFamily, u16)>,
    },
}

impl TransferredHostNetSocket {
    pub(super) fn kernel_transfer_guard(&self) -> Option<TransferredFd> {
        match self {
            Self::Tcp { socket, .. } => socket.kernel_transfer_guard.clone(),
            Self::TcpListener { listener, .. } => listener.kernel_transfer_guard.clone(),
            Self::Udp { socket, .. } => socket.kernel_transfer_guard.clone(),
            Self::Unix { .. } | Self::UnixListener { .. } | Self::Pending { .. } => None,
        }
    }
}

#[derive(Debug)]
pub(super) struct ManagedTransferredHostNetSocket {
    pub resource: TransferredHostNetSocket,
    pub transfer: TransferredFd,
}

impl ManagedTransferredHostNetSocket {
    pub(super) fn clone_for_fd_transfer(&self) -> Result<Self, SidecarError> {
        Ok(Self {
            resource: self.resource.clone_for_fd_transfer()?,
            transfer: self.transfer.clone(),
        })
    }
}

#[cfg(test)]
mod scm_rights_tests {
    use super::*;

    fn canonical_tcp_metadata(nonblocking: bool) -> TransferredHostNetMetadata {
        TransferredHostNetMetadata {
            domain: HOST_NET_AF_INET,
            socket_type: HOST_NET_SOCK_STREAM,
            protocol: HOST_NET_IPPROTO_TCP,
            nonblocking,
            recv_timeout_ms: Some(250),
            bind_options: None,
            local_info: Some(json!({ "address": "127.0.0.1", "port": 41000 })),
            local_unix_address: None,
            local_reservation: None,
            remote_info: Some(json!({ "address": "127.0.0.1", "port": 8080 })),
            remote_unix_address: None,
            listening: false,
        }
    }

    #[test]
    fn scm_rights_rejects_forged_ids_metadata_and_unbounded_open_state() {
        let canonical = canonical_tcp_metadata(false).as_value();
        let mut ambiguous = canonical.clone();
        ambiguous["socketId"] = json!("socket-1");
        ambiguous["serverId"] = json!("listener-1");
        assert!(scm_rights_host_net_source(&ambiguous)
            .expect_err("SCM_RIGHTS class ids must be unambiguous")
            .to_string()
            .contains("at most one resource id"));

        let mut wrong_class = canonical.clone();
        wrong_class["class"] = json!("listener");
        assert!(validate_host_net_metadata(
            &wrong_class,
            &canonical_tcp_metadata(false),
            "tcp",
            "SCM_RIGHTS host-network",
        )
        .expect_err("SCM class forgery")
        .to_string()
        .contains("class"));

        let mut wrong_address = canonical.clone();
        wrong_address["remoteInfo"] = json!({ "address": "203.0.113.8", "port": 22 });
        assert!(validate_host_net_metadata(
            &wrong_address,
            &canonical_tcp_metadata(false),
            "tcp",
            "SCM_RIGHTS host-network",
        )
        .expect_err("SCM address forgery")
        .to_string()
        .contains("remoteInfo"));

        let mut bad_nonblocking = canonical.clone();
        bad_nonblocking["nonblocking"] = json!("yes");
        assert!(
            host_net_open_description_options(&bad_nonblocking, "SCM_RIGHTS host-network")
                .expect_err("nonblocking must be strict boolean")
                .to_string()
                .contains("must be boolean")
        );

        let mut bad_timeout = canonical.clone();
        bad_timeout["recvTimeoutMs"] = json!(HOST_NET_RECV_TIMEOUT_MAX_MS + 1);
        assert!(
            host_net_open_description_options(&bad_timeout, "SCM_RIGHTS host-network")
                .expect_err("timeout must be range bounded")
                .to_string()
                .contains("exceeds")
        );

        let mut oversized = canonical;
        oversized["extra"] = json!("x".repeat(HOST_NET_METADATA_MAX_STRING_BYTES + 1));
        assert!(
            host_net_open_description_options(&oversized, "SCM_RIGHTS host-network")
                .expect_err("metadata strings must be bounded")
                .to_string()
                .contains("ENAMETOOLONG")
        );
    }

    #[test]
    fn scm_pending_accepts_only_canonical_unconnected_socket_tuples() {
        let pending = json!({
            "kind": "hostNet",
            "domain": HOST_NET_AF_UNIX,
            "socketType": HOST_NET_SOCK_STREAM,
            "protocol": 0,
            "nonblocking": true,
            "recvTimeoutMs": null,
            "bindOptions": null,
            "localInfo": null,
            "localUnixAddress": "unix-unnamed",
            "localReservation": null,
            "remoteInfo": null,
            "remoteUnixAddress": null,
            "listening": false,
        });
        let options = host_net_open_description_options(&pending, "SCM_RIGHTS pending socket")
            .expect("pending options");
        let canonical =
            TransferredHostNetMetadata::pending(&pending, options, "SCM_RIGHTS pending socket")
                .expect("valid unconnected Unix stream");
        assert_eq!(
            canonical.as_value()["localUnixAddress"],
            json!("unix-unnamed")
        );

        for (field, replacement) in [
            ("listening", json!(true)),
            ("bindOptions", json!({ "path": "/forged" })),
            ("remoteUnixAddress", json!("unix:/forged-peer")),
        ] {
            let mut forged = pending.clone();
            forged[field] = replacement;
            let options = host_net_open_description_options(&forged, "SCM_RIGHTS pending socket")
                .expect("open state remains syntactically valid");
            assert!(TransferredHostNetMetadata::pending(
                &forged,
                options,
                "SCM_RIGHTS pending socket",
            )
            .is_err());
        }

        let unsupported = json!({
            "domain": HOST_NET_AF_UNIX,
            "socketType": HOST_NET_SOCK_DGRAM,
            "protocol": 0,
            "listening": false,
        });
        let options = HostNetOpenDescriptionOptions {
            nonblocking: false,
            recv_timeout_ms: None,
        };
        assert!(TransferredHostNetMetadata::pending(
            &unsupported,
            options,
            "SCM_RIGHTS pending socket",
        )
        .expect_err("unsupported tuple")
        .to_string()
        .contains("EPROTONOSUPPORT"));
    }

    #[test]
    fn spawn_pending_socket_is_keyed_by_managed_description_identity() {
        let pending = ProcessSpawnHostNetworkDescriptor {
            guest_fd: 7,
            description_id: Some(String::from("42")),
            close_on_exec: false,
            socket_id: None,
            server_id: None,
            udp_socket_id: None,
            metadata: Value::Null,
        };
        assert_eq!(
            spawn_host_net_source(&pending).expect("managed pending spawn source"),
            SpawnHostNetSource::Pending(42)
        );

        let legacy = ProcessSpawnHostNetworkDescriptor {
            description_id: None,
            ..pending
        };
        assert!(spawn_host_net_source(&legacy).is_err());
    }

    #[test]
    fn duplicate_rights_share_one_description_and_queue_lifecycle_is_counted() {
        let registry = Arc::new(Mutex::new(BTreeMap::new()));
        let resource = TransferredHostNetSocket::Pending {
            metadata: canonical_tcp_metadata(false),
            description_handles: Arc::new(()),
            tcp_reservation: None,
        };
        let duplicate = resource
            .clone_for_fd_transfer()
            .expect("duplicate one open-file description");
        register_host_net_transfer_description(&registry, &resource)
            .expect("register source description");
        register_host_net_transfer_description(&registry, &duplicate)
            .expect("register duplicate description");

        let mut queued = BTreeMap::new();
        add_live_host_net_transfer_descriptions(&registry, &mut queued);
        assert_eq!(queued.len(), 1, "duplicate rights are one open description");
        check_spawn_host_net_resource_limit(
            Some(1),
            1,
            0,
            "EMFILE",
            "SCM_RIGHTS socket descriptions",
            "maxSockets",
        )
        .expect("transferring an existing description at the maximum is allowed");

        drop(resource);
        queued.clear();
        add_live_host_net_transfer_descriptions(&registry, &mut queued);
        assert_eq!(
            queued.len(),
            1,
            "queued/received alias keeps the description live"
        );

        drop(duplicate);
        queued.clear();
        add_live_host_net_transfer_descriptions(&registry, &mut queued);
        assert!(
            queued.is_empty(),
            "dropping the final right releases the queue lease"
        );

        assert!(check_spawn_host_net_resource_limit(
            Some(1),
            1,
            1,
            "EMFILE",
            "SCM_RIGHTS pending socket descriptions",
            "maxSockets",
        )
        .is_err());
    }
}

#[cfg(test)]
mod descendant_rpc_route_tests {
    #[test]
    fn descendant_dispatch_routes_context_owned_operations() {
        let source = include_str!("child_process.rs");
        let start = source
            .rfind("async fn poll_descendant_process")
            .expect("descendant pump must exist");
        let end = source[start..]
            .find("fn write_descendant_process_stdin")
            .map(|offset| start + offset)
            .expect("descendant pump end must exist");
        let dispatcher = &source[start..end];

        for method in ["process.exec", "process.exec_fd_image_commit"] {
            assert!(
                !dispatcher.contains(&format!("request.method == \"{method}\"")),
                "typed exec operation must not fall back to the descendant legacy RPC dispatcher"
            );
        }
        let process_dispatch_start = source
            .rfind("async fn dispatch_descendant_context_process_operation(")
            .expect("typed descendant process dispatcher");
        let process_dispatch = &source[process_dispatch_start..start];
        for required in [
            "ProcessOperation::Exec(request)",
            "validate_wasm_fd_image_commit_request(&request)",
            "self.commit_wasm_fd_process_image(",
            "self.exec_process_image(",
        ] {
            assert!(
                process_dispatch.contains(required),
                "typed descendant process dispatcher is missing {required}"
            );
        }

        for operation in ["SendDescriptorRights", "ReceiveDescriptorRights"] {
            assert!(
                dispatcher.contains(&format!(
                    "agentos_execution::host::NetworkOperation::{operation}"
                )),
                "descendant dispatcher does not classify {operation}"
            );
        }
        for operation in [
            "Socket", "Bind", "Connect", "Listen", "Accept", "Receive", "Send",
        ] {
            assert!(
                dispatcher.contains(&format!(
                    "agentos_execution::host::NetworkOperation::{operation}"
                )),
                "descendant dispatcher does not classify managed-fd {operation}"
            );
        }
        assert!(dispatcher.contains("self.dispatch_descendant_context_managed_network_operation("));
        assert!(dispatcher.contains("FilesystemOperation::StdinRead"));
        assert!(dispatcher.contains("self.service_descendant_kernel_stdin_read("));
        for operation in ["Close", "CloseFrom", "Renumber", "DuplicateTo", "Move"] {
            assert!(
                dispatcher.contains(&format!("FilesystemOperation::{operation}")),
                "descendant dispatcher does not classify {operation}"
            );
        }
        assert!(dispatcher.contains("self.dispatch_descendant_context_descriptor_operation("));

        let descriptor_start = source
            .rfind("fn dispatch_descendant_context_descriptor_operation(")
            .expect("descendant descriptor dispatcher");
        let descriptor_end = source[descriptor_start..]
            .find("fn dispatch_descendant_context_dns_operation(")
            .map(|offset| descriptor_start + offset)
            .expect("descendant descriptor dispatcher end");
        let descriptor = &source[descriptor_start..descriptor_end];
        for shared_service in [
            "host_dispatch::close_with_managed_retirement(",
            "host_dispatch::closefrom_with_managed_retirement(",
            "host_dispatch::replace_descriptor_with_managed_retirement(",
        ] {
            assert!(
                descriptor.contains(shared_service),
                "descendant descriptor dispatcher bypasses {shared_service}"
            );
        }
        assert!(descriptor.contains("host_dispatch::authorize_host_operation("));

        let managed_start = source
            .rfind("fn dispatch_descendant_context_managed_network_operation(")
            .expect("descendant managed-network dispatcher");
        let managed_end = source[managed_start..]
            .find("async fn dispatch_descendant_context_process_operation(")
            .map(|offset| managed_start + offset)
            .expect("descendant managed-network dispatcher end");
        let managed = &source[managed_start..managed_end];
        assert!(managed.contains("HostNetworkOperation::SendDescriptorRights"));
        assert!(managed.contains("HostNetworkOperation::ReceiveDescriptorRights"));
        assert!(managed.contains("descriptor_rights_compat_request("));
        assert!(managed.contains("service_javascript_sync_rpc("));
        assert!(managed.contains("host_dispatch::authorize_host_operation("));
        assert!(managed.contains("host_dispatch::service_descendant_managed_fd_network_operation("));

        assert!(
            dispatcher.contains("settle_host_call_completion_for_process("),
            "descendant deferred completions must share root connect finalization"
        );
    }
}

impl TransferredHostNetSocket {
    fn class(&self) -> &'static str {
        match self {
            Self::Tcp { .. } => "tcp",
            Self::TcpListener { .. } => "listener",
            Self::Udp { .. } => "udp",
            Self::Unix { .. } => "unix",
            Self::UnixListener { .. } => "unix-listener",
            Self::Pending { .. } => "pending",
        }
    }

    fn metadata(&self) -> &TransferredHostNetMetadata {
        match self {
            Self::Tcp { metadata, .. }
            | Self::TcpListener { metadata, .. }
            | Self::Udp { metadata, .. }
            | Self::Unix { metadata, .. }
            | Self::UnixListener { metadata, .. }
            | Self::Pending { metadata, .. } => metadata,
        }
    }

    fn description_identity(&self) -> (&Arc<()>, bool, bool) {
        match self {
            Self::Tcp { socket, .. } => (
                &socket.description_handles,
                true,
                socket.kernel_socket_id.is_some(),
            ),
            Self::TcpListener { listener, .. } => (
                &listener.description_handles,
                false,
                listener.kernel_socket_id.is_some(),
            ),
            Self::Udp { socket, .. } => (
                &socket.description_handles,
                false,
                socket.kernel_socket_id.is_some(),
            ),
            Self::Unix { socket, .. } => (&socket.description_handles, true, false),
            Self::UnixListener { listener, .. } => (&listener.description_handles, false, false),
            Self::Pending {
                description_handles,
                ..
            } => (description_handles, false, false),
        }
    }

    pub(super) fn clone_for_fd_transfer(&self) -> Result<Self, SidecarError> {
        match self {
            Self::Tcp { socket, metadata } => Ok(Self::Tcp {
                socket: Box::new(socket.clone_for_fd_transfer()),
                metadata: metadata.clone(),
            }),
            Self::TcpListener { listener, metadata } => Ok(Self::TcpListener {
                listener: listener.clone_for_fd_transfer()?,
                metadata: metadata.clone(),
            }),
            Self::Udp { socket, metadata } => Ok(Self::Udp {
                socket: socket.clone_for_fd_transfer()?,
                metadata: metadata.clone(),
            }),
            Self::Unix { socket, metadata } => Ok(Self::Unix {
                socket: socket.clone_for_fd_transfer(),
                metadata: metadata.clone(),
            }),
            Self::UnixListener { listener, metadata } => Ok(Self::UnixListener {
                listener: listener.clone_for_fd_transfer()?,
                metadata: metadata.clone(),
            }),
            Self::Pending {
                metadata,
                description_handles,
                tcp_reservation,
            } => Ok(Self::Pending {
                metadata: metadata.clone(),
                description_handles: Arc::clone(description_handles),
                tcp_reservation: *tcp_reservation,
            }),
        }
    }
}

pub(super) fn register_host_net_transfer_description(
    registry: &HostNetTransferDescriptionRegistry,
    resource: &TransferredHostNetSocket,
) -> Result<(), SidecarError> {
    let (handles, connected, kernel_backed) = resource.description_identity();
    // Adopted kernel sockets remain present in the kernel resource snapshot
    // while queued. Only sidecar-only descriptions need this weak queue lease.
    if kernel_backed {
        return Ok(());
    }
    let description_id = Arc::as_ptr(handles) as usize;
    let mut descriptions = registry
        .lock()
        .map_err(|_| SidecarError::host("EIO", "host-network transfer registry lock poisoned"))?;
    descriptions.retain(|_, description| description.handles.upgrade().is_some());
    descriptions
        .entry(description_id)
        .and_modify(|description| description.connected |= connected)
        .or_insert_with(|| HostNetTransferDescription {
            handles: Arc::downgrade(handles),
            connected,
        });
    Ok(())
}

#[derive(Debug, Clone)]
pub(super) struct TransferredHostNetMetadata {
    domain: u32,
    socket_type: u32,
    protocol: u32,
    nonblocking: bool,
    recv_timeout_ms: Option<u64>,
    bind_options: Option<Value>,
    local_info: Option<Value>,
    local_unix_address: Option<String>,
    local_reservation: Option<String>,
    remote_info: Option<Value>,
    remote_unix_address: Option<String>,
    pub(super) listening: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct HostNetOpenDescriptionOptions {
    nonblocking: bool,
    recv_timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum SpawnHostNetSource {
    Tcp(String),
    TcpListener(String),
    Udp(String),
    Pending(u64),
}

#[derive(Debug, Clone, Copy)]
pub(super) enum ResolvedHostNetSourceClass {
    Tcp,
    Unix,
    TcpListener,
    UnixListener,
    Udp,
}

#[derive(Debug)]
pub(super) struct PreparedSpawnHostNetDescription {
    guest_fds: Vec<u32>,
    description_id: Option<u64>,
    resource: TransferredHostNetSocket,
    metadata: Value,
}

#[derive(Debug, Default)]
pub(super) struct PreparedSpawnHostNetFds {
    descriptions: Vec<PreparedSpawnHostNetDescription>,
    kernel_actions: Vec<ProcessSpawnFileAction>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct SpawnHostNetFdState {
    description: usize,
    close_on_exec: bool,
}

pub(super) const HOST_NET_AF_INET: u32 = 1;
pub(super) const HOST_NET_AF_INET6: u32 = 2;
pub(super) const HOST_NET_AF_UNIX: u32 = 3;
pub(super) const HOST_NET_SOCK_DGRAM: u32 = 5;
pub(super) const HOST_NET_SOCK_STREAM: u32 = 6;
pub(super) const HOST_NET_SOCKET_TYPE_MASK: u32 = 0x0f;
pub(super) const HOST_NET_SOCK_CLOEXEC: u32 = 0x2000;
pub(super) const HOST_NET_SOCK_NONBLOCK: u32 = 0x4000;
pub(super) const HOST_NET_IPPROTO_TCP: u32 = 6;
pub(super) const HOST_NET_IPPROTO_UDP: u32 = 17;
pub(super) const HOST_NET_METADATA_MAX_BYTES: usize = 16 * 1024;
pub(super) const HOST_NET_METADATA_MAX_STRING_BYTES: usize = 4 * 1024;
pub(super) const HOST_NET_RECV_TIMEOUT_MAX_MS: u64 = u32::MAX as u64;
pub(super) const LINUX_SCM_MAX_FD: usize = 253;

pub(super) fn validate_host_net_metadata_size(
    value: &Value,
    label: &str,
) -> Result<(), SidecarError> {
    let encoded_len = serde_json::to_vec(value)
        .map_err(|error| {
            SidecarError::host("EINVAL", format!("invalid {label} metadata: {error}"))
        })?
        .len();
    if encoded_len > HOST_NET_METADATA_MAX_BYTES {
        return Err(SidecarError::host("E2BIG", format!("{label} metadata is {encoded_len} bytes, exceeding the {HOST_NET_METADATA_MAX_BYTES}-byte limit"
        )));
    }
    fn validate_strings(value: &Value, label: &str) -> Result<(), SidecarError> {
        match value {
            Value::String(value) if value.len() > HOST_NET_METADATA_MAX_STRING_BYTES => {
                Err(SidecarError::host("ENAMETOOLONG", format!("{label} metadata string exceeds {HOST_NET_METADATA_MAX_STRING_BYTES} bytes"
                )))
            }
            Value::Array(values) => {
                for value in values {
                    validate_strings(value, label)?;
                }
                Ok(())
            }
            Value::Object(values) => {
                for (key, value) in values {
                    if key.len() > HOST_NET_METADATA_MAX_STRING_BYTES {
                        return Err(SidecarError::host("ENAMETOOLONG", format!("{label} metadata key exceeds {HOST_NET_METADATA_MAX_STRING_BYTES} bytes"
                        )));
                    }
                    validate_strings(value, label)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
    validate_strings(value, label)
}

pub(super) fn host_net_open_description_options(
    value: &Value,
    label: &str,
) -> Result<HostNetOpenDescriptionOptions, SidecarError> {
    validate_host_net_metadata_size(value, label)?;
    let object = value.as_object().ok_or_else(|| {
        SidecarError::host(
            "EINVAL",
            String::from("host-network metadata must be an object"),
        )
    })?;
    let nonblocking = match object.get("nonblocking") {
        None => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => {
            return Err(SidecarError::host(
                "EINVAL",
                format!("{label} metadata nonblocking must be boolean"),
            ))
        }
    };
    let recv_timeout_ms = match object.get("recvTimeoutMs") {
        None | Some(Value::Null) => None,
        Some(value) => {
            let timeout = value.as_u64().ok_or_else(|| {
                SidecarError::host(
                    "EINVAL",
                    format!(
                        "{label} metadata recvTimeoutMs must be a non-negative integer or null"
                    ),
                )
            })?;
            if timeout > HOST_NET_RECV_TIMEOUT_MAX_MS {
                return Err(SidecarError::host(
                    "EINVAL",
                    format!(
                        "{label} metadata recvTimeoutMs exceeds {HOST_NET_RECV_TIMEOUT_MAX_MS}"
                    ),
                ));
            }
            Some(timeout)
        }
    };
    Ok(HostNetOpenDescriptionOptions {
        nonblocking,
        recv_timeout_ms,
    })
}

pub(super) fn host_net_domain(address: &SocketAddr) -> u32 {
    if address.is_ipv4() {
        HOST_NET_AF_INET
    } else {
        HOST_NET_AF_INET6
    }
}

pub(super) fn host_net_address_info(address: SocketAddr) -> Value {
    json!({
        "address": address.ip().to_string(),
        "port": address.port(),
    })
}

pub(super) fn host_net_bind_options(address: SocketAddr) -> Value {
    json!({
        "host": address.ip().to_string(),
        "port": address.port(),
    })
}

pub(super) fn host_net_unix_options(
    path: Option<&str>,
    abstract_path_hex: Option<&str>,
) -> Option<Value> {
    if let Some(abstract_path_hex) = abstract_path_hex {
        Some(json!({ "abstractPathHex": abstract_path_hex }))
    } else {
        path.map(|path| json!({ "path": path }))
    }
}

pub(super) fn host_net_unix_address(path: Option<&str>, abstract_path_hex: Option<&str>) -> String {
    if let Some(abstract_path_hex) = abstract_path_hex {
        format!("unix-abstract:{}", abstract_path_hex.to_ascii_lowercase())
    } else if let Some(path) = path {
        format!("unix:{path}")
    } else {
        String::from("unix-unnamed")
    }
}

impl TransferredHostNetMetadata {
    fn tcp_socket(socket: &ActiveTcpSocket, options: HostNetOpenDescriptionOptions) -> Self {
        Self {
            domain: host_net_domain(&socket.guest_remote_addr),
            socket_type: HOST_NET_SOCK_STREAM,
            protocol: HOST_NET_IPPROTO_TCP,
            nonblocking: options.nonblocking,
            recv_timeout_ms: options.recv_timeout_ms,
            bind_options: None,
            local_info: Some(host_net_address_info(socket.guest_local_addr)),
            local_unix_address: None,
            local_reservation: None,
            remote_info: Some(host_net_address_info(socket.guest_remote_addr)),
            remote_unix_address: None,
            listening: false,
        }
    }

    fn tcp_listener(listener: &ActiveTcpListener, options: HostNetOpenDescriptionOptions) -> Self {
        let local = listener.guest_local_addr();
        Self {
            domain: host_net_domain(&local),
            socket_type: HOST_NET_SOCK_STREAM,
            protocol: HOST_NET_IPPROTO_TCP,
            nonblocking: options.nonblocking,
            recv_timeout_ms: options.recv_timeout_ms,
            bind_options: Some(host_net_bind_options(local)),
            local_info: Some(host_net_address_info(local)),
            local_unix_address: None,
            local_reservation: None,
            remote_info: None,
            remote_unix_address: None,
            listening: true,
        }
    }

    fn udp_socket(socket: &ActiveUdpSocket, options: HostNetOpenDescriptionOptions) -> Self {
        let domain = match socket.family {
            UdpFamily::Ipv4 => HOST_NET_AF_INET,
            UdpFamily::Ipv6 => HOST_NET_AF_INET6,
        };
        Self {
            domain,
            socket_type: HOST_NET_SOCK_DGRAM,
            protocol: HOST_NET_IPPROTO_UDP,
            nonblocking: options.nonblocking,
            recv_timeout_ms: options.recv_timeout_ms,
            bind_options: socket.guest_local_addr.map(host_net_bind_options),
            local_info: socket.guest_local_addr.map(host_net_address_info),
            local_unix_address: None,
            local_reservation: None,
            remote_info: None,
            remote_unix_address: None,
            listening: false,
        }
    }

    fn unix_socket(socket: &ActiveUnixSocket, options: HostNetOpenDescriptionOptions) -> Self {
        Self {
            domain: HOST_NET_AF_UNIX,
            socket_type: HOST_NET_SOCK_STREAM,
            protocol: 0,
            nonblocking: options.nonblocking,
            recv_timeout_ms: options.recv_timeout_ms,
            bind_options: host_net_unix_options(
                socket.local_path.as_deref(),
                socket.local_abstract_path_hex.as_deref(),
            ),
            local_info: None,
            local_unix_address: Some(host_net_unix_address(
                socket.local_path.as_deref(),
                socket.local_abstract_path_hex.as_deref(),
            )),
            local_reservation: None,
            remote_info: None,
            remote_unix_address: Some(host_net_unix_address(
                socket.remote_path.as_deref(),
                socket.remote_abstract_path_hex.as_deref(),
            )),
            listening: false,
        }
    }

    fn unix_listener(
        listener: &ActiveUnixListener,
        options: HostNetOpenDescriptionOptions,
    ) -> Self {
        Self {
            domain: HOST_NET_AF_UNIX,
            socket_type: HOST_NET_SOCK_STREAM,
            protocol: 0,
            nonblocking: options.nonblocking,
            recv_timeout_ms: options.recv_timeout_ms,
            bind_options: host_net_unix_options(
                Some(listener.path.as_str()),
                listener.abstract_path_hex.as_deref(),
            ),
            local_info: None,
            local_unix_address: Some(host_net_unix_address(
                Some(listener.path.as_str()),
                listener.abstract_path_hex.as_deref(),
            )),
            local_reservation: None,
            remote_info: None,
            remote_unix_address: None,
            listening: listener.listener.is_some(),
        }
    }

    pub(super) fn pending(
        value: &Value,
        options: HostNetOpenDescriptionOptions,
        label: &str,
    ) -> Result<Self, SidecarError> {
        let object = value
            .as_object()
            .expect("open-description options validated object");
        let domain = required_host_net_u32(object, "domain", label)?;
        let raw_socket_type = required_host_net_u32(object, "socketType", label)?;
        if raw_socket_type
            & !(HOST_NET_SOCKET_TYPE_MASK | HOST_NET_SOCK_NONBLOCK | HOST_NET_SOCK_CLOEXEC)
            != 0
        {
            return Err(SidecarError::host(
                "EINVAL",
                format!("{label} metadata socketType contains unsupported flags"),
            ));
        }
        let socket_type = raw_socket_type & HOST_NET_SOCKET_TYPE_MASK;
        let requested_protocol = required_host_net_u32(object, "protocol", label)?;
        let protocol = match (domain, socket_type, requested_protocol) {
            (
                HOST_NET_AF_INET | HOST_NET_AF_INET6,
                HOST_NET_SOCK_STREAM,
                0 | HOST_NET_IPPROTO_TCP,
            ) => HOST_NET_IPPROTO_TCP,
            (
                HOST_NET_AF_INET | HOST_NET_AF_INET6,
                HOST_NET_SOCK_DGRAM,
                0 | HOST_NET_IPPROTO_UDP,
            ) => HOST_NET_IPPROTO_UDP,
            (HOST_NET_AF_UNIX, HOST_NET_SOCK_STREAM, 0) => 0,
            _ => {
                return Err(SidecarError::host(
                    "EPROTONOSUPPORT",
                    format!("{label} metadata does not describe a supported unconnected socket"),
                ))
            }
        };
        let metadata = Self {
            domain,
            socket_type,
            protocol,
            nonblocking: options.nonblocking,
            recv_timeout_ms: options.recv_timeout_ms,
            bind_options: None,
            local_info: None,
            local_unix_address: (domain == HOST_NET_AF_UNIX).then(|| String::from("unix-unnamed")),
            local_reservation: None,
            remote_info: None,
            remote_unix_address: None,
            listening: false,
        };
        validate_host_net_metadata(value, &metadata, "pending", label)?;
        Ok(metadata)
    }

    fn as_value(&self) -> Value {
        json!({
            "domain": self.domain,
            "socketType": self.socket_type,
            "protocol": self.protocol,
            "nonblocking": self.nonblocking,
            "recvTimeoutMs": self.recv_timeout_ms,
            "bindOptions": self.bind_options,
            "localInfo": self.local_info,
            "localUnixAddress": self.local_unix_address,
            "localReservation": self.local_reservation,
            "remoteInfo": self.remote_info,
            "remoteUnixAddress": self.remote_unix_address,
            "listening": self.listening,
        })
    }
}

pub(super) fn required_host_net_u32(
    object: &Map<String, Value>,
    name: &str,
    label: &str,
) -> Result<u32, SidecarError> {
    object
        .get(name)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| {
            SidecarError::host(
                "EINVAL",
                format!("{label} metadata field {name} must be u32"),
            )
        })
}

pub(super) fn validate_host_net_metadata(
    value: &Value,
    expected: &TransferredHostNetMetadata,
    expected_class: &str,
    label: &str,
) -> Result<(), SidecarError> {
    let options = host_net_open_description_options(value, label)?;
    if options.nonblocking != expected.nonblocking
        || options.recv_timeout_ms != expected.recv_timeout_ms
    {
        return Err(host_net_metadata_mismatch(label, "open-description state"));
    }
    let object = value.as_object().ok_or_else(|| {
        SidecarError::host("EINVAL", format!("{label} metadata must be an object"))
    })?;
    let domain = required_host_net_u32(object, "domain", label)?;
    if domain != expected.domain {
        return Err(host_net_metadata_mismatch(label, "domain"));
    }
    let socket_type = required_host_net_u32(object, "socketType", label)?;
    if socket_type & !(HOST_NET_SOCKET_TYPE_MASK | HOST_NET_SOCK_NONBLOCK | HOST_NET_SOCK_CLOEXEC)
        != 0
        || socket_type & HOST_NET_SOCKET_TYPE_MASK != expected.socket_type
    {
        return Err(SidecarError::InvalidState(format!(
            "EINVAL: {label} metadata socketType {socket_type:#x} does not match the sidecar-owned socket type {:#x}",
            expected.socket_type
        )));
    }
    let protocol = required_host_net_u32(object, "protocol", label)?;
    if protocol != 0 && protocol != expected.protocol {
        return Err(host_net_metadata_mismatch(label, "protocol"));
    }
    if let Some(class) = object.get("class") {
        if class.as_str() != Some(expected_class) {
            return Err(host_net_metadata_mismatch(label, "class"));
        }
    }
    let expected_value = expected.as_value();
    let expected_object = expected_value
        .as_object()
        .expect("canonical host-network metadata is an object");
    for name in [
        "bindOptions",
        "localInfo",
        "localUnixAddress",
        "localReservation",
        "remoteInfo",
        "remoteUnixAddress",
        "listening",
    ] {
        let actual = object.get(name).unwrap_or(&Value::Null);
        let canonical = expected_object.get(name).unwrap_or(&Value::Null);
        if actual != canonical {
            return Err(host_net_metadata_mismatch(label, name));
        }
    }
    Ok(())
}

pub(super) fn host_net_metadata_mismatch(label: &str, field: &str) -> SidecarError {
    SidecarError::host(
        "EINVAL",
        format!("{label} metadata {field} does not match the sidecar-owned socket"),
    )
}

pub(super) fn spawn_host_net_source(
    fd: &ProcessSpawnHostNetworkDescriptor,
) -> Result<SpawnHostNetSource, SidecarError> {
    let mut sources = Vec::new();
    if let Some(id) = fd.socket_id.as_deref().filter(|id| !id.is_empty()) {
        validate_host_net_resource_id(id, "inherited socket id")?;
        sources.push(SpawnHostNetSource::Tcp(id.to_owned()));
    }
    if let Some(id) = fd.server_id.as_deref().filter(|id| !id.is_empty()) {
        validate_host_net_resource_id(id, "inherited listener id")?;
        sources.push(SpawnHostNetSource::TcpListener(id.to_owned()));
    }
    if let Some(id) = fd.udp_socket_id.as_deref().filter(|id| !id.is_empty()) {
        validate_host_net_resource_id(id, "inherited UDP socket id")?;
        sources.push(SpawnHostNetSource::Udp(id.to_owned()));
    }
    if sources.is_empty() {
        if let Some(description_id) = fd
            .description_id
            .as_deref()
            .and_then(|value| value.parse::<u64>().ok())
        {
            return Ok(SpawnHostNetSource::Pending(description_id));
        }
    }
    if sources.len() != 1 {
        return Err(SidecarError::host(
            "EINVAL",
            String::from("inherited host-network fd requires exactly one resource id"),
        ));
    }
    Ok(sources.pop().expect("one source checked"))
}

pub(super) fn validate_host_net_resource_id(id: &str, label: &str) -> Result<(), SidecarError> {
    if id.len() > 256 {
        return Err(SidecarError::host(
            "ENAMETOOLONG",
            format!("{label} exceeds 256 bytes"),
        ));
    }
    Ok(())
}

pub(super) fn scm_rights_host_net_source(
    value: &Value,
) -> Result<Option<SpawnHostNetSource>, SidecarError> {
    validate_host_net_metadata_size(value, "SCM_RIGHTS host-network")?;
    let object = value.as_object().ok_or_else(|| {
        SidecarError::host(
            "EINVAL",
            String::from("SCM_RIGHTS host-network entry must be an object"),
        )
    })?;
    let mut sources = Vec::new();
    for (name, source) in [("socketId", 0u8), ("serverId", 1u8), ("udpSocketId", 2u8)] {
        let Some(value) = object.get(name) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        let id = value.as_str().filter(|id| !id.is_empty()).ok_or_else(|| {
            SidecarError::host(
                "EINVAL",
                format!("SCM_RIGHTS host-network {name} must be a non-empty string or null"),
            )
        })?;
        validate_host_net_resource_id(id, name)?;
        sources.push(match source {
            0 => SpawnHostNetSource::Tcp(id.to_owned()),
            1 => SpawnHostNetSource::TcpListener(id.to_owned()),
            2 => SpawnHostNetSource::Udp(id.to_owned()),
            _ => unreachable!(),
        });
    }
    if sources.len() > 1 {
        return Err(SidecarError::host(
            "EINVAL",
            String::from("SCM_RIGHTS host-network entry requires at most one resource id"),
        ));
    }
    Ok(sources.pop())
}

pub(super) fn posix_spawn_action_guest_fd(
    action: &ProcessSpawnFileAction,
    label: &str,
) -> Result<u32, SidecarError> {
    u32::try_from(action.guest_fd.unwrap_or(action.fd)).map_err(|_| {
        SidecarError::host(
            "EBADF",
            format!(
                "invalid posix_spawn {label} fd {}",
                action.guest_fd.unwrap_or(action.fd)
            ),
        )
    })
}

pub(super) fn posix_spawn_action_guest_source_fd(
    action: &ProcessSpawnFileAction,
) -> Result<u32, SidecarError> {
    u32::try_from(action.guest_source_fd.unwrap_or(action.source_fd)).map_err(|_| {
        SidecarError::host(
            "EBADF",
            format!(
                "invalid posix_spawn dup2 source {}",
                action.guest_source_fd.unwrap_or(action.source_fd)
            ),
        )
    })
}

impl PreparedSpawnHostNetFds {
    fn inherited_fd_count(&self) -> usize {
        self.descriptions
            .iter()
            .map(|description| description.guest_fds.len())
            .sum()
    }

    fn bootstrap_json(&self) -> Value {
        Value::Array(
            self.descriptions
                .iter()
                .enumerate()
                .map(|(index, description)| {
                    let mut value = json!({
                        "guestFds": description.guest_fds,
                        "descriptionId": description.description_id.map(|id| id.to_string()),
                        "metadata": description.metadata,
                    });
                    let object = value
                        .as_object_mut()
                        .expect("spawn host-network bootstrap value is an object");
                    let (key, id) = match description.resource {
                        TransferredHostNetSocket::Tcp { .. } => {
                            ("socketId", format!("spawn-tcp-{index}"))
                        }
                        TransferredHostNetSocket::TcpListener { .. } => {
                            ("serverId", format!("spawn-listener-{index}"))
                        }
                        TransferredHostNetSocket::Udp { .. } => {
                            ("udpSocketId", format!("spawn-udp-{index}"))
                        }
                        TransferredHostNetSocket::Unix { .. } => {
                            ("socketId", format!("spawn-unix-{index}"))
                        }
                        TransferredHostNetSocket::UnixListener { .. } => {
                            ("serverId", format!("spawn-unix-listener-{index}"))
                        }
                        TransferredHostNetSocket::Pending { .. } => return value,
                    };
                    object.insert(key.to_owned(), Value::String(id));
                    value
                })
                .collect(),
        )
    }

    fn validate_install(
        &self,
        managed_descriptions: &BTreeMap<u64, crate::state::ManagedHostNetDescription>,
        child_kernel_pid: u32,
    ) -> Result<(), SidecarError> {
        let pending_tcp_reservations = self
            .descriptions
            .iter()
            .filter(|description| {
                matches!(
                    &description.resource,
                    TransferredHostNetSocket::Pending {
                        tcp_reservation: Some(_),
                        ..
                    }
                )
            })
            .count();
        0usize
            .checked_add(pending_tcp_reservations)
            .ok_or_else(|| {
                SidecarError::host(
                    "EOVERFLOW",
                    "inherited TCP reservation identifiers exceed the child identifier space",
                )
            })?;
        for description in &self.descriptions {
            if let Some(description_id) = description.description_id {
                let canonical = managed_descriptions.get(&description_id).ok_or_else(|| {
                    SidecarError::host(
                        "ESTALE",
                        "managed spawn description disappeared before child installation",
                    )
                })?;
                if canonical.routes.contains_key(&child_kernel_pid) {
                    return Err(SidecarError::host(
                        "EEXIST",
                        format!(
                            "managed spawn description {description_id} already has a route for child PID {child_kernel_pid}"
                        ),
                    ));
                }
            }
        }
        Ok(())
    }

    fn install(
        self,
        child: &mut ActiveProcess,
        managed_descriptions: &mut BTreeMap<u64, crate::state::ManagedHostNetDescription>,
    ) {
        for (index, description) in self.descriptions.into_iter().enumerate() {
            let route = match description.resource {
                TransferredHostNetSocket::Tcp { mut socket, .. } => {
                    socket.listener_id = None;
                    let id = format!("spawn-tcp-{index}");
                    child.tcp_sockets.insert(id.clone(), *socket);
                    ManagedHostNetRoute::TcpSocket(id)
                }
                TransferredHostNetSocket::TcpListener { listener, .. } => {
                    let id = format!("spawn-listener-{index}");
                    child.tcp_listeners.insert(id.clone(), listener);
                    ManagedHostNetRoute::TcpListener(id)
                }
                TransferredHostNetSocket::Udp { socket, .. } => {
                    let id = format!("spawn-udp-{index}");
                    child.udp_sockets.insert(id.clone(), socket);
                    ManagedHostNetRoute::UdpSocket(id)
                }
                TransferredHostNetSocket::Unix { mut socket, .. } => {
                    socket.listener_id = None;
                    let id = format!("spawn-unix-{index}");
                    child.unix_sockets.insert(id.clone(), socket);
                    ManagedHostNetRoute::UnixSocket(id)
                }
                TransferredHostNetSocket::UnixListener { listener, metadata } => {
                    let id = format!("spawn-unix-listener-{index}");
                    let listening = metadata.listening;
                    child.unix_listeners.insert(id.clone(), listener);
                    if listening {
                        ManagedHostNetRoute::UnixListener(id)
                    } else {
                        ManagedHostNetRoute::UnixBound { listener_id: id }
                    }
                }
                TransferredHostNetSocket::Pending {
                    tcp_reservation, ..
                } => {
                    // No reactor transport exists until the child binds or
                    // connects this canonical pending socket description.
                    if let Some(reservation) = tcp_reservation {
                        let reservation_id = child.allocate_tcp_port_reservation_id();
                        child
                            .tcp_port_reservations
                            .insert(reservation_id.clone(), reservation);
                        ManagedHostNetRoute::TcpBound { reservation_id }
                    } else {
                        ManagedHostNetRoute::Unbound
                    }
                }
            };
            if let Some(description_id) = description.description_id {
                let canonical = managed_descriptions
                    .get_mut(&description_id)
                    .expect("managed spawn descriptions were prevalidated");
                canonical.routes.insert(child.kernel_pid, route);
            }
        }
    }
}

pub(super) fn transferred_hostnet_value(
    class: &str,
    metadata: TransferredHostNetMetadata,
    id: Option<(&str, String)>,
    capability_identity: Option<(
        agentos_runtime::capability::CapabilityId,
        agentos_runtime::capability::CapabilityGeneration,
    )>,
    local: Option<SocketAddr>,
    remote: Option<SocketAddr>,
) -> Value {
    let mut value = json!({
        "kind": "hostNet",
        "class": class,
        "domain": metadata.domain,
        "socketType": metadata.socket_type,
        "protocol": metadata.protocol,
        "nonblocking": metadata.nonblocking,
        "recvTimeoutMs": metadata.recv_timeout_ms,
        "bindOptions": metadata.bind_options,
        "localInfo": metadata.local_info,
        "localUnixAddress": metadata.local_unix_address,
        "localReservation": metadata.local_reservation,
        "remoteInfo": metadata.remote_info,
        "remoteUnixAddress": metadata.remote_unix_address,
        "listening": metadata.listening,
    });
    let object = value
        .as_object_mut()
        .expect("transferred host-net value is an object");
    if let Some((key, id)) = id {
        object.insert(key.to_owned(), Value::String(id));
    }
    if let Some((capability_id, capability_generation)) = capability_identity {
        object.insert(String::from("capabilityId"), Value::from(capability_id));
        object.insert(
            String::from("capabilityGeneration"),
            Value::from(capability_generation),
        );
    }
    if let Some(local) = local {
        object.insert(
            String::from("localAddress"),
            Value::String(local.ip().to_string()),
        );
        object.insert(String::from("localPort"), Value::from(local.port()));
    }
    if let Some(remote) = remote {
        object.insert(
            String::from("remoteAddress"),
            Value::String(remote.ip().to_string()),
        );
        object.insert(String::from("remotePort"), Value::from(remote.port()));
    }
    value
}

pub(super) fn adopt_kernel_socket_transfer_guard(
    kernel: &mut SidecarKernel,
    pid: u32,
    socket_id: SocketId,
    nonblocking: bool,
) -> Result<agentos_kernel::fd_table::TransferredFd, SidecarError> {
    let flags = if nonblocking {
        agentos_kernel::fd_table::O_NONBLOCK
    } else {
        0
    };
    kernel
        .fd_adopt_socket_transfer(EXECUTION_DRIVER_NAME, pid, socket_id, flags)
        .map_err(kernel_error)
}

pub(super) fn prepare_transferred_host_net_resource(
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    source: &SpawnHostNetSource,
    value: &Value,
    label: &str,
) -> Result<TransferredHostNetSocket, SidecarError> {
    prepare_transferred_host_net_resource_with_options(kernel, process, source, value, label, None)
}

fn prepare_transferred_host_net_resource_with_options(
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    source: &SpawnHostNetSource,
    value: &Value,
    label: &str,
    trusted_options: Option<HostNetOpenDescriptionOptions>,
) -> Result<TransferredHostNetSocket, SidecarError> {
    if matches!(source, SpawnHostNetSource::Pending(_)) {
        let options = trusted_options.unwrap_or(host_net_open_description_options(value, label)?);
        let metadata = TransferredHostNetMetadata::pending(value, options, label)?;
        return Ok(TransferredHostNetSocket::Pending {
            metadata,
            description_handles: Arc::new(()),
            tcp_reservation: None,
        });
    }
    // Resolve the sidecar-owned resource before reading any guest-controlled
    // metadata. Metadata may describe open-description flags, but it never
    // selects the resource class or lifecycle.
    let resolved_class = match source {
        SpawnHostNetSource::Tcp(socket_id) if process.tcp_sockets.contains_key(socket_id) => {
            ResolvedHostNetSourceClass::Tcp
        }
        SpawnHostNetSource::Tcp(socket_id) if process.unix_sockets.contains_key(socket_id) => {
            ResolvedHostNetSourceClass::Unix
        }
        SpawnHostNetSource::Tcp(socket_id) => {
            return Err(SidecarError::host(
                "EBADF",
                format!("unknown transferable socket {socket_id}"),
            ))
        }
        SpawnHostNetSource::TcpListener(listener_id)
            if process.tcp_listeners.contains_key(listener_id) =>
        {
            ResolvedHostNetSourceClass::TcpListener
        }
        SpawnHostNetSource::TcpListener(listener_id)
            if process.unix_listeners.contains_key(listener_id) =>
        {
            ResolvedHostNetSourceClass::UnixListener
        }
        SpawnHostNetSource::TcpListener(listener_id) => {
            return Err(SidecarError::host(
                "EBADF",
                format!("unknown transferable listener {listener_id}"),
            ))
        }
        SpawnHostNetSource::Udp(socket_id) if process.udp_sockets.contains_key(socket_id) => {
            ResolvedHostNetSourceClass::Udp
        }
        SpawnHostNetSource::Udp(socket_id) => {
            return Err(SidecarError::host(
                "EBADF",
                format!("unknown transferable UDP socket {socket_id}"),
            ))
        }
        SpawnHostNetSource::Pending(_) => {
            unreachable!("pending resources return before sidecar lookup")
        }
    };
    let validate_metadata = trusted_options.is_none();
    let options = trusted_options.unwrap_or(host_net_open_description_options(value, label)?);
    let resource = match (source, resolved_class) {
        (SpawnHostNetSource::Tcp(socket_id), ResolvedHostNetSourceClass::Tcp) => {
            let socket = process
                .tcp_sockets
                .get_mut(socket_id)
                .expect("resolved TCP socket remains present");
            let metadata = TransferredHostNetMetadata::tcp_socket(socket, options);
            if validate_metadata {
                validate_host_net_metadata(value, &metadata, "tcp", label)?;
            }
            if socket.kernel_transfer_guard.is_none() {
                if let Some(kernel_socket_id) = socket.kernel_socket_id {
                    socket.kernel_transfer_guard = Some(adopt_kernel_socket_transfer_guard(
                        kernel,
                        process.kernel_pid,
                        kernel_socket_id,
                        options.nonblocking,
                    )?);
                }
            }
            TransferredHostNetSocket::Tcp {
                socket: Box::new(socket.clone_for_fd_transfer()),
                metadata,
            }
        }
        (SpawnHostNetSource::Tcp(socket_id), ResolvedHostNetSourceClass::Unix) => {
            let socket = process
                .unix_sockets
                .get(socket_id)
                .expect("resolved Unix socket remains present");
            let metadata = TransferredHostNetMetadata::unix_socket(socket, options);
            if validate_metadata {
                validate_host_net_metadata(value, &metadata, "unix", label)?;
            }
            TransferredHostNetSocket::Unix {
                socket: socket.clone_for_fd_transfer(),
                metadata,
            }
        }
        (SpawnHostNetSource::TcpListener(listener_id), ResolvedHostNetSourceClass::TcpListener) => {
            let listener = process
                .tcp_listeners
                .get_mut(listener_id)
                .expect("resolved TCP listener remains present");
            let metadata = TransferredHostNetMetadata::tcp_listener(listener, options);
            if validate_metadata {
                validate_host_net_metadata(value, &metadata, "listener", label)?;
            }
            if listener.kernel_transfer_guard.is_none() {
                if let Some(kernel_socket_id) = listener.kernel_socket_id {
                    listener.kernel_transfer_guard = Some(adopt_kernel_socket_transfer_guard(
                        kernel,
                        process.kernel_pid,
                        kernel_socket_id,
                        options.nonblocking,
                    )?);
                }
            }
            TransferredHostNetSocket::TcpListener {
                listener: listener.clone_for_fd_transfer()?,
                metadata,
            }
        }
        (
            SpawnHostNetSource::TcpListener(listener_id),
            ResolvedHostNetSourceClass::UnixListener,
        ) => {
            let listener = process
                .unix_listeners
                .get(listener_id)
                .expect("resolved Unix listener remains present");
            let metadata = TransferredHostNetMetadata::unix_listener(listener, options);
            if validate_metadata {
                validate_host_net_metadata(value, &metadata, "unix-listener", label)?;
            }
            TransferredHostNetSocket::UnixListener {
                listener: listener.clone_for_fd_transfer()?,
                metadata,
            }
        }
        (SpawnHostNetSource::Udp(socket_id), ResolvedHostNetSourceClass::Udp) => {
            let socket = process.udp_sockets.get_mut(socket_id).ok_or_else(|| {
                SidecarError::host(
                    "EBADF",
                    format!("unknown transferable UDP socket {socket_id}"),
                )
            })?;
            let metadata = TransferredHostNetMetadata::udp_socket(socket, options);
            if validate_metadata {
                validate_host_net_metadata(value, &metadata, "udp", label)?;
            }
            if socket.kernel_transfer_guard.is_none() {
                if let Some(kernel_socket_id) = socket.kernel_socket_id {
                    socket.kernel_transfer_guard = Some(adopt_kernel_socket_transfer_guard(
                        kernel,
                        process.kernel_pid,
                        kernel_socket_id,
                        options.nonblocking,
                    )?);
                }
            }
            TransferredHostNetSocket::Udp {
                socket: socket.clone_for_fd_transfer()?,
                metadata,
            }
        }
        (SpawnHostNetSource::Pending(_), _) => {
            unreachable!("pending resources return before sidecar lookup")
        }
        _ => unreachable!("resource source and resolved class must agree"),
    };
    Ok(resource)
}

pub(super) fn prepare_managed_transferred_host_net_resource(
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    description_id: u64,
    kernel_fd: u32,
    description: &crate::state::ManagedHostNetDescription,
    label: &str,
) -> Result<TransferredHostNetSocket, SidecarError> {
    let mut tcp_reservation = None;
    let source = match description.route_for(process.kernel_pid).ok_or_else(|| {
        SidecarError::host(
            "ESTALE",
            "managed transfer description has no process route",
        )
    })? {
        ManagedHostNetRoute::TcpSocket(id) | ManagedHostNetRoute::UnixSocket(id) => {
            SpawnHostNetSource::Tcp(id.clone())
        }
        ManagedHostNetRoute::TcpListener(id)
        | ManagedHostNetRoute::UnixListener(id)
        | ManagedHostNetRoute::UnixBound { listener_id: id } => {
            SpawnHostNetSource::TcpListener(id.clone())
        }
        ManagedHostNetRoute::UdpSocket(id) => SpawnHostNetSource::Udp(id.clone()),
        ManagedHostNetRoute::Unbound => SpawnHostNetSource::Pending(description_id),
        ManagedHostNetRoute::TcpBound { reservation_id } => {
            tcp_reservation = Some(
                *process
                    .tcp_port_reservations
                    .get(reservation_id)
                    .ok_or_else(|| {
                        SidecarError::host(
                            "ESTALE",
                            "managed bound TCP reservation disappeared before transfer",
                        )
                    })?,
            );
            SpawnHostNetSource::Pending(description_id)
        }
    };
    let nonblocking = kernel
        .fd_stat(EXECUTION_DRIVER_NAME, process.kernel_pid, kernel_fd)
        .map_err(kernel_error)?
        .flags
        & agentos_kernel::fd_table::O_NONBLOCK
        != 0;
    let domain = match description.domain {
        HostSocketDomain::Inet4 => HOST_NET_AF_INET,
        HostSocketDomain::Inet6 => HOST_NET_AF_INET6,
        HostSocketDomain::Unix => HOST_NET_AF_UNIX,
    };
    let socket_type = match description.kind {
        HostSocketKind::Stream => HOST_NET_SOCK_STREAM,
        HostSocketKind::Datagram => HOST_NET_SOCK_DGRAM,
        HostSocketKind::SeqPacket => {
            return Err(SidecarError::host(
                "EPROTONOSUPPORT",
                "managed host-network transfer does not support SOCK_SEQPACKET",
            ));
        }
    };
    let protocol = match (description.domain, description.kind) {
        (HostSocketDomain::Unix, _) => 0,
        (_, HostSocketKind::Stream) => HOST_NET_IPPROTO_TCP,
        (_, HostSocketKind::Datagram) => HOST_NET_IPPROTO_UDP,
        (_, HostSocketKind::SeqPacket) => unreachable!("SOCK_SEQPACKET rejected above"),
    };
    let canonical = json!({
        "domain": domain,
        "socketType": socket_type,
        "protocol": protocol,
        "nonblocking": nonblocking,
        "recvTimeoutMs": description.receive_timeout_ms,
        "bindOptions": Value::Null,
        "localInfo": Value::Null,
        "localUnixAddress": if description.domain == HostSocketDomain::Unix { json!("unix-unnamed") } else { Value::Null },
        "localReservation": Value::Null,
        "remoteInfo": Value::Null,
        "remoteUnixAddress": Value::Null,
        "listening": false,
    });
    let mut resource = prepare_transferred_host_net_resource_with_options(
        kernel,
        process,
        &source,
        &canonical,
        label,
        Some(HostNetOpenDescriptionOptions {
            nonblocking,
            recv_timeout_ms: description.receive_timeout_ms,
        }),
    )?;
    if let TransferredHostNetSocket::Pending {
        tcp_reservation: transferred_reservation,
        ..
    } = &mut resource
    {
        *transferred_reservation = tcp_reservation;
    }
    Ok(resource)
}

pub(super) const POSIX_SPAWN_RESETIDS: u32 = 1 << 0;
pub(super) const POSIX_SPAWN_SETPGROUP: u32 = 1 << 1;
pub(super) const POSIX_SPAWN_SETSIGDEF: u32 = 1 << 2;
pub(super) const POSIX_SPAWN_SETSIGMASK: u32 = 1 << 3;
pub(super) const POSIX_SPAWN_SETSCHEDPARAM: u32 = 1 << 4;
pub(super) const POSIX_SPAWN_SETSCHEDULER: u32 = 1 << 5;
pub(super) const POSIX_SPAWN_SETSID: u32 = 1 << 7;
pub(super) const SUPPORTED_POSIX_SPAWN_FLAGS: u32 = POSIX_SPAWN_RESETIDS
    | POSIX_SPAWN_SETPGROUP
    | POSIX_SPAWN_SETSIGDEF
    | POSIX_SPAWN_SETSIGMASK
    | POSIX_SPAWN_SETSCHEDPARAM
    | POSIX_SPAWN_SETSCHEDULER
    | (1 << 6)
    | POSIX_SPAWN_SETSID;

pub(super) fn kernel_open_flags_from_wasi(oflag: i32) -> u32 {
    let oflag = oflag as u32;
    let mut flags = if oflag & 0x1000_0000 != 0 {
        if oflag & 0x0400_0000 != 0 {
            agentos_kernel::fd_table::O_RDWR
        } else {
            agentos_kernel::fd_table::O_WRONLY
        }
    } else {
        agentos_kernel::fd_table::O_RDONLY
    };
    if oflag & 0x0000_0001 != 0 {
        flags |= agentos_kernel::fd_table::O_APPEND;
    }
    if oflag & 0x0000_0004 != 0 {
        flags |= agentos_kernel::fd_table::O_NONBLOCK;
    }
    if oflag & (1 << 12) != 0 {
        flags |= agentos_kernel::fd_table::O_CREAT;
    }
    if oflag & (2 << 12) != 0 {
        flags |= agentos_kernel::fd_table::O_DIRECTORY;
    }
    if oflag & (4 << 12) != 0 {
        flags |= agentos_kernel::fd_table::O_EXCL;
    }
    if oflag & (8 << 12) != 0 {
        flags |= agentos_kernel::fd_table::O_TRUNC;
    }
    if oflag & 0x0100_0000 != 0 {
        flags |= agentos_kernel::fd_table::O_NOFOLLOW;
    }
    flags
}

#[derive(Default)]
pub(super) struct AppliedPosixSpawnFileActions {
    fd_mappings: Vec<[u32; 2]>,
    closed_guest_fds: Vec<u32>,
}

/// JavaScript and Python issue stdio bridge calls with the POSIX fd numbers
/// 0/1/2 directly. Unlike the WASM runner, they do not consume the
/// guest-to-kernel fd mapping emitted by posix_spawn file actions. Install the
/// mapped descriptions at their canonical stdio numbers before execution
/// starts so pipes and redirections observe the same descriptors as the guest.
fn materialize_direct_runtime_stdio_mappings(
    kernel: &mut SidecarKernel,
    pid: u32,
    applied: &AppliedPosixSpawnFileActions,
) -> Result<(), SidecarError> {
    for guest_fd in 0..=2 {
        if let Some(source_fd) = applied
            .fd_mappings
            .iter()
            .find_map(|mapping| (mapping[0] == guest_fd).then_some(mapping[1]))
        {
            if source_fd != guest_fd {
                kernel
                    .fd_dup2(EXECUTION_DRIVER_NAME, pid, source_fd, guest_fd)
                    .map_err(kernel_error)?;
            }
        } else if applied.closed_guest_fds.contains(&guest_fd) {
            if let Err(error) = kernel.fd_close(EXECUTION_DRIVER_NAME, pid, guest_fd) {
                if error.code() != "EBADF" {
                    return Err(kernel_error(error));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod direct_runtime_stdio_mapping_tests {
    use super::*;
    use agentos_kernel::command_registry::CommandDriver;
    use agentos_kernel::kernel::{KernelVmConfig, SpawnOptions};
    use agentos_kernel::mount_table::MountTable;
    use agentos_kernel::permissions::Permissions;
    use agentos_kernel::vfs::MemoryFileSystem;

    fn test_kernel(name: &str) -> SidecarKernel {
        let mut config = KernelVmConfig::new(name);
        config.permissions = Permissions::allow_all();
        let mut kernel = SidecarKernel::new(MountTable::new(MemoryFileSystem::new()), config);
        kernel
            .register_driver(CommandDriver::new(EXECUTION_DRIVER_NAME, [WASM_COMMAND]))
            .unwrap();
        kernel
    }

    #[test]
    fn inherited_output_authority_survives_parent_close_and_cross_dup() {
        let mut kernel = test_kernel("vm-inherited-output-authority");
        let parent = kernel
            .spawn_process(
                WASM_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    ..SpawnOptions::default()
                },
            )
            .expect("spawn parent");

        kernel
            .fd_dup2(EXECUTION_DRIVER_NAME, parent.pid(), 2, 1)
            .expect("redirect parent stdout to stderr");
        let stderr_child = kernel
            .spawn_process(
                WASM_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    parent_pid: Some(parent.pid()),
                    ..SpawnOptions::default()
                },
            )
            .expect("spawn stderr-redirected child");
        let stderr_description = kernel
            .fd_description_identity(EXECUTION_DRIVER_NAME, stderr_child.pid(), 1)
            .expect("child stdout description")
            .0;
        let stderr_path = kernel
            .fd_path(EXECUTION_DRIVER_NAME, stderr_child.pid(), 1)
            .expect("child stdout path");
        assert_eq!(
            classify_inherited_output_stream(stderr_description, stderr_path.as_str()),
            Some(InheritedOutputStream::Stderr),
            "1>&2 must route by the inherited open description"
        );

        kernel
            .fd_close(EXECUTION_DRIVER_NAME, parent.pid(), 1)
            .expect("parent closes inherited stdout alias");
        kernel.write_file("/replacement", Vec::new()).unwrap();
        let replacement_source = kernel
            .fd_open(EXECUTION_DRIVER_NAME, parent.pid(), "/replacement", 1, None)
            .expect("parent reassigns fd 1");
        kernel
            .fd_dup2(EXECUTION_DRIVER_NAME, parent.pid(), replacement_source, 1)
            .expect("install replacement at fd 1");
        assert_ne!(
            kernel
                .fd_description_identity(EXECUTION_DRIVER_NAME, parent.pid(), 1)
                .unwrap()
                .0,
            stderr_description
        );
        assert_eq!(
            kernel
                .fd_description_identity(EXECUTION_DRIVER_NAME, stderr_child.pid(), 1)
                .unwrap()
                .0,
            stderr_description,
            "the child description remains authoritative after parent reassignment"
        );

        // Build the reverse cross-dup from a fresh process so fd 1 still owns
        // a /dev/stdout description rather than the prior stderr authority.
        let reverse_parent = kernel
            .spawn_process(
                WASM_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    ..SpawnOptions::default()
                },
            )
            .expect("spawn reverse parent");
        kernel
            .fd_dup2(EXECUTION_DRIVER_NAME, reverse_parent.pid(), 1, 2)
            .expect("redirect reverse parent stderr to stdout");
        let stdout_child = kernel
            .spawn_process(
                WASM_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    parent_pid: Some(reverse_parent.pid()),
                    ..SpawnOptions::default()
                },
            )
            .expect("spawn stdout-redirected child");
        let stdout_description = kernel
            .fd_description_identity(EXECUTION_DRIVER_NAME, stdout_child.pid(), 2)
            .unwrap()
            .0;
        let stdout_path = kernel
            .fd_path(EXECUTION_DRIVER_NAME, stdout_child.pid(), 2)
            .unwrap();
        assert_eq!(
            classify_inherited_output_stream(stdout_description, stdout_path.as_str()),
            Some(InheritedOutputStream::Stdout),
            "2>&1 must route by the inherited open description"
        );
        assert_eq!(classify_inherited_output_stream(91, "/replacement"), None);
        assert_eq!(classify_inherited_output_stream(92, "pipe:17"), None);

        stderr_child.finish(0);
        stdout_child.finish(0);
        kernel
            .waitpid_detailed_with_options(
                EXECUTION_DRIVER_NAME,
                parent.pid(),
                stderr_child.pid() as i32,
                WaitPidFlags::empty(),
            )
            .expect("wait stderr child")
            .expect("reap stderr child");
        kernel
            .waitpid_detailed_with_options(
                EXECUTION_DRIVER_NAME,
                reverse_parent.pid(),
                stdout_child.pid() as i32,
                WaitPidFlags::empty(),
            )
            .expect("wait stdout child")
            .expect("reap stdout child");
        parent.finish(0);
        reverse_parent.finish(0);
        kernel.waitpid(parent.pid()).expect("reap parent");
        kernel
            .waitpid(reverse_parent.pid())
            .expect("reap reverse parent");
    }

    #[test]
    fn wasm_exit_event_precedes_kernel_authoritative_wait_and_reap() {
        let mut kernel = test_kernel("vm-wasm-exit-before-wait");
        let parent = kernel
            .spawn_process(
                WASM_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    ..SpawnOptions::default()
                },
            )
            .expect("spawn WASM parent");
        let child = kernel
            .spawn_process(
                WASM_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    parent_pid: Some(parent.pid()),
                    ..SpawnOptions::default()
                },
            )
            .expect("spawn WASM child");

        child.finish(7);
        let bridge_exit_event = json!({ "type": "exit", "exitCode": 7 });
        assert_eq!(bridge_exit_event["exitCode"], 7);

        let waited = kernel
            .waitpid_detailed_with_options(
                EXECUTION_DRIVER_NAME,
                parent.pid(),
                child.pid() as i32,
                WaitPidFlags::WNOHANG,
            )
            .expect("kernel wait must remain valid after bridge exit delivery")
            .expect("exited WASM child must be waitable");
        assert_eq!(waited.pid, child.pid());
        assert_eq!(waited.status, 7);
        assert_eq!(waited.event, agentos_kernel::kernel::WaitPidEvent::Exited);
        assert_eq!(
            kernel
                .waitpid_detailed_with_options(
                    EXECUTION_DRIVER_NAME,
                    parent.pid(),
                    child.pid() as i32,
                    WaitPidFlags::WNOHANG,
                )
                .expect_err("successful wait must reap the child")
                .code(),
            "ECHILD"
        );
    }

    #[test]
    fn runtime_signal_exit_stays_signaled_in_kernel_wait_status() {
        let mut kernel = test_kernel("vm-runtime-signal-exit");
        let parent = kernel
            .spawn_process(
                WASM_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    ..SpawnOptions::default()
                },
            )
            .expect("spawn WASM parent");
        let child = kernel
            .spawn_process(
                WASM_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    parent_pid: Some(parent.pid()),
                    ..SpawnOptions::default()
                },
            )
            .expect("spawn WASM child");

        finish_kernel_child_from_runtime_exit(
            &child,
            128 + libc::SIGUSR1,
            Some(libc::SIGUSR1),
            false,
        );

        let waited = kernel
            .waitpid_detailed_with_options(
                EXECUTION_DRIVER_NAME,
                parent.pid(),
                child.pid() as i32,
                WaitPidFlags::WNOHANG,
            )
            .expect("kernel wait must succeed")
            .expect("signaled child must be waitable");
        assert_eq!(waited.status, 128 + libc::SIGUSR1);
        assert_eq!(waited.event, agentos_kernel::kernel::WaitPidEvent::Exited);
        assert_eq!(
            waited.termination,
            Some(agentos_kernel::process_runtime::ProcessExit::Signaled {
                signal: libc::SIGUSR1,
                core_dumped: false,
            })
        );
    }

    #[test]
    fn posix_spawn_signal_attributes_are_committed_to_the_kernel_child() {
        let mut kernel = test_kernel("vm-posix-spawn-signal-attributes");
        let parent = kernel
            .spawn_process(
                WASM_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    ..SpawnOptions::default()
                },
            )
            .unwrap();
        parent
            .signal_action(
                libc::SIGUSR1,
                Some(agentos_kernel::process_table::SignalAction {
                    disposition: agentos_kernel::process_table::SignalDisposition::Ignore,
                    ..agentos_kernel::process_table::SignalAction::DEFAULT
                }),
            )
            .unwrap();
        parent
            .sigprocmask(
                SigmaskHow::SetMask,
                SignalSet::from_signals([libc::SIGUSR2]).unwrap(),
            )
            .unwrap();

        let child = kernel
            .spawn_process(
                WASM_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    parent_pid: Some(parent.pid()),
                    ..SpawnOptions::default()
                },
            )
            .unwrap();
        let options = ProcessLaunchOptions {
            spawn_attr_flags: POSIX_SPAWN_SETSIGDEF | POSIX_SPAWN_SETSIGMASK,
            spawn_signal_defaults: vec![
                libc::SIGUSR1 as u32,
                libc::SIGKILL as u32,
                libc::SIGSTOP as u32,
            ],
            spawn_signal_mask: vec![libc::SIGTERM as u32],
            ..ProcessLaunchOptions::default()
        };
        apply_spawn_process_attributes_or_rollback(&mut kernel, &child, &options).unwrap();

        assert_eq!(
            child.signal_action(libc::SIGUSR1, None).unwrap(),
            agentos_kernel::process_table::SignalAction::DEFAULT
        );
        let mask = child
            .sigprocmask(SigmaskHow::Block, SignalSet::empty())
            .unwrap();
        assert!(mask.contains(libc::SIGTERM));
        assert!(!mask.contains(libc::SIGUSR2));
    }

    #[test]
    fn materializes_guest_fd_mappings_at_canonical_fds() {
        let mut kernel = test_kernel("vm-python-stdio-mappings");
        let process = kernel
            .spawn_process(
                WASM_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    ..SpawnOptions::default()
                },
            )
            .unwrap();
        let (read_fd, write_fd) = kernel
            .open_pipe(EXECUTION_DRIVER_NAME, process.pid())
            .unwrap();
        kernel
            .fd_write(
                EXECUTION_DRIVER_NAME,
                process.pid(),
                write_fd,
                b"python-stdin",
            )
            .unwrap();

        materialize_direct_runtime_stdio_mappings(
            &mut kernel,
            process.pid(),
            &AppliedPosixSpawnFileActions {
                fd_mappings: vec![[0, read_fd]],
                closed_guest_fds: vec![1],
            },
        )
        .unwrap();

        assert_eq!(
            kernel
                .fd_read(EXECUTION_DRIVER_NAME, process.pid(), 0, 64)
                .unwrap(),
            b"python-stdin"
        );
        assert_eq!(
            kernel
                .fd_path(EXECUTION_DRIVER_NAME, process.pid(), 1)
                .expect_err("closed stdout must remain closed")
                .code(),
            "EBADF"
        );
    }

    #[test]
    fn posix_spawn_file_actions_preserve_hidden_wasi_preopens() {
        let mut kernel = test_kernel("vm-posix-spawn-hidden-preopens");
        kernel.mkdir("/workspace", true).unwrap();
        let parent = kernel
            .spawn_process(
                WASM_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    ..SpawnOptions::default()
                },
            )
            .unwrap();
        let parent_preopens = kernel
            .initialize_wasi_preopens(EXECUTION_DRIVER_NAME, parent.pid())
            .expect("initialize parent preopens");
        assert!(!parent_preopens.is_empty());
        kernel.write_file("/redirect", Vec::new()).unwrap();
        let redirect_fd = kernel
            .fd_open(EXECUTION_DRIVER_NAME, parent.pid(), "/redirect", 1, None)
            .unwrap();
        let child = kernel
            .spawn_process(
                WASM_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    parent_pid: Some(parent.pid()),
                    ..SpawnOptions::default()
                },
            )
            .unwrap();
        let actions = [
            ProcessSpawnFileAction {
                command: 2,
                guest_fd: Some(3),
                fd: 5,
                source_fd: redirect_fd as i32,
                guest_source_fd: Some(5),
                oflag: 0,
                mode: 0,
                path: String::new(),
                close_from_guest_fds: Vec::new(),
            },
            ProcessSpawnFileAction {
                command: 1,
                guest_fd: Some(5),
                fd: redirect_fd as i32,
                source_fd: 0,
                guest_source_fd: None,
                oflag: 0,
                mode: 0,
                path: String::new(),
                close_from_guest_fds: Vec::new(),
            },
            ProcessSpawnFileAction {
                command: 6,
                guest_fd: Some(3),
                fd: 3,
                source_fd: 0,
                guest_source_fd: None,
                oflag: 0,
                mode: 0,
                path: String::new(),
                close_from_guest_fds: vec![3, 5],
            },
        ];
        apply_posix_spawn_file_actions(
            &mut kernel,
            child.pid(),
            "/",
            &[[5, redirect_fd]],
            &actions,
        )
        .expect("apply guest redirection actions");

        let child_preopens = kernel
            .wasi_preopens(EXECUTION_DRIVER_NAME, child.pid())
            .expect("read inherited child preopens");
        assert_eq!(
            child_preopens
                .iter()
                .map(|preopen| preopen.guest_path.as_str())
                .collect::<Vec<_>>(),
            parent_preopens
                .iter()
                .map(|preopen| preopen.guest_path.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn preapplied_posix_spawn_snapshot_preserves_hidden_wasi_preopens() {
        let mut kernel = test_kernel("vm-preapplied-spawn-hidden-preopens");
        kernel.mkdir("/workspace", true).unwrap();
        let parent = kernel
            .spawn_process(
                WASM_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    ..SpawnOptions::default()
                },
            )
            .unwrap();
        let parent_preopens = kernel
            .initialize_wasi_preopens(EXECUTION_DRIVER_NAME, parent.pid())
            .expect("initialize parent preopens");
        kernel.write_file("/redirect", Vec::new()).unwrap();
        let redirect_fd = kernel
            .fd_open(EXECUTION_DRIVER_NAME, parent.pid(), "/redirect", 1, None)
            .unwrap();
        let actions = [
            ProcessSpawnFileAction {
                command: 2,
                guest_fd: Some(3),
                fd: 5,
                source_fd: redirect_fd as i32,
                guest_source_fd: Some(5),
                oflag: 0,
                mode: 0,
                path: String::new(),
                close_from_guest_fds: Vec::new(),
            },
            ProcessSpawnFileAction {
                command: 1,
                guest_fd: Some(5),
                fd: redirect_fd as i32,
                source_fd: 0,
                guest_source_fd: None,
                oflag: 0,
                mode: 0,
                path: String::new(),
                close_from_guest_fds: Vec::new(),
            },
            ProcessSpawnFileAction {
                command: 6,
                guest_fd: Some(3),
                fd: 3,
                source_fd: 0,
                guest_source_fd: None,
                oflag: 0,
                mode: 0,
                path: String::new(),
                close_from_guest_fds: vec![3, 5],
            },
        ];
        let prepared = preapply_posix_spawn_file_actions(
            &mut kernel,
            parent.pid(),
            "/",
            None,
            &[[5, redirect_fd]],
            &actions,
        )
        .expect("preapply spawn file actions");
        let child = kernel
            .spawn_process(
                WASM_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    parent_pid: Some(parent.pid()),
                    ..SpawnOptions::default()
                },
            )
            .unwrap();
        install_preapplied_posix_spawn_file_actions(&mut kernel, &child, prepared)
            .expect("install preapplied spawn snapshot");

        let child_preopens = kernel
            .wasi_preopens(EXECUTION_DRIVER_NAME, child.pid())
            .expect("read restored child preopens");
        assert_eq!(
            child_preopens
                .iter()
                .map(|preopen| preopen.guest_path.as_str())
                .collect::<Vec<_>>(),
            parent_preopens
                .iter()
                .map(|preopen| preopen.guest_path.as_str())
                .collect::<Vec<_>>()
        );
    }

    fn assert_closed_stdin_canonicalization_is_idempotent(materialize_direct_runtime_first: bool) {
        let mut kernel = test_kernel("vm-closed-stdin-canonicalization");
        let process = kernel
            .spawn_process(
                WASM_COMMAND,
                Vec::new(),
                SpawnOptions {
                    requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                    ..SpawnOptions::default()
                },
            )
            .unwrap();
        let close_stdin = ProcessSpawnFileAction {
            command: 1,
            guest_fd: Some(0),
            fd: 0,
            source_fd: -1,
            guest_source_fd: None,
            oflag: 0,
            mode: 0,
            path: String::new(),
            close_from_guest_fds: Vec::new(),
        };
        let (applied, _) =
            apply_posix_spawn_file_actions(&mut kernel, process.pid(), "/", &[], &[close_stdin])
                .expect("apply close(0) file action");

        if materialize_direct_runtime_first {
            materialize_direct_runtime_stdio_mappings(&mut kernel, process.pid(), &applied)
                .expect("direct-runtime stdio materialization accepts already-closed stdin");
        }
        canonicalize_host_runtime_posix_stdin(&mut kernel, process.pid(), &applied)
            .expect("host runtime canonicalization accepts already-closed stdin");
        assert_eq!(
            kernel
                .fd_path(EXECUTION_DRIVER_NAME, process.pid(), 0)
                .expect_err("stdin must remain closed")
                .code(),
            "EBADF"
        );
    }

    #[test]
    fn canonicalization_accepts_posix_spawn_close_stdin() {
        assert_closed_stdin_canonicalization_is_idempotent(false);
    }

    #[test]
    fn direct_runtime_launch_accepts_posix_spawn_close_stdin() {
        assert_closed_stdin_canonicalization_is_idempotent(true);
    }
}

pub(super) struct PreparedPosixSpawnFd {
    fd: u32,
    fd_flags: u32,
    transfer: TransferredFd,
}

pub(super) struct PreparedPosixSpawnFileActions {
    applied: AppliedPosixSpawnFileActions,
    fds: Vec<PreparedPosixSpawnFd>,
    cwd: String,
}

pub(super) fn prepare_spawn_host_net_fds(
    kernel: &mut SidecarKernel,
    parent: &mut ActiveProcess,
    managed_descriptions: &crate::state::ManagedHostNetDescriptionRegistry,
    current_network_counts: NetworkResourceCounts,
    inherited_fds: &[ProcessSpawnHostNetworkDescriptor],
    inherited_kernel_mappings: &[[u32; 2]],
    actions: &[ProcessSpawnFileAction],
) -> Result<PreparedSpawnHostNetFds, SidecarError> {
    const LINUX_GUEST_FD_LIMIT: u32 = 1 << 20;
    if let Some(limit) = kernel.resource_limits().max_open_fds {
        if inherited_fds.len() > limit {
            return Err(SidecarError::host("EMFILE", format!("inherited host-network fd list has {} entries, exceeding limits.resources.maxOpenFds ({limit}); raise limits.resources.maxOpenFds",
                inherited_fds.len()
            )));
        }
    }

    let inherited_kernel_guest_fds = inherited_kernel_mappings
        .iter()
        .map(|mapping| mapping[0])
        .collect::<BTreeSet<_>>();
    let mut fd_states = BTreeMap::<u32, SpawnHostNetFdState>::new();
    let mut source_descriptions = BTreeMap::<SpawnHostNetSource, usize>::new();
    let mut description_metadata = Vec::<Value>::new();
    let mut description_ids = Vec::<Option<u64>>::new();
    let mut description_resources = Vec::<Option<TransferredHostNetSocket>>::new();

    for inherited in inherited_fds {
        let managed_description_id = inherited
            .description_id
            .as_deref()
            .map(|value| {
                value.parse::<u64>().map_err(|_| {
                    SidecarError::host(
                        "EINVAL",
                        "managed host-network descriptionId must be a u64 decimal string",
                    )
                })
            })
            .transpose()?;
        if inherited.guest_fd >= LINUX_GUEST_FD_LIMIT {
            return Err(SidecarError::host(
                "EBADF",
                format!(
                    "inherited host-network guest fd {} exceeds the Linux descriptor limit",
                    inherited.guest_fd
                ),
            ));
        }
        if (inherited_kernel_guest_fds.contains(&inherited.guest_fd)
            && managed_description_id.is_none())
            || fd_states.contains_key(&inherited.guest_fd)
        {
            return Err(SidecarError::host(
                "EINVAL",
                format!("duplicate inherited guest fd {}", inherited.guest_fd),
            ));
        }
        let managed_kernel_fd = if let Some(description_id) = managed_description_id {
            let kernel_fd = inherited_kernel_mappings
                .iter()
                .find(|mapping| mapping[0] == inherited.guest_fd)
                .map(|mapping| mapping[1])
                .ok_or_else(|| {
                    SidecarError::host(
                        "EBADF",
                        format!(
                            "managed host-network guest fd {} has no canonical kernel mapping",
                            inherited.guest_fd
                        ),
                    )
                })?;
            let (actual_description_id, _) = kernel
                .fd_description_identity(EXECUTION_DRIVER_NAME, parent.kernel_pid, kernel_fd)
                .map_err(kernel_error)?;
            if actual_description_id != description_id {
                return Err(SidecarError::host(
                    "EINVAL",
                    format!(
                        "managed host-network guest fd {} description identity does not match kernel fd {}",
                        inherited.guest_fd, kernel_fd
                    ),
                ));
            }
            Some(kernel_fd)
        } else {
            None
        };

        let managed_description = managed_description_id
            .map(|description_id| {
                managed_descriptions
                    .lock()
                    .map_err(|_| {
                        SidecarError::host("EIO", "managed description registry lock poisoned")
                    })?
                    .get(&description_id)
                    .cloned()
                    .ok_or_else(|| {
                        SidecarError::host("ENOTSOCK", "managed spawn description is unknown")
                    })
            })
            .transpose()?;
        let source = if let Some(description) = managed_description.as_ref() {
            match description.route_for(parent.kernel_pid).ok_or_else(|| {
                SidecarError::host("ESTALE", "managed spawn description has no parent route")
            })? {
                ManagedHostNetRoute::TcpSocket(id) | ManagedHostNetRoute::UnixSocket(id) => {
                    SpawnHostNetSource::Tcp(id.clone())
                }
                ManagedHostNetRoute::TcpListener(id)
                | ManagedHostNetRoute::UnixListener(id)
                | ManagedHostNetRoute::UnixBound { listener_id: id } => {
                    SpawnHostNetSource::TcpListener(id.clone())
                }
                ManagedHostNetRoute::UdpSocket(id) => SpawnHostNetSource::Udp(id.clone()),
                ManagedHostNetRoute::Unbound => {
                    SpawnHostNetSource::Pending(managed_description_id.expect("managed id exists"))
                }
                ManagedHostNetRoute::TcpBound { .. } => {
                    return Err(SidecarError::host(
                        "EOPNOTSUPP",
                        "forking a bound non-listening TCP socket is not yet supported",
                    ));
                }
            }
        } else {
            spawn_host_net_source(inherited)?
        };
        let description = if let Some(index) = source_descriptions.get(&source).copied() {
            let existing = description_resources[index]
                .as_ref()
                .expect("spawn host-network description resource exists");
            if managed_description_id.is_none()
                && validate_host_net_metadata(
                    &inherited.metadata,
                    existing.metadata(),
                    existing.class(),
                    "spawn host-network",
                )
                .is_err()
            {
                return Err(SidecarError::host(
                    "EINVAL",
                    String::from(
                        "aliases of one inherited host-network description disagree on metadata",
                    ),
                ));
            }
            if description_ids[index] != managed_description_id {
                return Err(SidecarError::host(
                    "EINVAL",
                    String::from(
                        "aliases of one inherited host-network resource disagree on kernel description identity",
                    ),
                ));
            }
            index
        } else {
            let (metadata, resource) = if let Some(description) = managed_description.as_ref() {
                let kernel_fd = managed_kernel_fd.expect("managed kernel fd exists");
                let nonblocking = kernel
                    .fd_stat(EXECUTION_DRIVER_NAME, parent.kernel_pid, kernel_fd)
                    .map_err(kernel_error)?
                    .flags
                    & agentos_kernel::fd_table::O_NONBLOCK
                    != 0;
                let domain = match description.domain {
                    HostSocketDomain::Inet4 => HOST_NET_AF_INET,
                    HostSocketDomain::Inet6 => HOST_NET_AF_INET6,
                    HostSocketDomain::Unix => HOST_NET_AF_UNIX,
                };
                let socket_type = match description.kind {
                    HostSocketKind::Stream => HOST_NET_SOCK_STREAM,
                    HostSocketKind::Datagram => HOST_NET_SOCK_DGRAM,
                    HostSocketKind::SeqPacket => {
                        return Err(SidecarError::host(
                            "EPROTONOSUPPORT",
                            "managed spawn does not support SOCK_SEQPACKET",
                        ));
                    }
                };
                let protocol = match (description.domain, description.kind) {
                    (HostSocketDomain::Unix, _) => 0,
                    (_, HostSocketKind::Stream) => HOST_NET_IPPROTO_TCP,
                    (_, HostSocketKind::Datagram) => HOST_NET_IPPROTO_UDP,
                    (_, HostSocketKind::SeqPacket) => {
                        unreachable!("SOCK_SEQPACKET rejected above")
                    }
                };
                let canonical = json!({
                    "domain": domain,
                    "socketType": socket_type,
                    "protocol": protocol,
                    "nonblocking": nonblocking,
                    "recvTimeoutMs": description.receive_timeout_ms,
                    "bindOptions": Value::Null,
                    "localInfo": Value::Null,
                    "localUnixAddress": if description.domain == HostSocketDomain::Unix { json!("unix-unnamed") } else { Value::Null },
                    "localReservation": Value::Null,
                    "remoteInfo": Value::Null,
                    "remoteUnixAddress": Value::Null,
                    "listening": false,
                });
                let resource = prepare_transferred_host_net_resource_with_options(
                    kernel,
                    parent,
                    &source,
                    &canonical,
                    "managed spawn host-network",
                    Some(HostNetOpenDescriptionOptions {
                        nonblocking,
                        recv_timeout_ms: description.receive_timeout_ms,
                    }),
                )?;
                (resource.metadata().as_value(), resource)
            } else {
                let resource = prepare_transferred_host_net_resource(
                    kernel,
                    parent,
                    &source,
                    &inherited.metadata,
                    "spawn host-network",
                )?;
                (resource.metadata().as_value(), resource)
            };
            let index = description_resources.len();
            source_descriptions.insert(source, index);
            description_metadata.push(metadata);
            description_ids.push(managed_description_id);
            description_resources.push(Some(resource));
            index
        };
        fd_states.insert(
            inherited.guest_fd,
            SpawnHostNetFdState {
                description,
                close_on_exec: inherited.close_on_exec,
            },
        );
    }

    let mut kernel_actions = apply_spawn_host_net_file_actions(&mut fd_states, actions)?;
    if inherited_fds
        .iter()
        .any(|inherited| inherited.description_id.is_some())
    {
        // The metadata simulation above determines which descriptions reach
        // the child, but managed descriptors already live in the canonical
        // table, so the kernel must execute every file action as well.
        kernel_actions = actions.to_vec();
    }
    fd_states.retain(|_, state| !state.close_on_exec);

    // fork/exec inheritance installs new descriptor references to the same
    // Linux open-file descriptions. maxOpenFds bounds those references; the
    // socket/connection limits continue to count each description once.
    check_spawn_host_net_resource_limit(
        kernel.resource_limits().max_sockets,
        current_network_counts.sockets,
        0,
        "EMFILE",
        "socket descriptions",
        "maxSockets",
    )?;
    check_spawn_host_net_resource_limit(
        kernel.resource_limits().max_connections,
        current_network_counts.connections,
        0,
        "EAGAIN",
        "connected socket descriptions",
        "maxConnections",
    )?;

    let mut final_guest_fds = vec![Vec::new(); description_resources.len()];
    for (guest_fd, state) in fd_states {
        final_guest_fds[state.description].push(guest_fd);
    }
    let mut descriptions = Vec::new();
    for (index, guest_fds) in final_guest_fds.into_iter().enumerate() {
        if guest_fds.is_empty() {
            continue;
        }
        descriptions.push(PreparedSpawnHostNetDescription {
            guest_fds,
            description_id: description_ids[index],
            resource: description_resources[index]
                .take()
                .expect("spawn host-network description resource exists"),
            metadata: description_metadata[index].clone(),
        });
    }
    Ok(PreparedSpawnHostNetFds {
        descriptions,
        kernel_actions,
    })
}

pub(super) fn check_spawn_host_net_resource_limit(
    limit: Option<usize>,
    current: usize,
    additional: usize,
    errno: &str,
    label: &str,
    config_name: &str,
) -> Result<(), SidecarError> {
    let Some(limit) = limit else {
        return Ok(());
    };
    let requested = current.saturating_add(additional);
    if additional > 0 && requested > limit {
        return Err(SidecarError::InvalidState(format!(
            "{errno}: inheriting {additional} host-network {label} would raise recursive VM usage from {current} to {requested}, exceeding limits.resources.{config_name} ({limit}); raise limits.resources.{config_name}"
        )));
    }
    Ok(())
}

pub(super) fn apply_spawn_host_net_file_actions(
    fd_states: &mut BTreeMap<u32, SpawnHostNetFdState>,
    actions: &[ProcessSpawnFileAction],
) -> Result<Vec<ProcessSpawnFileAction>, SidecarError> {
    let mut kernel_actions = Vec::with_capacity(actions.len());
    for action in actions {
        match action.command {
            1 => {
                let guest_fd = posix_spawn_action_guest_fd(action, "close")?;
                let removed_host_net = fd_states.remove(&guest_fd).is_some();
                if !removed_host_net || guest_fd <= 2 {
                    kernel_actions.push(action.clone());
                }
            }
            2 => {
                let guest_fd = posix_spawn_action_guest_fd(action, "dup2 target")?;
                let source_fd = posix_spawn_action_guest_source_fd(action)?;
                if let Some(mut state) = fd_states.get(&source_fd).copied() {
                    // POSIX spawn dup2 actions clear FD_CLOEXEC even for a
                    // same-fd action; direct dup2(2) remains a no-op.
                    state.close_on_exec = false;
                    if guest_fd == source_fd {
                        fd_states.insert(guest_fd, state);
                        continue;
                    }
                    fd_states.insert(guest_fd, state);
                    let mut close_target = action.clone();
                    close_target.command = 1;
                    close_target.guest_fd = Some(i32::try_from(guest_fd).map_err(|_| {
                        SidecarError::host(
                            "EBADF",
                            format!("posix_spawn dup2 target {guest_fd} exceeds i32"),
                        )
                    })?);
                    close_target.source_fd = 0;
                    close_target.guest_source_fd = None;
                    close_target.path.clear();
                    close_target.close_from_guest_fds.clear();
                    kernel_actions.push(close_target);
                } else {
                    fd_states.remove(&guest_fd);
                    kernel_actions.push(action.clone());
                }
            }
            3 => {
                let guest_fd = posix_spawn_action_guest_fd(action, "open")?;
                fd_states.remove(&guest_fd);
                kernel_actions.push(action.clone());
            }
            4 => kernel_actions.push(action.clone()),
            5 => {
                let guest_fd = posix_spawn_action_guest_fd(action, "fchdir")?;
                if fd_states.contains_key(&guest_fd) {
                    return Err(SidecarError::host(
                        "ENOTDIR",
                        format!("posix_spawn fchdir fd {guest_fd} is a socket"),
                    ));
                }
                kernel_actions.push(action.clone());
            }
            6 => {
                let low_fd = posix_spawn_action_guest_fd(action, "closefrom")?;
                fd_states.retain(|guest_fd, _| *guest_fd < low_fd);
                kernel_actions.push(action.clone());
            }
            command => {
                return Err(SidecarError::host(
                    "EINVAL",
                    format!("unknown posix_spawn file action {command}"),
                ));
            }
        }
    }
    Ok(kernel_actions)
}

pub(super) fn apply_posix_spawn_file_actions(
    kernel: &mut SidecarKernel,
    pid: u32,
    initial_cwd: &str,
    inherited_mappings: &[[u32; 2]],
    actions: &[ProcessSpawnFileAction],
) -> Result<(AppliedPosixSpawnFileActions, String), SidecarError> {
    let inherited_kernel_fds = kernel
        .fd_snapshot(EXECUTION_DRIVER_NAME, pid)
        .map_err(kernel_error)?
        .into_iter()
        .map(|entry| entry.fd)
        .collect::<BTreeSet<_>>();
    // WASI preopens are kernel-owned capability roots used only by libc's
    // tagged pathname resolver. Their kernel descriptor numbers are not
    // Linux guest descriptor numbers, so guest closefrom actions must not
    // close them merely because the internal number is above the cutoff.
    let hidden_wasi_preopen_kernel_fds = kernel
        .wasi_preopens(EXECUTION_DRIVER_NAME, pid)
        .map_err(kernel_error)?
        .into_iter()
        .map(|preopen| preopen.fd)
        .collect::<BTreeSet<_>>();
    let mut mappings = BTreeMap::new();
    let mut mapped_kernel_fds = BTreeSet::new();
    let mut closed_guest_fds = BTreeSet::new();
    let mut cwd = kernel
        .realpath_for_process(EXECUTION_DRIVER_NAME, pid, initial_cwd)
        .map(|path| normalize_path(&path))
        .map_err(kernel_error)?;
    for [guest_fd, kernel_fd] in inherited_mappings {
        // Runner-local descriptors can appear in the guest mapping without a
        // kernel entry. Kernel-backed FD_CLOEXEC descriptors remain present
        // until file actions finish, matching fork + exec ordering on Linux.
        if !inherited_kernel_fds.contains(kernel_fd) {
            continue;
        }
        if mappings.insert(*guest_fd, *kernel_fd).is_some() || !mapped_kernel_fds.insert(*kernel_fd)
        {
            return Err(SidecarError::host(
                "EINVAL",
                String::from("duplicate posix_spawn guest/kernel fd mapping"),
            ));
        }
    }

    let action_guest_fd = |action: &ProcessSpawnFileAction| {
        u32::try_from(action.guest_fd.unwrap_or(action.fd)).map_err(|_| {
            SidecarError::host(
                "EBADF",
                format!(
                    "invalid posix_spawn guest fd {}",
                    action.guest_fd.unwrap_or(action.fd)
                ),
            )
        })
    };
    for action in actions {
        match action.command {
            1 => {
                let guest_fd = action_guest_fd(action)?;
                closed_guest_fds.insert(guest_fd);
                let fd = if let Some(fd) = mappings.remove(&guest_fd) {
                    Some(fd)
                } else if action.guest_fd.is_some() && guest_fd > 2 {
                    // Runner-local descriptors do not exist in the kernel
                    // namespace; never fall through to an unrelated canonical
                    // fd with the same number.
                    None
                } else {
                    let raw_fd = u32::try_from(action.fd).map_err(|_| {
                        SidecarError::host(
                            "EBADF",
                            format!("invalid posix_spawn close fd {}", action.fd),
                        )
                    })?;
                    if mapped_kernel_fds.contains(&raw_fd) {
                        return Err(SidecarError::host("EBADF", format!("posix_spawn guest fd {guest_fd} collides with another mapped descriptor"
                        )));
                    }
                    Some(raw_fd)
                };
                if let Some(fd) = fd {
                    mapped_kernel_fds.remove(&fd);
                    kernel
                        .fd_close(EXECUTION_DRIVER_NAME, pid, fd)
                        .map_err(kernel_error)?;
                }
            }
            2 => {
                let guest_fd = action_guest_fd(action)?;
                let guest_source_fd = u32::try_from(
                    action.guest_source_fd.unwrap_or(action.source_fd),
                )
                .map_err(|_| {
                    SidecarError::host(
                        "EBADF",
                        format!(
                            "invalid posix_spawn dup2 source {}",
                            action.guest_source_fd.unwrap_or(action.source_fd)
                        ),
                    )
                })?;
                if guest_source_fd == guest_fd {
                    let fd = if let Some(fd) = mappings.get(&guest_source_fd).copied() {
                        fd
                    } else if action.guest_source_fd.is_some() && guest_source_fd > 2 {
                        return Err(SidecarError::host("EBADF", format!("posix_spawn dup2 source guest fd {guest_source_fd} is not kernel-backed"
                        )));
                    } else {
                        u32::try_from(action.source_fd).map_err(|_| {
                            SidecarError::host(
                                "EBADF",
                                format!("invalid posix_spawn dup2 source {}", action.source_fd),
                            )
                        })?
                    };
                    // POSIX spawn dup2(fd, fd) clears FD_CLOEXEC.
                    kernel
                        .fd_fcntl(
                            EXECUTION_DRIVER_NAME,
                            pid,
                            fd,
                            agentos_kernel::fd_table::F_SETFD,
                            0,
                        )
                        .map_err(kernel_error)?;
                    closed_guest_fds.remove(&guest_fd);
                    continue;
                }
                let source_fd = if let Some(fd) = mappings.get(&guest_source_fd).copied() {
                    fd
                } else if action.guest_source_fd.is_some() && guest_source_fd > 2 {
                    return Err(SidecarError::host("EBADF", format!("posix_spawn dup2 source guest fd {guest_source_fd} is not kernel-backed"
                    )));
                } else {
                    let raw_fd = u32::try_from(action.source_fd).map_err(|_| {
                        SidecarError::host(
                            "EBADF",
                            format!("invalid posix_spawn dup2 source {}", action.source_fd),
                        )
                    })?;
                    if mapped_kernel_fds.contains(&raw_fd) {
                        return Err(SidecarError::host("EBADF", format!("posix_spawn dup2 source guest fd {guest_source_fd} collides with another mapped descriptor"
                        )));
                    }
                    raw_fd
                };
                let fd = if let Some(fd) = mappings.get(&guest_fd).copied() {
                    kernel
                        .fd_dup2(EXECUTION_DRIVER_NAME, pid, source_fd, fd)
                        .map_err(kernel_error)?;
                    fd
                } else {
                    kernel
                        .fd_dup(EXECUTION_DRIVER_NAME, pid, source_fd)
                        .map_err(kernel_error)?
                };
                mappings.insert(guest_fd, fd);
                mapped_kernel_fds.insert(fd);
                closed_guest_fds.remove(&guest_fd);
            }
            3 => {
                let guest_fd = action_guest_fd(action)?;
                if let Some(fd) = mappings.remove(&guest_fd) {
                    mapped_kernel_fds.remove(&fd);
                    kernel
                        .fd_close(EXECUTION_DRIVER_NAME, pid, fd)
                        .map_err(kernel_error)?;
                }
                let action_path = resolve_posix_spawn_action_path(&cwd, &action.path);
                let opened_fd = kernel
                    .fd_open(
                        EXECUTION_DRIVER_NAME,
                        pid,
                        &action_path,
                        kernel_open_flags_from_wasi(action.oflag),
                        Some(action.mode),
                    )
                    .map_err(kernel_error)?;
                if action.oflag as u32 & (2 << 12) != 0 {
                    let stat = kernel
                        .fd_stat(EXECUTION_DRIVER_NAME, pid, opened_fd)
                        .map_err(kernel_error)?;
                    if stat.filetype != agentos_kernel::fd_table::FILETYPE_DIRECTORY {
                        kernel
                            .fd_close(EXECUTION_DRIVER_NAME, pid, opened_fd)
                            .map_err(kernel_error)?;
                        return Err(SidecarError::host(
                            "ENOTDIR",
                            format!("posix_spawn open path is not a directory: {}", action.path),
                        ));
                    }
                }
                mappings.insert(guest_fd, opened_fd);
                mapped_kernel_fds.insert(opened_fd);
                closed_guest_fds.remove(&guest_fd);
            }
            4 => {
                let action_path = resolve_posix_spawn_action_path(&cwd, &action.path);
                let stat = kernel
                    .stat_for_process(EXECUTION_DRIVER_NAME, pid, &action_path)
                    .map_err(kernel_error)?;
                if !stat.is_directory {
                    return Err(SidecarError::host(
                        "ENOTDIR",
                        format!("posix_spawn chdir path is not a directory: {}", action.path),
                    ));
                }
                cwd = kernel
                    .realpath_for_process(EXECUTION_DRIVER_NAME, pid, &action_path)
                    .map(|path| normalize_path(&path))
                    .map_err(kernel_error)?;
            }
            5 => {
                let guest_fd = action_guest_fd(action)?;
                let fd = if let Some(fd) = mappings.get(&guest_fd).copied() {
                    fd
                } else if action.guest_fd.is_some() && guest_fd > 2 {
                    return Err(SidecarError::host(
                        "EBADF",
                        format!("posix_spawn fchdir guest fd {guest_fd} is not kernel-backed"),
                    ));
                } else {
                    u32::try_from(action.fd).map_err(|_| {
                        SidecarError::host(
                            "EBADF",
                            format!("invalid posix_spawn fchdir fd {}", action.fd),
                        )
                    })?
                };
                let stat = kernel
                    .fd_stat(EXECUTION_DRIVER_NAME, pid, fd)
                    .map_err(kernel_error)?;
                if stat.filetype != agentos_kernel::fd_table::FILETYPE_DIRECTORY {
                    return Err(SidecarError::host(
                        "ENOTDIR",
                        format!("posix_spawn fchdir fd {guest_fd} is not a directory"),
                    ));
                }
                cwd = normalize_path(
                    &kernel
                        .fd_path(EXECUTION_DRIVER_NAME, pid, fd)
                        .map_err(kernel_error)?,
                );
            }
            6 => {
                let low_fd = action_guest_fd(action)?;
                if let Some(limit) = kernel.resource_limits().max_open_fds {
                    if action.close_from_guest_fds.len() > limit {
                        return Err(SidecarError::host("EMFILE", format!("posix_spawn closefrom guest fd list has {} entries, exceeding limits.resources.maxOpenFds ({limit}); raise limits.resources.maxOpenFds",
                            action.close_from_guest_fds.len()
                        )));
                    }
                }
                for guest_fd in &action.close_from_guest_fds {
                    if *guest_fd < low_fd {
                        return Err(SidecarError::host("EINVAL", format!("posix_spawn closefrom guest fd {guest_fd} is below cutoff {low_fd}"
                        )));
                    }
                    closed_guest_fds.insert(*guest_fd);
                }
                let open_kernel_fds = kernel
                    .fd_snapshot(EXECUTION_DRIVER_NAME, pid)
                    .map_err(kernel_error)?
                    .into_iter()
                    .map(|entry| entry.fd)
                    .collect::<BTreeSet<_>>();

                // File actions operate in the guest descriptor namespace.
                let mapped_fds = mappings
                    .iter()
                    .map(|(guest_fd, kernel_fd)| (*guest_fd, *kernel_fd))
                    .collect::<Vec<_>>();
                let mut to_close = BTreeMap::new();
                for (guest_fd, kernel_fd) in mapped_fds {
                    if guest_fd >= low_fd && open_kernel_fds.contains(&kernel_fd) {
                        to_close.insert(guest_fd, kernel_fd);
                    }
                }
                for kernel_fd in open_kernel_fds {
                    if !mapped_kernel_fds.contains(&kernel_fd)
                        && !hidden_wasi_preopen_kernel_fds.contains(&kernel_fd)
                        && kernel_fd >= low_fd
                    {
                        to_close.insert(kernel_fd, kernel_fd);
                    }
                }

                for (guest_fd, kernel_fd) in to_close {
                    closed_guest_fds.insert(guest_fd);
                    if mappings.remove(&guest_fd).is_some() {
                        mapped_kernel_fds.remove(&kernel_fd);
                    }
                    kernel
                        .fd_close(EXECUTION_DRIVER_NAME, pid, kernel_fd)
                        .map_err(kernel_error)?;
                }
            }
            command => {
                return Err(SidecarError::host(
                    "EINVAL",
                    format!("unknown posix_spawn file action {command}"),
                ));
            }
        }
    }
    let child_kernel_fds = kernel
        .fd_snapshot(EXECUTION_DRIVER_NAME, pid)
        .map_err(kernel_error)?
        .into_iter()
        .map(|entry| entry.fd)
        .collect::<BTreeSet<_>>();
    Ok((
        AppliedPosixSpawnFileActions {
            fd_mappings: mappings
                .into_iter()
                .filter(|(_, kernel_fd)| child_kernel_fds.contains(kernel_fd))
                .map(|(guest_fd, kernel_fd)| [guest_fd, kernel_fd])
                .collect(),
            closed_guest_fds: closed_guest_fds.into_iter().collect(),
        },
        cwd,
    ))
}

pub(super) fn resolve_posix_spawn_action_path(cwd: &str, action_path: &str) -> String {
    if action_path.starts_with('/') {
        normalize_path(action_path)
    } else {
        normalize_path(&format!("{cwd}/{action_path}"))
    }
}

pub(super) fn apply_posix_spawn_file_actions_or_rollback(
    kernel: &mut SidecarKernel,
    process: &KernelProcessHandle,
    cwd: &str,
    inherited_mappings: &[[u32; 2]],
    actions: &[ProcessSpawnFileAction],
) -> Result<AppliedPosixSpawnFileActions, SidecarError> {
    match apply_posix_spawn_file_actions(kernel, process.pid(), cwd, inherited_mappings, actions) {
        Ok((mappings, _)) => Ok(mappings),
        Err(error) => {
            process.finish(127);
            if let Err(cleanup_error) = kernel.waitpid(process.pid()) {
                eprintln!(
                    "[agentos] failed to reap rejected posix_spawn child {}: {}",
                    process.pid(),
                    cleanup_error
                );
            }
            Err(error)
        }
    }
}

pub(super) fn rollback_unregistered_spawn_child(
    kernel: &mut SidecarKernel,
    process: &KernelProcessHandle,
    execution: Option<&mut ActiveExecution>,
    context: &str,
) {
    if let Some(execution) = execution {
        if let Err(error) = execution.terminate() {
            eprintln!(
                "[agentos] failed to terminate rejected {context} runtime for PID {}: {error}",
                process.pid()
            );
        }
    }
    process.finish(127);
    if let Err(error) = kernel.waitpid(process.pid()) {
        eprintln!(
            "[agentos] failed to reap rejected {context} child {}: {error}",
            process.pid()
        );
    }
}

pub(super) fn apply_spawn_session_or_rollback(
    kernel: &mut SidecarKernel,
    process: &KernelProcessHandle,
    create_session: bool,
) -> Result<(), SidecarError> {
    if !create_session {
        return Ok(());
    }
    if let Err(error) = kernel.setsid(EXECUTION_DRIVER_NAME, process.pid()) {
        process.finish(127);
        if let Err(cleanup_error) = kernel.waitpid(process.pid()) {
            eprintln!(
                "[agentos] failed to reap rejected setsid child {}: {}",
                process.pid(),
                cleanup_error
            );
        }
        return Err(kernel_error(error));
    }
    Ok(())
}

/// Apply the POSIX spawn attributes that belong to the newly allocated kernel
/// process before any executor can run guest instructions.
pub(super) fn apply_spawn_process_attributes_or_rollback(
    kernel: &mut SidecarKernel,
    process: &KernelProcessHandle,
    options: &ProcessLaunchOptions,
) -> Result<(), SidecarError> {
    let apply = (|| {
        if options.spawn_attr_flags & POSIX_SPAWN_RESETIDS != 0 {
            let uid = kernel
                .getuid(EXECUTION_DRIVER_NAME, process.pid())
                .map_err(kernel_error)?;
            let gid = kernel
                .getgid(EXECUTION_DRIVER_NAME, process.pid())
                .map_err(kernel_error)?;
            kernel
                .seteuid(EXECUTION_DRIVER_NAME, process.pid(), uid)
                .map_err(kernel_error)?;
            kernel
                .setegid(EXECUTION_DRIVER_NAME, process.pid(), gid)
                .map_err(kernel_error)?;
        }

        if options.spawn_attr_flags & POSIX_SPAWN_SETSIGDEF != 0 {
            for signal in &options.spawn_signal_defaults {
                if matches!(*signal as i32, libc::SIGKILL | libc::SIGSTOP) {
                    continue;
                }
                process
                    .signal_action(
                        *signal as i32,
                        Some(agentos_kernel::process_table::SignalAction::DEFAULT),
                    )
                    .map_err(kernel_error)?;
            }
        }

        if options.spawn_attr_flags & POSIX_SPAWN_SETSIGMASK != 0 {
            let mask = SignalSet::from_signals(
                options
                    .spawn_signal_mask
                    .iter()
                    .map(|signal| *signal as i32),
            )
            .map_err(|error| SidecarError::host("EINVAL", error.to_string()))?;
            process
                .sigprocmask(SigmaskHow::SetMask, mask)
                .map_err(kernel_error)?;
        }
        Ok(())
    })();

    if let Err(error) = apply {
        rollback_unregistered_spawn_child(
            kernel,
            process,
            None,
            "POSIX spawn attribute application",
        );
        return Err(error);
    }
    Ok(())
}

fn canonicalize_host_runtime_posix_stdin(
    kernel: &mut SidecarKernel,
    pid: u32,
    applied: &AppliedPosixSpawnFileActions,
) -> Result<u32, SidecarError> {
    if applied.closed_guest_fds.contains(&0) {
        if let Err(error) = kernel.fd_close(EXECUTION_DRIVER_NAME, pid, 0) {
            // POSIX spawn file actions already closed the descriptor. Host
            // runtimes canonicalize stdio afterward, so an already-closed fd
            // is the desired state rather than a launch failure.
            if error.code() != "EBADF" {
                return Err(kernel_error(error));
            }
        }
        return Ok(0);
    }
    let Some(source_fd) = applied
        .fd_mappings
        .iter()
        .find_map(|mapping| (mapping[0] == 0).then_some(mapping[1]))
    else {
        return Ok(0);
    };
    kernel
        .fd_dup2(EXECUTION_DRIVER_NAME, pid, source_fd, 0)
        .map_err(kernel_error)?;
    Ok(0)
}

pub(super) fn preapply_posix_spawn_file_actions(
    kernel: &mut SidecarKernel,
    parent_pid: u32,
    cwd: &str,
    requested_pgid: Option<u32>,
    inherited_mappings: &[[u32; 2]],
    actions: &[ProcessSpawnFileAction],
) -> Result<PreparedPosixSpawnFileActions, SidecarError> {
    let process = kernel
        .spawn_process_with_process_group_preserving_cloexec(
            WASM_COMMAND,
            vec![String::from("posix-spawn-file-actions")],
            SpawnOptions {
                requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                parent_pid: Some(parent_pid),
                env: BTreeMap::new(),
                cwd: Some(cwd.to_owned()),
                ..SpawnOptions::default()
            },
            requested_pgid,
        )
        .map_err(kernel_error)?;
    let prepared = (|| {
        let (mut applied, cwd) = apply_posix_spawn_file_actions(
            kernel,
            process.pid(),
            cwd,
            inherited_mappings,
            actions,
        )?;
        kernel
            .close_process_cloexec_fds(EXECUTION_DRIVER_NAME, process.pid())
            .map_err(kernel_error)?;
        let snapshot = kernel
            .fd_snapshot(EXECUTION_DRIVER_NAME, process.pid())
            .map_err(kernel_error)?;
        let surviving_fds = snapshot
            .iter()
            .map(|entry| entry.fd)
            .collect::<BTreeSet<_>>();
        applied
            .fd_mappings
            .retain(|mapping| surviving_fds.contains(&mapping[1]));
        let mut fds = Vec::with_capacity(snapshot.len());
        for entry in snapshot {
            fds.push(PreparedPosixSpawnFd {
                fd: entry.fd,
                fd_flags: entry.fd_flags,
                transfer: kernel
                    .fd_transfer(EXECUTION_DRIVER_NAME, process.pid(), entry.fd)
                    .map_err(kernel_error)?,
            });
        }
        Ok(PreparedPosixSpawnFileActions { applied, fds, cwd })
    })();
    process.finish(if prepared.is_ok() { 0 } else { 127 });
    let reap_result = kernel.waitpid(process.pid()).map_err(kernel_error);
    match prepared {
        Ok(prepared) => {
            reap_result?;
            Ok(prepared)
        }
        Err(error) => {
            if let Err(reap_error) = reap_result {
                eprintln!(
                    "[agentos] failed to reap rejected preapplied posix_spawn child {}: {}",
                    process.pid(),
                    reap_error
                );
            }
            Err(error)
        }
    }
}

pub(super) fn install_preapplied_posix_spawn_file_actions(
    kernel: &mut SidecarKernel,
    process: &KernelProcessHandle,
    prepared: PreparedPosixSpawnFileActions,
) -> Result<AppliedPosixSpawnFileActions, SidecarError> {
    let result = (|| {
        let inherited_fds = kernel
            .fd_snapshot(EXECUTION_DRIVER_NAME, process.pid())
            .map_err(kernel_error)?;
        for entry in inherited_fds {
            kernel
                .fd_close(EXECUTION_DRIVER_NAME, process.pid(), entry.fd)
                .map_err(kernel_error)?;
        }
        for entry in &prepared.fds {
            kernel
                .fd_install_spawn_transfer_at(
                    EXECUTION_DRIVER_NAME,
                    process.pid(),
                    entry.fd,
                    entry.fd_flags,
                    &entry.transfer,
                )
                .map_err(kernel_error)?;
        }
        Ok(prepared.applied)
    })();
    if result.is_err() {
        process.finish(127);
        if let Err(cleanup_error) = kernel.waitpid(process.pid()) {
            eprintln!(
                "[agentos] failed to reap rejected preapplied posix_spawn target {}: {}",
                process.pid(),
                cleanup_error
            );
        }
    }
    result
}

#[derive(Debug)]
pub(super) struct JavascriptSpawnAttributes {
    process_group: Option<u32>,
    new_session: bool,
}

pub(super) fn javascript_spawn_attributes(
    options: &ProcessLaunchOptions,
) -> Result<JavascriptSpawnAttributes, SidecarError> {
    if options.spawn_attr_flags & !SUPPORTED_POSIX_SPAWN_FLAGS != 0 {
        return Err(SidecarError::InvalidState(format!(
            "unsupported POSIX spawn attribute flags: {:#x}",
            options.spawn_attr_flags & !SUPPORTED_POSIX_SPAWN_FLAGS
        )));
    }
    for signal in options
        .spawn_signal_defaults
        .iter()
        .chain(options.spawn_signal_mask.iter())
    {
        if !(1..=64).contains(signal) {
            return Err(SidecarError::InvalidState(format!(
                "invalid POSIX spawn signal number {signal}"
            )));
        }
    }

    let new_session = options.spawn_attr_flags & POSIX_SPAWN_SETSID != 0;
    if new_session && options.spawn_attr_flags & POSIX_SPAWN_SETPGROUP != 0 {
        return Err(SidecarError::host(
            "EPERM",
            String::from("POSIX_SPAWN_SETSID cannot be combined with POSIX_SPAWN_SETPGROUP"),
        ));
    }
    if new_session && options.detached {
        return Err(SidecarError::host(
            "EINVAL",
            String::from("POSIX_SPAWN_SETSID cannot be combined with detached child-process mode"),
        ));
    }
    if options.spawn_attr_flags & (POSIX_SPAWN_SETSCHEDPARAM | POSIX_SPAWN_SETSCHEDULER) != 0
        && options.spawn_sched_priority.unwrap_or_default() != 0
    {
        return Err(SidecarError::host(
            "EINVAL",
            String::from("SCHED_OTHER requires scheduling priority zero"),
        ));
    }
    if options.spawn_attr_flags & POSIX_SPAWN_SETSCHEDULER != 0
        && options.spawn_sched_policy.unwrap_or_default() != 0
    {
        return Err(SidecarError::host(
            "EPERM",
            String::from("requested POSIX spawn scheduler policy requires host privilege"),
        ));
    }

    if options.spawn_attr_flags & POSIX_SPAWN_SETPGROUP == 0 {
        if options.spawn_pgroup.unwrap_or(0) != 0 {
            return Err(SidecarError::InvalidState(String::from(
                "spawnPgroup requires POSIX_SPAWN_SETPGROUP",
            )));
        }
        return Ok(JavascriptSpawnAttributes {
            process_group: None,
            new_session,
        });
    }
    if options.detached {
        return Err(SidecarError::InvalidState(String::from(
            "POSIX_SPAWN_SETPGROUP cannot be combined with detached child-process mode",
        )));
    }
    let pgid = options.spawn_pgroup.unwrap_or(0);
    let process_group = u32::try_from(pgid).map_err(|_| {
        SidecarError::InvalidState(format!("invalid POSIX spawn process group {pgid}"))
    })?;
    Ok(JavascriptSpawnAttributes {
        process_group: Some(process_group),
        new_session,
    })
}

pub(super) fn apply_child_process_argv0(
    resolved: &mut ResolvedChildProcessExecution,
    argv0: Option<&str>,
) {
    let Some(argv0) = argv0 else {
        return;
    };
    if resolved.process_args.is_empty() {
        resolved.process_args.push(argv0.to_owned());
    } else {
        resolved.process_args[0] = argv0.to_owned();
    }
}

pub(super) fn validate_process_launch_request(
    request: &ProcessLaunchRequest,
    exec_replacement: bool,
) -> Result<(), SidecarError> {
    if request.command.is_empty() {
        return Err(SidecarError::host(
            "ENOENT",
            "process launch executable path is empty",
        ));
    }
    javascript_spawn_attributes(&request.options)?;
    if exec_replacement {
        if request.options.executable_fd.is_some() {
            return Err(SidecarError::host(
                "EINVAL",
                "executableFd is only valid for process.exec_fd_image_commit",
            ));
        }
        if request.options.shell || request.options.detached {
            return Err(SidecarError::host(
                "EINVAL",
                "execve does not accept shell or detached process options",
            ));
        }
    }
    Ok(())
}

pub(super) fn validate_wasm_fd_image_commit_request(
    request: &ProcessLaunchRequest,
) -> Result<(), SidecarError> {
    if !request.options.local_replacement || request.options.executable_fd.is_none() {
        return Err(SidecarError::host(
            "EINVAL",
            String::from("fd-image exec commit requires localReplacement and executableFd"),
        ));
    }
    if request.options.shell || request.options.detached || request.options.cwd.is_some() {
        return Err(SidecarError::host(
            "EINVAL",
            String::from("fexecve does not accept shell, detached, or cwd options"),
        ));
    }
    Ok(())
}

fn reserve_child_process_sync_budget(
    count_budget: &Arc<VmPendingByteBudget>,
    bytes_budget: &Arc<VmPendingByteBudget>,
    max_buffer: usize,
    input_bytes: usize,
) -> Result<(VmPendingBudgetReservation, VmPendingBudgetReservation), SidecarError> {
    let count_reservation = VmPendingBudgetReservation::try_new(Arc::clone(count_budget), 1)
        .ok_or_else(|| {
            let limit = count_budget.limit();
            let observed = count_budget.used().saturating_add(1);
            SidecarError::host_resource_limit(
                "limits.process.maxPendingChildSyncCount",
                limit,
                observed,
                format!(
                    "pending child-process sync calls ({observed}) exceed limits.process.maxPendingChildSyncCount ({limit}); raise limits.process.maxPendingChildSyncCount"
                ),
            )
        })?;

    // stdout and stderr each retain up to maxBuffer plus one overflow byte,
    // while request input remains live until the child has been spawned and
    // its stdin has been written. Reserve the conservative combined envelope
    // before starting the child so rejection has no process side effects.
    let retained_bytes = max_buffer
        .saturating_add(1)
        .saturating_mul(2)
        .saturating_add(input_bytes);
    let bytes_reservation =
        VmPendingBudgetReservation::try_new(Arc::clone(bytes_budget), retained_bytes).ok_or_else(
            || {
                let limit = bytes_budget.limit();
                let observed = bytes_budget.used().saturating_add(retained_bytes);
                SidecarError::host_resource_limit(
                    "limits.process.maxPendingChildSyncBytes",
                    limit,
                    observed,
                    format!(
                        "pending child-process sync retained bytes ({observed}) exceed limits.process.maxPendingChildSyncBytes ({limit}); raise limits.process.maxPendingChildSyncBytes"
                    ),
                )
            },
        )?;
    Ok((count_reservation, bytes_reservation))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SpawnedChildIdentity {
    child_process_id: String,
    pid: u32,
}

fn checked_child_process_sync_deadline(
    timeout_ms: Option<u64>,
) -> Result<Option<Instant>, SidecarError> {
    let Some(timeout_ms) = timeout_ms else {
        return Ok(None);
    };
    Instant::now()
        .checked_add(Duration::from_millis(timeout_ms))
        .map(Some)
        .ok_or_else(|| {
            SidecarError::Host(
                HostServiceError::new(
                    "EINVAL",
                    format!(
                        "child process timeout {timeout_ms}ms cannot be represented by the host monotonic clock"
                    ),
                )
                .with_details(json!({
                    "field": "timeout",
                    "timeoutMs": timeout_ms,
                })),
            )
        })
}

fn parse_spawned_child_identity(spawned: &Value) -> Result<SpawnedChildIdentity, SidecarError> {
    let child_process_id = spawned
        .get("childId")
        .and_then(Value::as_str)
        .filter(|child_process_id| !child_process_id.is_empty())
        .ok_or_else(|| {
            SidecarError::InvalidState(String::from(
                "child_process.spawn_sync response is missing childId",
            ))
        })?
        .to_owned();
    let pid = spawned
        .get("pid")
        .and_then(Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid != 0)
        .ok_or_else(|| {
            SidecarError::InvalidState(String::from(
                "child_process.spawn_sync response is missing a valid pid",
            ))
        })?;
    Ok(SpawnedChildIdentity {
        child_process_id,
        pid,
    })
}

#[cfg(test)]
#[derive(Debug)]
struct ChildSyncTimerAdmissionFailureHook {
    vm_id: String,
    root_process_id: String,
    parent_path: Vec<String>,
    consumed: bool,
    rolled_back_child: Option<SpawnedChildIdentity>,
}

#[cfg(test)]
static CHILD_SYNC_TIMER_ADMISSION_FAILURE_HOOK: Mutex<Option<ChildSyncTimerAdmissionFailureHook>> =
    Mutex::new(None);

#[cfg(test)]
fn child_sync_test_hook_matches(
    hook: &ChildSyncTimerAdmissionFailureHook,
    vm_id: &str,
    root_process_id: &str,
    parent_path: &[&str],
) -> bool {
    hook.vm_id == vm_id
        && hook.root_process_id == root_process_id
        && hook
            .parent_path
            .iter()
            .map(String::as_str)
            .eq(parent_path.iter().copied())
}

#[cfg(test)]
fn force_child_sync_timer_admission_failure_for_test(
    vm_id: &str,
    root_process_id: &str,
    parent_path: &[&str],
) -> bool {
    let mut hook = CHILD_SYNC_TIMER_ADMISSION_FAILURE_HOOK
        .lock()
        .unwrap_or_else(|poisoned| {
            eprintln!("ERR_AGENTOS_CHILD_SYNC_TEST_HOOK_POISONED: recovering timer-admission hook");
            poisoned.into_inner()
        });
    let Some(hook) = hook.as_mut() else {
        return false;
    };
    if hook.consumed || !child_sync_test_hook_matches(hook, vm_id, root_process_id, parent_path) {
        return false;
    }
    hook.consumed = true;
    true
}

#[cfg(test)]
fn record_child_sync_rollback_for_test(
    vm_id: &str,
    root_process_id: &str,
    parent_path: &[&str],
    identity: &SpawnedChildIdentity,
) {
    let mut hook = CHILD_SYNC_TIMER_ADMISSION_FAILURE_HOOK
        .lock()
        .unwrap_or_else(|poisoned| {
            eprintln!("ERR_AGENTOS_CHILD_SYNC_TEST_HOOK_POISONED: recovering rollback hook");
            poisoned.into_inner()
        });
    let Some(hook) = hook.as_mut() else {
        return;
    };
    if hook.consumed && child_sync_test_hook_matches(hook, vm_id, root_process_id, parent_path) {
        hook.rolled_back_child = Some(identity.clone());
    }
}

fn admit_child_process_sync_timer(
    runtime: &agentos_runtime::RuntimeContext,
    notify: Arc<tokio::sync::Notify>,
    deadline: Instant,
    vm_id: &str,
    root_process_id: &str,
    parent_path: &[&str],
) -> Result<(), SidecarError> {
    #[cfg(not(test))]
    let _ = (vm_id, root_process_id, parent_path);
    #[cfg(test)]
    {
        if force_child_sync_timer_admission_failure_for_test(vm_id, root_process_id, parent_path) {
            return Err(SidecarError::Execution(format!(
                "ERR_AGENTOS_TASK_ADMISSION_CLOSED: forced test failure for vm={vm_id} root={root_process_id}"
            )));
        }
    }
    let delay = deadline.saturating_duration_since(Instant::now());
    runtime
        .spawn(agentos_runtime::TaskClass::Timer, async move {
            tokio::time::sleep(delay).await;
            notify.notify_one();
        })
        .map(|_| ())
        .map_err(SidecarError::from)
}

#[cfg(test)]
mod child_process_sync_budget_tests {
    use super::*;

    fn budget(
        limit: usize,
        tracked: agentos_bridge::queue_tracker::TrackedLimit,
    ) -> Arc<VmPendingByteBudget> {
        VmPendingByteBudget::new(limit, tracked)
    }

    fn assert_limit(error: SidecarError, name: &str, limit: u64, observed: u64) {
        assert_eq!(error.code(), Some("ERR_AGENTOS_RESOURCE_LIMIT"));
        let SidecarError::Host(host) = error else {
            panic!("expected typed host limit error");
        };
        let details = host.details.expect("typed limit details");
        assert_eq!(details["limitName"], name);
        assert_eq!(details["limit"], limit);
        assert_eq!(details["observed"], observed);
    }

    #[test]
    fn pending_child_sync_count_and_bytes_admit_at_limit_reject_over_and_reclaim() {
        let count = budget(
            1,
            agentos_bridge::queue_tracker::TrackedLimit::PendingChildProcessSyncCount,
        );
        let bytes = budget(
            12,
            agentos_bridge::queue_tracker::TrackedLimit::PendingChildProcessSyncBytes,
        );

        // maxBuffer=4 reserves five bytes for each output stream, plus two
        // request-input bytes: exactly the configured twelve-byte envelope.
        let admitted = reserve_child_process_sync_budget(&count, &bytes, 4, 2)
            .expect("the exact count and byte limits must be admitted");
        assert_eq!(count.used(), 1);
        assert_eq!(bytes.used(), 12);

        let count_error = reserve_child_process_sync_budget(&count, &bytes, 0, 0)
            .expect_err("one more pending call must fail before spawning a child");
        assert_limit(count_error, "limits.process.maxPendingChildSyncCount", 1, 2);
        assert_eq!(count.used(), 1, "rejection must not change count usage");
        assert_eq!(bytes.used(), 12, "rejection must not change byte usage");

        drop(admitted);
        assert_eq!(count.used(), 0, "completion/removal must reclaim count");
        assert_eq!(bytes.used(), 0, "completion/removal must reclaim bytes");

        let too_small_bytes = budget(
            11,
            agentos_bridge::queue_tracker::TrackedLimit::PendingChildProcessSyncBytes,
        );
        let byte_error = reserve_child_process_sync_budget(&count, &too_small_bytes, 4, 2)
            .expect_err("an over-limit byte envelope must fail before spawning a child");
        assert_limit(
            byte_error,
            "limits.process.maxPendingChildSyncBytes",
            11,
            12,
        );
        assert_eq!(
            count.used(),
            0,
            "byte rejection must roll back its count reservation"
        );
        assert_eq!(too_small_bytes.used(), 0);
    }

    #[test]
    fn child_sync_deadline_handles_u64_max_without_panicking() {
        // Linux can represent this deadline even though some host Instant
        // implementations cannot. Either result is valid; the contract is
        // that checked conversion never panics and any rejection is typed.
        if let Err(error) = checked_child_process_sync_deadline(Some(u64::MAX)) {
            assert_eq!(error.code(), Some("EINVAL"));
            let SidecarError::Host(host) = error else {
                panic!("unrepresentable timeout must be a typed host error");
            };
            assert_eq!(
                host.details.expect("timeout details")["timeoutMs"],
                u64::MAX
            );
        }
        assert!(checked_child_process_sync_deadline(None)
            .expect("an absent timeout is valid")
            .is_none());
        assert!(checked_child_process_sync_deadline(Some(0))
            .expect("a zero timeout is valid")
            .is_some());
    }

    #[test]
    fn spawned_child_identity_requires_nonempty_id_and_nonzero_u32_pid() {
        assert!(parse_spawned_child_identity(&json!({ "pid": 1 })).is_err());
        assert!(parse_spawned_child_identity(&json!({ "childId": "", "pid": 1 })).is_err());
        assert!(parse_spawned_child_identity(&json!({ "childId": "child-1", "pid": 0 })).is_err());
        assert!(parse_spawned_child_identity(&json!({
            "childId": "child-1",
            "pid": u64::from(u32::MAX) + 1,
        }))
        .is_err());
        assert_eq!(
            parse_spawned_child_identity(&json!({ "childId": "child-1", "pid": 42 }))
                .expect("valid child identity"),
            SpawnedChildIdentity {
                child_process_id: String::from("child-1"),
                pid: 42,
            }
        );
    }
}

impl<B> NativeSidecar<B>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    fn child_process_ids_at_path(
        &self,
        vm_id: &str,
        root_process_id: &str,
        parent_path: &[&str],
    ) -> Result<BTreeSet<String>, SidecarError> {
        let vm = self.vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
        let root = vm
            .active_processes
            .get(root_process_id)
            .ok_or_else(|| missing_process_error(vm_id, root_process_id))?;
        let parent = Self::active_process_by_path(root, parent_path).ok_or_else(|| {
            SidecarError::InvalidState(format!(
                "unknown child process path during spawnSync admission: {}",
                parent_path.join("/")
            ))
        })?;
        Ok(parent.child_processes.keys().cloned().collect())
    }

    fn rollback_registered_child_process_sync(
        &mut self,
        vm_id: &str,
        root_process_id: &str,
        parent_path: &[&str],
        prior_child_ids: &BTreeSet<String>,
        hinted_child_id: Option<&str>,
        hinted_pid: Option<u32>,
        context: &str,
    ) {
        let bridge = self.bridge.clone();
        let Some(vm) = self.vms.get_mut(vm_id) else {
            eprintln!(
                "ERR_AGENTOS_CHILD_SYNC_ROLLBACK: {context}: VM {vm_id} disappeared before rollback"
            );
            return;
        };
        let identity = {
            let Some(root) = vm.active_processes.get(root_process_id) else {
                eprintln!(
                    "ERR_AGENTOS_CHILD_SYNC_ROLLBACK: {context}: root process {root_process_id} disappeared before rollback"
                );
                return;
            };
            let Some(parent) = Self::active_process_by_path(root, parent_path) else {
                eprintln!(
                    "ERR_AGENTOS_CHILD_SYNC_ROLLBACK: {context}: parent path {} disappeared before rollback",
                    parent_path.join("/")
                );
                return;
            };
            let new_children = parent
                .child_processes
                .iter()
                .filter(|(child_id, _)| !prior_child_ids.contains(*child_id))
                .collect::<Vec<_>>();
            let selected = if new_children.len() == 1 {
                new_children.first().copied()
            } else {
                let matching = new_children
                    .iter()
                    .copied()
                    .filter(|(child_id, child)| {
                        hinted_child_id.is_none_or(|hint| hint == child_id.as_str())
                            && hinted_pid.is_none_or(|hint| hint == child.kernel_pid)
                    })
                    .collect::<Vec<_>>();
                (matching.len() == 1).then(|| matching[0])
            };
            let Some((child_process_id, child)) = selected else {
                eprintln!(
                    "ERR_AGENTOS_CHILD_SYNC_ROLLBACK: {context}: could not identify exactly one newly registered child under {root_process_id}/{} (childId={hinted_child_id:?}, pid={hinted_pid:?}, candidates={})",
                    parent_path.join("/"),
                    new_children.len()
                );
                return;
            };
            SpawnedChildIdentity {
                child_process_id: child_process_id.clone(),
                pid: child.kernel_pid,
            }
        };

        #[cfg(test)]
        record_child_sync_rollback_for_test(vm_id, root_process_id, parent_path, &identity);

        let terminating_kernel_pids = {
            let Some(root) = vm.active_processes.get(root_process_id) else {
                return;
            };
            let Some(parent) = Self::active_process_by_path(root, parent_path) else {
                return;
            };
            let Some(child) = parent.child_processes.get(&identity.child_process_id) else {
                return;
            };
            Self::terminating_process_tree_kernel_pids(child)
        };
        for kernel_pid in terminating_kernel_pids {
            if let Err(error) = retire_managed_process_routes(&bridge, vm_id, vm, kernel_pid) {
                eprintln!(
                    "ERR_AGENTOS_CHILD_SYNC_ROLLBACK_ROUTE: {context}: failed to retire managed routes for PID {kernel_pid}: {error}"
                );
            }
        }

        let mut child = {
            let Some(root) = vm.active_processes.get_mut(root_process_id) else {
                return;
            };
            let Some(parent) = Self::active_process_by_path_mut(root, parent_path) else {
                return;
            };
            if parent
                .pending_child_process_sync
                .get(&identity.child_process_id)
                .is_some_and(|pending| pending.pid == identity.pid)
            {
                parent
                    .pending_child_process_sync
                    .remove(&identity.child_process_id);
            }
            let Some(child) = parent.child_processes.remove(&identity.child_process_id) else {
                eprintln!(
                    "ERR_AGENTOS_CHILD_SYNC_ROLLBACK: {context}: child {} disappeared during rollback",
                    identity.child_process_id
                );
                return;
            };
            child
        };

        if let Err(error) = release_inherited_child_raw_mode(&mut vm.kernel, &child) {
            eprintln!(
                "ERR_AGENTOS_CHILD_SYNC_ROLLBACK_TTY: {context}: failed to release child {} raw mode: {error}",
                identity.child_process_id
            );
        }
        let kernel_readiness = Arc::clone(&vm.kernel_socket_readiness);
        let unix_address_registry = Arc::clone(&vm.unix_address_registry);
        terminate_child_process_tree(
            &mut vm.kernel,
            &mut child,
            &kernel_readiness,
            &unix_address_registry,
        );
        if let Err(error) = child.execution.terminate() {
            eprintln!(
                "ERR_AGENTOS_CHILD_SYNC_ROLLBACK_EXECUTOR: {context}: failed to terminate child {} runtime: {error}",
                identity.child_process_id
            );
        }
        child.kernel_handle.finish(127);
        if let Err(error) = vm.kernel.wait_and_reap(child.kernel_pid) {
            eprintln!(
                "ERR_AGENTOS_CHILD_SYNC_ROLLBACK_REAP: {context}: failed to reap child {} PID {}: {error}",
                identity.child_process_id, child.kernel_pid
            );
        }
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn arm_child_sync_timer_admission_failure_for_test(
        vm_id: &str,
        root_process_id: &str,
        parent_path: &[&str],
    ) {
        let mut hook = CHILD_SYNC_TIMER_ADMISSION_FAILURE_HOOK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(
            hook.is_none(),
            "child sync timer-admission hook already armed"
        );
        *hook = Some(ChildSyncTimerAdmissionFailureHook {
            vm_id: vm_id.to_owned(),
            root_process_id: root_process_id.to_owned(),
            parent_path: parent_path.iter().map(|part| (*part).to_owned()).collect(),
            consumed: false,
            rolled_back_child: None,
        });
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn take_child_sync_rollback_for_test() -> Option<(String, u32)> {
        CHILD_SYNC_TIMER_ADMISSION_FAILURE_HOOK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_mut()
            .and_then(|hook| hook.rolled_back_child.take())
            .map(|identity| (identity.child_process_id, identity.pid))
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn clear_child_sync_timer_admission_failure_for_test() {
        *CHILD_SYNC_TIMER_ADMISSION_FAILURE_HOOK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }

    pub(crate) async fn pump_child_process_events(
        &mut self,
        vm_id: &str,
    ) -> Result<bool, SidecarError> {
        let mut emitted_any = false;
        let root_process_ids = self
            .vms
            .get(vm_id)
            .map(|vm| vm.active_processes.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        let mut child_candidates = Vec::new();

        for process_id in root_process_ids {
            if self
                .vms
                .get(vm_id)
                .is_some_and(|vm| vm.detached_child_processes.contains(&process_id))
            {
                continue;
            }
            let mut child_paths = Vec::new();
            if let Some(root) = self
                .vms
                .get(vm_id)
                .and_then(|vm| vm.active_processes.get(&process_id))
            {
                Self::collect_attached_child_paths(root, &mut Vec::new(), &mut child_paths);
            }
            child_candidates.extend(
                child_paths
                    .into_iter()
                    .map(|child_path| (process_id.clone(), child_path)),
            );
        }

        if child_candidates.is_empty() {
            return Ok(emitted_any);
        }
        let start = self
            .vms
            .get(vm_id)
            .map(|vm| vm.attached_child_event_cursor % child_candidates.len())
            .unwrap_or_default();
        child_candidates.rotate_left(start);
        if let Some(vm) = self.vms.get_mut(vm_id) {
            vm.attached_child_event_cursor = (start + 1) % child_candidates.len();
        }

        let vm_work_limit = self.config.runtime.fairness.vm_quantum_operations;
        let child_work_limit = self.config.runtime.fairness.capability_quantum_operations;
        let mut work = 0usize;
        let mut child_work = vec![0usize; child_candidates.len()];
        let mut yielded = false;
        let mut delivery_backpressured = false;

        loop {
            let mut emitted_this_round = false;
            for (candidate_index, (process_id, child_path)) in child_candidates.iter().enumerate() {
                if work >= vm_work_limit {
                    yielded = true;
                    break;
                }
                if child_work[candidate_index] >= child_work_limit {
                    yielded = true;
                    continue;
                }

                let Some(child_process_id) = child_path.last().cloned() else {
                    continue;
                };
                let parent_path = child_path[..child_path.len() - 1]
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>();

                // Deadline and capacity wakes must service the child's parked
                // synchronous RPC even when a standalone WASM parent owns
                // output delivery through child_process.poll.
                self.recheck_child_deferred_kernel_wait_rpc(
                    vm_id,
                    process_id,
                    &parent_path,
                    &child_process_id,
                )?;
                self.service_descendant_guest_wait(
                    vm_id,
                    process_id,
                    child_path
                        .iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                        .as_slice(),
                    None,
                )?;
                self.service_descendant_kernel_read(
                    vm_id,
                    process_id,
                    child_path
                        .iter()
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                        .as_slice(),
                    None,
                )?;

                self.expire_child_process_sync_if_needed(
                    vm_id,
                    process_id,
                    &parent_path,
                    &child_process_id,
                )?;

                let guest_owns_child_output = self
                    .vms
                    .get(vm_id)
                    .and_then(|vm| vm.active_processes.get(process_id))
                    .and_then(|root| Self::active_process_by_path(root, &parent_path))
                    .is_some_and(|parent| {
                        parent.execution.descendant_output_ownership()
                            == DescendantOutputOwnership::GuestDescriptors
                            && parent
                                .child_processes
                                .get(&child_process_id)
                                .is_some_and(|child| child.child_process_bridge_owns_output)
                            && !parent
                                .pending_child_process_sync
                                .contains_key(&child_process_id)
                    });
                if guest_owns_child_output {
                    // Standalone WASM consumes this child's output and exit
                    // through child_process.poll. The proactive bridge pump
                    // still services controls and deferred kernel waits above,
                    // but must not steal or translate the pull-owned event.
                    continue;
                }

                let event = match self
                    .poll_descendant_process(vm_id, process_id, &parent_path, &child_process_id, 0)
                    .await
                {
                    Ok(event) => event,
                    Err(error) if is_javascript_child_process_gone_error(&error) => continue,
                    Err(error) => return Err(error),
                };
                if event.is_null() {
                    continue;
                }
                if !self.route_child_process_bridge_event(
                    vm_id,
                    process_id,
                    &parent_path,
                    &child_process_id,
                    event,
                )? {
                    yielded = true;
                    delivery_backpressured = true;
                    break;
                }
                emitted_any = true;
                emitted_this_round = true;
                work += 1;
                child_work[candidate_index] += 1;
            }
            if yielded || !emitted_this_round {
                break;
            }
        }

        if delivery_backpressured {
            // The parent V8 lane is bounded and deliberately nonblocking: a
            // blocking send here can deadlock when that isolate is waiting on
            // a synchronous sidecar RPC. Retry after a short gap so the
            // isolate can drain its lane and the stdio loop can continue
            // servicing control requests. An immediate self-notification
            // hot-loops this pump and can starve session/new for seconds.
            let notify = Arc::clone(&self.process_event_notify);
            let runtime = self
                .vms
                .get(vm_id)
                .ok_or_else(|| SidecarError::InvalidState(format!("unknown VM {vm_id}")))?
                .runtime_context
                .clone();
            runtime
                .spawn(agentos_runtime::TaskClass::Timer, async move {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                    notify.notify_one();
                })
                .map_err(SidecarError::from)?;
        } else if yielded {
            self.process_event_notify.notify_one();
        }
        Ok(emitted_any)
    }

    fn expire_child_process_sync_if_needed(
        &mut self,
        vm_id: &str,
        process_id: &str,
        parent_path: &[&str],
        child_process_id: &str,
    ) -> Result<(), SidecarError> {
        let signal = {
            let Some(vm) = self.vms.get_mut(vm_id) else {
                return Ok(());
            };
            let Some(root) = vm.active_processes.get_mut(process_id) else {
                return Ok(());
            };
            let Some(parent) = Self::active_process_by_path_mut(root, parent_path) else {
                return Ok(());
            };
            let Some(pending) = parent.pending_child_process_sync.get_mut(child_process_id) else {
                return Ok(());
            };
            if pending.kill_sent
                || pending
                    .deadline
                    .is_none_or(|deadline| Instant::now() < deadline)
            {
                None
            } else {
                pending.kill_sent = true;
                pending.timed_out = true;
                Some(pending.timeout_signal.clone())
            }
        };
        if let Some(signal) = signal {
            self.kill_descendant_javascript_child_process(
                vm_id,
                process_id,
                parent_path,
                child_process_id,
                &signal,
            )?;
        }
        Ok(())
    }

    fn route_child_process_bridge_event(
        &mut self,
        vm_id: &str,
        process_id: &str,
        parent_path: &[&str],
        child_process_id: &str,
        event: Value,
    ) -> Result<bool, SidecarError> {
        let event_type = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let chunk = match event_type {
            "stdout" | "stderr" => Some(javascript_sync_rpc_bytes_arg(
                &[event.get("data").cloned().unwrap_or(Value::Null)],
                0,
                "child process event data",
            )?),
            _ => None,
        };
        let mut kill_for_buffer = false;
        let mut direct_delivery = None;
        let completion = {
            let Some(vm) = self.vms.get_mut(vm_id) else {
                return Ok(true);
            };
            let Some(root) = vm.active_processes.get_mut(process_id) else {
                return Ok(true);
            };
            let Some(parent) = Self::active_process_by_path_mut(root, parent_path) else {
                return Ok(true);
            };
            if let Some(pending) = parent.pending_child_process_sync.get_mut(child_process_id) {
                match event_type {
                    "stdout" | "stderr" => {
                        let output = if event_type == "stdout" {
                            &mut pending.stdout
                        } else {
                            &mut pending.stderr
                        };
                        let remaining = pending
                            .max_buffer
                            .saturating_add(1)
                            .saturating_sub(output.len());
                        if let Some(chunk) = chunk.as_deref() {
                            output.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
                        }
                        if output.len() > pending.max_buffer && !pending.kill_sent {
                            pending.max_buffer_exceeded = true;
                            pending.kill_sent = true;
                            kill_for_buffer = true;
                        }
                        None
                    }
                    "exit" => parent
                        .pending_child_process_sync
                        .remove(child_process_id)
                        .map(|pending| {
                            let exit_code = event
                                .get("exitCode")
                                .and_then(Value::as_i64)
                                .map(|value| value as i32)
                                .unwrap_or(1);
                            (pending, exit_code)
                        }),
                    _ => None,
                }
            } else {
                let payload = match event_type {
                    "stdout" => json!({
                        "sessionId": child_process_id,
                        "dataBase64": base64::engine::general_purpose::STANDARD.encode(
                            chunk.as_deref().unwrap_or_default(),
                        ),
                    }),
                    "stderr" => json!({
                        "sessionId": child_process_id,
                        "dataBase64": base64::engine::general_purpose::STANDARD.encode(
                            chunk.as_deref().unwrap_or_default(),
                        ),
                    }),
                    "exit" => json!({
                        "sessionId": child_process_id,
                        "code": event.get("exitCode").and_then(Value::as_i64).unwrap_or(1),
                        "signal": event.get("signal").cloned().unwrap_or(Value::Null),
                    }),
                    _ => return Ok(true),
                };
                direct_delivery = Some(parent.execution.send_javascript_stream_event(
                    match event_type {
                        "stdout" => "child_stdout",
                        "stderr" => "child_stderr",
                        _ => "child_exit",
                    },
                    payload,
                ));
                None
            }
        };

        if let Some(delivery) = direct_delivery {
            match delivery {
                Ok(()) => return Ok(true),
                Err(SidecarError::Execution(message))
                    if message.contains("ERR_AGENTOS_SESSION_COMMAND_LIMIT") =>
                {
                    let retry_event = match event_type {
                        "stdout" => ActiveExecutionEvent::Stdout(chunk.unwrap_or_default()),
                        "stderr" => ActiveExecutionEvent::Stderr(chunk.unwrap_or_default()),
                        "exit" => ActiveExecutionEvent::Exited(
                            event
                                .get("exitCode")
                                .and_then(Value::as_i64)
                                .map(|value| value as i32)
                                .unwrap_or(1),
                        ),
                        _ => return Ok(true),
                    };
                    let Some(child) = self
                        .vms
                        .get_mut(vm_id)
                        .and_then(|vm| vm.active_processes.get_mut(process_id))
                        .and_then(|root| Self::active_process_by_path_mut(root, parent_path))
                        .and_then(|parent| parent.child_processes.get_mut(child_process_id))
                    else {
                        return Ok(true);
                    };
                    child.requeue_pending_execution_event(PolledExecutionEvent::unreserved(
                        retry_event,
                    ))?;
                    return Ok(false);
                }
                Err(error) => return Err(error),
            }
        }

        if kill_for_buffer {
            self.kill_descendant_javascript_child_process(
                vm_id,
                process_id,
                parent_path,
                child_process_id,
                "SIGTERM",
            )?;
        }

        if let Some((pending, exit_code)) = completion {
            let result = json!({
                "pid": pending.pid,
                "stdout": String::from_utf8_lossy(&pending.stdout),
                "stderr": String::from_utf8_lossy(&pending.stderr),
                "code": exit_code,
                "signal": if pending.timed_out {
                    Value::String(pending.timeout_signal.clone())
                } else {
                    Value::Null
                },
                "timedOut": pending.timed_out,
                "maxBufferExceeded": pending.max_buffer_exceeded,
            });
            match pending.completion {
                PendingChildProcessSyncCompletion::Javascript(respond_to) => {
                    if respond_to.send(Ok(result)).is_err() {
                        eprintln!(
                            "ERR_AGENTOS_CHILD_PROCESS_SYNC_CANCELLED: spawnSync caller stopped waiting before child exit"
                        );
                    }
                }
                PendingChildProcessSyncCompletion::Direct(reply) => reply
                    .succeed_json(json!({
                        "pid": pending.pid,
                        "stdout": String::from_utf8_lossy(&pending.stdout),
                        "stderr": String::from_utf8_lossy(&pending.stderr),
                        "code": exit_code,
                        "signal": if pending.timed_out {
                            Value::String(pending.timeout_signal)
                        } else {
                            Value::Null
                        },
                        "timedOut": pending.timed_out,
                        "maxBufferExceeded": pending.max_buffer_exceeded,
                    }))
                    .map_err(SidecarError::from)?,
            }
        }
        Ok(true)
    }

    pub(super) async fn pump_detached_child_process_events(
        &mut self,
        vm_id: &str,
    ) -> Result<bool, SidecarError> {
        let mut detached_process_ids = self
            .vms
            .get(vm_id)
            .map(|vm| {
                vm.detached_child_processes
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if detached_process_ids.is_empty() {
            return Ok(false);
        }
        let start = self
            .vms
            .get(vm_id)
            .map(|vm| vm.detached_child_event_cursor % detached_process_ids.len())
            .unwrap_or_default();
        detached_process_ids.rotate_left(start);
        if let Some(vm) = self.vms.get_mut(vm_id) {
            vm.detached_child_event_cursor = (start + 1) % detached_process_ids.len();
        }
        let vm_work_limit = self.config.runtime.fairness.vm_quantum_operations;
        let child_work_limit = self.config.runtime.fairness.capability_quantum_operations;
        let mut emitted_any = false;
        let mut work = 0usize;
        let mut yielded = false;
        for detached_process_id in detached_process_ids {
            if work >= vm_work_limit {
                yielded = true;
                break;
            }
            let mut detached_work = 0usize;
            let Some((root_process_id, child_path)) = self
                .vms
                .get(vm_id)
                .and_then(|vm| Self::resolve_detached_child_process_path(vm, &detached_process_id))
            else {
                if let Some(vm) = self.vms.get_mut(vm_id) {
                    vm.detached_child_processes.remove(&detached_process_id);
                }
                continue;
            };
            if child_path.is_empty() {
                loop {
                    if work >= vm_work_limit || detached_work >= child_work_limit {
                        yielded = true;
                        break;
                    }
                    enum ProcessPollResult {
                        Event(Box<Option<PolledExecutionEvent>>),
                        RecoverClosedChannel,
                    }
                    let poll_result = {
                        let Some(vm) = self.vms.get_mut(vm_id) else {
                            break;
                        };
                        let Some(process) = vm.active_processes.get_mut(&root_process_id) else {
                            break;
                        };
                        if let Some(event) = process.lease_pending_execution_event() {
                            ProcessPollResult::Event(Box::new(Some(event)))
                        } else {
                            match process.poll_execution_event(Duration::ZERO).await {
                                Ok(event) => ProcessPollResult::Event(Box::new(event)),
                                Err(SidecarError::ExecutionEventChannelClosed { .. }) => {
                                    ProcessPollResult::RecoverClosedChannel
                                }
                                Err(error) => return Err(error),
                            }
                        }
                    };
                    let event = match poll_result {
                        ProcessPollResult::Event(event) => *event,
                        ProcessPollResult::RecoverClosedChannel => self
                            .recover_closed_root_runtime_process_event(vm_id, &root_process_id)?
                            .map(PolledExecutionEvent::unreserved),
                    };
                    let Some(event) = event else {
                        break;
                    };
                    work += 1;
                    detached_work += 1;
                    if matches!(event.event(), ActiveExecutionEvent::Exited(_)) {
                        record_execute_response_to_exit_milestone(
                            "execute_response_to_detached_exit_event_polled",
                            vm_id,
                            &detached_process_id,
                        );
                    }
                    let Some((connection_id, session_id)) = self
                        .vms
                        .get(vm_id)
                        .map(|vm| (vm.connection_id.clone(), vm.session_id.clone()))
                    else {
                        break;
                    };
                    let PolledExecutionEvent { event, reservation } = event;
                    match event {
                        ActiveExecutionEvent::Common(ExecutionEvent::HostCall {
                            operation,
                            reply,
                        }) => {
                            drop(reservation);
                            let Some((operation, reply)) = dispatch_context_host_operation(
                                self,
                                vm_id,
                                &root_process_id,
                                operation,
                                reply,
                            )
                            .await?
                            else {
                                continue;
                            };
                            let Some(vm) = self.vms.get_mut(vm_id) else {
                                break;
                            };
                            let generation = vm.generation;
                            let (kernel, active_processes) =
                                (&mut vm.kernel, &mut vm.active_processes);
                            let Some(process) = active_processes.get_mut(&detached_process_id)
                            else {
                                break;
                            };
                            let effects = dispatch_host_operation(
                                generation, kernel, process, operation, reply,
                            )?;
                            if effects.may_make_fd_readable {
                                Self::wake_ready_deferred_fd_reads(vm)?;
                            }
                            if effects.may_make_fd_writable {
                                Self::wake_ready_deferred_fd_writes(vm)?;
                            }
                        }
                        ActiveExecutionEvent::Common(other) => {
                            drop(reservation);
                            return Err(SidecarError::InvalidState(format!(
                                "unsupported common detached-child event: {other:?}"
                            )));
                        }
                        ActiveExecutionEvent::Stdout(chunk) => {
                            let envelope = ProcessEventEnvelope {
                                connection_id,
                                session_id,
                                vm_id: vm_id.to_owned(),
                                process_id: detached_process_id.clone(),
                                event: ActiveExecutionEvent::Stdout(chunk),
                            };
                            if let Err(error) = self.check_pending_process_event_capacity(&envelope)
                            {
                                if let Some(process) = self
                                    .vms
                                    .get_mut(vm_id)
                                    .and_then(|vm| vm.active_processes.get_mut(&root_process_id))
                                {
                                    process.requeue_pending_execution_event(
                                        PolledExecutionEvent {
                                            event: envelope.event,
                                            reservation,
                                        },
                                    )?;
                                }
                                return Err(error);
                            }
                            self.queue_pending_process_event(envelope)?;
                            drop(reservation);
                            emitted_any = true;
                        }
                        ActiveExecutionEvent::Stderr(chunk) => {
                            let envelope = ProcessEventEnvelope {
                                connection_id,
                                session_id,
                                vm_id: vm_id.to_owned(),
                                process_id: detached_process_id.clone(),
                                event: ActiveExecutionEvent::Stderr(chunk),
                            };
                            if let Err(error) = self.check_pending_process_event_capacity(&envelope)
                            {
                                if let Some(process) = self
                                    .vms
                                    .get_mut(vm_id)
                                    .and_then(|vm| vm.active_processes.get_mut(&root_process_id))
                                {
                                    process.requeue_pending_execution_event(
                                        PolledExecutionEvent {
                                            event: envelope.event,
                                            reservation,
                                        },
                                    )?;
                                }
                                return Err(error);
                            }
                            self.queue_pending_process_event(envelope)?;
                            drop(reservation);
                            emitted_any = true;
                        }
                        ActiveExecutionEvent::Exited(exit_code) => {
                            let envelope = ProcessEventEnvelope {
                                connection_id,
                                session_id,
                                vm_id: vm_id.to_owned(),
                                process_id: detached_process_id.clone(),
                                event: ActiveExecutionEvent::Exited(exit_code),
                            };
                            if let Err(error) = self.check_pending_process_event_capacity(&envelope)
                            {
                                if let Some(process) = self
                                    .vms
                                    .get_mut(vm_id)
                                    .and_then(|vm| vm.active_processes.get_mut(&root_process_id))
                                {
                                    process.requeue_pending_execution_event(
                                        PolledExecutionEvent {
                                            event: envelope.event,
                                            reservation,
                                        },
                                    )?;
                                }
                                return Err(error);
                            }
                            if let Some(vm) = self.vms.get_mut(vm_id) {
                                vm.detached_child_processes.remove(&detached_process_id);
                            }
                            self.queue_pending_process_event(envelope)?;
                            drop(reservation);
                            emitted_any = true;
                            break;
                        }
                        ActiveExecutionEvent::HostRpcRequest(request) => {
                            drop(reservation);
                            self.handle_javascript_sync_rpc_request(
                                vm_id,
                                &root_process_id,
                                request,
                            )
                            .await?;
                        }
                        ActiveExecutionEvent::HostCallCompletion(completion) => {
                            drop(reservation);
                            self.handle_host_call_completion(vm_id, &root_process_id, completion)?;
                        }
                        ActiveExecutionEvent::ManagedStreamReadRecheck(pending) => {
                            drop(reservation);
                            pending
                                .reply
                                .fail(HostServiceError::new(
                                    "ESTALE",
                                    "stream read re-entry targeted detached child routing",
                                ))
                                .map_err(SidecarError::from)?;
                        }
                        ActiveExecutionEvent::ManagedUdpPollRecheck(pending) => {
                            drop(reservation);
                            pending
                                .reply
                                .fail(HostServiceError::new(
                                    "ESTALE",
                                    "UDP poll re-entry targeted detached child routing",
                                ))
                                .map_err(SidecarError::from)?;
                        }
                        ActiveExecutionEvent::SignalState {
                            signal,
                            registration,
                        } => {
                            drop(reservation);
                            if let Some(process) = self
                                .vms
                                .get(vm_id)
                                .and_then(|vm| vm.active_processes.get(&root_process_id))
                            {
                                apply_kernel_signal_registration(process, signal, &registration)?;
                            }
                        }
                    }
                }
                continue;
            }

            let parent_path = child_path[..child_path.len() - 1]
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>();
            let child_process_id = child_path.last().expect("child path cannot be empty");

            loop {
                if work >= vm_work_limit || detached_work >= child_work_limit {
                    yielded = true;
                    break;
                }
                let event = match self
                    .poll_descendant_process(
                        vm_id,
                        &root_process_id,
                        &parent_path,
                        child_process_id,
                        0,
                    )
                    .await
                {
                    Ok(event) => event,
                    Err(error) if is_javascript_child_process_gone_error(&error) => {
                        if let Some(vm) = self.vms.get_mut(vm_id) {
                            vm.detached_child_processes.remove(&detached_process_id);
                        }
                        break;
                    }
                    Err(error) => return Err(error),
                };

                let Some(event_type) = event.get("type").and_then(Value::as_str) else {
                    break;
                };
                work += 1;
                detached_work += 1;
                let Some((connection_id, session_id)) = self
                    .vms
                    .get(vm_id)
                    .map(|vm| (vm.connection_id.clone(), vm.session_id.clone()))
                else {
                    break;
                };

                let envelope = match event_type {
                    "stdout" => Some(ProcessEventEnvelope {
                        connection_id: connection_id.clone(),
                        session_id: session_id.clone(),
                        vm_id: vm_id.to_owned(),
                        process_id: detached_process_id.clone(),
                        event: ActiveExecutionEvent::Stdout(javascript_sync_rpc_bytes_arg(
                            &[event.get("data").cloned().unwrap_or(Value::Null)],
                            0,
                            "detached child_process stdout",
                        )?),
                    }),
                    "stderr" => Some(ProcessEventEnvelope {
                        connection_id: connection_id.clone(),
                        session_id: session_id.clone(),
                        vm_id: vm_id.to_owned(),
                        process_id: detached_process_id.clone(),
                        event: ActiveExecutionEvent::Stderr(javascript_sync_rpc_bytes_arg(
                            &[event.get("data").cloned().unwrap_or(Value::Null)],
                            0,
                            "detached child_process stderr",
                        )?),
                    }),
                    "exit" => {
                        if let Some(vm) = self.vms.get_mut(vm_id) {
                            vm.detached_child_processes.remove(&detached_process_id);
                        }
                        Some(ProcessEventEnvelope {
                            connection_id,
                            session_id,
                            vm_id: vm_id.to_owned(),
                            process_id: detached_process_id.clone(),
                            event: ActiveExecutionEvent::Exited(
                                event
                                    .get("exitCode")
                                    .and_then(Value::as_i64)
                                    .map(|value| value as i32)
                                    .unwrap_or(1),
                            ),
                        })
                    }
                    _ => None,
                };

                let Some(envelope) = envelope else {
                    break;
                };
                self.queue_pending_process_event(envelope)?;
                emitted_any = true;

                if event_type == "exit" {
                    break;
                }
            }
        }

        if yielded {
            self.process_event_notify.notify_one();
        }
        Ok(emitted_any)
    }
    pub(crate) fn drain_queued_descendant_javascript_child_process_events(
        &mut self,
        vm_id: &str,
        process_id: &str,
        child_path: &[&str],
    ) -> Result<(), SidecarError> {
        if child_path.is_empty() {
            return Ok(());
        }
        let target_process_id = Self::child_process_path_label(process_id, child_path);
        let mut child_capacity = self
            .vms
            .get(vm_id)
            .and_then(|vm| vm.active_processes.get(process_id))
            .and_then(|root| descendant_pending_execution_event_capacity(root, child_path));

        let mut deferred = VecDeque::new();
        while let Some(envelope) = self.pending_process_events.pop_front() {
            if envelope.vm_id == vm_id && envelope.process_id == target_process_id {
                if matches!(child_capacity, Some(0)) {
                    self.pending_process_events.push_front(envelope);
                    while let Some(deferred_envelope) = deferred.pop_back() {
                        self.pending_process_events.push_front(deferred_envelope);
                    }
                    self.observe_pending_process_event_depth();
                    return Err(process_event_queue_overflow_error(
                        self.config.runtime.protocol.max_process_events,
                    ));
                }
                if let Some(vm) = self.vms.get_mut(vm_id) {
                    if let Some(root) = vm.active_processes.get_mut(process_id) {
                        if let Some(child) = Self::active_process_by_path_mut(root, child_path) {
                            match child.try_queue_pending_execution_envelope(envelope) {
                                Ok(()) => {
                                    child_capacity = child_capacity.map(|capacity| capacity - 1);
                                    continue;
                                }
                                Err((error, envelope)) => {
                                    self.pending_process_events.push_front(envelope);
                                    while let Some(deferred_envelope) = deferred.pop_back() {
                                        self.pending_process_events.push_front(deferred_envelope);
                                    }
                                    self.observe_pending_process_event_depth();
                                    return Err(error);
                                }
                            }
                        }
                    }
                }
            }
            deferred.push_back(envelope);
        }
        self.pending_process_events = deferred;
        self.observe_pending_process_event_depth();

        let mut queued = VecDeque::new();
        {
            let transfer_capacity = self
                .pending_process_event_capacity()
                .min(child_capacity.unwrap_or(usize::MAX));
            let receiver = self.process_event_receiver.as_mut().ok_or_else(|| {
                SidecarError::InvalidState(String::from("process event receiver unavailable"))
            })?;
            loop {
                if queued.len() >= transfer_capacity {
                    if receiver.is_empty() {
                        break;
                    }
                    self.pending_process_events.append(&mut queued);
                    self.observe_pending_process_event_depth();
                    return Err(process_event_queue_overflow_error(
                        self.config.runtime.protocol.max_process_events,
                    ));
                }
                match receiver.try_recv() {
                    Ok(envelope) => queued.push_back(envelope),
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
                }
            }
        }
        while let Some(envelope) = queued.pop_front() {
            if envelope.vm_id == vm_id && envelope.process_id == target_process_id {
                if let Some(vm) = self.vms.get_mut(vm_id) {
                    if let Some(root) = vm.active_processes.get_mut(process_id) {
                        if let Some(child) = Self::active_process_by_path_mut(root, child_path) {
                            match child.try_queue_pending_execution_envelope(envelope) {
                                Ok(()) => continue,
                                Err((error, envelope)) => {
                                    self.pending_process_events.push_back(envelope);
                                    self.pending_process_events.append(&mut queued);
                                    self.observe_pending_process_event_depth();
                                    return Err(error);
                                }
                            }
                        }
                    }
                }
            }
            if let Err((error, envelope)) = self.try_queue_pending_process_event(envelope) {
                self.pending_process_events.push_back(envelope);
                self.pending_process_events.append(&mut queued);
                self.observe_pending_process_event_depth();
                return Err(error);
            }
        }

        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn resolve_javascript_child_process_execution(
        &self,
        vm: &mut VmState,
        parent_env: &BTreeMap<String, String>,
        parent_guest_cwd: &str,
        parent_host_cwd: &Path,
        request: &ProcessLaunchRequest,
    ) -> Result<ResolvedChildProcessExecution, SidecarError> {
        Self::resolve_javascript_child_process_execution_with_mode(
            vm,
            parent_env,
            parent_guest_cwd,
            parent_host_cwd,
            request,
            false,
            None,
        )
    }

    // Resolution keeps host/guest cwd and PATH policy explicit because they
    // are distinct security inputs, not interchangeable options.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn resolve_javascript_child_process_execution_with_mode(
        vm: &mut VmState,
        parent_env: &BTreeMap<String, String>,
        parent_guest_cwd: &str,
        parent_host_cwd: &Path,
        request: &ProcessLaunchRequest,
        exact_exec_path: bool,
        search_path_override: Option<&str>,
    ) -> Result<ResolvedChildProcessExecution, SidecarError> {
        if exact_exec_path && search_path_override.is_some() {
            return Err(SidecarError::host(
                "EINVAL",
                String::from("exact spawn path cannot also request PATH search"),
            ));
        }
        let mut runtime_env = parent_env.clone();
        runtime_env.extend(request.options.internal_bootstrap_env.clone());
        let (guest_cwd, host_cwd_override) = request
            .options
            .cwd
            .as_deref()
            .map(|cwd| {
                let normalized_parent_host_cwd = normalize_host_path(parent_host_cwd);
                let requested_host_cwd = normalize_host_path(Path::new(cwd));
                if path_is_within_root(&requested_host_cwd, &normalized_parent_host_cwd) {
                    let relative = requested_host_cwd
                        .strip_prefix(&normalized_parent_host_cwd)
                        .unwrap_or_else(|_| Path::new(""));
                    let relative = relative.to_string_lossy().replace('\\', "/");
                    let guest_cwd = if relative.is_empty() {
                        parent_guest_cwd.to_owned()
                    } else {
                        normalize_path(&format!("{parent_guest_cwd}/{relative}"))
                    };
                    (guest_cwd, Some(requested_host_cwd))
                } else if Path::new(cwd).is_relative() {
                    (
                        normalize_path(&format!("{parent_guest_cwd}/{cwd}")),
                        Some(normalize_host_path(&parent_host_cwd.join(cwd))),
                    )
                } else {
                    (normalize_path(cwd), None)
                }
            })
            .unwrap_or_else(|| (parent_guest_cwd.to_owned(), None));
        let inherited_host_cwd = (host_cwd_override.is_none() && guest_cwd == parent_guest_cwd)
            .then(|| normalize_host_path(parent_host_cwd));
        let host_cwd = host_cwd_override
            .or(inherited_host_cwd)
            .or_else(|| {
                host_runtime_path_for_guest_path_with_env(
                    vm,
                    &runtime_env,
                    &guest_cwd,
                    parent_host_cwd,
                )
            })
            .unwrap_or_else(|| {
                let candidate = PathBuf::from(&guest_cwd);
                if guest_cwd == parent_guest_cwd {
                    normalize_host_path(parent_host_cwd)
                } else if candidate.is_absolute() {
                    runtime_asset_path_for_guest(vm, &guest_cwd)
                } else {
                    vm.host_cwd.clone()
                }
            });
        let mut env = parent_env.clone();
        env.extend(request.options.env.clone());
        // Child JavaScript executions must resolve their own entrypoint/eval state.
        // Reusing the parent's values makes the sidecar load the wrong source file.
        env.remove("AGENTOS_GUEST_ENTRYPOINT");
        env.remove("AGENTOS_NODE_EVAL");

        let (command, process_args) = if request.options.shell {
            let tokens = tokenize_shell_free_command(&request.command);
            let requires_shell = request.options.argv0.is_some()
                || command_requires_shell(&request.command)
                || tokens.first().is_some_and(|command| {
                    is_posix_shell_builtin(command) || shell_first_token_requires_shell(command)
                });
            if requires_shell {
                if resolve_guest_command_entrypoint(
                    vm,
                    &guest_cwd,
                    "sh",
                    env.get("PATH").map(String::as_str),
                )
                .is_none()
                {
                    return Err(SidecarError::InvalidState(format!(
                        "shell-mode child_process command requires /bin/sh, which is not \
                         installed in this VM (install a software package that provides sh, \
                         for example @agentos-software/coreutils): {}",
                        request.command
                    )));
                }
                (
                    String::from("sh"),
                    vec![String::from("-c"), request.command.clone()],
                )
            } else {
                let Some((command, args)) = tokens.split_first() else {
                    return Err(SidecarError::InvalidState(String::from(
                        "child_process shell command must not be empty",
                    )));
                };
                (command.clone(), args.to_vec())
            }
        } else {
            (request.command.clone(), request.args.clone())
        };
        let process_args = apply_shell_cwd_prefix(&command, process_args, &guest_cwd);
        if !exact_exec_path && is_binding_command(vm, &command) {
            let command = normalized_binding_command_name(&command).unwrap_or(command);
            return Ok(ResolvedChildProcessExecution {
                command: command.clone(),
                process_args: std::iter::once(command.clone())
                    .chain(process_args.iter().cloned())
                    .collect(),
                runtime: GuestRuntimeKind::JavaScript,
                entrypoint: command,
                execution_args: process_args,
                env,
                guest_cwd,
                host_cwd,
                wasm_permission_tier: None,
                binding_command: true,
                adapter_policy: ExecutionAdapterPolicy::BINDING,
            });
        }

        if is_path_like_specifier(&command)
            && matches!(
                Path::new(&command).extension().and_then(|ext| ext.to_str()),
                Some("js" | "mjs" | "cjs" | "ts" | "mts" | "cts")
            )
        {
            let guest_entrypoint = if command.starts_with('/') {
                normalize_path(&command)
            } else if command.starts_with("file:") {
                normalize_path(command.trim_start_matches("file:"))
            } else {
                normalize_path(&format!("{guest_cwd}/{command}"))
            };
            let host_entrypoint = if command.starts_with("./") || command.starts_with("../") {
                normalize_host_path(&host_cwd.join(&command))
            } else {
                host_runtime_path_for_guest_path_with_env(
                    vm,
                    &runtime_env,
                    &guest_entrypoint,
                    parent_host_cwd,
                )
                .unwrap_or_else(|| {
                    let candidate = PathBuf::from(&guest_entrypoint);
                    if candidate.is_absolute() {
                        candidate
                    } else {
                        host_cwd.join(&guest_entrypoint)
                    }
                })
            };
            env.insert(String::from("AGENTOS_GUEST_ENTRYPOINT"), guest_entrypoint);
            let guest_entrypoint = env.get("AGENTOS_GUEST_ENTRYPOINT").cloned();
            prepare_guest_runtime_env(vm, &mut env, &guest_cwd, &host_cwd, guest_entrypoint)?;

            return Ok(ResolvedChildProcessExecution {
                command: command.clone(),
                process_args: std::iter::once(command)
                    .chain(process_args.iter().cloned())
                    .collect(),
                runtime: GuestRuntimeKind::JavaScript,
                entrypoint: host_entrypoint.to_string_lossy().into_owned(),
                execution_args: process_args,
                env,
                guest_cwd,
                host_cwd,
                wasm_permission_tier: None,
                binding_command: false,
                adapter_policy: ExecutionAdapterPolicy::DIRECT_RUNTIME,
            });
        }

        let resolves_to_registered_node_runtime = exact_exec_path
            && registered_command_name_for_path(&vm.kernel, &command)
                .is_some_and(|name| is_node_runtime_command(&name));
        if (!exact_exec_path || resolves_to_registered_node_runtime)
            && is_node_runtime_command(&command)
        {
            if let Some(cli) = resolve_host_node_cli_entrypoint(&command) {
                env.insert(
                    String::from("AGENTOS_NODE_EVAL"),
                    build_host_node_cli_eval(&cli),
                );
                prepare_guest_runtime_env(vm, &mut env, &guest_cwd, &host_cwd, None)?;
                add_runtime_guest_path_mapping(&mut env, &cli.guest_root, &cli.package_root);
                add_runtime_host_access_path(
                    &mut env,
                    "AGENTOS_EXTRA_FS_READ_PATHS",
                    &cli.package_root,
                    true,
                );

                return Ok(ResolvedChildProcessExecution {
                    command: command.clone(),
                    process_args: std::iter::once(command.clone())
                        .chain(process_args.iter().cloned())
                        .collect(),
                    runtime: GuestRuntimeKind::JavaScript,
                    entrypoint: String::from("-e"),
                    execution_args: std::iter::once(cli.guest_entrypoint.clone())
                        .chain(process_args.iter().cloned())
                        .collect(),
                    env,
                    guest_cwd,
                    host_cwd,
                    wasm_permission_tier: None,
                    binding_command: false,
                    adapter_policy: ExecutionAdapterPolicy::DIRECT_RUNTIME,
                });
            }

            if process_args.is_empty() {
                env.insert(String::from("AGENTOS_NODE_EVAL"), String::new());
                prepare_guest_runtime_env(vm, &mut env, &guest_cwd, &host_cwd, None)?;

                return Ok(ResolvedChildProcessExecution {
                    command: command.clone(),
                    process_args: vec![command.clone()],
                    runtime: GuestRuntimeKind::JavaScript,
                    entrypoint: String::from("-e"),
                    execution_args: Vec::new(),
                    env,
                    guest_cwd,
                    host_cwd,
                    wasm_permission_tier: None,
                    binding_command: false,
                    adapter_policy: ExecutionAdapterPolicy::DIRECT_RUNTIME,
                });
            }

            if let Some((entrypoint, execution_args)) =
                resolve_special_node_cli_invocation(&process_args, &mut env)
            {
                prepare_guest_runtime_env(vm, &mut env, &guest_cwd, &host_cwd, None)?;

                return Ok(ResolvedChildProcessExecution {
                    command: command.clone(),
                    process_args: std::iter::once(command.clone())
                        .chain(process_args.iter().cloned())
                        .collect(),
                    runtime: GuestRuntimeKind::JavaScript,
                    entrypoint,
                    execution_args,
                    env,
                    guest_cwd,
                    host_cwd,
                    wasm_permission_tier: None,
                    binding_command: false,
                    adapter_policy: ExecutionAdapterPolicy::DIRECT_RUNTIME,
                });
            }

            let Some(entrypoint_specifier) = process_args.first() else {
                return Err(SidecarError::InvalidState(format!(
                    "{command} child_process spawn requires an entrypoint"
                )));
            };

            let (entrypoint, execution_args) = if is_path_like_specifier(entrypoint_specifier) {
                let requested_guest_entrypoint = if entrypoint_specifier.starts_with('/') {
                    normalize_path(entrypoint_specifier)
                } else if entrypoint_specifier.starts_with("file:") {
                    normalize_path(entrypoint_specifier.trim_start_matches("file:"))
                } else {
                    normalize_path(&format!("{guest_cwd}/{entrypoint_specifier}"))
                };
                let preserve_main_symlink = process_args
                    .iter()
                    .any(|argument| argument == "--preserve-symlinks-main");
                let (guest_entrypoint, host_entrypoint) = if preserve_main_symlink {
                    let host_entrypoint = if entrypoint_specifier.starts_with("./")
                        || entrypoint_specifier.starts_with("../")
                    {
                        normalize_host_path(&host_cwd.join(entrypoint_specifier))
                    } else {
                        host_runtime_path_for_guest_path_with_env(
                            vm,
                            &runtime_env,
                            &requested_guest_entrypoint,
                            parent_host_cwd,
                        )
                        .unwrap_or_else(|| {
                            let candidate = PathBuf::from(&requested_guest_entrypoint);
                            if candidate.is_absolute() {
                                candidate
                            } else {
                                host_cwd.join(&requested_guest_entrypoint)
                            }
                        })
                    };
                    (requested_guest_entrypoint, host_entrypoint)
                } else {
                    resolve_javascript_main_entrypoint(vm, &requested_guest_entrypoint)
                };
                env.insert(String::from("AGENTOS_GUEST_ENTRYPOINT"), guest_entrypoint);
                (
                    host_entrypoint.to_string_lossy().into_owned(),
                    process_args.iter().skip(1).cloned().collect(),
                )
            } else {
                (
                    entrypoint_specifier.clone(),
                    process_args.iter().skip(1).cloned().collect(),
                )
            };
            let guest_entrypoint = env.get("AGENTOS_GUEST_ENTRYPOINT").cloned();
            prepare_guest_runtime_env(vm, &mut env, &guest_cwd, &host_cwd, guest_entrypoint)?;

            return Ok(ResolvedChildProcessExecution {
                command: command.clone(),
                process_args: std::iter::once(command)
                    .chain(process_args.iter().cloned())
                    .collect(),
                runtime: GuestRuntimeKind::JavaScript,
                entrypoint,
                execution_args,
                env,
                guest_cwd,
                host_cwd,
                wasm_permission_tier: None,
                binding_command: false,
                adapter_policy: ExecutionAdapterPolicy::DIRECT_RUNTIME,
            });
        }

        if !exact_exec_path && is_python_runtime_command(&command) {
            return resolve_python_command_execution(
                vm,
                &command,
                &process_args,
                env,
                guest_cwd,
                host_cwd,
            );
        }

        let guest_entrypoint = if exact_exec_path {
            resolve_exact_guest_command_entrypoint(vm, &guest_cwd, &command)
        } else {
            resolve_guest_command_entrypoint(
                vm,
                &guest_cwd,
                &command,
                search_path_override.or_else(|| env.get("PATH").map(String::as_str)),
            )
        }
        .ok_or_else(|| SidecarError::host("ENOENT", format!("command not found: {command}")))?;
        let host_entrypoint = runtime_launch_path_for_guest(vm, &guest_entrypoint);
        let wasm_permission_tier = vm.command_permissions.get(&command).copied().or_else(|| {
            Path::new(&guest_entrypoint)
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| vm.command_permissions.get(name).copied())
        });
        if let Some((javascript_guest_entrypoint, javascript_host_entrypoint)) =
            resolve_javascript_command_entrypoint(vm, &guest_entrypoint, &host_entrypoint)
        {
            prepare_guest_runtime_env(
                vm,
                &mut env,
                &guest_cwd,
                &host_cwd,
                Some(javascript_guest_entrypoint),
            )?;

            return Ok(ResolvedChildProcessExecution {
                command: command.clone(),
                process_args: std::iter::once(command)
                    .chain(process_args.iter().cloned())
                    .collect(),
                runtime: GuestRuntimeKind::JavaScript,
                entrypoint: javascript_host_entrypoint.to_string_lossy().into_owned(),
                execution_args: process_args,
                env,
                guest_cwd,
                host_cwd,
                wasm_permission_tier: None,
                binding_command: false,
                adapter_policy: ExecutionAdapterPolicy::DIRECT_RUNTIME,
            });
        }
        prepare_guest_runtime_env(
            vm,
            &mut env,
            &guest_cwd,
            &host_cwd,
            Some(guest_entrypoint.clone()),
        )?;

        Ok(ResolvedChildProcessExecution {
            command: command.clone(),
            process_args: std::iter::once(command)
                .chain(process_args.iter().cloned())
                .collect(),
            runtime: GuestRuntimeKind::WebAssembly,
            entrypoint: host_entrypoint.to_string_lossy().into_owned(),
            execution_args: process_args,
            env,
            guest_cwd,
            host_cwd,
            wasm_permission_tier,
            binding_command: false,
            adapter_policy: ExecutionAdapterPolicy::KERNEL_HOST_CALL_POSIX,
        })
    }

    fn resolve_javascript_child_process_with_shebang(
        &mut self,
        vm_id: &str,
        parent_env: &BTreeMap<String, String>,
        parent_guest_cwd: &str,
        parent_host_cwd: &Path,
        request: &mut ProcessLaunchRequest,
    ) -> Result<ResolvedChildProcessExecution, SidecarError> {
        const MAX_SHEBANG_REDIRECTS: usize = 4;

        let mut resolved = {
            let vm = self
                .vms
                .get_mut(vm_id)
                .ok_or_else(|| missing_vm_error(vm_id))?;
            Self::resolve_javascript_child_process_execution_with_mode(
                vm,
                parent_env,
                parent_guest_cwd,
                parent_host_cwd,
                request,
                false,
                None,
            )?
        };

        for redirects in 0..=MAX_SHEBANG_REDIRECTS {
            let redirected = {
                let vm = self
                    .vms
                    .get_mut(vm_id)
                    .ok_or_else(|| missing_vm_error(vm_id))?;
                rewrite_javascript_shebang_request(vm, &resolved, request)?
            };
            if !redirected {
                return Ok(resolved);
            }
            if redirects == MAX_SHEBANG_REDIRECTS {
                return Err(SidecarError::host(
                    "ELOOP",
                    format!("exceeded {MAX_SHEBANG_REDIRECTS} shebang redirects"),
                ));
            }
            resolved = {
                let vm = self
                    .vms
                    .get_mut(vm_id)
                    .ok_or_else(|| missing_vm_error(vm_id))?;
                Self::resolve_javascript_child_process_execution_with_mode(
                    vm,
                    parent_env,
                    parent_guest_cwd,
                    parent_host_cwd,
                    request,
                    false,
                    None,
                )?
            };
        }

        Ok(resolved)
    }

    pub(crate) async fn spawn_child_process(
        &mut self,
        vm_id: &str,
        process_id: &str,
        mut request: ProcessLaunchRequest,
    ) -> Result<Value, SidecarError> {
        let spawn_attributes = javascript_spawn_attributes(&request.options)?;
        let requested_pgid = spawn_attributes.process_group;
        let prepared_host_net_fds = {
            let vm = self
                .vms
                .get_mut(vm_id)
                .ok_or_else(|| missing_vm_error(vm_id))?;
            let current_network_counts = vm_spawn_host_net_resource_counts(vm);
            let managed_descriptions = Arc::clone(&vm.managed_host_net_descriptions);
            let (kernel, active_processes) = (&mut vm.kernel, &mut vm.active_processes);
            let parent = active_processes
                .get_mut(process_id)
                .ok_or_else(|| missing_process_error(vm_id, process_id))?;
            prepare_spawn_host_net_fds(
                kernel,
                parent,
                &managed_descriptions,
                current_network_counts,
                &request.options.spawn_host_net_fds,
                &request.options.spawn_fd_mappings,
                &request.options.spawn_file_actions,
            )?
        };
        let prepared_spawn_actions = if !prepared_host_net_fds.kernel_actions.is_empty() {
            let (parent_pid, parent_cwd) = {
                let vm = self
                    .vms
                    .get_mut(vm_id)
                    .ok_or_else(|| missing_vm_error(vm_id))?;
                let parent = vm
                    .active_processes
                    .get(process_id)
                    .ok_or_else(|| missing_process_error(vm_id, process_id))?;
                let initial_cwd = request
                    .options
                    .cwd
                    .as_deref()
                    .map(|cwd| {
                        if cwd.starts_with('/') {
                            normalize_path(cwd)
                        } else {
                            normalize_path(&format!("{}/{cwd}", parent.guest_cwd))
                        }
                    })
                    .unwrap_or_else(|| parent.guest_cwd.clone());
                (parent.kernel_pid, initial_cwd)
            };
            let vm = self
                .vms
                .get_mut(vm_id)
                .ok_or_else(|| missing_vm_error(vm_id))?;
            Some(preapply_posix_spawn_file_actions(
                &mut vm.kernel,
                parent_pid,
                &parent_cwd,
                requested_pgid,
                &request.options.spawn_fd_mappings,
                &prepared_host_net_fds.kernel_actions,
            )?)
        } else {
            None
        };
        if let Some(prepared) = prepared_spawn_actions.as_ref() {
            request.options.cwd = Some(prepared.cwd.clone());
        }
        {
            let parent_guest_cwd = self
                .vms
                .get(vm_id)
                .ok_or_else(|| missing_vm_error(vm_id))?
                .active_processes
                .get(process_id)
                .ok_or_else(|| missing_process_error(vm_id, process_id))?
                .guest_cwd
                .clone();
            let vm = self
                .vms
                .get_mut(vm_id)
                .ok_or_else(|| missing_vm_error(vm_id))?;
            resolve_posix_spawn_program(vm, &parent_guest_cwd, &mut request)?;
        }
        let total_start = Instant::now();
        let process_event_capacity = self.config.runtime.protocol.max_process_events;
        let phase_start = Instant::now();
        let (parent_env, parent_guest_cwd, parent_host_cwd, parent_kernel_pid) = {
            let vm = self
                .vms
                .get_mut(vm_id)
                .ok_or_else(|| missing_vm_error(vm_id))?;
            let parent = vm
                .active_processes
                .get(process_id)
                .ok_or_else(|| missing_process_error(vm_id, process_id))?;
            (
                parent.env.clone(),
                parent.guest_cwd.clone(),
                parent.host_cwd.clone(),
                parent.kernel_pid,
            )
        };
        let mut resolved =
            if !request.options.spawn_exact_path && request.options.spawn_search_path.is_none() {
                self.resolve_javascript_child_process_with_shebang(
                    vm_id,
                    &parent_env,
                    &parent_guest_cwd,
                    &parent_host_cwd,
                    &mut request,
                )?
            } else {
                let vm = self
                    .vms
                    .get_mut(vm_id)
                    .ok_or_else(|| missing_vm_error(vm_id))?;
                Self::resolve_javascript_child_process_execution_with_mode(
                    vm,
                    &parent_env,
                    &parent_guest_cwd,
                    &parent_host_cwd,
                    &request,
                    request.options.spawn_exact_path,
                    request.options.spawn_search_path.as_deref(),
                )?
            };
        apply_child_process_argv0(&mut resolved, request.options.argv0.as_deref());
        {
            let vm = self
                .vms
                .get_mut(vm_id)
                .ok_or_else(|| missing_vm_error(vm_id))?;
            stage_agentos_package_command(
                vm,
                &mut resolved,
                WasmLaunchAuthority::GuestProcessImage {
                    requester_pid: parent_kernel_pid,
                },
            )?;
            stage_kernel_wasm_launch_asset(
                vm,
                &mut resolved,
                WasmLaunchAuthority::GuestProcessImage {
                    requester_pid: parent_kernel_pid,
                },
            )?;
        }
        tracing::debug!(
            vm_id,
            process_id,
            command = %resolved.command,
            runtime = ?resolved.runtime,
            entrypoint = %resolved.entrypoint,
            execution_args = ?resolved.execution_args,
            parent_guest_cwd = %parent_guest_cwd,
            requested_cwd = ?request.options.cwd,
            guest_cwd = %resolved.guest_cwd,
            host_cwd = %resolved.host_cwd.display(),
            "resolved JavaScript child process"
        );
        let resolved = resolved;
        if prepared_host_net_fds.inherited_fd_count() != 0
            && !resolved.adapter_policy.accepts_inherited_host_network_fds
        {
            return Err(SidecarError::host(
                "ENOTSUP",
                String::from("inherited host-network fds require a WebAssembly child runtime"),
            ));
        }
        record_execute_phase("child_process_resolve_execution", phase_start.elapsed());
        let (parent_kernel_pid, child_process_id) = {
            let vm = self
                .vms
                .get_mut(vm_id)
                .ok_or_else(|| missing_vm_error(vm_id))?;
            let process = vm
                .active_processes
                .get_mut(process_id)
                .ok_or_else(|| missing_process_error(vm_id, process_id))?;
            (process.kernel_pid, process.allocate_child_process_id())
        };
        let sidecar_requests = self.sidecar_requests.clone();
        let vm = self
            .vms
            .get_mut(vm_id)
            .ok_or_else(|| missing_vm_error(vm_id))?;
        let vm_pending_stdin_bytes_budget = Arc::clone(&vm.pending_stdin_bytes_budget);
        let vm_pending_event_bytes_budget = Arc::clone(&vm.pending_event_bytes_budget);
        let phase_start = Instant::now();
        let (
            kernel_pid,
            kernel_handle,
            mut execution,
            runtime_control,
            binding_event_request,
            kernel_stdin_writer_fd,
            kernel_stdin_reader_fd,
            direct_posix_stdin,
        ) = if resolved.binding_command {
            let binding_resolution = resolve_binding_command(
                vm,
                &resolved.command,
                &resolved.execution_args,
                Some(&resolved.guest_cwd),
            )?
            .ok_or_else(|| {
                SidecarError::InvalidState(format!(
                    "binding command no longer resolves: {}",
                    resolved.command
                ))
            })?;
            let kernel_handle = vm
                .kernel
                .create_virtual_process_with_process_group(
                    EXECUTION_DRIVER_NAME,
                    BINDING_DRIVER_NAME,
                    &resolved.command,
                    resolved.process_args.clone(),
                    VirtualProcessOptions {
                        parent_pid: Some(parent_kernel_pid),
                        env: resolved.env.clone(),
                        cwd: Some(resolved.guest_cwd.clone()),
                        permission_tier: resolved
                            .wasm_permission_tier
                            .map(kernel_process_permission_tier),
                    },
                    requested_pgid,
                )
                .map_err(kernel_error)?;
            let kernel_pid = kernel_handle.pid();
            if let Some(prepared) = prepared_spawn_actions {
                install_preapplied_posix_spawn_file_actions(
                    &mut vm.kernel,
                    &kernel_handle,
                    prepared,
                )?;
            } else {
                apply_posix_spawn_file_actions_or_rollback(
                    &mut vm.kernel,
                    &kernel_handle,
                    &resolved.guest_cwd,
                    &request.options.spawn_fd_mappings,
                    &prepared_host_net_fds.kernel_actions,
                )?;
            }
            apply_spawn_session_or_rollback(
                &mut vm.kernel,
                &kernel_handle,
                spawn_attributes.new_session || request.options.detached,
            )?;
            apply_spawn_process_attributes_or_rollback(
                &mut vm.kernel,
                &kernel_handle,
                &request.options,
            )?;
            let runtime_control = match ActiveProcess::attach_runtime_control_before_start(
                &kernel_handle,
                Arc::clone(&self.process_event_notify),
            ) {
                Ok(runtime_control) => runtime_control,
                Err(error) => {
                    rollback_unregistered_spawn_child(
                        &mut vm.kernel,
                        &kernel_handle,
                        None,
                        "child_process.spawn binding runtime-control attachment",
                    );
                    return Err(error);
                }
            };
            let binding_execution = BindingExecution::with_event_notify(
                Arc::clone(&self.process_event_notify),
                process_event_capacity,
            )
            .with_vm_pending_event_bytes_budget(Arc::clone(&vm_pending_event_bytes_budget));
            let cancelled = binding_execution.cancelled.clone();
            let pending_events = binding_execution.pending_events.clone();
            let event_overflow_reason = binding_execution.event_overflow_reason.clone();
            let pending_event_bytes = binding_execution.pending_event_bytes.clone();
            let pending_event_count_limit = binding_execution.pending_event_count_limit.clone();
            let pending_event_bytes_limit = binding_execution.pending_event_bytes_limit.clone();
            let binding_vm_pending_event_bytes_budget =
                binding_execution.vm_pending_event_bytes_budget.clone();
            let event_notify = binding_execution.event_notify.clone();
            let binding_event_request = BindingProcessEventRequest {
                runtime_context: vm.runtime_context.clone(),
                sidecar_requests: sidecar_requests.clone(),
                connection_id: vm.connection_id.clone(),
                session_id: vm.session_id.clone(),
                vm_id: vm_id.to_owned(),
                binding_resolution,
                cancelled,
                paused: Arc::clone(&binding_execution.paused),
                pause_notify: Arc::clone(&binding_execution.pause_notify),
                pending_events,
                event_overflow_reason,
                pending_event_bytes,
                pending_event_count_limit,
                pending_event_bytes_limit,
                vm_pending_event_bytes_budget: binding_vm_pending_event_bytes_budget,
                event_notify,
            };
            (
                kernel_pid,
                kernel_handle,
                ActiveExecution::Binding(binding_execution),
                runtime_control,
                Some(binding_event_request),
                None,
                0,
                false,
            )
        } else {
            let kernel_command = resolved.adapter_policy.kernel_driver_command;
            let kernel_handle = vm
                .kernel
                .spawn_process_with_process_group(
                    kernel_command,
                    resolved.process_args.clone(),
                    SpawnOptions {
                        requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                        parent_pid: Some(parent_kernel_pid),
                        env: resolved.env.clone(),
                        cwd: Some(resolved.guest_cwd.clone()),
                        permission_tier: resolved
                            .wasm_permission_tier
                            .map(kernel_process_permission_tier),
                    },
                    requested_pgid,
                )
                .map_err(kernel_error)?;
            let kernel_pid = kernel_handle.pid();
            let applied_spawn_actions = if let Some(prepared) = prepared_spawn_actions {
                install_preapplied_posix_spawn_file_actions(
                    &mut vm.kernel,
                    &kernel_handle,
                    prepared,
                )?
            } else {
                apply_posix_spawn_file_actions_or_rollback(
                    &mut vm.kernel,
                    &kernel_handle,
                    &resolved.guest_cwd,
                    &request.options.spawn_fd_mappings,
                    &prepared_host_net_fds.kernel_actions,
                )?
            };
            let posix_spawn_controls_stdin = !request.options.spawn_file_actions.is_empty()
                && (applied_spawn_actions
                    .fd_mappings
                    .iter()
                    .any(|mapping| mapping[0] == 0)
                    || applied_spawn_actions.closed_guest_fds.contains(&0)
                    || prepared_host_net_fds
                        .descriptions
                        .iter()
                        .any(|description| description.guest_fds.contains(&0)));
            if resolved.adapter_policy.materializes_direct_runtime_stdio {
                materialize_direct_runtime_stdio_mappings(
                    &mut vm.kernel,
                    kernel_pid,
                    &applied_spawn_actions,
                )?;
            }
            let kernel_stdin_reader_fd = if resolved.adapter_policy.canonicalizes_runtime_stdin {
                canonicalize_host_runtime_posix_stdin(
                    &mut vm.kernel,
                    kernel_pid,
                    &applied_spawn_actions,
                )?
            } else {
                0
            };
            apply_spawn_session_or_rollback(
                &mut vm.kernel,
                &kernel_handle,
                spawn_attributes.new_session || request.options.detached,
            )?;
            apply_spawn_process_attributes_or_rollback(
                &mut vm.kernel,
                &kernel_handle,
                &request.options,
            )?;
            let mut execution_env = resolved.env.clone();
            if resolved.adapter_policy.forwards_kernel_stdin_rpc {
                execution_env.insert(
                    String::from("AGENTOS_FORWARD_KERNEL_STDIN_RPC"),
                    String::from("1"),
                );
            }
            if resolved.adapter_policy.encodes_inherited_fd_bootstrap {
                execution_env.insert(
                    String::from("AGENTOS_WASM_INHERITED_FD_MAPPINGS"),
                    serde_json::to_string(&applied_spawn_actions.fd_mappings).map_err(|error| {
                        SidecarError::InvalidState(format!(
                            "failed to serialize inherited WASM fd mappings: {error}"
                        ))
                    })?,
                );
                execution_env.insert(
                    String::from("AGENTOS_WASM_CLOSED_INHERITED_FDS"),
                    serde_json::to_string(&applied_spawn_actions.closed_guest_fds).map_err(
                        |error| {
                            SidecarError::InvalidState(format!(
                                "failed to serialize closed inherited WASM fds: {error}"
                            ))
                        },
                    )?,
                );
                execution_env.insert(
                    String::from("AGENTOS_WASM_INHERITED_HOSTNET_FDS"),
                    serde_json::to_string(&prepared_host_net_fds.bootstrap_json()).map_err(
                        |error| {
                            SidecarError::InvalidState(format!(
                                "failed to serialize inherited WASM host-network fds: {error}"
                            ))
                        },
                    )?,
                );
            }
            execution_env.insert(
                String::from(EXECUTION_SANDBOX_ROOT_ENV),
                normalize_host_path(&vm.runtime_scratch_root)
                    .to_string_lossy()
                    .into_owned(),
            );

            macro_rules! attach_child_runtime_control {
                ($label:literal) => {
                    match ActiveProcess::attach_runtime_control_before_start(
                        &kernel_handle,
                        Arc::clone(&self.process_event_notify),
                    ) {
                        Ok(runtime_control) => runtime_control,
                        Err(error) => {
                            rollback_unregistered_spawn_child(
                                &mut vm.kernel,
                                &kernel_handle,
                                None,
                                $label,
                            );
                            return Err(error);
                        }
                    }
                };
            }

            let (execution, runtime_control) = match resolved.runtime {
                GuestRuntimeKind::JavaScript => {
                    execution_env.extend(sanitize_javascript_child_process_internal_bootstrap_env(
                        &request.options.internal_bootstrap_env,
                    ));
                    execution_env
                        .insert(String::from("AGENTOS_KEEP_STDIN_OPEN"), String::from("1"));
                    let launch_entrypoint = resolve_agentos_package_javascript_launch_entrypoint(
                        vm,
                        kernel_pid,
                        &mut execution_env,
                    )?
                    .unwrap_or_else(|| resolved.entrypoint.clone());
                    let inline_code = load_javascript_entrypoint_source(
                        vm,
                        kernel_pid,
                        &resolved.guest_cwd,
                        &launch_entrypoint,
                        &execution_env,
                    )?;
                    prepare_javascript_launch_assets(
                        vm,
                        &resolved,
                        &execution_env,
                        WasmLaunchAuthority::GuestProcessImage {
                            requester_pid: kernel_pid,
                        },
                        inline_code.as_deref(),
                    )?;
                    let context =
                        self.javascript_engine
                            .create_context(CreateJavascriptContextRequest {
                                vm_id: vm_id.to_owned(),
                                bootstrap_module: None,
                                compile_cache_root: Some(
                                    self.cache_root.join("node-compile-cache"),
                                ),
                            });
                    let context_id = context.context_id;
                    let runtime_control = attach_child_runtime_control!(
                        "child_process.spawn JavaScript runtime-control attachment"
                    );
                    let execution_result = self
                        .javascript_engine
                        .start_execution_with_module_reader_and_runtime(
                            StartJavascriptExecutionRequest {
                                guest_runtime: guest_runtime_identity(
                                    vm,
                                    Some(u64::from(kernel_pid)),
                                    Some(u64::from(parent_kernel_pid)),
                                ),
                                vm_id: vm_id.to_owned(),
                                context_id: context_id.clone(),
                                argv: std::iter::once(launch_entrypoint)
                                    .chain(resolved.execution_args.clone())
                                    .collect(),
                                argv0: request.options.argv0.clone(),
                                env: execution_env,
                                cwd: resolved.host_cwd.clone(),
                                limits: javascript_execution_limits(vm),
                                inline_code,
                                wasm_module_bytes: None,
                            },
                            None,
                            None,
                            vm.runtime_context.clone(),
                        );
                    self.javascript_engine.dispose_context(&context_id);
                    let execution = match execution_result.map_err(javascript_error) {
                        Ok(execution) => execution,
                        Err(error) => {
                            rollback_unregistered_spawn_child(
                                &mut vm.kernel,
                                &kernel_handle,
                                None,
                                "child_process.spawn JavaScript engine start",
                            );
                            return Err(error);
                        }
                    };
                    (ActiveExecution::Javascript(execution), runtime_control)
                }
                GuestRuntimeKind::WebAssembly => {
                    // These values configure the trusted WASM runner, not
                    // the guest-visible Linux environment.
                    execution_env.extend(sanitize_javascript_child_process_internal_bootstrap_env(
                        &request.options.internal_bootstrap_env,
                    ));
                    execution_env.insert(String::from(WASM_STDIO_SYNC_RPC_ENV), String::from("1"));
                    execution_env.insert(String::from(WASM_EXEC_COMMIT_RPC_ENV), String::from("1"));
                    let wasm_limits = wasm_execution_limits(vm);
                    let wasm_guest_runtime = guest_runtime_identity(
                        vm,
                        Some(u64::from(kernel_pid)),
                        Some(u64::from(parent_kernel_pid)),
                    );
                    let context = self.wasm_engine.create_context(CreateWasmContextRequest {
                        vm_id: vm_id.to_owned(),
                        module_path: Some(resolved.entrypoint.clone()),
                    });
                    let context_id = context.context_id;
                    let runtime_control = attach_child_runtime_control!(
                        "child_process.spawn WebAssembly runtime-control attachment"
                    );
                    let execution_result = self
                        .wasm_engine
                        .start_execution_with_runtime_async(
                            StartWasmExecutionRequest {
                                vm_id: vm_id.to_owned(),
                                context_id: context_id.clone(),
                                managed_kernel_host: true,
                                argv: resolved.process_args.clone(),
                                env: execution_env,
                                cwd: resolved.host_cwd.clone(),
                                permission_tier: execution_wasm_permission_tier(
                                    vm.kernel
                                        .process_permission_tier(EXECUTION_DRIVER_NAME, kernel_pid)
                                        .map_err(kernel_error)?,
                                ),
                                limits: wasm_limits,
                                guest_runtime: wasm_guest_runtime,
                            },
                            vm.runtime_context.clone(),
                        )
                        .await;
                    self.wasm_engine.dispose_context(&context_id);
                    let execution = match execution_result.map_err(wasm_error) {
                        Ok(execution) => execution,
                        Err(error) => {
                            rollback_unregistered_spawn_child(
                                &mut vm.kernel,
                                &kernel_handle,
                                None,
                                "child_process.spawn WebAssembly engine start",
                            );
                            return Err(error);
                        }
                    };
                    (ActiveExecution::Wasm(Box::new(execution)), runtime_control)
                }
                GuestRuntimeKind::Python => {
                    // Nested `python` child_process: set up the Pyodide context the
                    // same way the top-level execute path does, so a guest shell or
                    // node parent can spawn `python` exactly like `node`.
                    let python_file_path = if execution_env.contains_key("AGENTOS_PYTHON_ARGV") {
                        execution_env.get("AGENTOS_PYTHON_FILE").map(PathBuf::from)
                    } else {
                        python_file_entrypoint(&resolved.entrypoint)
                    };
                    let pyodide_dist_path = self
                        .python_engine
                        .bundled_pyodide_dist_path_for_vm(vm_id)
                        .map_err(python_error)?;
                    let pyodide_cache_path = pyodide_dist_path
                        .parent()
                        .and_then(Path::parent)
                        .unwrap_or(pyodide_dist_path.as_path())
                        .join("pyodide-package-cache");
                    add_runtime_guest_path_mapping(
                        &mut execution_env,
                        PYTHON_PYODIDE_GUEST_ROOT,
                        &pyodide_dist_path,
                    );
                    add_runtime_guest_path_mapping(
                        &mut execution_env,
                        PYTHON_PYODIDE_CACHE_GUEST_ROOT,
                        &pyodide_cache_path,
                    );
                    add_runtime_host_access_path(
                        &mut execution_env,
                        "AGENTOS_EXTRA_FS_READ_PATHS",
                        &pyodide_dist_path,
                        true,
                    );
                    add_runtime_host_access_path(
                        &mut execution_env,
                        "AGENTOS_EXTRA_FS_READ_PATHS",
                        &pyodide_cache_path,
                        true,
                    );
                    add_runtime_host_access_path(
                        &mut execution_env,
                        "AGENTOS_EXTRA_FS_WRITE_PATHS",
                        &pyodide_cache_path,
                        false,
                    );
                    let context = self
                        .python_engine
                        .create_context(CreatePythonContextRequest {
                            vm_id: vm_id.to_owned(),
                            pyodide_dist_path,
                        });
                    let context_id = context.context_id;
                    let runtime_control = attach_child_runtime_control!(
                        "child_process.spawn Python runtime-control attachment"
                    );
                    let execution_result = self
                        .python_engine
                        .start_execution_with_runtime_async(
                            StartPythonExecutionRequest {
                                vm_id: vm_id.to_owned(),
                                context_id: context_id.clone(),
                                code: resolved.entrypoint.clone(),
                                file_path: python_file_path,
                                env: execution_env,
                                cwd: resolved.host_cwd.clone(),
                                limits: python_execution_limits(vm),
                                guest_runtime: guest_runtime_identity(
                                    vm,
                                    Some(u64::from(kernel_pid)),
                                    Some(u64::from(parent_kernel_pid)),
                                ),
                            },
                            vm.runtime_context.clone(),
                        )
                        .await;
                    self.python_engine.dispose_context(&context_id);
                    let execution = match execution_result.map_err(python_error) {
                        Ok(execution) => execution,
                        Err(error) => {
                            rollback_unregistered_spawn_child(
                                &mut vm.kernel,
                                &kernel_handle,
                                None,
                                "child_process.spawn Python engine start",
                            );
                            return Err(error);
                        }
                    };
                    (ActiveExecution::Python(execution), runtime_control)
                }
            };
            let kernel_stdin_writer_fd = if posix_spawn_controls_stdin {
                None
            } else {
                match javascript_child_process_stdin_mode(&request) {
                    "pipe" => Some(install_kernel_stdin_pipe(&mut vm.kernel, kernel_pid)?),
                    "ignore" => {
                        install_kernel_ignored_stdin(&mut vm.kernel, kernel_pid)?;
                        None
                    }
                    "inherit" => None,
                    _ => Some(install_kernel_stdin_pipe(&mut vm.kernel, kernel_pid)?),
                }
            };
            (
                kernel_pid,
                kernel_handle,
                execution,
                runtime_control,
                None,
                kernel_stdin_writer_fd,
                kernel_stdin_reader_fd,
                posix_spawn_controls_stdin,
            )
        };
        record_execute_phase(
            "child_process_spawn_and_start_execution",
            phase_start.elapsed(),
        );

        let phase_start = Instant::now();
        let managed_descriptions = Arc::clone(&vm.managed_host_net_descriptions);
        let mut managed_description_guard = match managed_descriptions.lock() {
            Ok(descriptions) => descriptions,
            Err(_) => {
                rollback_unregistered_spawn_child(
                    &mut vm.kernel,
                    &kernel_handle,
                    Some(&mut execution),
                    "child_process.spawn managed-description preflight",
                );
                return Err(SidecarError::host(
                    "EIO",
                    "managed description registry lock poisoned",
                ));
            }
        };
        if let Err(error) =
            prepared_host_net_fds.validate_install(&managed_description_guard, kernel_pid)
        {
            rollback_unregistered_spawn_child(
                &mut vm.kernel,
                &kernel_handle,
                Some(&mut execution),
                "child_process.spawn managed-description preflight",
            );
            return Err(error);
        }
        // Shared-terminal detection: when the child's kernel fd 1 is a PTY (the
        // slave inherited from a TTY shell), record who owns the host-facing
        // master so the child's stdio writes surface through master drains
        // instead of child stdout events (see `tty_master_owner`).
        let child_fd1_is_tty = match vm.kernel.isatty(EXECUTION_DRIVER_NAME, kernel_pid, 1) {
            Ok(is_tty) => is_tty,
            Err(error) if error.code() == "EBADF" => false,
            Err(error) => {
                rollback_unregistered_spawn_child(
                    &mut vm.kernel,
                    &kernel_handle,
                    Some(&mut execution),
                    "child_process.spawn tty preflight",
                );
                return Err(kernel_error(error));
            }
        };
        let child_process_group = match vm.kernel.getpgid(EXECUTION_DRIVER_NAME, kernel_pid) {
            Ok(process_group) => process_group,
            Err(error) => {
                rollback_unregistered_spawn_child(
                    &mut vm.kernel,
                    &kernel_handle,
                    Some(&mut execution),
                    "child_process.spawn process-group preflight",
                );
                return Err(kernel_error(error));
            }
        };
        let process_event_limits = vm.limits.process.clone();
        let process = match vm.active_processes.get_mut(process_id) {
            Some(process) => process,
            None => {
                let error = missing_process_error(vm_id, process_id);
                rollback_unregistered_spawn_child(
                    &mut vm.kernel,
                    &kernel_handle,
                    Some(&mut execution),
                    "child_process.spawn parent lookup",
                );
                return Err(error);
            }
        };
        let inherited_tty_master_owner = if child_fd1_is_tty {
            process
                .tty_master_fd
                .map(|master_fd| (process.kernel_pid, master_fd))
                .or(process.tty_master_owner)
        } else {
            None
        };
        let child_process_bridge_owns_output = !request.options.stdio.is_empty()
            && process.execution.descendant_output_ownership()
                == DescendantOutputOwnership::SidecarBridge;
        let mut child = ActiveProcess::new_with_attached_runtime_control(
            kernel_pid,
            kernel_handle,
            process.runtime_context.clone(),
            process.limits.clone(),
            process_event_capacity,
            resolved.runtime,
            execution,
            runtime_control,
            Arc::clone(&self.process_event_notify),
        )
        .with_adapter_policy(resolved.adapter_policy)
        .with_process_event_limits(&process_event_limits)
        .with_vm_pending_byte_budgets(
            Arc::clone(&vm_pending_stdin_bytes_budget),
            Arc::clone(&vm_pending_event_bytes_budget),
        )
        .with_detached(request.options.detached)
        .with_guest_cwd(resolved.guest_cwd.clone())
        .with_env(resolved.env.clone())
        .with_host_cwd(resolved.host_cwd.clone());
        child.child_process_bridge_owns_output = child_process_bridge_owns_output;
        child.tty_master_owner = inherited_tty_master_owner;
        child.direct_posix_stdin = direct_posix_stdin;
        child.kernel_stdin_reader_fd = kernel_stdin_reader_fd;
        if let Some(kernel_stdin_writer_fd) = kernel_stdin_writer_fd {
            child.kernel_stdin_writer_fd = Some(kernel_stdin_writer_fd);
        }
        prepared_host_net_fds.install(&mut child, &mut managed_description_guard);
        if let Err(error) = child.apply_runtime_controls() {
            let rollback_handle = child.kernel_handle.clone();
            rollback_unregistered_spawn_child(
                &mut vm.kernel,
                &rollback_handle,
                Some(&mut child.execution),
                "child_process.spawn pending runtime control",
            );
            return Err(error);
        }
        process
            .child_processes
            .insert(child_process_id.clone(), child);
        if let Some(binding_event_request) = binding_event_request {
            spawn_binding_process_events(binding_event_request);
        }
        record_execute_phase("child_process_register", phase_start.elapsed());
        record_execute_phase("child_process_spawn_total", total_start.elapsed());
        Ok(json!({
            "childId": child_process_id,
            "pid": kernel_pid,
            "pgid": child_process_group,
            "directPosixStdin": direct_posix_stdin,
            "command": resolved.command,
            "args": resolved.process_args,
        }))
    }

    fn child_process_sync_max_buffer(
        process: &ActiveProcess,
        requested: Option<usize>,
    ) -> Result<usize, SidecarError> {
        let limit = (process.adapter_policy.captured_output_limit)(&process.limits);
        let setting = process.adapter_policy.captured_output_limit_setting;
        let requested = requested.unwrap_or(1024 * 1024);
        if requested > limit {
            return Err(SidecarError::host("ERR_AGENTOS_CHILD_PROCESS_BUFFER_LIMIT", format!("child process maxBuffer {requested} exceeds {setting} ({limit}); raise {setting} for larger captured output"
            )));
        }
        Ok(requested)
    }

    pub(super) async fn begin_javascript_child_process_sync(
        &mut self,
        vm_id: &str,
        process_id: &str,
        request: ProcessLaunchRequest,
        max_buffer: Option<usize>,
        completion: PendingChildProcessSyncCompletion,
    ) -> Result<(), SidecarError> {
        let max_buffer = {
            let vm = self.vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
            let process = vm
                .active_processes
                .get(process_id)
                .ok_or_else(|| missing_process_error(vm_id, process_id))?;
            Self::child_process_sync_max_buffer(process, max_buffer)?
        };
        let deadline = checked_child_process_sync_deadline(request.options.timeout)?;
        let sync_input = javascript_child_process_sync_input_bytes(request.options.input.as_ref())?;
        let (count_reservation, bytes_reservation) = {
            let vm = self.vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
            reserve_child_process_sync_budget(
                &vm.pending_child_sync_count_budget,
                &vm.pending_child_sync_bytes_budget,
                max_buffer,
                sync_input.as_deref().map_or(0, <[u8]>::len),
            )?
        };
        let timeout_signal = request
            .options
            .kill_signal
            .clone()
            .unwrap_or_else(|| String::from("SIGTERM"));
        let prior_child_ids = self.child_process_ids_at_path(vm_id, process_id, &[])?;
        let spawned = self.spawn_child_process(vm_id, process_id, request).await?;
        let identity = match parse_spawned_child_identity(&spawned) {
            Ok(identity) => identity,
            Err(error) => {
                let hinted_child_id = spawned.get("childId").and_then(Value::as_str);
                let hinted_pid = spawned
                    .get("pid")
                    .and_then(Value::as_u64)
                    .and_then(|pid| u32::try_from(pid).ok());
                self.rollback_registered_child_process_sync(
                    vm_id,
                    process_id,
                    &[],
                    &prior_child_ids,
                    hinted_child_id,
                    hinted_pid,
                    "root spawnSync response parsing",
                );
                return Err(error);
            }
        };

        if let Some(input) = sync_input.as_deref() {
            if let Err(error) =
                self.write_child_process_stdin(vm_id, process_id, &identity.child_process_id, input)
            {
                self.rollback_registered_child_process_sync(
                    vm_id,
                    process_id,
                    &[],
                    &prior_child_ids,
                    Some(&identity.child_process_id),
                    Some(identity.pid),
                    "root spawnSync stdin write",
                );
                return Err(error);
            }
        }
        if let Err(error) =
            self.close_child_process_stdin(vm_id, process_id, &identity.child_process_id)
        {
            self.rollback_registered_child_process_sync(
                vm_id,
                process_id,
                &[],
                &prior_child_ids,
                Some(&identity.child_process_id),
                Some(identity.pid),
                "root spawnSync stdin close",
            );
            return Err(error);
        }

        let pending = PendingChildProcessSync {
            pid: identity.pid,
            stdout: Vec::new(),
            stderr: Vec::new(),
            max_buffer,
            deadline,
            timeout_signal,
            kill_sent: false,
            timed_out: false,
            max_buffer_exceeded: false,
            completion,
            _count_reservation: count_reservation,
            _bytes_reservation: bytes_reservation,
        };
        let registration = (|| -> Result<_, SidecarError> {
            let vm = self
                .vms
                .get_mut(vm_id)
                .ok_or_else(|| missing_vm_error(vm_id))?;
            let process = vm
                .active_processes
                .get_mut(process_id)
                .ok_or_else(|| missing_process_error(vm_id, process_id))?;
            if process
                .pending_child_process_sync
                .contains_key(&identity.child_process_id)
            {
                return Err(SidecarError::host(
                    "EEXIST",
                    format!(
                        "pending child-process sync entry {} already exists",
                        identity.child_process_id
                    ),
                ));
            }
            process
                .pending_child_process_sync
                .insert(identity.child_process_id.clone(), pending);
            Ok((
                process.runtime_context.clone(),
                Arc::clone(&process.process_event_notify),
            ))
        })();
        let (runtime, notify) = match registration {
            Ok(registration) => registration,
            Err(error) => {
                self.rollback_registered_child_process_sync(
                    vm_id,
                    process_id,
                    &[],
                    &prior_child_ids,
                    Some(&identity.child_process_id),
                    Some(identity.pid),
                    "root spawnSync pending registration",
                );
                return Err(error);
            }
        };
        if let Some(deadline) = deadline {
            if let Err(error) =
                admit_child_process_sync_timer(&runtime, notify, deadline, vm_id, process_id, &[])
            {
                self.rollback_registered_child_process_sync(
                    vm_id,
                    process_id,
                    &[],
                    &prior_child_ids,
                    Some(&identity.child_process_id),
                    Some(identity.pid),
                    "root spawnSync timer admission",
                );
                return Err(error);
            }
        }
        Ok(())
    }

    pub(crate) async fn defer_javascript_child_process_sync(
        &mut self,
        vm_id: &str,
        process_id: &str,
        request: ProcessLaunchRequest,
        max_buffer: Option<usize>,
    ) -> Result<HostServiceResponse, SidecarError> {
        let (respond_to, receiver) = tokio::sync::oneshot::channel();
        self.begin_javascript_child_process_sync(
            vm_id,
            process_id,
            request,
            max_buffer,
            PendingChildProcessSyncCompletion::Javascript(respond_to),
        )
        .await?;
        Ok(HostServiceResponse::Deferred {
            receiver,
            timeout: None,
            task_class: agentos_runtime::TaskClass::Vm,
        })
    }

    /// Replace a running guest process image for execve(2) without creating a
    /// child. Resolution is deliberately performed against an empty inherited
    /// environment: execve's supplied envp replaces the old environment rather
    /// than being overlaid on it. The existing PID, process tree, cwd, stdio,
    /// and non-CLOEXEC kernel descriptors remain attached to `ActiveProcess`.
    pub(crate) fn exec_process_image(
        &mut self,
        vm_id: &str,
        root_process_id: &str,
        process_path: &[&str],
        mut request: ProcessLaunchRequest,
    ) -> Result<(), SidecarError> {
        if request.options.executable_fd.is_some() {
            return Err(SidecarError::host(
                "EINVAL",
                String::from("executableFd is only valid for process.exec_fd_image_commit"),
            ));
        }
        if request.options.shell || request.options.detached {
            return Err(SidecarError::host(
                "EINVAL",
                String::from("execve does not accept shell or detached process options"),
            ));
        }

        let (guest_cwd, host_cwd, kernel_pid, parent_kernel_pid, current_adapter_policy) = {
            let vm = self.vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
            let root = vm
                .active_processes
                .get(root_process_id)
                .ok_or_else(|| missing_process_error(vm_id, root_process_id))?;
            let process = Self::active_process_by_path(root, process_path).ok_or_else(|| {
                SidecarError::InvalidState(format!(
                    "unknown process path {} during execve",
                    Self::child_process_path_label(root_process_id, process_path)
                ))
            })?;
            let parent_kernel_pid = vm
                .kernel
                .list_processes()
                .get(&process.kernel_pid)
                .map(|entry| entry.ppid)
                .unwrap_or_default();
            (
                process.guest_cwd.clone(),
                process.host_cwd.clone(),
                process.kernel_pid,
                parent_kernel_pid,
                process.adapter_policy,
            )
        };

        if request.command.is_empty() {
            return Err(SidecarError::host(
                "ENOENT",
                String::from("execve path is empty"),
            ));
        }
        // execve resolves a relative pathname from cwd; it never searches
        // PATH. Making the command explicitly path-like keeps the shared child
        // resolver from taking its spawnp/PATH branch for a bare relative name.
        request.command = if request.command.starts_with('/') {
            normalize_path(&request.command)
        } else {
            normalize_path(&format!("{guest_cwd}/{}", request.command))
        };
        let literal_exec_path = request.command.clone();
        request.command = {
            let vm = self
                .vms
                .get_mut(vm_id)
                .ok_or_else(|| missing_vm_error(vm_id))?;
            vm.kernel
                .validate_executable_path(&literal_exec_path, &guest_cwd)
                .map_err(kernel_error)?
        };
        request.options.cwd = None;
        request.options.shell = false;
        request.options.detached = false;

        let mut resolved = {
            let vm = self
                .vms
                .get_mut(vm_id)
                .ok_or_else(|| missing_vm_error(vm_id))?;
            Self::resolve_javascript_child_process_execution_with_mode(
                vm,
                &BTreeMap::new(),
                &guest_cwd,
                &host_cwd,
                &request,
                true,
                None,
            )?
        };
        apply_child_process_argv0(&mut resolved, request.options.argv0.as_deref());
        if resolved.binding_command {
            return Err(SidecarError::host(
                "ENOEXEC",
                format!("exec format error: {}", request.command),
            ));
        }
        if request.options.local_replacement
            && current_adapter_policy.supports_prepared_in_place_exec
            && resolved.adapter_policy.supports_prepared_in_place_exec
        {
            let vm = self
                .vms
                .get_mut(vm_id)
                .ok_or_else(|| missing_vm_error(vm_id))?;
            // The runner has already compiled the replacement before it asks
            // the sidecar to commit. Recursively validate the original image
            // and every `#!` interpreter in the kernel so scripts retain
            // Linux pathname, mode, format, and recursion errors without
            // requiring the script itself to contain raw WASM bytes.
            vm.kernel
                .validate_wasm_exec_image(&request.command, &guest_cwd)
                .map_err(kernel_error)?;
        } else {
            let vm = self
                .vms
                .get_mut(vm_id)
                .ok_or_else(|| missing_vm_error(vm_id))?;
            validate_exact_exec_image_format(vm, &request.command, &resolved.runtime)?;
        }
        {
            let vm = self
                .vms
                .get_mut(vm_id)
                .ok_or_else(|| missing_vm_error(vm_id))?;
            stage_agentos_package_command(
                vm,
                &mut resolved,
                WasmLaunchAuthority::GuestProcessImage {
                    requester_pid: kernel_pid,
                },
            )?;
            stage_kernel_wasm_launch_asset(
                vm,
                &mut resolved,
                WasmLaunchAuthority::GuestProcessImage {
                    requester_pid: kernel_pid,
                },
            )?;
        }
        // Keep guest-visible envp separate from executor bootstrap variables
        // added during resolution. Both local and separate-runtime exec paths
        // must publish and inherit exactly the supplied environment.
        let replacement_guest_env = request.options.env.clone();
        let requested_exec_permission_tier = resolved
            .wasm_permission_tier
            .map(kernel_process_permission_tier)
            .unwrap_or(ProcessPermissionTier::Full);

        if request.options.local_replacement {
            if !current_adapter_policy.supports_prepared_in_place_exec
                || !resolved.adapter_policy.supports_prepared_in_place_exec
            {
                return Err(SidecarError::host(
                    "ENOEXEC",
                    format!(
                        "in-place exec only supports WebAssembly images: {}",
                        literal_exec_path
                    ),
                ));
            }
            let bridge = self.bridge.clone();
            let vm = self
                .vms
                .get_mut(vm_id)
                .ok_or_else(|| missing_vm_error(vm_id))?;
            // execve's envp is a complete replacement. `resolved.env` also
            // contains host/runtime control variables injected while locating
            // the module; those must never leak into the process's Linux-visible
            // environment or become inherited by its future children.
            let retained_internal_fds = Self::active_process_by_path(
                vm.active_processes
                    .get(root_process_id)
                    .ok_or_else(|| missing_process_error(vm_id, root_process_id))?,
                process_path,
            )
            .and_then(|process| process.kernel_stdin_writer_fd)
            .into_iter()
            .collect::<Vec<_>>();
            vm.kernel
                .exec_process_retaining_internal_fds(
                    EXECUTION_DRIVER_NAME,
                    kernel_pid,
                    WASM_COMMAND,
                    resolved.process_args.clone(),
                    replacement_guest_env.clone(),
                    resolved.guest_cwd.clone(),
                    &retained_internal_fds,
                    &request.options.cloexec_fds,
                    Some(&literal_exec_path),
                    Some(requested_exec_permission_tier),
                )
                .map_err(kernel_error)?;
            prune_managed_process_routes_without_aliases(&bridge, vm_id, vm, kernel_pid)?;

            let root = vm
                .active_processes
                .get_mut(root_process_id)
                .ok_or_else(|| missing_process_error(vm_id, root_process_id))?;
            let process =
                Self::active_process_by_path_mut(root, process_path).ok_or_else(|| {
                    SidecarError::InvalidState(format!(
                        "process disappeared during execve: {}",
                        Self::child_process_path_label(root_process_id, process_path)
                    ))
                })?;
            process.guest_cwd = resolved.guest_cwd;
            process.host_cwd = resolved.host_cwd;
            process.env = replacement_guest_env;
            process.exit_signal = None;
            process.exit_core_dumped = false;
            process.clear_deferred_kernel_wait_rpc();
            discard_exec_signal_state(process);
            process.module_resolution_cache = Default::default();
            discard_replaced_image_pending_events(process);

            return Ok(());
        }

        let bridge = self.bridge.clone();
        let vm = self
            .vms
            .get_mut(vm_id)
            .ok_or_else(|| missing_vm_error(vm_id))?;
        let mut execution_env = resolved.env.clone();
        execution_env.insert(
            String::from(EXECUTION_SANDBOX_ROOT_ENV),
            normalize_host_path(&vm.runtime_scratch_root)
                .to_string_lossy()
                .into_owned(),
        );
        let mut replacement = match resolved.runtime {
            GuestRuntimeKind::JavaScript => {
                execution_env.extend(sanitize_javascript_child_process_internal_bootstrap_env(
                    &request.options.internal_bootstrap_env,
                ));
                if !process_path.is_empty() {
                    execution_env.remove("AGENTOS_EAGER_STDIN_HANDLE");
                }
                execution_env.insert(String::from("AGENTOS_KEEP_STDIN_OPEN"), String::from("1"));
                execution_env.insert(
                    String::from("AGENTOS_FORWARD_KERNEL_STDIN_RPC"),
                    String::from("1"),
                );
                let launch_entrypoint = resolve_agentos_package_javascript_launch_entrypoint(
                    vm,
                    kernel_pid,
                    &mut execution_env,
                )?
                .unwrap_or_else(|| resolved.entrypoint.clone());
                let inline_code = load_javascript_entrypoint_source(
                    vm,
                    kernel_pid,
                    &resolved.guest_cwd,
                    &launch_entrypoint,
                    &execution_env,
                )?;
                prepare_javascript_launch_assets(
                    vm,
                    &resolved,
                    &execution_env,
                    WasmLaunchAuthority::GuestProcessImage {
                        requester_pid: kernel_pid,
                    },
                    inline_code.as_deref(),
                )?;
                let context =
                    self.javascript_engine
                        .create_context(CreateJavascriptContextRequest {
                            vm_id: vm_id.to_owned(),
                            bootstrap_module: None,
                            compile_cache_root: Some(self.cache_root.join("node-compile-cache")),
                        });
                let context_id = context.context_id;
                let replacement_result = self
                    .javascript_engine
                    .prepare_execution_with_module_reader_and_runtime(
                        StartJavascriptExecutionRequest {
                            guest_runtime: guest_runtime_identity(
                                vm,
                                Some(u64::from(kernel_pid)),
                                Some(u64::from(parent_kernel_pid)),
                            ),
                            vm_id: vm_id.to_owned(),
                            context_id: context_id.clone(),
                            argv: std::iter::once(launch_entrypoint)
                                .chain(resolved.execution_args.clone())
                                .collect(),
                            argv0: request.options.argv0.clone(),
                            env: execution_env,
                            cwd: resolved.host_cwd.clone(),
                            limits: javascript_execution_limits(vm),
                            inline_code,
                            wasm_module_bytes: None,
                        },
                        None,
                        None,
                        vm.runtime_context.clone(),
                    );
                self.javascript_engine.dispose_context(&context_id);
                ActiveExecution::Javascript(replacement_result.map_err(javascript_error)?)
            }
            GuestRuntimeKind::WebAssembly => {
                execution_env.extend(sanitize_javascript_child_process_internal_bootstrap_env(
                    &request.options.internal_bootstrap_env,
                ));
                execution_env.insert(String::from(WASM_STDIO_SYNC_RPC_ENV), String::from("1"));
                execution_env.insert(String::from(WASM_EXEC_COMMIT_RPC_ENV), String::from("1"));
                let context = self.wasm_engine.create_context(CreateWasmContextRequest {
                    vm_id: vm_id.to_owned(),
                    module_path: Some(resolved.entrypoint.clone()),
                });
                let context_id = context.context_id;
                let replacement_result =
                    self.wasm_engine
                        .prepare_execution(StartWasmExecutionRequest {
                            vm_id: vm_id.to_owned(),
                            context_id: context_id.clone(),
                            managed_kernel_host: true,
                            argv: resolved.process_args.clone(),
                            env: execution_env,
                            cwd: resolved.host_cwd.clone(),
                            permission_tier: execution_wasm_permission_tier(
                                vm.kernel
                                    .effective_exec_permission_tier(
                                        EXECUTION_DRIVER_NAME,
                                        kernel_pid,
                                        requested_exec_permission_tier,
                                    )
                                    .map_err(kernel_error)?,
                            ),
                            limits: wasm_execution_limits(vm),
                            guest_runtime: guest_runtime_identity(
                                vm,
                                Some(u64::from(kernel_pid)),
                                Some(u64::from(parent_kernel_pid)),
                            ),
                        });
                self.wasm_engine.dispose_context(&context_id);
                ActiveExecution::Wasm(Box::new(replacement_result.map_err(wasm_error)?))
            }
            GuestRuntimeKind::Python => {
                let python_file_path = if execution_env.contains_key("AGENTOS_PYTHON_ARGV") {
                    execution_env.get("AGENTOS_PYTHON_FILE").map(PathBuf::from)
                } else {
                    python_file_entrypoint(&resolved.entrypoint)
                };
                let pyodide_dist_path = self
                    .python_engine
                    .bundled_pyodide_dist_path_for_vm(vm_id)
                    .map_err(python_error)?;
                let pyodide_cache_path = pyodide_dist_path
                    .parent()
                    .and_then(Path::parent)
                    .unwrap_or(pyodide_dist_path.as_path())
                    .join("pyodide-package-cache");
                add_runtime_guest_path_mapping(
                    &mut execution_env,
                    PYTHON_PYODIDE_GUEST_ROOT,
                    &pyodide_dist_path,
                );
                add_runtime_guest_path_mapping(
                    &mut execution_env,
                    PYTHON_PYODIDE_CACHE_GUEST_ROOT,
                    &pyodide_cache_path,
                );
                add_runtime_host_access_path(
                    &mut execution_env,
                    "AGENTOS_EXTRA_FS_READ_PATHS",
                    &pyodide_dist_path,
                    true,
                );
                add_runtime_host_access_path(
                    &mut execution_env,
                    "AGENTOS_EXTRA_FS_READ_PATHS",
                    &pyodide_cache_path,
                    true,
                );
                add_runtime_host_access_path(
                    &mut execution_env,
                    "AGENTOS_EXTRA_FS_WRITE_PATHS",
                    &pyodide_cache_path,
                    false,
                );
                let context = self
                    .python_engine
                    .create_context(CreatePythonContextRequest {
                        vm_id: vm_id.to_owned(),
                        pyodide_dist_path,
                    });
                let context_id = context.context_id;
                let replacement_result =
                    self.python_engine
                        .prepare_execution(StartPythonExecutionRequest {
                            vm_id: vm_id.to_owned(),
                            context_id: context_id.clone(),
                            code: resolved.entrypoint.clone(),
                            file_path: python_file_path,
                            env: execution_env,
                            cwd: resolved.host_cwd.clone(),
                            limits: python_execution_limits(vm),
                            guest_runtime: guest_runtime_identity(
                                vm,
                                Some(u64::from(kernel_pid)),
                                Some(u64::from(parent_kernel_pid)),
                            ),
                        });
                self.python_engine.dispose_context(&context_id);
                ActiveExecution::Python(replacement_result.map_err(python_error)?)
            }
        };

        // Hard production invariant: a cross-runtime exec image must still
        // own its deferred execute payload here. This check makes it
        // impossible to accidentally regress to the old start-before-commit
        // path without failing execve before kernel state is mutated.
        if !replacement.is_prepared_for_start() {
            return Err(SidecarError::host(
                "EIO",
                String::from("cross-runtime execve replacement started before kernel commit"),
            ));
        }

        let kernel_command = resolved.adapter_policy.kernel_driver_command;
        let retained_internal_fds = Self::active_process_by_path(
            vm.active_processes
                .get(root_process_id)
                .ok_or_else(|| missing_process_error(vm_id, root_process_id))?,
            process_path,
        )
        .and_then(|process| process.kernel_stdin_writer_fd)
        .into_iter()
        .collect::<Vec<_>>();
        if let Err(error) = vm.kernel.exec_process_retaining_internal_fds(
            EXECUTION_DRIVER_NAME,
            kernel_pid,
            kernel_command,
            resolved.process_args.clone(),
            replacement_guest_env.clone(),
            resolved.guest_cwd.clone(),
            &retained_internal_fds,
            &request.options.cloexec_fds,
            Some(&literal_exec_path),
            Some(requested_exec_permission_tier),
        ) {
            let mut replacement = replacement;
            if let Err(terminate_error) = replacement.terminate() {
                tracing::warn!(
                    vm_id,
                    kernel_pid,
                    error = %terminate_error,
                    "failed to terminate prepared replacement after execve kernel commit was rejected"
                );
            }
            return Err(kernel_error(error));
        }
        prune_managed_process_routes_without_aliases(&bridge, vm_id, vm, kernel_pid)?;

        let kernel_readiness = Arc::clone(&vm.kernel_socket_readiness);
        let root = vm
            .active_processes
            .get_mut(root_process_id)
            .ok_or_else(|| missing_process_error(vm_id, root_process_id))?;
        let process = Self::active_process_by_path_mut(root, process_path).ok_or_else(|| {
            SidecarError::InvalidState(format!(
                "process disappeared during execve: {}",
                Self::child_process_path_label(root_process_id, process_path)
            ))
        })?;
        ExecutionBackend::configure_host_services(
            &mut replacement,
            process.host_capabilities.clone(),
        );
        let mut old_execution = std::mem::replace(&mut process.execution, replacement);
        process.runtime = resolved.runtime;
        process.adapter_policy = resolved.adapter_policy;
        process.guest_cwd = resolved.guest_cwd;
        process.host_cwd = resolved.host_cwd;
        process.env = replacement_guest_env;
        process.exit_signal = None;
        process.exit_core_dumped = false;
        process.clear_deferred_kernel_wait_rpc();
        discard_exec_signal_state(process);
        process.module_resolution_cache = Default::default();
        discard_replaced_image_pending_events(process);
        rebind_process_runtime_event_targets(process, &kernel_readiness);

        // The committed kernel exec operation already reset caught
        // dispositions while preserving ignored dispositions.
        let signal_key = Self::child_process_path_label(root_process_id, process_path);
        // The replacement isolate was registered and fully loaded before the
        // kernel commit, but no guest code was enqueued. Only start it now,
        // after both kernel-visible process state and sidecar-owned descriptors
        // and event targets point at the replacement image.
        #[cfg(test)]
        let replacement_start_error = if std::mem::take(&mut self.fail_next_exec_start_after_commit)
        {
            Some(SidecarError::Execution(String::from(
                "injected post-commit execve start failure",
            )))
        } else {
            process.execution.start_prepared().err()
        };
        #[cfg(not(test))]
        let replacement_start_error = process.execution.start_prepared().err();
        if let Some(error) = replacement_start_error.as_ref() {
            let message = format!("execve replacement runtime failed to start: {error}\n");
            if let Err(queue_error) = process
                .queue_pending_execution_event(ActiveExecutionEvent::Stderr(message.into_bytes()))
                .and_then(|_| {
                    process.queue_pending_execution_event(ActiveExecutionEvent::Exited(127))
                })
            {
                tracing::error!(
                    vm_id,
                    process_id = %signal_key,
                    error = %queue_error,
                    "failed to queue fatal post-commit execve start failure"
                );
            }
            process.kernel_handle.finish(127);
            if let Err(terminate_error) = process.execution.terminate() {
                tracing::error!(
                    vm_id,
                    process_id = %signal_key,
                    error = %terminate_error,
                    "failed to terminate replacement runtime after post-commit execve start failure"
                );
            }
        }
        // The old image is blocked in the exec RPC. Terminating it after the
        // atomic state swap ensures no success response can resume old code.
        if let Err(error) = old_execution.terminate() {
            tracing::warn!(
                vm_id,
                process_id = %signal_key,
                error = %error,
                "execve committed but the replaced runtime image reported a termination error"
            );
        }
        if let Some(error) = replacement_start_error {
            // The kernel execve commit is irrevocable. Linux does not return an
            // errno into the old image when a post-commit loader/start failure
            // occurs; the new process image dies. Log the typed host failure,
            // leave the queued exit for normal cleanup, and report committed
            // success to the service loop so it never replies to old code.
            tracing::error!(
                vm_id,
                process_id = %signal_key,
                error = %error,
                "execve replacement failed after commit; terminating the replacement process"
            );
        }
        Ok(())
    }

    /// Commit metadata for an fexecve image that the trusted WASM runner has
    /// already read and compiled from its live private descriptor. This route
    /// deliberately performs no pathname resolution or reopen: the descriptor
    /// may name an unlinked file, and the runner owns that open-file identity.
    pub(crate) fn commit_wasm_fd_process_image(
        &mut self,
        vm_id: &str,
        root_process_id: &str,
        process_path: &[&str],
        request: ProcessLaunchRequest,
    ) -> Result<(), SidecarError> {
        validate_wasm_fd_image_commit_request(&request)?;

        let (kernel_pid, retained_internal_fds) = {
            let vm = self.vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
            let root = vm
                .active_processes
                .get(root_process_id)
                .ok_or_else(|| missing_process_error(vm_id, root_process_id))?;
            let process = Self::active_process_by_path(root, process_path).ok_or_else(|| {
                SidecarError::InvalidState(format!(
                    "unknown process path {} during fexecve commit",
                    Self::child_process_path_label(root_process_id, process_path)
                ))
            })?;
            if !process.adapter_policy.supports_prepared_in_place_exec {
                return Err(SidecarError::host(
                    "ENOEXEC",
                    String::from(
                        "fd-image exec commit requires an adapter with prepared in-place exec",
                    ),
                ));
            }
            (
                process.kernel_pid,
                process
                    .kernel_stdin_writer_fd
                    .into_iter()
                    .collect::<Vec<_>>(),
            )
        };

        let mut argv = Vec::with_capacity(request.args.len().saturating_add(1));
        argv.push(
            request
                .options
                .argv0
                .clone()
                .unwrap_or_else(|| request.command.clone()),
        );
        argv.extend(request.args);
        let replacement_guest_env = request.options.env;

        let bridge = self.bridge.clone();
        let vm = self
            .vms
            .get_mut(vm_id)
            .ok_or_else(|| missing_vm_error(vm_id))?;
        vm.kernel
            .exec_process_retaining_internal_fds(
                EXECUTION_DRIVER_NAME,
                kernel_pid,
                WASM_COMMAND,
                argv,
                replacement_guest_env.clone(),
                String::new(),
                &retained_internal_fds,
                &request.options.cloexec_fds,
                None,
                None,
            )
            .map_err(kernel_error)?;
        prune_managed_process_routes_without_aliases(&bridge, vm_id, vm, kernel_pid)?;

        let root = vm
            .active_processes
            .get_mut(root_process_id)
            .ok_or_else(|| missing_process_error(vm_id, root_process_id))?;
        let process = Self::active_process_by_path_mut(root, process_path).ok_or_else(|| {
            SidecarError::InvalidState(format!(
                "process disappeared during fexecve commit: {}",
                Self::child_process_path_label(root_process_id, process_path)
            ))
        })?;
        process.env = replacement_guest_env;
        process.exit_signal = None;
        process.exit_core_dumped = false;
        process.clear_deferred_kernel_wait_rpc();
        discard_exec_signal_state(process);
        process.module_resolution_cache = Default::default();
        discard_replaced_image_pending_events(process);
        Ok(())
    }

    async fn spawn_descendant_process(
        &mut self,
        vm_id: &str,
        process_id: &str,
        current_process_path: &[&str],
        mut request: ProcessLaunchRequest,
    ) -> Result<Value, SidecarError> {
        let spawn_attributes = javascript_spawn_attributes(&request.options)?;
        let requested_pgid = spawn_attributes.process_group;
        let current_process_label =
            Self::child_process_path_label(process_id, current_process_path);
        let prepared_host_net_fds = {
            let vm = self
                .vms
                .get_mut(vm_id)
                .ok_or_else(|| missing_vm_error(vm_id))?;
            let current_network_counts = vm_spawn_host_net_resource_counts(vm);
            let managed_descriptions = Arc::clone(&vm.managed_host_net_descriptions);
            let (kernel, active_processes) = (&mut vm.kernel, &mut vm.active_processes);
            let root = active_processes
                .get_mut(process_id)
                .ok_or_else(|| missing_process_error(vm_id, process_id))?;
            let parent =
                Self::active_process_by_path_mut(root, current_process_path).ok_or_else(|| {
                    SidecarError::InvalidState(format!(
                        "unknown child process path {} during host-network fd inheritance",
                        Self::child_process_path_label(process_id, current_process_path)
                    ))
                })?;
            prepare_spawn_host_net_fds(
                kernel,
                parent,
                &managed_descriptions,
                current_network_counts,
                &request.options.spawn_host_net_fds,
                &request.options.spawn_fd_mappings,
                &request.options.spawn_file_actions,
            )?
        };
        let prepared_spawn_actions = if !prepared_host_net_fds.kernel_actions.is_empty() {
            let (parent_pid, parent_cwd) = {
                let vm = self.vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
                let root = vm
                    .active_processes
                    .get(process_id)
                    .ok_or_else(|| missing_process_error(vm_id, process_id))?;
                let parent =
                    Self::active_process_by_path(root, current_process_path).ok_or_else(|| {
                        SidecarError::InvalidState(format!(
                            "unknown child process path {} during spawn file actions",
                            Self::child_process_path_label(process_id, current_process_path)
                        ))
                    })?;
                let initial_cwd = request
                    .options
                    .cwd
                    .as_deref()
                    .map(|cwd| {
                        if cwd.starts_with('/') {
                            normalize_path(cwd)
                        } else {
                            normalize_path(&format!("{}/{cwd}", parent.guest_cwd))
                        }
                    })
                    .unwrap_or_else(|| parent.guest_cwd.clone());
                (parent.kernel_pid, initial_cwd)
            };
            let vm = self
                .vms
                .get_mut(vm_id)
                .ok_or_else(|| missing_vm_error(vm_id))?;
            Some(preapply_posix_spawn_file_actions(
                &mut vm.kernel,
                parent_pid,
                &parent_cwd,
                requested_pgid,
                &request.options.spawn_fd_mappings,
                &prepared_host_net_fds.kernel_actions,
            )?)
        } else {
            None
        };
        if let Some(prepared) = prepared_spawn_actions.as_ref() {
            request.options.cwd = Some(prepared.cwd.clone());
        }
        {
            let parent_guest_cwd = {
                let vm = self.vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
                let root = vm
                    .active_processes
                    .get(process_id)
                    .ok_or_else(|| missing_process_error(vm_id, process_id))?;
                Self::active_process_by_path(root, current_process_path)
                    .ok_or_else(|| {
                        SidecarError::InvalidState(format!(
                            "unknown child process path {} during program resolution",
                            Self::child_process_path_label(process_id, current_process_path)
                        ))
                    })?
                    .guest_cwd
                    .clone()
            };
            let vm = self
                .vms
                .get_mut(vm_id)
                .ok_or_else(|| missing_vm_error(vm_id))?;
            resolve_posix_spawn_program(vm, &parent_guest_cwd, &mut request)?;
        }
        let total_start = Instant::now();
        let process_event_capacity = self.config.runtime.protocol.max_process_events;
        let phase_start = Instant::now();
        let (parent_env, parent_guest_cwd, parent_host_cwd, parent_kernel_pid) = {
            let vm = self.vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
            let root = vm
                .active_processes
                .get(process_id)
                .ok_or_else(|| missing_process_error(vm_id, process_id))?;
            let parent =
                Self::active_process_by_path(root, current_process_path).ok_or_else(|| {
                    SidecarError::InvalidState(format!(
                        "unknown child process path {current_process_label} during nested spawn"
                    ))
                })?;
            (
                parent.env.clone(),
                parent.guest_cwd.clone(),
                parent.host_cwd.clone(),
                parent.kernel_pid,
            )
        };
        let mut resolved =
            if !request.options.spawn_exact_path && request.options.spawn_search_path.is_none() {
                self.resolve_javascript_child_process_with_shebang(
                    vm_id,
                    &parent_env,
                    &parent_guest_cwd,
                    &parent_host_cwd,
                    &mut request,
                )?
            } else {
                let vm = self
                    .vms
                    .get_mut(vm_id)
                    .ok_or_else(|| missing_vm_error(vm_id))?;
                Self::resolve_javascript_child_process_execution_with_mode(
                    vm,
                    &parent_env,
                    &parent_guest_cwd,
                    &parent_host_cwd,
                    &request,
                    request.options.spawn_exact_path,
                    request.options.spawn_search_path.as_deref(),
                )?
            };
        apply_child_process_argv0(&mut resolved, request.options.argv0.as_deref());
        {
            let vm = self
                .vms
                .get_mut(vm_id)
                .ok_or_else(|| missing_vm_error(vm_id))?;
            stage_agentos_package_command(
                vm,
                &mut resolved,
                WasmLaunchAuthority::GuestProcessImage {
                    requester_pid: parent_kernel_pid,
                },
            )?;
            stage_kernel_wasm_launch_asset(
                vm,
                &mut resolved,
                WasmLaunchAuthority::GuestProcessImage {
                    requester_pid: parent_kernel_pid,
                },
            )?;
        }
        tracing::debug!(
            vm_id,
            process_id,
            parent = %current_process_label,
            command = %resolved.command,
            runtime = ?resolved.runtime,
            entrypoint = %resolved.entrypoint,
            execution_args = ?resolved.execution_args,
            parent_guest_cwd = %parent_guest_cwd,
            requested_cwd = ?request.options.cwd,
            guest_cwd = %resolved.guest_cwd,
            host_cwd = %resolved.host_cwd.display(),
            "resolved nested JavaScript child process"
        );
        let resolved = resolved;
        if prepared_host_net_fds.inherited_fd_count() != 0
            && !resolved.adapter_policy.accepts_inherited_host_network_fds
        {
            return Err(SidecarError::host(
                "ENOTSUP",
                String::from("inherited host-network fds require a WebAssembly child runtime"),
            ));
        }
        record_execute_phase("child_process_resolve_execution", phase_start.elapsed());
        let sidecar_requests = self.sidecar_requests.clone();
        let vm = self
            .vms
            .get_mut(vm_id)
            .ok_or_else(|| missing_vm_error(vm_id))?;
        let vm_pending_stdin_bytes_budget = Arc::clone(&vm.pending_stdin_bytes_budget);
        let vm_pending_event_bytes_budget = Arc::clone(&vm.pending_event_bytes_budget);
        let phase_start = Instant::now();
        let child_process_id = {
            let root = vm
                .active_processes
                .get_mut(process_id)
                .ok_or_else(|| missing_process_error(vm_id, process_id))?;
            let parent =
                Self::active_process_by_path_mut(root, current_process_path).ok_or_else(|| {
                    SidecarError::InvalidState(format!(
                        "unknown child process path {current_process_label} during nested spawn"
                    ))
                })?;
            parent.allocate_child_process_id()
        };
        let mut child_path = current_process_path.to_vec();
        child_path.push(child_process_id.as_str());
        let mut pending_kernel_handle = None;
        let spawn_result = (async {
            let spawned = if resolved.binding_command {
                let binding_resolution = resolve_binding_command(
                    vm,
                    &resolved.command,
                    &resolved.execution_args,
                    Some(&resolved.guest_cwd),
                )?
                .ok_or_else(|| {
                    SidecarError::InvalidState(format!(
                        "binding command no longer resolves: {}",
                        resolved.command
                    ))
                })?;
                let kernel_handle = vm
                    .kernel
                    .create_virtual_process_with_process_group(
                        EXECUTION_DRIVER_NAME,
                        BINDING_DRIVER_NAME,
                        &resolved.command,
                        resolved.process_args.clone(),
                        VirtualProcessOptions {
                            parent_pid: Some(parent_kernel_pid),
                            env: resolved.env.clone(),
                            cwd: Some(resolved.guest_cwd.clone()),
                            permission_tier: resolved
                                .wasm_permission_tier
                                .map(kernel_process_permission_tier),
                        },
                        requested_pgid,
                    )
                    .map_err(kernel_error)?;
                let kernel_pid = kernel_handle.pid();
                if let Some(prepared) = prepared_spawn_actions {
                    install_preapplied_posix_spawn_file_actions(
                        &mut vm.kernel,
                        &kernel_handle,
                        prepared,
                    )?;
                } else {
                    apply_posix_spawn_file_actions_or_rollback(
                        &mut vm.kernel,
                        &kernel_handle,
                        &resolved.guest_cwd,
                        &request.options.spawn_fd_mappings,
                        &prepared_host_net_fds.kernel_actions,
                    )?;
                }
                apply_spawn_session_or_rollback(
                    &mut vm.kernel,
                    &kernel_handle,
                    spawn_attributes.new_session || request.options.detached,
                )?;
                apply_spawn_process_attributes_or_rollback(
                    &mut vm.kernel,
                    &kernel_handle,
                    &request.options,
                )?;
                pending_kernel_handle = Some(kernel_handle.clone());
                let runtime_control = ActiveProcess::attach_runtime_control_before_start(
                    &kernel_handle,
                    Arc::clone(&self.process_event_notify),
                )?;
                let binding_execution = BindingExecution::with_event_notify(
                    Arc::clone(&self.process_event_notify),
                    process_event_capacity,
                )
                .with_vm_pending_event_bytes_budget(Arc::clone(&vm_pending_event_bytes_budget));
                let cancelled = binding_execution.cancelled.clone();
                let pending_events = binding_execution.pending_events.clone();
                let event_overflow_reason = binding_execution.event_overflow_reason.clone();
                let pending_event_bytes = binding_execution.pending_event_bytes.clone();
                let pending_event_count_limit = binding_execution.pending_event_count_limit.clone();
                let pending_event_bytes_limit = binding_execution.pending_event_bytes_limit.clone();
                let binding_vm_pending_event_bytes_budget =
                    binding_execution.vm_pending_event_bytes_budget.clone();
                let event_notify = binding_execution.event_notify.clone();
                let binding_event_request = BindingProcessEventRequest {
                    runtime_context: vm.runtime_context.clone(),
                    sidecar_requests: sidecar_requests.clone(),
                    connection_id: vm.connection_id.clone(),
                    session_id: vm.session_id.clone(),
                    vm_id: vm_id.to_owned(),
                    binding_resolution,
                    cancelled,
                    paused: Arc::clone(&binding_execution.paused),
                    pause_notify: Arc::clone(&binding_execution.pause_notify),
                    pending_events,
                    event_overflow_reason,
                    pending_event_bytes,
                    pending_event_count_limit,
                    pending_event_bytes_limit,
                    vm_pending_event_bytes_budget: binding_vm_pending_event_bytes_budget,
                    event_notify,
                };
                (
                    kernel_pid,
                    kernel_handle,
                    ActiveExecution::Binding(binding_execution),
                    runtime_control,
                    Some(binding_event_request),
                    None,
                    0,
                    false,
                )
            } else {
                let kernel_command = resolved.adapter_policy.kernel_driver_command;
                let kernel_handle = vm
                    .kernel
                    .spawn_process_with_process_group(
                        kernel_command,
                        resolved.process_args.clone(),
                        SpawnOptions {
                            requester_driver: Some(String::from(EXECUTION_DRIVER_NAME)),
                            parent_pid: Some(parent_kernel_pid),
                            env: resolved.env.clone(),
                            cwd: Some(resolved.guest_cwd.clone()),
                            permission_tier: resolved
                                .wasm_permission_tier
                                .map(kernel_process_permission_tier),
                        },
                        requested_pgid,
                    )
                    .map_err(kernel_error)?;
                let kernel_pid = kernel_handle.pid();
                let applied_spawn_actions = if let Some(prepared) = prepared_spawn_actions {
                    install_preapplied_posix_spawn_file_actions(
                        &mut vm.kernel,
                        &kernel_handle,
                        prepared,
                    )?
                } else {
                    apply_posix_spawn_file_actions_or_rollback(
                        &mut vm.kernel,
                        &kernel_handle,
                        &resolved.guest_cwd,
                        &request.options.spawn_fd_mappings,
                        &prepared_host_net_fds.kernel_actions,
                    )?
                };
                let posix_spawn_controls_stdin = !request.options.spawn_file_actions.is_empty()
                    && (applied_spawn_actions
                        .fd_mappings
                        .iter()
                        .any(|mapping| mapping[0] == 0)
                        || applied_spawn_actions.closed_guest_fds.contains(&0)
                        || prepared_host_net_fds
                            .descriptions
                            .iter()
                            .any(|description| description.guest_fds.contains(&0)));
                if resolved.adapter_policy.materializes_direct_runtime_stdio {
                    materialize_direct_runtime_stdio_mappings(
                        &mut vm.kernel,
                        kernel_pid,
                        &applied_spawn_actions,
                    )?;
                }
                let kernel_stdin_reader_fd = if resolved.adapter_policy.canonicalizes_runtime_stdin
                {
                    canonicalize_host_runtime_posix_stdin(
                        &mut vm.kernel,
                        kernel_pid,
                        &applied_spawn_actions,
                    )?
                } else {
                    0
                };
                apply_spawn_session_or_rollback(
                    &mut vm.kernel,
                    &kernel_handle,
                    spawn_attributes.new_session || request.options.detached,
                )?;
                apply_spawn_process_attributes_or_rollback(
                    &mut vm.kernel,
                    &kernel_handle,
                    &request.options,
                )?;
                pending_kernel_handle = Some(kernel_handle.clone());
                let mut execution_env = resolved.env.clone();
                if resolved.adapter_policy.forwards_kernel_stdin_rpc {
                    execution_env.insert(
                        String::from("AGENTOS_FORWARD_KERNEL_STDIN_RPC"),
                        String::from("1"),
                    );
                }
                if resolved.adapter_policy.encodes_inherited_fd_bootstrap {
                    execution_env.insert(
                        String::from("AGENTOS_WASM_INHERITED_FD_MAPPINGS"),
                        serde_json::to_string(&applied_spawn_actions.fd_mappings).map_err(
                            |error| {
                                SidecarError::InvalidState(format!(
                                    "failed to serialize inherited WASM fd mappings: {error}"
                                ))
                            },
                        )?,
                    );
                    execution_env.insert(
                        String::from("AGENTOS_WASM_CLOSED_INHERITED_FDS"),
                        serde_json::to_string(&applied_spawn_actions.closed_guest_fds).map_err(
                            |error| {
                                SidecarError::InvalidState(format!(
                                    "failed to serialize closed inherited WASM fds: {error}"
                                ))
                            },
                        )?,
                    );
                    execution_env.insert(
                        String::from("AGENTOS_WASM_INHERITED_HOSTNET_FDS"),
                        serde_json::to_string(&prepared_host_net_fds.bootstrap_json()).map_err(
                            |error| {
                                SidecarError::InvalidState(format!(
                                    "failed to serialize inherited WASM host-network fds: {error}"
                                ))
                            },
                        )?,
                    );
                }
                execution_env.insert(
                    String::from(EXECUTION_SANDBOX_ROOT_ENV),
                    normalize_host_path(&vm.runtime_scratch_root)
                        .to_string_lossy()
                        .into_owned(),
                );
                let (execution, runtime_control) = match resolved.runtime {
                    GuestRuntimeKind::JavaScript => {
                        execution_env.extend(
                            sanitize_javascript_child_process_internal_bootstrap_env(
                                &request.options.internal_bootstrap_env,
                            ),
                        );
                        execution_env.remove("AGENTOS_EAGER_STDIN_HANDLE");
                        execution_env
                            .insert(String::from("AGENTOS_KEEP_STDIN_OPEN"), String::from("1"));
                        execution_env.insert(
                            String::from("AGENTOS_FORWARD_KERNEL_STDIN_RPC"),
                            String::from("1"),
                        );
                        let launch_entrypoint =
                            resolve_agentos_package_javascript_launch_entrypoint(
                                vm,
                                kernel_pid,
                                &mut execution_env,
                            )?
                            .unwrap_or_else(|| resolved.entrypoint.clone());
                        let inline_code = load_javascript_entrypoint_source(
                            vm,
                            kernel_pid,
                            &resolved.guest_cwd,
                            &launch_entrypoint,
                            &execution_env,
                        )?;
                        prepare_javascript_launch_assets(
                            vm,
                            &resolved,
                            &execution_env,
                            WasmLaunchAuthority::GuestProcessImage {
                                requester_pid: kernel_pid,
                            },
                            inline_code.as_deref(),
                        )?;
                        let context =
                            self.javascript_engine
                                .create_context(CreateJavascriptContextRequest {
                                    vm_id: vm_id.to_owned(),
                                    bootstrap_module: None,
                                    compile_cache_root: Some(
                                        self.cache_root.join("node-compile-cache"),
                                    ),
                                });
                        let context_id = context.context_id;
                        let runtime_control = ActiveProcess::attach_runtime_control_before_start(
                            &kernel_handle,
                            Arc::clone(&self.process_event_notify),
                        )?;
                        let execution_result = self
                            .javascript_engine
                            .start_execution_with_module_reader_and_runtime(
                                StartJavascriptExecutionRequest {
                                    guest_runtime: guest_runtime_identity(
                                        vm,
                                        Some(u64::from(kernel_pid)),
                                        Some(u64::from(parent_kernel_pid)),
                                    ),
                                    vm_id: vm_id.to_owned(),
                                    context_id: context_id.clone(),
                                    argv: std::iter::once(launch_entrypoint)
                                        .chain(resolved.execution_args.clone())
                                        .collect(),
                                    argv0: request.options.argv0.clone(),
                                    env: execution_env,
                                    cwd: resolved.host_cwd.clone(),
                                    limits: javascript_execution_limits(vm),
                                    inline_code,
                                    wasm_module_bytes: None,
                                },
                                None,
                                None,
                                vm.runtime_context.clone(),
                            );
                        self.javascript_engine.dispose_context(&context_id);
                        let execution = execution_result.map_err(javascript_error)?;
                        (ActiveExecution::Javascript(execution), runtime_control)
                    }
                    GuestRuntimeKind::WebAssembly => {
                        execution_env.extend(
                            sanitize_javascript_child_process_internal_bootstrap_env(
                                &request.options.internal_bootstrap_env,
                            ),
                        );
                        execution_env
                            .insert(String::from(WASM_STDIO_SYNC_RPC_ENV), String::from("1"));
                        execution_env
                            .insert(String::from(WASM_EXEC_COMMIT_RPC_ENV), String::from("1"));
                        let wasm_limits = wasm_execution_limits(vm);
                        let wasm_guest_runtime = guest_runtime_identity(
                            vm,
                            Some(u64::from(kernel_pid)),
                            Some(u64::from(parent_kernel_pid)),
                        );
                        let context = self.wasm_engine.create_context(CreateWasmContextRequest {
                            vm_id: vm_id.to_owned(),
                            module_path: Some(resolved.entrypoint.clone()),
                        });
                        let context_id = context.context_id;
                        let runtime_control = ActiveProcess::attach_runtime_control_before_start(
                            &kernel_handle,
                            Arc::clone(&self.process_event_notify),
                        )?;
                        let execution_result = self
                            .wasm_engine
                            .start_execution_with_runtime_async(
                                StartWasmExecutionRequest {
                                    vm_id: vm_id.to_owned(),
                                    context_id: context_id.clone(),
                                    managed_kernel_host: true,
                                    argv: resolved.process_args.clone(),
                                    env: execution_env,
                                    cwd: resolved.host_cwd.clone(),
                                    permission_tier: execution_wasm_permission_tier(
                                        vm.kernel
                                            .process_permission_tier(
                                                EXECUTION_DRIVER_NAME,
                                                kernel_pid,
                                            )
                                            .map_err(kernel_error)?,
                                    ),
                                    limits: wasm_limits,
                                    guest_runtime: wasm_guest_runtime,
                                },
                                vm.runtime_context.clone(),
                            )
                            .await;
                        self.wasm_engine.dispose_context(&context_id);
                        let execution = execution_result.map_err(wasm_error)?;
                        (ActiveExecution::Wasm(Box::new(execution)), runtime_control)
                    }
                    GuestRuntimeKind::Python => {
                        // Nested `python` child_process: set up the Pyodide context the
                        // same way the top-level execute path does, so a guest shell or
                        // node parent can spawn `python` exactly like `node`.
                        let python_file_path = if execution_env.contains_key("AGENTOS_PYTHON_ARGV")
                        {
                            execution_env.get("AGENTOS_PYTHON_FILE").map(PathBuf::from)
                        } else {
                            python_file_entrypoint(&resolved.entrypoint)
                        };
                        let pyodide_dist_path = self
                            .python_engine
                            .bundled_pyodide_dist_path_for_vm(vm_id)
                            .map_err(python_error)?;
                        let pyodide_cache_path = pyodide_dist_path
                            .parent()
                            .and_then(Path::parent)
                            .unwrap_or(pyodide_dist_path.as_path())
                            .join("pyodide-package-cache");
                        add_runtime_guest_path_mapping(
                            &mut execution_env,
                            PYTHON_PYODIDE_GUEST_ROOT,
                            &pyodide_dist_path,
                        );
                        add_runtime_guest_path_mapping(
                            &mut execution_env,
                            PYTHON_PYODIDE_CACHE_GUEST_ROOT,
                            &pyodide_cache_path,
                        );
                        add_runtime_host_access_path(
                            &mut execution_env,
                            "AGENTOS_EXTRA_FS_READ_PATHS",
                            &pyodide_dist_path,
                            true,
                        );
                        add_runtime_host_access_path(
                            &mut execution_env,
                            "AGENTOS_EXTRA_FS_READ_PATHS",
                            &pyodide_cache_path,
                            true,
                        );
                        add_runtime_host_access_path(
                            &mut execution_env,
                            "AGENTOS_EXTRA_FS_WRITE_PATHS",
                            &pyodide_cache_path,
                            false,
                        );
                        let context =
                            self.python_engine
                                .create_context(CreatePythonContextRequest {
                                    vm_id: vm_id.to_owned(),
                                    pyodide_dist_path,
                                });
                        let context_id = context.context_id;
                        let runtime_control = ActiveProcess::attach_runtime_control_before_start(
                            &kernel_handle,
                            Arc::clone(&self.process_event_notify),
                        )?;
                        let execution_result = self
                            .python_engine
                            .start_execution_with_runtime_async(
                                StartPythonExecutionRequest {
                                    vm_id: vm_id.to_owned(),
                                    context_id: context_id.clone(),
                                    code: resolved.entrypoint.clone(),
                                    file_path: python_file_path,
                                    env: execution_env,
                                    cwd: resolved.host_cwd.clone(),
                                    limits: python_execution_limits(vm),
                                    guest_runtime: guest_runtime_identity(
                                        vm,
                                        Some(u64::from(kernel_pid)),
                                        Some(u64::from(parent_kernel_pid)),
                                    ),
                                },
                                vm.runtime_context.clone(),
                            )
                            .await;
                        self.python_engine.dispose_context(&context_id);
                        let execution = execution_result.map_err(python_error)?;
                        (ActiveExecution::Python(execution), runtime_control)
                    }
                };
                let kernel_stdin_writer_fd = if posix_spawn_controls_stdin {
                    None
                } else {
                    match javascript_child_process_stdin_mode(&request) {
                        "pipe" => Some(install_kernel_stdin_pipe(&mut vm.kernel, kernel_pid)?),
                        "ignore" => {
                            install_kernel_ignored_stdin(&mut vm.kernel, kernel_pid)?;
                            None
                        }
                        "inherit" => None,
                        _ => Some(install_kernel_stdin_pipe(&mut vm.kernel, kernel_pid)?),
                    }
                };
                (
                    kernel_pid,
                    kernel_handle,
                    execution,
                    runtime_control,
                    None,
                    kernel_stdin_writer_fd,
                    kernel_stdin_reader_fd,
                    posix_spawn_controls_stdin,
                )
            };
            Ok::<_, SidecarError>(spawned)
        })
        .await;
        let (
            kernel_pid,
            kernel_handle,
            mut execution,
            runtime_control,
            binding_event_request,
            kernel_stdin_writer_fd,
            kernel_stdin_reader_fd,
            direct_posix_stdin,
        ) = match spawn_result {
            Ok(spawned) => spawned,
            Err(error) => {
                if let Some(process) = pending_kernel_handle.take() {
                    rollback_unregistered_spawn_child(
                        &mut vm.kernel,
                        &process,
                        None,
                        "nested child_process.spawn",
                    );
                }
                return Err(error);
            }
        };
        record_execute_phase(
            "child_process_spawn_and_start_execution",
            phase_start.elapsed(),
        );

        let phase_start = Instant::now();
        let managed_descriptions = Arc::clone(&vm.managed_host_net_descriptions);
        let mut managed_description_guard = match managed_descriptions.lock() {
            Ok(descriptions) => descriptions,
            Err(_) => {
                if let Some(process) = pending_kernel_handle.take() {
                    rollback_unregistered_spawn_child(
                        &mut vm.kernel,
                        &process,
                        Some(&mut execution),
                        "nested child_process.spawn managed-description preflight",
                    );
                }
                return Err(SidecarError::host(
                    "EIO",
                    "managed description registry lock poisoned",
                ));
            }
        };
        if let Err(error) =
            prepared_host_net_fds.validate_install(&managed_description_guard, kernel_pid)
        {
            if let Some(process) = pending_kernel_handle.take() {
                rollback_unregistered_spawn_child(
                    &mut vm.kernel,
                    &process,
                    Some(&mut execution),
                    "nested child_process.spawn managed-description preflight",
                );
            }
            return Err(error);
        }
        let child_fd1_is_tty = match vm.kernel.isatty(EXECUTION_DRIVER_NAME, kernel_pid, 1) {
            Ok(is_tty) => is_tty,
            Err(error) if error.code() == "EBADF" => false,
            Err(error) => {
                if let Some(process) = pending_kernel_handle.take() {
                    rollback_unregistered_spawn_child(
                        &mut vm.kernel,
                        &process,
                        Some(&mut execution),
                        "nested child_process.spawn tty preflight",
                    );
                }
                return Err(kernel_error(error));
            }
        };
        let child_process_group = match vm.kernel.getpgid(EXECUTION_DRIVER_NAME, kernel_pid) {
            Ok(process_group) => process_group,
            Err(error) => {
                if let Some(process) = pending_kernel_handle.take() {
                    rollback_unregistered_spawn_child(
                        &mut vm.kernel,
                        &process,
                        Some(&mut execution),
                        "nested child_process.spawn",
                    );
                }
                return Err(kernel_error(error));
            }
        };
        let process_event_limits = vm.limits.process.clone();
        let root = match vm.active_processes.get_mut(process_id) {
            Some(root) => root,
            None => {
                let error = missing_process_error(vm_id, process_id);
                if let Some(child) = pending_kernel_handle.take() {
                    rollback_unregistered_spawn_child(
                        &mut vm.kernel,
                        &child,
                        Some(&mut execution),
                        "nested child_process.spawn",
                    );
                }
                return Err(error);
            }
        };
        let parent = match Self::active_process_by_path_mut(root, current_process_path) {
            Some(parent) => parent,
            None => {
                let error = SidecarError::InvalidState(format!(
                    "unknown child process path {current_process_label} during nested spawn"
                ));
                if let Some(child) = pending_kernel_handle.take() {
                    rollback_unregistered_spawn_child(
                        &mut vm.kernel,
                        &child,
                        Some(&mut execution),
                        "nested child_process.spawn",
                    );
                }
                return Err(error);
            }
        };
        let inherited_tty_master_owner = if child_fd1_is_tty {
            parent
                .tty_master_fd
                .map(|master_fd| (parent.kernel_pid, master_fd))
                .or(parent.tty_master_owner)
        } else {
            None
        };
        let child_process_bridge_owns_output = !request.options.stdio.is_empty()
            && parent.execution.descendant_output_ownership()
                == DescendantOutputOwnership::SidecarBridge;
        let mut child = ActiveProcess::new_with_attached_runtime_control(
            kernel_pid,
            kernel_handle,
            parent.runtime_context.clone(),
            parent.limits.clone(),
            process_event_capacity,
            resolved.runtime,
            execution,
            runtime_control,
            Arc::clone(&self.process_event_notify),
        )
        .with_adapter_policy(resolved.adapter_policy)
        .with_process_event_limits(&process_event_limits)
        .with_vm_pending_byte_budgets(
            Arc::clone(&vm_pending_stdin_bytes_budget),
            Arc::clone(&vm_pending_event_bytes_budget),
        )
        .with_detached(request.options.detached)
        .with_guest_cwd(resolved.guest_cwd.clone())
        .with_env(resolved.env.clone())
        .with_host_cwd(resolved.host_cwd.clone());
        child.child_process_bridge_owns_output = child_process_bridge_owns_output;
        child.tty_master_owner = inherited_tty_master_owner;
        child.direct_posix_stdin = direct_posix_stdin;
        child.kernel_stdin_reader_fd = kernel_stdin_reader_fd;
        if let Some(kernel_stdin_writer_fd) = kernel_stdin_writer_fd {
            child.kernel_stdin_writer_fd = Some(kernel_stdin_writer_fd);
        }
        prepared_host_net_fds.install(&mut child, &mut managed_description_guard);
        if let Err(error) = child.apply_runtime_controls() {
            if let Some(rollback_handle) = pending_kernel_handle.take() {
                rollback_unregistered_spawn_child(
                    &mut vm.kernel,
                    &rollback_handle,
                    Some(&mut child.execution),
                    "nested child_process.spawn pending runtime control",
                );
            }
            return Err(error);
        }
        pending_kernel_handle.take();
        parent
            .child_processes
            .insert(child_process_id.clone(), child);
        if let Some(binding_event_request) = binding_event_request {
            spawn_binding_process_events(binding_event_request);
        }
        record_execute_phase("child_process_register", phase_start.elapsed());
        record_execute_phase("child_process_spawn_total", total_start.elapsed());
        Ok(json!({
            "childId": child_process_id,
            "pid": kernel_pid,
            "pgid": child_process_group,
            "directPosixStdin": direct_posix_stdin,
            "command": resolved.command,
            "args": resolved.process_args,
        }))
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) async fn spawn_descendant_process_for_test(
        &mut self,
        vm_id: &str,
        process_id: &str,
        current_process_path: &[&str],
        request: ProcessLaunchRequest,
    ) -> Result<Value, SidecarError> {
        self.spawn_descendant_process(vm_id, process_id, current_process_path, request)
            .await
    }

    async fn begin_descendant_child_process_sync(
        &mut self,
        vm_id: &str,
        process_id: &str,
        current_process_path: &[&str],
        request: ProcessLaunchRequest,
        max_buffer: Option<usize>,
        completion: PendingChildProcessSyncCompletion,
    ) -> Result<(), SidecarError> {
        let max_buffer = {
            let vm = self.vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
            let root = vm
                .active_processes
                .get(process_id)
                .ok_or_else(|| missing_process_error(vm_id, process_id))?;
            let parent =
                Self::active_process_by_path(root, current_process_path).ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "unknown child process path during nested spawnSync",
                    ))
                })?;
            Self::child_process_sync_max_buffer(parent, max_buffer)?
        };
        let deadline = checked_child_process_sync_deadline(request.options.timeout)?;
        let sync_input = javascript_child_process_sync_input_bytes(request.options.input.as_ref())?;
        let (count_reservation, bytes_reservation) = {
            let vm = self.vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
            reserve_child_process_sync_budget(
                &vm.pending_child_sync_count_budget,
                &vm.pending_child_sync_bytes_budget,
                max_buffer,
                sync_input.as_deref().map_or(0, <[u8]>::len),
            )?
        };
        let timeout_signal = request
            .options
            .kill_signal
            .clone()
            .unwrap_or_else(|| String::from("SIGTERM"));
        let prior_child_ids =
            self.child_process_ids_at_path(vm_id, process_id, current_process_path)?;
        let spawned = self
            .spawn_descendant_process(vm_id, process_id, current_process_path, request)
            .await?;
        let identity = match parse_spawned_child_identity(&spawned) {
            Ok(identity) => identity,
            Err(error) => {
                let hinted_child_id = spawned.get("childId").and_then(Value::as_str);
                let hinted_pid = spawned
                    .get("pid")
                    .and_then(Value::as_u64)
                    .and_then(|pid| u32::try_from(pid).ok());
                self.rollback_registered_child_process_sync(
                    vm_id,
                    process_id,
                    current_process_path,
                    &prior_child_ids,
                    hinted_child_id,
                    hinted_pid,
                    "nested spawnSync response parsing",
                );
                return Err(error);
            }
        };

        if let Some(input) = sync_input.as_deref() {
            if let Err(error) = self.write_descendant_process_stdin(
                vm_id,
                process_id,
                current_process_path,
                &identity.child_process_id,
                input,
            ) {
                self.rollback_registered_child_process_sync(
                    vm_id,
                    process_id,
                    current_process_path,
                    &prior_child_ids,
                    Some(&identity.child_process_id),
                    Some(identity.pid),
                    "nested spawnSync stdin write",
                );
                return Err(error);
            }
        }
        if let Err(error) = self.close_descendant_process_stdin(
            vm_id,
            process_id,
            current_process_path,
            &identity.child_process_id,
        ) {
            self.rollback_registered_child_process_sync(
                vm_id,
                process_id,
                current_process_path,
                &prior_child_ids,
                Some(&identity.child_process_id),
                Some(identity.pid),
                "nested spawnSync stdin close",
            );
            return Err(error);
        }

        let pending = PendingChildProcessSync {
            pid: identity.pid,
            stdout: Vec::new(),
            stderr: Vec::new(),
            max_buffer,
            deadline,
            timeout_signal,
            kill_sent: false,
            timed_out: false,
            max_buffer_exceeded: false,
            completion,
            _count_reservation: count_reservation,
            _bytes_reservation: bytes_reservation,
        };
        let registration = (|| -> Result<_, SidecarError> {
            let vm = self
                .vms
                .get_mut(vm_id)
                .ok_or_else(|| missing_vm_error(vm_id))?;
            let root = vm
                .active_processes
                .get_mut(process_id)
                .ok_or_else(|| missing_process_error(vm_id, process_id))?;
            let parent =
                Self::active_process_by_path_mut(root, current_process_path).ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "unknown child process path during nested spawnSync",
                    ))
                })?;
            if parent
                .pending_child_process_sync
                .contains_key(&identity.child_process_id)
            {
                return Err(SidecarError::host(
                    "EEXIST",
                    format!(
                        "pending nested child-process sync entry {} already exists",
                        identity.child_process_id
                    ),
                ));
            }
            parent
                .pending_child_process_sync
                .insert(identity.child_process_id.clone(), pending);
            Ok((
                parent.runtime_context.clone(),
                Arc::clone(&parent.process_event_notify),
            ))
        })();
        let (runtime, notify) = match registration {
            Ok(registration) => registration,
            Err(error) => {
                self.rollback_registered_child_process_sync(
                    vm_id,
                    process_id,
                    current_process_path,
                    &prior_child_ids,
                    Some(&identity.child_process_id),
                    Some(identity.pid),
                    "nested spawnSync pending registration",
                );
                return Err(error);
            }
        };
        if let Some(deadline) = deadline {
            if let Err(error) = admit_child_process_sync_timer(
                &runtime,
                notify,
                deadline,
                vm_id,
                process_id,
                current_process_path,
            ) {
                self.rollback_registered_child_process_sync(
                    vm_id,
                    process_id,
                    current_process_path,
                    &prior_child_ids,
                    Some(&identity.child_process_id),
                    Some(identity.pid),
                    "nested spawnSync timer admission",
                );
                return Err(error);
            }
        }
        Ok(())
    }

    async fn defer_descendant_javascript_child_process_sync(
        &mut self,
        vm_id: &str,
        process_id: &str,
        current_process_path: &[&str],
        request: ProcessLaunchRequest,
        max_buffer: Option<usize>,
    ) -> Result<HostServiceResponse, SidecarError> {
        let (respond_to, receiver) = tokio::sync::oneshot::channel();
        self.begin_descendant_child_process_sync(
            vm_id,
            process_id,
            current_process_path,
            request,
            max_buffer,
            PendingChildProcessSyncCompletion::Javascript(respond_to),
        )
        .await?;
        Ok(HostServiceResponse::Deferred {
            receiver,
            timeout: None,
            task_class: agentos_runtime::TaskClass::Vm,
        })
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) async fn defer_descendant_javascript_child_process_sync_for_test(
        &mut self,
        vm_id: &str,
        process_id: &str,
        current_process_path: &[&str],
        request: ProcessLaunchRequest,
        max_buffer: Option<usize>,
    ) -> Result<HostServiceResponse, SidecarError> {
        self.defer_descendant_javascript_child_process_sync(
            vm_id,
            process_id,
            current_process_path,
            request,
            max_buffer,
        )
        .await
    }

    fn settle_descendant_managed_network_response(
        &self,
        vm_id: &str,
        root_process_id: &str,
        caller_process_path: &[&str],
        runtime: agentos_runtime::RuntimeContext,
        reply: DirectHostReplyHandle,
        operation: &str,
        response: Result<HostServiceResponse, SidecarError>,
    ) -> Result<(), SidecarError> {
        let response = match response {
            Ok(HostServiceResponse::Deferred {
                receiver,
                timeout,
                task_class,
            }) => {
                let Some(vm) = self.vms.get(vm_id) else {
                    reply
                        .fail(HostServiceError::new(
                            "ESTALE",
                            "managed-network descendant VM no longer exists",
                        ))
                        .map_err(SidecarError::from)?;
                    return Ok(());
                };
                let connection_id = vm.connection_id.clone();
                let session_id = vm.session_id.clone();
                let sender = self.process_event_sender.clone();
                let event_notify = Arc::clone(&self.process_event_notify);
                let envelope_vm_id = vm_id.to_owned();
                let envelope_process_id =
                    Self::child_process_path_label(root_process_id, caller_process_path);
                let task_reply = reply.clone();
                let method = operation.to_owned();
                if let Err(error) = runtime.spawn(task_class, async move {
                    let receive = async {
                        receiver.await.unwrap_or_else(|_| {
                            Err(crate::state::DeferredRpcError {
                                code: String::from(
                                    "ERR_AGENTOS_DEFERRED_RPC_RESPONSE_CHANNEL_CLOSED",
                                ),
                                message: format!(
                                    "deferred managed-network response channel closed for {method}"
                                ),
                                details: None,
                            })
                        })
                    };
                    let result = match timeout {
                        Some(timeout) => match crate::execution::operation_deadline_timeout(
                            &method,
                            timeout,
                            receive,
                        )
                        .await
                        {
                            Ok(result) => result,
                            Err(_) => Err(crate::state::DeferredRpcError {
                                code: String::from("ETIMEDOUT"),
                                message: format!(
                                    "{method} exceeded limits.reactor.operationDeadlineMs ({} ms)",
                                    timeout.as_millis()
                                ),
                                details: None,
                            }),
                        },
                        None => receive.await,
                    };
                    let envelope = ProcessEventEnvelope {
                        connection_id,
                        session_id,
                        vm_id: envelope_vm_id,
                        process_id: envelope_process_id,
                        event: ActiveExecutionEvent::HostCallCompletion(
                            crate::state::HostCallCompletion {
                                reply: task_reply,
                                result,
                            },
                        ),
                    };
                    if let Err(error) = sender.send(envelope).await {
                        if let ActiveExecutionEvent::HostCallCompletion(completion) =
                            error.0.event
                        {
                            if let Err(settlement_error) = completion.reply.fail(HostServiceError::new(
                                "ECANCELED",
                                "descendant managed-network completion lane closed",
                            )) {
                                eprintln!(
                                    "ERR_AGENTOS_DESCENDANT_COMPLETION_SETTLEMENT: {settlement_error}"
                                );
                            }
                        }
                        eprintln!(
                            "ERR_AGENTOS_PROCESS_EVENT_CHANNEL_CLOSED: descendant managed-network completion could not be delivered"
                        );
                    } else {
                        event_notify.notify_one();
                    }
                }) {
                    reply
                        .fail(host_service_error(&SidecarError::from(error)))
                        .map_err(SidecarError::from)?;
                }
                return Ok(());
            }
            other => other,
        };
        settle_execution_host_call(&reply, response)
    }

    fn dispatch_descendant_context_descriptor_operation(
        &mut self,
        vm_id: &str,
        root_process_id: &str,
        caller_process_path: &[&str],
        operation: agentos_execution::host::FilesystemOperation,
        reply: DirectHostReplyHandle,
    ) -> Result<(), SidecarError> {
        let (generation, caller_pid) = {
            let Some(vm) = self.vms.get(vm_id) else {
                reply
                    .fail(HostServiceError::new(
                        "ESTALE",
                        "host call VM no longer exists",
                    ))
                    .map_err(SidecarError::from)?;
                return Ok(());
            };
            let Some(root) = vm.active_processes.get(root_process_id) else {
                reply
                    .fail(HostServiceError::new(
                        "ESTALE",
                        "host call root process no longer exists",
                    ))
                    .map_err(SidecarError::from)?;
                return Ok(());
            };
            let Some(caller) = Self::active_process_by_path(root, caller_process_path) else {
                reply
                    .fail(HostServiceError::new(
                        "ESTALE",
                        "host call descendant process no longer exists",
                    ))
                    .map_err(SidecarError::from)?;
                return Ok(());
            };
            (vm.generation, caller.kernel_pid)
        };
        let identity = reply.identity();
        if identity.generation != generation || identity.pid != caller_pid {
            reply
                .fail(
                    HostServiceError::new(
                        "ESTALE",
                        "descriptor host call identity does not match the descendant process",
                    )
                    .with_details(json!({
                        "expectedGeneration": generation,
                        "expectedPid": caller_pid,
                        "observedGeneration": identity.generation,
                        "observedPid": identity.pid,
                    })),
                )
                .map_err(SidecarError::from)?;
            return Ok(());
        }

        let host_operation = HostOperation::Filesystem(operation.clone());
        let vm = self
            .vms
            .get(vm_id)
            .expect("validated descriptor descendant VM remains registered");
        if let Err(error) =
            host_dispatch::authorize_host_operation(&vm.kernel, caller_pid, &host_operation)
        {
            reply.fail(error).map_err(SidecarError::from)?;
            return Ok(());
        }
        let socket_paths = build_socket_path_context(vm)?;
        if !reply.claim().map_err(SidecarError::from)? {
            return Ok(());
        }

        let bridge = self.bridge.clone();
        let vm = self
            .vms
            .get_mut(vm_id)
            .expect("validated descriptor descendant VM remains registered");
        let result = match operation {
            agentos_execution::host::FilesystemOperation::Close { fd } => {
                host_dispatch::close_with_managed_retirement(
                    &bridge,
                    vm_id,
                    &socket_paths,
                    vm,
                    caller_pid,
                    fd,
                )
            }
            agentos_execution::host::FilesystemOperation::CloseFrom { min_fd, exact_fds } => {
                host_dispatch::closefrom_with_managed_retirement(
                    &bridge,
                    vm_id,
                    &socket_paths,
                    vm,
                    caller_pid,
                    min_fd,
                    exact_fds,
                )
            }
            operation @ (agentos_execution::host::FilesystemOperation::Renumber { .. }
            | agentos_execution::host::FilesystemOperation::DuplicateTo { .. }
            | agentos_execution::host::FilesystemOperation::Move { .. }) => {
                host_dispatch::replace_descriptor_with_managed_retirement(
                    &bridge,
                    vm_id,
                    &socket_paths,
                    vm,
                    caller_pid,
                    operation,
                )
            }
            other => Err(SidecarError::host(
                "EINVAL",
                format!(
                    "descendant descriptor dispatcher received unsupported operation: {other:?}"
                ),
            )),
        };
        match result {
            Ok(response) => reply.succeed(response),
            Err(error) => reply.fail(host_service_error(&error)),
        }
        .map_err(SidecarError::from)
    }

    fn dispatch_descendant_context_dns_operation(
        &self,
        vm_id: &str,
        root_process_id: &str,
        caller_process_path: &[&str],
        operation: agentos_execution::host::NetworkOperation,
        reply: DirectHostReplyHandle,
    ) -> Result<(), SidecarError> {
        let Some(vm) = self.vms.get(vm_id) else {
            reply
                .fail(HostServiceError::new(
                    "ESTALE",
                    "host call VM no longer exists",
                ))
                .map_err(SidecarError::from)?;
            return Ok(());
        };
        let Some(root) = vm.active_processes.get(root_process_id) else {
            reply
                .fail(HostServiceError::new(
                    "ESTALE",
                    "host call root process no longer exists",
                ))
                .map_err(SidecarError::from)?;
            return Ok(());
        };
        let Some(caller) = Self::active_process_by_path(root, caller_process_path) else {
            reply
                .fail(HostServiceError::new(
                    "ESTALE",
                    "host call descendant process no longer exists",
                ))
                .map_err(SidecarError::from)?;
            return Ok(());
        };
        let identity = reply.identity();
        if identity.generation != vm.generation || identity.pid != caller.kernel_pid {
            reply
                .fail(HostServiceError::new(
                    "ESTALE",
                    "DNS host call identity does not match the descendant process",
                ))
                .map_err(SidecarError::from)?;
            return Ok(());
        }
        let runtime = vm.runtime_context.clone();
        let response = service_host_dns_operation(
            self.bridge.clone(),
            &vm.kernel,
            vm_id.to_owned(),
            vm.dns.clone(),
            operation,
        );
        let task_reply = reply.clone();
        if let Err(error) = runtime.spawn(agentos_runtime::TaskClass::Dns, async move {
            let settled = match response.await {
                Ok(response) => task_reply.succeed(response),
                Err(error) => task_reply.fail(error),
            };
            if let Err(error) = settled {
                eprintln!("ERR_AGENTOS_DESCENDANT_DNS_DIRECT_REPLY: {error}");
            }
        }) {
            reply
                .fail(host_service_error(&SidecarError::from(error)))
                .map_err(SidecarError::from)?;
        }
        Ok(())
    }

    async fn dispatch_descendant_context_managed_network_operation(
        &mut self,
        vm_id: &str,
        root_process_id: &str,
        caller_process_path: &[&str],
        operation: agentos_execution::host::NetworkOperation,
        reply: DirectHostReplyHandle,
    ) -> Result<(), SidecarError> {
        let (generation, caller_pid) = {
            let Some(vm) = self.vms.get(vm_id) else {
                reply
                    .fail(HostServiceError::new(
                        "ESTALE",
                        "host call VM no longer exists",
                    ))
                    .map_err(SidecarError::from)?;
                return Ok(());
            };
            let Some(root) = vm.active_processes.get(root_process_id) else {
                reply
                    .fail(HostServiceError::new(
                        "ESTALE",
                        "host call root process no longer exists",
                    ))
                    .map_err(SidecarError::from)?;
                return Ok(());
            };
            let Some(caller) = Self::active_process_by_path(root, caller_process_path) else {
                reply
                    .fail(HostServiceError::new(
                        "ESTALE",
                        "host call descendant process no longer exists",
                    ))
                    .map_err(SidecarError::from)?;
                return Ok(());
            };
            (vm.generation, caller.kernel_pid)
        };
        let identity = reply.identity();
        if identity.generation != generation || identity.pid != caller_pid {
            reply
                .fail(
                    HostServiceError::new(
                        "ESTALE",
                        "managed-network host call identity does not match the descendant process",
                    )
                    .with_details(json!({
                        "expectedGeneration": generation,
                        "expectedPid": caller_pid,
                        "observedGeneration": identity.generation,
                        "observedPid": identity.pid,
                    })),
                )
                .map_err(SidecarError::from)?;
            return Ok(());
        }

        let host_operation = HostOperation::Network(operation.clone());
        let vm = self
            .vms
            .get(vm_id)
            .expect("validated managed-network descendant VM remains registered");
        if let Err(error) =
            host_dispatch::authorize_host_operation(&vm.kernel, caller_pid, &host_operation)
        {
            reply.fail(error).map_err(SidecarError::from)?;
            return Ok(());
        }
        if !reply.claim().map_err(SidecarError::from)? {
            return Ok(());
        }

        let bridge = self.bridge.clone();
        let socket_paths = build_socket_path_context(
            self.vms
                .get(vm_id)
                .expect("validated managed-network descendant VM remains registered"),
        )?;
        let (runtime, response, label) = {
            let vm = self
                .vms
                .get_mut(vm_id)
                .expect("validated managed-network descendant VM remains registered");
            let runtime = vm.runtime_context.clone();
            let capabilities = vm.capabilities.clone();
            let dns = vm.dns.clone();
            let kernel_readiness = Arc::clone(&vm.kernel_socket_readiness);
            let managed_descriptions = Arc::clone(&vm.managed_host_net_descriptions);
            let root = vm
                .active_processes
                .get_mut(root_process_id)
                .expect("validated managed-network root remains registered");
            let process = Self::active_process_by_path_mut(root, caller_process_path)
                .expect("validated managed-network descendant remains registered");
            use agentos_execution::host::NetworkOperation as HostNetworkOperation;
            let (response, label) = match operation {
                operation @ (HostNetworkOperation::Socket { .. }
                | HostNetworkOperation::Bind { .. }
                | HostNetworkOperation::Connect { .. }
                | HostNetworkOperation::Listen { .. }
                | HostNetworkOperation::Accept { .. }
                | HostNetworkOperation::Validate { .. }
                | HostNetworkOperation::Receive { .. }
                | HostNetworkOperation::Send { .. }
                | HostNetworkOperation::LocalAddress { .. }
                | HostNetworkOperation::PeerAddress { .. }
                | HostNetworkOperation::GetOption { .. }
                | HostNetworkOperation::SetOption { .. }
                | HostNetworkOperation::Poll { .. }
                | HostNetworkOperation::TlsConnect { .. }) => (
                    host_dispatch::service_descendant_managed_fd_network_operation(
                        &bridge,
                        vm_id,
                        &dns,
                        &socket_paths,
                        &mut vm.kernel,
                        kernel_readiness,
                        process,
                        capabilities,
                        managed_descriptions,
                        reply.identity().call_id,
                        operation,
                    ),
                    "managed fd network",
                ),
                operation @ (HostNetworkOperation::ManagedUdpCreate { .. }
                | HostNetworkOperation::ManagedUdpBind { .. }
                | HostNetworkOperation::ManagedUdpSend { .. }
                | HostNetworkOperation::ManagedUdpClose { .. }) => (
                    service_managed_udp_operation(
                        ManagedUdpServiceRequest {
                            bridge: &bridge,
                            kernel: &mut vm.kernel,
                            vm_id,
                            dns: &dns,
                            socket_paths: &socket_paths,
                            process,
                            kernel_readiness,
                            capabilities,
                        },
                        operation,
                    ),
                    "managed UDP",
                ),
                operation @ (HostNetworkOperation::ManagedPoll { .. }
                | HostNetworkOperation::ManagedWaitConnect { .. }
                | HostNetworkOperation::ManagedRead { .. }
                | HostNetworkOperation::ManagedWrite { .. }
                | HostNetworkOperation::ManagedDestroy { .. }
                | HostNetworkOperation::ManagedAccept { .. }
                | HostNetworkOperation::ManagedCloseListener { .. }
                | HostNetworkOperation::ManagedTlsUpgrade { .. }) => (
                    service_managed_network_operation(
                        ManagedNetworkServiceContext {
                            vm_id,
                            socket_paths: &socket_paths,
                            kernel: &mut vm.kernel,
                            kernel_readiness,
                            process,
                            capabilities,
                        },
                        operation,
                    ),
                    "managed network",
                ),
                operation @ (HostNetworkOperation::ManagedBindUnix { .. }
                | HostNetworkOperation::ManagedBindConnectedUnix { .. }
                | HostNetworkOperation::ManagedReserveTcpPort { .. }
                | HostNetworkOperation::ManagedReleaseTcpPort { .. }
                | HostNetworkOperation::ManagedConnect { .. }
                | HostNetworkOperation::ManagedListen { .. }) => (
                    service_managed_endpoint_operation(
                        ManagedEndpointServiceContext {
                            bridge: &bridge,
                            vm_id,
                            dns: &dns,
                            socket_paths: &socket_paths,
                            kernel: &mut vm.kernel,
                            kernel_readiness,
                            process,
                            capabilities,
                            call_id: reply.identity().call_id,
                        },
                        operation,
                    ),
                    "managed endpoint",
                ),
                operation @ (HostNetworkOperation::SendDescriptorRights { .. }
                | HostNetworkOperation::ReceiveDescriptorRights { .. }) => {
                    let request = descriptor_rights_compat_request(
                        reply.identity().call_id,
                        operation,
                    )?;
                    (
                        service_javascript_sync_rpc(JavascriptSyncRpcServiceRequest {
                            bridge: &bridge,
                            vm_id,
                            dns: &dns,
                            socket_paths: &socket_paths,
                            kernel: &mut vm.kernel,
                            kernel_readiness,
                            process,
                            sync_request: &request,
                            capabilities,
                            managed_descriptions: Some(Arc::clone(
                                &vm.managed_host_net_descriptions,
                            )),
                        })
                        .await,
                        "descriptor rights",
                    )
                }
                other => (
                    Err(SidecarError::host(
                        "EINVAL",
                        format!(
                            "descendant managed-network dispatcher received unsupported operation: {other:?}"
                        ),
                    )),
                    "managed network",
                ),
            };
            (runtime, response, label)
        };
        self.settle_descendant_managed_network_response(
            vm_id,
            root_process_id,
            caller_process_path,
            runtime,
            reply,
            label,
            response,
        )
    }

    async fn dispatch_descendant_context_process_operation(
        &mut self,
        vm_id: &str,
        root_process_id: &str,
        caller_process_path: &[&str],
        operation: HostOperation,
        reply: DirectHostReplyHandle,
    ) -> Result<(), SidecarError> {
        let (generation, caller_pid) = {
            let Some(vm) = self.vms.get(vm_id) else {
                reply
                    .fail(HostServiceError::new(
                        "ESTALE",
                        "host call VM no longer exists",
                    ))
                    .map_err(SidecarError::from)?;
                return Ok(());
            };
            let Some(root) = vm.active_processes.get(root_process_id) else {
                reply
                    .fail(HostServiceError::new(
                        "ESTALE",
                        "host call root process no longer exists",
                    ))
                    .map_err(SidecarError::from)?;
                return Ok(());
            };
            let Some(caller) = Self::active_process_by_path(root, caller_process_path) else {
                reply
                    .fail(HostServiceError::new(
                        "ESTALE",
                        "host call descendant process no longer exists",
                    ))
                    .map_err(SidecarError::from)?;
                return Ok(());
            };
            (vm.generation, caller.kernel_pid)
        };
        let identity = reply.identity();
        if identity.generation != generation || identity.pid != caller_pid {
            reply
                .fail(
                    HostServiceError::new(
                        "ESTALE",
                        "host call identity does not match the descendant kernel process",
                    )
                    .with_details(json!({
                        "expectedGeneration": generation,
                        "expectedPid": caller_pid,
                        "observedGeneration": identity.generation,
                        "observedPid": identity.pid,
                    })),
                )
                .map_err(SidecarError::from)?;
            return Ok(());
        }

        let HostOperation::Process(operation) = operation else {
            reply
                .fail(HostServiceError::new(
                    "EINVAL",
                    "descendant context dispatcher requires a process operation",
                ))
                .map_err(SidecarError::from)?;
            return Ok(());
        };

        let result = match operation {
            ProcessOperation::Spawn(request) => {
                let mut request = request.into_request();
                if let Err(error) = merge_process_internal_bootstrap_env(self, vm_id, &mut request)
                    .and_then(|()| validate_process_launch_request(&request, false))
                {
                    reply
                        .fail(host_service_error(&error))
                        .map_err(SidecarError::from)?;
                    return Ok(());
                }
                if !reply.claim().map_err(SidecarError::from)? {
                    return Ok(());
                }
                self.spawn_descendant_process(vm_id, root_process_id, caller_process_path, request)
                    .await
                    .map(HostCallReply::Json)
            }
            ProcessOperation::RunCaptured {
                request,
                max_buffer,
            } => {
                let mut request = request.into_request();
                if let Err(error) = merge_process_internal_bootstrap_env(self, vm_id, &mut request)
                    .and_then(|()| validate_process_launch_request(&request, false))
                {
                    reply
                        .fail(host_service_error(&error))
                        .map_err(SidecarError::from)?;
                    return Ok(());
                }
                if !reply.claim().map_err(SidecarError::from)? {
                    return Ok(());
                }
                if let Err(error) = self
                    .begin_descendant_child_process_sync(
                        vm_id,
                        root_process_id,
                        caller_process_path,
                        request,
                        Some(max_buffer.get()),
                        PendingChildProcessSyncCompletion::Direct(reply.clone()),
                    )
                    .await
                {
                    reply
                        .fail(host_service_error(&error))
                        .map_err(SidecarError::from)?;
                }
                return Ok(());
            }
            ProcessOperation::PollChild { child_id, wait_ms } => {
                if let Err(error) = self.validate_child_poll_target(
                    vm_id,
                    root_process_id,
                    caller_process_path,
                    child_id.as_str(),
                ) {
                    reply
                        .fail(host_service_error(&error))
                        .map_err(SidecarError::from)?;
                    return Ok(());
                }
                if !reply.claim().map_err(SidecarError::from)? {
                    return Ok(());
                }
                // Keep descendant polling nonblocking and reply in the same
                // event turn. Boxing is required because a descendant can in
                // turn poll one of its own descendants through this path.
                Box::pin(self.poll_descendant_process(
                    vm_id,
                    root_process_id,
                    caller_process_path,
                    child_id.as_str(),
                    wait_ms,
                ))
                .await
                .map(HostCallReply::Json)
            }
            ProcessOperation::WriteChildStdin { child_id, chunk } => {
                if !reply.claim().map_err(SidecarError::from)? {
                    return Ok(());
                }
                self.write_descendant_process_stdin(
                    vm_id,
                    root_process_id,
                    caller_process_path,
                    child_id.as_str(),
                    chunk.as_slice(),
                )
                .map(|()| HostCallReply::Json(Value::Null))
            }
            ProcessOperation::CloseChildStdin { child_id } => {
                if !reply.claim().map_err(SidecarError::from)? {
                    return Ok(());
                }
                self.close_descendant_process_stdin(
                    vm_id,
                    root_process_id,
                    caller_process_path,
                    child_id.as_str(),
                )
                .map(|()| HostCallReply::Json(Value::Null))
            }
            ProcessOperation::Exec(request) => {
                let mut request = request.into_request();
                let fd_image_commit = request.options.executable_fd.is_some();
                let preflight = if fd_image_commit {
                    validate_wasm_fd_image_commit_request(&request)
                } else {
                    merge_process_internal_bootstrap_env(self, vm_id, &mut request)
                        .and_then(|()| validate_process_launch_request(&request, true))
                };
                if let Err(error) = preflight {
                    reply
                        .fail(host_service_error(&error))
                        .map_err(SidecarError::from)?;
                    return Ok(());
                }
                if !reply.claim().map_err(SidecarError::from)? {
                    return Ok(());
                }
                let local_replacement = request.options.local_replacement;
                let result = if fd_image_commit {
                    self.commit_wasm_fd_process_image(
                        vm_id,
                        root_process_id,
                        caller_process_path,
                        request,
                    )
                } else {
                    self.exec_process_image(vm_id, root_process_id, caller_process_path, request)
                };
                match result {
                    Ok(()) if local_replacement => {
                        reply
                            .succeed_json(json!({ "committed": true }))
                            .map_err(SidecarError::from)?;
                        return Ok(());
                    }
                    Ok(()) => {
                        reply.dismiss_claimed().map_err(SidecarError::from)?;
                        return Ok(());
                    }
                    Err(error) => Err(error),
                }
            }
            other => Err(SidecarError::host(
                "ENOSYS",
                format!("unsupported descendant process operation: {other:?}"),
            )),
        };
        match result {
            Ok(response) => reply.succeed(response),
            Err(error) => reply.fail(host_service_error(&error)),
        }
        .map_err(SidecarError::from)
    }

    async fn handle_descendant_javascript_child_process_rpc(
        &mut self,
        vm_id: &str,
        process_id: &str,
        current_process_path: &[&str],
        request: &ExecutionHostCall,
    ) -> Result<HostServiceResponse, SidecarError> {
        match request.method.as_str() {
            "child_process.spawn" => {
                let Some(vm) = self.vms.get(vm_id) else {
                    return Ok(Value::Null.into());
                };
                let (payload, _) = parse_javascript_child_process_spawn_request(vm, &request.args)?;
                self.spawn_descendant_process(vm_id, process_id, current_process_path, payload)
                    .await
                    .map(Into::into)
            }
            "child_process.spawn_sync" => {
                let Some(vm) = self.vms.get(vm_id) else {
                    return Ok(Value::Null.into());
                };
                let (payload, max_buffer) =
                    parse_javascript_child_process_spawn_request(vm, &request.args)?;
                self.defer_descendant_javascript_child_process_sync(
                    vm_id,
                    process_id,
                    current_process_path,
                    payload,
                    max_buffer,
                )
                .await
            }
            "child_process.poll" => {
                let child_process_id =
                    javascript_sync_rpc_arg_str(&request.args, 0, "child_process.poll child id")?;
                let wait_ms = javascript_sync_rpc_arg_u64_optional(
                    &request.args,
                    1,
                    "child_process.poll wait ms",
                )?
                .unwrap_or_default();
                Box::pin(self.poll_descendant_process(
                    vm_id,
                    process_id,
                    current_process_path,
                    child_process_id,
                    wait_ms,
                ))
                .await
                .map(Into::into)
            }
            "child_process.write_stdin" => {
                let child_process_id = javascript_sync_rpc_arg_str(
                    &request.args,
                    0,
                    "child_process.write_stdin child id",
                )?;
                let chunk = javascript_sync_rpc_bytes_arg(
                    &request.args,
                    1,
                    "child_process.write_stdin chunk",
                )?;
                self.write_descendant_process_stdin(
                    vm_id,
                    process_id,
                    current_process_path,
                    child_process_id,
                    &chunk,
                )
                .map(|()| Value::Null.into())
            }
            "child_process.close_stdin" => {
                let child_process_id = javascript_sync_rpc_arg_str(
                    &request.args,
                    0,
                    "child_process.close_stdin child id",
                )?;
                self.close_descendant_process_stdin(
                    vm_id,
                    process_id,
                    current_process_path,
                    child_process_id,
                )
                .map(|()| Value::Null.into())
            }
            "child_process.kill" => {
                let child_process_id =
                    javascript_sync_rpc_arg_str(&request.args, 0, "child_process.kill child id")?;
                let signal =
                    javascript_sync_rpc_arg_str(&request.args, 1, "child_process.kill signal")?;
                self.kill_descendant_javascript_child_process(
                    vm_id,
                    process_id,
                    current_process_path,
                    child_process_id,
                    signal,
                )
                .map(|()| Value::Null.into())
            }
            _ => Err(SidecarError::InvalidState(format!(
                "unsupported nested child process RPC method {}",
                request.method
            ))),
        }
    }

    /// Deferred servicing for a child's blocking kernel read, poll, or stdio
    /// write inside the child-event pump. Each operation is probed without
    /// blocking the sidecar actor; an unavailable write is parked by reply
    /// token and retried after the parent consumes pipe data. The pump loop
    /// re-checks the parked RPC every iteration. Returns false when the RPC
    /// must use the normal inline path (local JS stdin or shared TTY output).
    fn service_child_kernel_wait_rpc(
        &mut self,
        vm_id: &str,
        process_id: &str,
        current_process_path: &[&str],
        child_process_id: &str,
        request: &ExecutionHostCall,
    ) -> Result<bool, SidecarError> {
        let event_notify = Arc::clone(&self.process_event_notify);
        let Some(vm) = self.vms.get_mut(vm_id) else {
            return Ok(true);
        };
        let operation_deadline_ms = vm.limits.reactor.operation_deadline_ms;
        let runtime = vm.runtime_context.clone();
        let kernel = &mut vm.kernel;
        let Some(root) = vm.active_processes.get_mut(process_id) else {
            return Ok(true);
        };
        let Some(parent) = Self::active_process_by_path_mut(root, current_process_path) else {
            return Ok(true);
        };
        let Some(child) = parent.child_processes.get_mut(child_process_id) else {
            return Ok(true);
        };
        let pending_call_id = child
            .deferred_kernel_wait_rpc
            .as_ref()
            .map(|(pending, _)| pending.id);
        if let Err(error) = admit_one_slot_rpc(pending_call_id, request.id, "deferredKernelWaitRpc")
        {
            request
                .reply
                .fail(error)
                .map_err(|error| SidecarError::Execution(error.to_string()))?;
            return Ok(true);
        }
        if request.method == "__kernel_stdio_write" || request.method == "process.fd_write" {
            if request.method == "__kernel_stdio_write" && child.tty_master_owner.is_some() {
                return Ok(false);
            }
            let now = Instant::now();
            let (deadline, arm_deadline_wake) = match &child.deferred_kernel_wait_rpc {
                Some((parked, parked_deadline)) if parked.id == request.id => (
                    parked_deadline.unwrap_or(now + Duration::from_millis(operation_deadline_ms)),
                    false,
                ),
                _ => (now + Duration::from_millis(operation_deadline_ms), true),
            };
            let response = if request.method == "__kernel_stdio_write" {
                service_javascript_kernel_stdio_write_sync_rpc(kernel, child, request)
            } else {
                service_javascript_kernel_fd_write_sync_rpc(kernel, child, request)
            };
            match response {
                Ok(response) => {
                    child.clear_deferred_kernel_wait_rpc();
                    settle_execution_host_call(&request.reply, Ok(response.into()))?;
                }
                Err(error) if host_service_error_code(&error) == "EAGAIN" && now >= deadline => {
                    child.clear_deferred_kernel_wait_rpc();
                    request
                        .reply
                        .fail(HostServiceError::new(
                            "ETIMEDOUT",
                            format!(
                                "pipe write exceeded limits.reactor.operationDeadlineMs ({operation_deadline_ms} ms); raise that limit for slower readers"
                            ),
                        ))
                        .map_err(|error| SidecarError::Execution(error.to_string()))?;
                }
                Err(error) if host_service_error_code(&error) == "EAGAIN" => {
                    if arm_deadline_wake {
                        let limit = Duration::from_millis(operation_deadline_ms);
                        let operation = if request.method == "__kernel_stdio_write" {
                            "deferred child stdio write"
                        } else {
                            "deferred child fd write"
                        };
                        child.clear_deferred_kernel_wait_rpc();
                        child.deferred_kernel_wait_rpc = Some((request.clone(), Some(deadline)));
                        let timer = runtime.spawn(agentos_runtime::TaskClass::Timer, async move {
                            let mut deadline =
                                crate::execution::OperationDeadlineTracker::from_deadline(
                                    deadline, limit, false,
                                );
                            tokio::time::sleep(deadline.remaining_until_next_edge()).await;
                            deadline.observe(operation);
                            event_notify.notify_one();
                            tokio::time::sleep(deadline.remaining_until_deadline()).await;
                            event_notify.notify_one();
                        });
                        match timer {
                            Ok(timer) => {
                                child.deferred_child_write_timer = Some(timer);
                            }
                            Err(agentos_runtime::TaskSpawnError::ResourceLimit(limit)) => {
                                child.clear_deferred_kernel_wait_rpc();
                                let error = SidecarError::ResourceLimit(limit);
                                request
                                    .reply
                                    .fail(host_service_error(&error))
                                    .map_err(|error| SidecarError::Execution(error.to_string()))?;
                            }
                            Err(
                                error @ agentos_runtime::TaskSpawnError::AdmissionClosed { .. },
                            ) => {
                                child.clear_deferred_kernel_wait_rpc();
                                request
                                    .reply
                                    .fail(HostServiceError::new(
                                        "ERR_AGENTOS_TASK_ADMISSION_CLOSED",
                                        error.to_string(),
                                    ))
                                    .map_err(|error| SidecarError::Execution(error.to_string()))?;
                            }
                        }
                    } else {
                        child.deferred_kernel_wait_rpc = Some((request.clone(), Some(deadline)));
                    }
                }
                Err(error) => {
                    child.clear_deferred_kernel_wait_rpc();
                    tracing::warn!(
                        method = %request.method,
                        error = %error,
                        "child JavaScript sync RPC failed"
                    );
                    request
                        .reply
                        .fail(host_service_error(&error))
                        .map_err(|error| SidecarError::Execution(error.to_string()))?;
                }
            }
            return Ok(true);
        }
        if request.method == "process.fd_read" {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "fd_read fd")?;
            let stat = kernel
                .fd_stat(EXECUTION_DRIVER_NAME, child.kernel_pid, fd)
                .map_err(kernel_error)?;
            if matches!(
                stat.filetype,
                agentos_kernel::fd_table::FILETYPE_REGULAR_FILE
                    | agentos_kernel::fd_table::FILETYPE_DIRECTORY
                    | agentos_kernel::fd_table::FILETYPE_SYMBOLIC_LINK
            ) {
                // Ordinary VFS descriptors are immediately serviced by the
                // normal RPC handler. Parking them behind poll readiness can
                // deadlock a nested command before it produces pipeline data.
                return Ok(false);
            }
        }
        // The child draining its stdin pipe is what frees capacity for queued
        // host bytes and eventually delivers a deferred close/EOF.
        flush_pending_kernel_stdin(kernel, child)?;
        let now = Instant::now();
        let requested_timeout_ms = match request.method.as_str() {
            "__kernel_stdin_read" => parse_kernel_stdin_read_args(request)?.1,
            "__kernel_poll" => {
                let timeout_ms = parse_kernel_poll_args(request)?.1;
                (timeout_ms >= 0).then_some(timeout_ms as u64)
            }
            "process.fd_read" => Some(
                javascript_sync_rpc_arg_u64_optional(&request.args, 2, "fd_read timeout ms")?
                    .unwrap_or(DEFAULT_KERNEL_STDIN_READ_TIMEOUT_MS),
            ),
            _ => return Ok(false),
        };
        let deadline = match &child.deferred_kernel_wait_rpc {
            Some((parked, parked_deadline)) if parked.id == request.id => *parked_deadline,
            _ => match requested_timeout_ms {
                Some(timeout_ms) => match checked_deferred_guest_wait_deadline(timeout_ms) {
                    Ok(deadline) => Some(deadline),
                    Err(error) => {
                        child.clear_deferred_kernel_wait_rpc();
                        request
                            .reply
                            .fail(error)
                            .map_err(|error| SidecarError::Execution(error.to_string()))?;
                        return Ok(true);
                    }
                },
                None => None,
            },
        };
        let kernel_pid = child.kernel_pid;
        let mut fd_read = None;
        let probe = match request.method.as_str() {
            "__kernel_stdin_read" => {
                let (max_bytes, _) = parse_kernel_stdin_read_args(request)?;
                kernel_stdin_read_response(
                    kernel,
                    kernel_pid,
                    child.kernel_stdin_reader_fd,
                    max_bytes,
                    Duration::ZERO,
                )
                .map(|value| (value, true))
            }
            "__kernel_poll" => {
                let (fd_requests, _) = parse_kernel_poll_args(request)?;
                kernel_poll_response(kernel, kernel_pid, &fd_requests, 0).map(|value| (value, true))
            }
            "process.fd_read" => {
                let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "fd_read fd")?;
                let length = usize::try_from(javascript_sync_rpc_arg_u64(
                    &request.args,
                    1,
                    "fd_read length",
                )?)
                .map_err(|_| SidecarError::InvalidState("fd_read length is too large".into()))?;
                fd_read = Some((fd, length));
                kernel
                    .poll_fds(
                        EXECUTION_DRIVER_NAME,
                        kernel_pid,
                        vec![PollFd::new(fd, POLLIN)],
                        0,
                    )
                    .map(|result| (Value::Null, result.ready_count > 0))
                    .map_err(kernel_error)
            }
            _ => unreachable!("unsupported deferred kernel wait method"),
        };
        let (probe, fd_read_ready) = match probe {
            Ok(probe) => probe,
            Err(error) => {
                child.clear_deferred_kernel_wait_rpc();
                request
                    .reply
                    .fail(host_service_error(&error))
                    .map_err(|error| SidecarError::Execution(error.to_string()))?;
                return Ok(true);
            }
        };
        let ready = match request.method.as_str() {
            "__kernel_stdin_read" => !probe.is_null(),
            "__kernel_poll" => probe.get("readyCount").and_then(Value::as_u64).unwrap_or(0) > 0,
            "process.fd_read" => fd_read_ready,
            _ => unreachable!("unsupported deferred kernel wait method"),
        };
        if ready
            || requested_timeout_ms == Some(0)
            || deadline.is_some_and(|deadline| now >= deadline)
        {
            child.clear_deferred_kernel_wait_rpc();
            if let Some((fd, length)) = fd_read {
                // Claim before the destructive read. A stale reply token must
                // not consume bytes intended for a later read on this fd.
                let claimed = request
                    .reply
                    .claim()
                    .map_err(|error| SidecarError::Execution(error.to_string()))?;
                if !claimed {
                    return Ok(true);
                }
                let read_result = kernel.fd_read_with_timeout_result(
                    EXECUTION_DRIVER_NAME,
                    kernel_pid,
                    fd,
                    length,
                    Some(Duration::ZERO),
                );
                match read_result {
                    Ok(Some(bytes)) => request
                        .reply
                        .succeed(HostCallReply::Json(host_bytes_value(&bytes)))
                        .map_err(|error| SidecarError::Execution(error.to_string()))?,
                    Ok(None) => request
                        .reply
                        .succeed(HostCallReply::Json(host_bytes_value(&[])))
                        .map_err(|error| SidecarError::Execution(error.to_string()))?,
                    Err(error) => {
                        let error = kernel_error(error);
                        request
                            .reply
                            .fail(host_service_error(&error))
                            .map_err(|error| SidecarError::Execution(error.to_string()))?;
                    }
                }
                return Ok(true);
            }
            settle_execution_host_call(&request.reply, Ok(probe.into()))?;
            return Ok(true);
        }
        child.deferred_kernel_wait_rpc = Some((request.clone(), deadline));
        Ok(true)
    }

    /// Service `__kernel_stdio_write` for a process writing to the SHARED
    /// terminal (`tty_master_owner` set): write through the process's own PTY
    /// slave (line discipline applies), then drain the master and surface the
    /// drained bytes as the OWNER's ordered output stream — the single
    /// host-facing path. No child stdout event is queued, so nothing gets
    /// relayed (and re-rendered) by the parent shell.
    pub(crate) fn service_shared_tty_stdio_write(
        &mut self,
        vm_id: &str,
        writer_kernel_pid: u32,
        owner: (u32, u32),
        request: &HostRpcRequest,
    ) -> Result<Value, SidecarError> {
        let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "__kernel_stdio_write fd")?;
        let chunk = javascript_sync_rpc_bytes_arg(&request.args, 1, "__kernel_stdio_write chunk")?;
        let Some(vm) = self.vms.get_mut(vm_id) else {
            return Ok(json!(chunk.len()));
        };
        kernel_stdio_output_is_stdout(&vm.kernel, writer_kernel_pid, fd)?;
        let written = vm
            .kernel
            .fd_write(EXECUTION_DRIVER_NAME, writer_kernel_pid, fd, &chunk)
            .map_err(kernel_error)?;
        let (owner_pid, master_fd) = owner;
        let mut drained: Vec<u8> = Vec::new();
        loop {
            match vm.kernel.fd_read_with_timeout_result(
                EXECUTION_DRIVER_NAME,
                owner_pid,
                master_fd,
                MAX_PTY_BUFFER_BYTES,
                Some(Duration::ZERO),
            ) {
                Ok(Some(bytes)) if !bytes.is_empty() => drained.extend(bytes),
                Ok(_) => break,
                Err(error) if error.code() == "EAGAIN" => break,
                Err(error) => return Err(kernel_error(error)),
            }
        }
        if !drained.is_empty() {
            if let Some(owner_process) = vm
                .active_processes
                .values_mut()
                .find(|process| process.kernel_pid == owner_pid)
            {
                owner_process
                    .queue_pending_execution_event(ActiveExecutionEvent::Stdout(drained))?;
            }
        }
        Ok(json!(written))
    }

    /// Re-check a child's parked kernel-wait RPC (see
    /// `service_child_kernel_wait_rpc`); called once per pump-loop iteration.
    fn recheck_child_deferred_kernel_wait_rpc(
        &mut self,
        vm_id: &str,
        process_id: &str,
        current_process_path: &[&str],
        child_process_id: &str,
    ) -> Result<(), SidecarError> {
        let parked = {
            let Some(vm) = self.vms.get_mut(vm_id) else {
                return Ok(());
            };
            let Some(root) = vm.active_processes.get_mut(process_id) else {
                return Ok(());
            };
            let Some(parent) = Self::active_process_by_path_mut(root, current_process_path) else {
                return Ok(());
            };
            let Some(child) = parent.child_processes.get_mut(child_process_id) else {
                return Ok(());
            };
            child
                .deferred_kernel_wait_rpc
                .as_ref()
                .map(|(request, _)| request.clone())
        };
        if let Some(request) = parked {
            let handled = self.service_child_kernel_wait_rpc(
                vm_id,
                process_id,
                current_process_path,
                child_process_id,
                &request,
            )?;
            if !handled {
                return Err(SidecarError::InvalidState(format!(
                    "parked child kernel-wait RPC {} no longer belongs to the deferred path",
                    request.id
                )));
            }
        }
        Ok(())
    }

    fn service_descendant_guest_wait(
        &mut self,
        vm_id: &str,
        process_id: &str,
        child_path: &[&str],
        incoming: Option<(
            DeferredGuestWaitKind,
            Option<Instant>,
            DirectHostReplyHandle,
        )>,
    ) -> Result<(), SidecarError> {
        let notify = Arc::clone(&self.process_event_notify);
        let Some(vm) = self.vms.get_mut(vm_id) else {
            return Ok(());
        };
        let runtime = vm.runtime_context.clone();
        let wait_handle = vm.kernel.process_wait_handle();
        let generation = vm.generation;
        let (kernel, active_processes) = (&mut vm.kernel, &mut vm.active_processes);
        let Some(root) = active_processes.get_mut(process_id) else {
            return Ok(());
        };
        let Some(process) = Self::active_process_by_path_mut(root, child_path) else {
            return Ok(());
        };
        process.apply_runtime_controls()?;
        if incoming.is_none() && process.deferred_guest_wait.is_none() {
            return Ok(());
        }
        service_deferred_guest_wait(
            generation,
            &runtime,
            wait_handle,
            notify,
            kernel,
            process,
            incoming,
        )
    }

    fn service_descendant_kernel_poll(
        &mut self,
        vm_id: &str,
        process_id: &str,
        child_path: &[&str],
        incoming: Option<(
            agentos_execution::host::BoundedVec<agentos_execution::host::KernelPollInterest>,
            Option<Instant>,
            DirectHostReplyHandle,
        )>,
    ) -> Result<(), SidecarError> {
        let notify = Arc::clone(&self.process_event_notify);
        let socket_paths = self
            .vms
            .get(vm_id)
            .map(build_socket_path_context)
            .transpose()?;
        let Some(vm) = self.vms.get_mut(vm_id) else {
            return Ok(());
        };
        let runtime = vm.runtime_context.clone();
        let wait_handle = vm.kernel.poll_wait_handle();
        let generation = vm.generation;
        let kernel_readiness = Arc::clone(&vm.kernel_socket_readiness);
        let capabilities = vm.capabilities.clone();
        let managed_descriptions = Arc::clone(&vm.managed_host_net_descriptions);
        let (kernel, active_processes) = (&mut vm.kernel, &mut vm.active_processes);
        let Some(root) = active_processes.get_mut(process_id) else {
            return Ok(());
        };
        let Some(process) = Self::active_process_by_path_mut(root, child_path) else {
            return Ok(());
        };
        process.apply_runtime_controls()?;
        if incoming.is_none() && process.deferred_kernel_poll.is_none() {
            return Ok(());
        }
        if incoming.is_none()
            && process
                .deferred_kernel_poll
                .as_ref()
                .is_some_and(|poll| poll.combined)
        {
            return host_dispatch::service_deferred_posix_poll(
                generation,
                &runtime,
                wait_handle,
                notify,
                socket_paths
                    .as_ref()
                    .expect("registered VM has a socket path context"),
                kernel_readiness,
                capabilities,
                managed_descriptions,
                kernel,
                process,
                None,
            );
        }
        host_dispatch::service_deferred_kernel_poll(
            generation,
            &runtime,
            wait_handle,
            notify,
            kernel,
            process,
            incoming,
        )
    }

    fn service_descendant_posix_poll(
        &mut self,
        vm_id: &str,
        process_id: &str,
        child_path: &[&str],
        incoming: (
            agentos_execution::host::BoundedVec<agentos_execution::host::KernelPollInterest>,
            Option<Instant>,
            Option<agentos_execution::host::SignalSetValue>,
            DirectHostReplyHandle,
        ),
    ) -> Result<(), SidecarError> {
        let notify = Arc::clone(&self.process_event_notify);
        let socket_paths = self
            .vms
            .get(vm_id)
            .map(build_socket_path_context)
            .transpose()?;
        let Some(vm) = self.vms.get_mut(vm_id) else {
            return Ok(());
        };
        let runtime = vm.runtime_context.clone();
        let wait_handle = vm.kernel.poll_wait_handle();
        let generation = vm.generation;
        let kernel_readiness = Arc::clone(&vm.kernel_socket_readiness);
        let capabilities = vm.capabilities.clone();
        let managed_descriptions = Arc::clone(&vm.managed_host_net_descriptions);
        let (kernel, active_processes) = (&mut vm.kernel, &mut vm.active_processes);
        let Some(root) = active_processes.get_mut(process_id) else {
            return Ok(());
        };
        let Some(process) = Self::active_process_by_path_mut(root, child_path) else {
            return Ok(());
        };
        process.apply_runtime_controls()?;
        host_dispatch::service_deferred_posix_poll(
            generation,
            &runtime,
            wait_handle,
            notify,
            socket_paths
                .as_ref()
                .expect("registered VM has a socket path context"),
            kernel_readiness,
            capabilities,
            managed_descriptions,
            kernel,
            process,
            Some(incoming),
        )
    }

    fn service_descendant_kernel_read(
        &mut self,
        vm_id: &str,
        process_id: &str,
        child_path: &[&str],
        incoming: Option<(
            u32,
            agentos_execution::host::BoundedUsize,
            Instant,
            DirectHostReplyHandle,
        )>,
    ) -> Result<(), SidecarError> {
        let notify = Arc::clone(&self.process_event_notify);
        let Some(vm) = self.vms.get_mut(vm_id) else {
            return Ok(());
        };
        let runtime = vm.runtime_context.clone();
        let wait_handle = vm.kernel.poll_wait_handle();
        let generation = vm.generation;
        let (kernel, active_processes) = (&mut vm.kernel, &mut vm.active_processes);
        let Some(root) = active_processes.get_mut(process_id) else {
            return Ok(());
        };
        let Some(process) = Self::active_process_by_path_mut(root, child_path) else {
            return Ok(());
        };
        process.apply_runtime_controls()?;
        if incoming.is_none() && process.deferred_kernel_read.is_none() {
            return Ok(());
        }
        host_dispatch::service_deferred_kernel_read(
            generation,
            &runtime,
            wait_handle,
            notify,
            kernel,
            process,
            incoming,
        )
    }

    fn service_descendant_kernel_stdin_read(
        &mut self,
        vm_id: &str,
        process_id: &str,
        child_path: &[&str],
        incoming: Option<(
            agentos_execution::host::BoundedUsize,
            Instant,
            DirectHostReplyHandle,
        )>,
    ) -> Result<(), SidecarError> {
        let notify = Arc::clone(&self.process_event_notify);
        let Some(vm) = self.vms.get_mut(vm_id) else {
            return Ok(());
        };
        let runtime = vm.runtime_context.clone();
        let wait_handle = vm.kernel.poll_wait_handle();
        let generation = vm.generation;
        let (kernel, active_processes) = (&mut vm.kernel, &mut vm.active_processes);
        let Some(root) = active_processes.get_mut(process_id) else {
            return Ok(());
        };
        let Some(process) = Self::active_process_by_path_mut(root, child_path) else {
            return Ok(());
        };
        process.apply_runtime_controls()?;
        if incoming.is_none() && process.deferred_kernel_read.is_none() {
            return Ok(());
        }
        host_dispatch::service_deferred_kernel_stdin_read(
            generation,
            &runtime,
            wait_handle,
            notify,
            kernel,
            process,
            incoming,
        )
    }

    fn descendant_output_stream(
        &self,
        vm_id: &str,
        process_id: &str,
        current_process_path: &[&str],
        child_process_id: &str,
        emitted_stream: InheritedOutputStream,
    ) -> Result<Option<InheritedOutputStream>, SidecarError> {
        let Some(vm) = self.vms.get(vm_id) else {
            return Ok(None);
        };
        let Some(root) = vm.active_processes.get(process_id) else {
            return Ok(None);
        };
        let Some(parent) = Self::active_process_by_path(root, current_process_path) else {
            return Ok(None);
        };
        let Some(child) = parent.child_processes.get(child_process_id) else {
            return Ok(None);
        };
        if child.child_process_bridge_owns_output
            || parent
                .pending_child_process_sync
                .contains_key(child_process_id)
        {
            return Ok(None);
        }
        let source_fd = match emitted_stream {
            InheritedOutputStream::Stdout => 1,
            InheritedOutputStream::Stderr => 2,
        };
        let description = vm
            .kernel
            .fd_description_identity(EXECUTION_DRIVER_NAME, child.kernel_pid, source_fd)
            .map_err(kernel_error)?
            .0;
        let path = vm
            .kernel
            .fd_path(EXECUTION_DRIVER_NAME, child.kernel_pid, source_fd)
            .map_err(kernel_error)?;
        Ok(classify_inherited_output_stream(description, path.as_str()))
    }

    fn route_descendant_output_to_parent(
        &mut self,
        vm_id: &str,
        process_id: &str,
        current_process_path: &[&str],
        child_process_id: &str,
        polled: PolledExecutionEvent,
    ) -> Result<(), SidecarError> {
        let queued = {
            let Some(vm) = self.vms.get_mut(vm_id) else {
                return Ok(());
            };
            let Some(parent) =
                Self::descendant_parent_process_mut(vm, process_id, current_process_path)
            else {
                return Ok(());
            };
            parent.try_queue_pending_polled_execution_event(polled)
        };
        let Err((error, polled)) = queued else {
            return Ok(());
        };

        // Parent output admission is backpressure, not permission to discard
        // an already-accounted child event. Return the same leased envelope to
        // its pull-owned queue without releasing/reacquiring VM byte budget.
        let Some(vm) = self.vms.get_mut(vm_id) else {
            return Ok(());
        };
        let Some(parent) =
            Self::descendant_parent_process_mut(vm, process_id, current_process_path)
        else {
            return Ok(());
        };
        let Some(child) = parent.child_processes.get_mut(child_process_id) else {
            return Ok(());
        };
        child.queue_pull_owned_polled_execution_event(polled)?;
        if error.code() == Some("ERR_AGENTOS_RESOURCE_LIMIT") {
            return Ok(());
        }
        Err(error)
    }

    async fn poll_descendant_process(
        &mut self,
        vm_id: &str,
        process_id: &str,
        current_process_path: &[&str],
        child_process_id: &str,
        wait_ms: u64,
    ) -> Result<Value, SidecarError> {
        let mut child_path = current_process_path.to_vec();
        child_path.push(child_process_id);
        let child_gone_error = || javascript_child_process_gone_error(process_id, &child_path);
        // `wait_ms` remains on the compatibility RPC surface for WASM/native
        // callers, but the sidecar never parks while servicing it. Runtime
        // event producers wake the shared process pump instead.
        let _ = wait_ms;
        let internal_work_limit = self
            .config
            .runtime
            .fairness
            .capability_quantum_operations
            .max(1);
        let mut internal_work = 0usize;

        loop {
            if internal_work >= internal_work_limit {
                // The child event remains durable. Rearm the coalesced broker
                // before yielding so one descendant cannot monopolize a
                // sidecar turn with an unbounded stream of internal HostCalls.
                self.process_event_notify.notify_one();
                return Ok(Value::Null);
            }
            self.drain_queued_descendant_javascript_child_process_events(
                vm_id,
                process_id,
                &child_path,
            )?;
            self.recheck_child_deferred_kernel_wait_rpc(
                vm_id,
                process_id,
                current_process_path,
                child_process_id,
            )?;
            self.service_descendant_guest_wait(vm_id, process_id, &child_path, None)?;
            self.service_descendant_kernel_poll(vm_id, process_id, &child_path, None)?;
            self.service_descendant_kernel_read(vm_id, process_id, &child_path, None)?;
            enum ChildPollResult {
                Event(Box<Option<PolledExecutionEvent>>),
                RecoverRuntimeExit,
                Timeout,
            }
            let poll_result = {
                let Some(vm) = self.vms.get_mut(vm_id) else {
                    return Ok(Value::Null);
                };
                let Some(parent) =
                    Self::descendant_parent_process_mut(vm, process_id, current_process_path)
                else {
                    return Err(child_gone_error());
                };
                let Some(child) = parent.child_processes.get_mut(child_process_id) else {
                    return Err(child_gone_error());
                };
                if let Some(event) = child.lease_pending_execution_event() {
                    ChildPollResult::Event(Box::new(Some(event)))
                } else {
                    match child.try_poll_execution_event() {
                        Ok(Some(event)) => ChildPollResult::Event(Box::new(Some(event))),
                        Ok(None) => ChildPollResult::Timeout,
                        Err(SidecarError::ExecutionEventChannelClosed { .. }) => {
                            ChildPollResult::RecoverRuntimeExit
                        }
                        Err(error) => return Err(error),
                    }
                }
            };
            let event = match poll_result {
                ChildPollResult::Event(event) => *event,
                ChildPollResult::Timeout => return Ok(Value::Null),
                ChildPollResult::RecoverRuntimeExit => self
                    .recover_descendant_runtime_child_process_event(
                        vm_id,
                        process_id,
                        current_process_path,
                        child_process_id,
                    )?
                    .map(PolledExecutionEvent::unreserved),
            };

            let Some(event) = event else {
                return Ok(Value::Null);
            };

            let PolledExecutionEvent { event, reservation } = event;
            let synthetic_signal_termination = matches!(
                &event,
                ActiveExecutionEvent::Stderr(chunk)
                    if chunk.as_slice() == SYNTHETIC_V8_TERMINATION_STDERR
            ) && self
                .vms
                .get(vm_id)
                .and_then(|vm| vm.active_processes.get(process_id))
                .and_then(|root| Self::active_process_by_path(root, current_process_path))
                .and_then(|parent| parent.child_processes.get(child_process_id))
                .is_some_and(|child| child.exit_signal.is_some());
            if synthetic_signal_termination {
                // The following exit event carries the authoritative signal status.
                drop(reservation);
                internal_work += 1;
                continue;
            }
            if matches!(
                &event,
                ActiveExecutionEvent::Common(ExecutionEvent::HostCall { .. })
                    | ActiveExecutionEvent::ManagedStreamReadRecheck(_)
                    | ActiveExecutionEvent::ManagedUdpPollRecheck(_)
                    | ActiveExecutionEvent::HostRpcRequest(_)
                    | ActiveExecutionEvent::HostCallCompletion(_)
            ) {
                internal_work += 1;
            }
            match event {
                ActiveExecutionEvent::Common(ExecutionEvent::HostCall { operation, reply }) => {
                    drop(reservation);
                    let deferred_kernel_read = match &operation {
                        HostOperation::Filesystem(
                            agentos_execution::host::FilesystemOperation::Read {
                                fd,
                                max_bytes,
                                offset: None,
                                deadline_ms: Some(timeout_ms),
                            },
                        ) => Some((Some(*fd), max_bytes.clone(), *timeout_ms)),
                        HostOperation::Filesystem(
                            agentos_execution::host::FilesystemOperation::StdinRead {
                                max_bytes,
                                timeout_ms,
                            },
                        ) => Some((None, max_bytes.clone(), *timeout_ms)),
                        _ => None,
                    };
                    if let Some((fd, max_bytes, timeout_ms)) = deferred_kernel_read {
                        let deadline =
                            match host_dispatch::checked_deferred_guest_wait_deadline(timeout_ms) {
                                Ok(deadline) => deadline,
                                Err(error) => {
                                    reply.fail(error).map_err(SidecarError::from)?;
                                    continue;
                                }
                            };
                        if let Some(fd) = fd {
                            self.service_descendant_kernel_read(
                                vm_id,
                                process_id,
                                &child_path,
                                Some((fd, max_bytes, deadline, reply)),
                            )?;
                        } else {
                            self.service_descendant_kernel_stdin_read(
                                vm_id,
                                process_id,
                                &child_path,
                                Some((max_bytes, deadline, reply)),
                            )?;
                        }
                        continue;
                    }
                    let descriptor_operation = match &operation {
                        HostOperation::Filesystem(
                            operation @ (agentos_execution::host::FilesystemOperation::Close {
                                ..
                            }
                            | agentos_execution::host::FilesystemOperation::CloseFrom { .. }
                            | agentos_execution::host::FilesystemOperation::Renumber { .. }
                            | agentos_execution::host::FilesystemOperation::DuplicateTo { .. }
                            | agentos_execution::host::FilesystemOperation::Move { .. }),
                        ) => Some(operation.clone()),
                        _ => None,
                    };
                    if let Some(operation) = descriptor_operation {
                        self.dispatch_descendant_context_descriptor_operation(
                            vm_id,
                            process_id,
                            &child_path,
                            operation,
                            reply,
                        )?;
                        continue;
                    }
                    let deferred_posix_poll = match &operation {
                        HostOperation::Network(
                            agentos_execution::host::NetworkOperation::PosixPoll {
                                interests,
                                timeout_ms,
                                signal_mask,
                            },
                        ) => Some((interests.clone(), *timeout_ms, *signal_mask)),
                        _ => None,
                    };
                    if let Some((interests, timeout_ms, signal_mask)) = deferred_posix_poll {
                        let deadline = match timeout_ms
                            .map(host_dispatch::checked_deferred_guest_wait_deadline)
                            .transpose()
                        {
                            Ok(deadline) => deadline,
                            Err(error) => {
                                reply.fail(error).map_err(SidecarError::from)?;
                                continue;
                            }
                        };
                        self.service_descendant_posix_poll(
                            vm_id,
                            process_id,
                            &child_path,
                            (interests, deadline, signal_mask, reply),
                        )?;
                        continue;
                    }
                    let deferred_kernel_poll = match &operation {
                        HostOperation::Network(
                            agentos_execution::host::NetworkOperation::KernelPoll {
                                interests,
                                timeout_ms,
                            },
                        ) => Some((interests.clone(), *timeout_ms)),
                        _ => None,
                    };
                    if let Some((interests, timeout_ms)) = deferred_kernel_poll {
                        let deadline = match timeout_ms
                            .map(host_dispatch::checked_deferred_guest_wait_deadline)
                            .transpose()
                        {
                            Ok(deadline) => deadline,
                            Err(error) => {
                                reply.fail(error).map_err(SidecarError::from)?;
                                continue;
                            }
                        };
                        self.service_descendant_kernel_poll(
                            vm_id,
                            process_id,
                            &child_path,
                            Some((interests, deadline, reply)),
                        )?;
                        continue;
                    }
                    let dns_operation = match &operation {
                        HostOperation::Network(operation)
                            if matches!(
                                operation,
                                agentos_execution::host::NetworkOperation::ResolveDns { .. }
                                    | agentos_execution::host::NetworkOperation::ResolveDnsRecord { .. }
                            ) =>
                        {
                            Some(operation.clone())
                        }
                        _ => None,
                    };
                    if let Some(operation) = dns_operation {
                        self.dispatch_descendant_context_dns_operation(
                            vm_id,
                            process_id,
                            &child_path,
                            operation,
                            reply,
                        )?;
                        continue;
                    }
                    let descendant_udp_poll = match &operation {
                        HostOperation::Network(
                            operation @ agentos_execution::host::NetworkOperation::ManagedUdpPoll {
                                ..
                            },
                        ) => Some(operation.clone()),
                        _ => None,
                    };
                    if let Some(operation) = descendant_udp_poll {
                        dispatch_descendant_context_udp_poll(
                            self,
                            vm_id,
                            process_id,
                            &child_path,
                            operation,
                            reply,
                        )?;
                        continue;
                    }
                    let descendant_stream_read = match &operation {
                        HostOperation::Network(
                            agentos_execution::host::NetworkOperation::ManagedRead {
                                socket_id,
                                max_bytes,
                                peek,
                                wait_ms,
                            },
                        ) => Some((socket_id.as_str().to_owned(), *max_bytes, *peek, *wait_ms)),
                        _ => None,
                    };
                    if let Some((socket_id, max_bytes, peek, wait_ms)) = descendant_stream_read {
                        dispatch_descendant_context_stream_read(
                            self,
                            vm_id,
                            process_id,
                            &child_path,
                            socket_id,
                            max_bytes,
                            peek,
                            wait_ms,
                            reply,
                        )?;
                        continue;
                    }
                    let managed_network = match &operation {
                        HostOperation::Network(operation)
                            if matches!(
                                operation,
                                agentos_execution::host::NetworkOperation::Socket { .. }
                                    | agentos_execution::host::NetworkOperation::Bind { .. }
                                    | agentos_execution::host::NetworkOperation::Connect { .. }
                                    | agentos_execution::host::NetworkOperation::Listen { .. }
                                    | agentos_execution::host::NetworkOperation::Accept { .. }
                                    | agentos_execution::host::NetworkOperation::Validate { .. }
                                    | agentos_execution::host::NetworkOperation::Receive { .. }
                                    | agentos_execution::host::NetworkOperation::Send { .. }
                                    | agentos_execution::host::NetworkOperation::LocalAddress { .. }
                                    | agentos_execution::host::NetworkOperation::PeerAddress { .. }
                                    | agentos_execution::host::NetworkOperation::GetOption { .. }
                                    | agentos_execution::host::NetworkOperation::SetOption { .. }
                                    | agentos_execution::host::NetworkOperation::Poll { .. }
                                    | agentos_execution::host::NetworkOperation::TlsConnect { .. }
                                    | agentos_execution::host::NetworkOperation::ManagedBindUnix { .. }
                                    | agentos_execution::host::NetworkOperation::ManagedBindConnectedUnix { .. }
                                    | agentos_execution::host::NetworkOperation::ManagedReserveTcpPort { .. }
                                    | agentos_execution::host::NetworkOperation::ManagedReleaseTcpPort { .. }
                                    | agentos_execution::host::NetworkOperation::ManagedConnect { .. }
                                    | agentos_execution::host::NetworkOperation::ManagedListen { .. }
                                    | agentos_execution::host::NetworkOperation::ManagedPoll { .. }
                                    | agentos_execution::host::NetworkOperation::ManagedWaitConnect { .. }
                                    | agentos_execution::host::NetworkOperation::ManagedRead { .. }
                                    | agentos_execution::host::NetworkOperation::ManagedWrite { .. }
                                    | agentos_execution::host::NetworkOperation::ManagedDestroy { .. }
                                    | agentos_execution::host::NetworkOperation::ManagedAccept { .. }
                                    | agentos_execution::host::NetworkOperation::ManagedCloseListener { .. }
                                    | agentos_execution::host::NetworkOperation::ManagedTlsUpgrade { .. }
                                    | agentos_execution::host::NetworkOperation::ManagedUdpCreate { .. }
                                    | agentos_execution::host::NetworkOperation::ManagedUdpBind { .. }
                                    | agentos_execution::host::NetworkOperation::ManagedUdpSend { .. }
                                    | agentos_execution::host::NetworkOperation::ManagedUdpClose { .. }
                                    | agentos_execution::host::NetworkOperation::SendDescriptorRights { .. }
                                    | agentos_execution::host::NetworkOperation::ReceiveDescriptorRights { .. }
                            ) => Some(operation.clone()),
                        _ => None,
                    };
                    if let Some(operation) = managed_network {
                        self.dispatch_descendant_context_managed_network_operation(
                            vm_id,
                            process_id,
                            &child_path,
                            operation,
                            reply,
                        )
                        .await?;
                        continue;
                    }
                    let deferred_guest_wait = match &operation {
                        HostOperation::Process(ProcessOperation::Wait {
                            target,
                            options,
                            deadline_ms,
                            temporary_mask,
                        }) => {
                            if temporary_mask.is_some() {
                                reply
                                    .fail(HostServiceError::new(
                                        "EINVAL",
                                        "waitpid does not accept a temporary signal mask",
                                    ))
                                    .map_err(SidecarError::from)?;
                                continue;
                            }
                            let deadline = match deadline_ms
                                .map(host_dispatch::checked_deferred_guest_wait_deadline)
                                .transpose()
                            {
                                Ok(deadline) => deadline,
                                Err(error) => {
                                    reply.fail(error).map_err(SidecarError::from)?;
                                    continue;
                                }
                            };
                            Some((
                                DeferredGuestWaitKind::Process {
                                    target: *target,
                                    options: *options,
                                },
                                deadline,
                            ))
                        }
                        HostOperation::Clock(ClockOperation::Sleep { duration_ms }) => {
                            let deadline = match host_dispatch::checked_deferred_guest_wait_deadline(
                                *duration_ms,
                            ) {
                                Ok(deadline) => deadline,
                                Err(error) => {
                                    reply.fail(error).map_err(SidecarError::from)?;
                                    continue;
                                }
                            };
                            Some((DeferredGuestWaitKind::Sleep, Some(deadline)))
                        }
                        _ => None,
                    };
                    if let Some((kind, deadline)) = deferred_guest_wait {
                        self.service_descendant_guest_wait(
                            vm_id,
                            process_id,
                            &child_path,
                            Some((kind, deadline, reply)),
                        )?;
                        continue;
                    }
                    if matches!(
                        operation,
                        HostOperation::Process(
                            ProcessOperation::Spawn(_)
                                | ProcessOperation::RunCaptured { .. }
                                | ProcessOperation::Exec(_)
                                | ProcessOperation::PollChild { .. }
                                | ProcessOperation::WriteChildStdin { .. }
                                | ProcessOperation::CloseChildStdin { .. }
                        )
                    ) {
                        self.dispatch_descendant_context_process_operation(
                            vm_id,
                            process_id,
                            &child_path,
                            operation,
                            reply,
                        )
                        .await?;
                        continue;
                    }
                    let Some(vm) = self.vms.get_mut(vm_id) else {
                        cancel_direct_host_reply(
                            &reply,
                            "descendant host-call target VM no longer exists",
                        )?;
                        return Ok(Value::Null);
                    };
                    let generation = vm.generation;
                    let (kernel, active_processes) = (&mut vm.kernel, &mut vm.active_processes);
                    let Some(root) = active_processes.get_mut(process_id) else {
                        cancel_direct_host_reply(
                            &reply,
                            "descendant host-call root process no longer exists",
                        )?;
                        return Ok(Value::Null);
                    };
                    let Some(parent) = Self::active_process_by_path_mut(root, current_process_path)
                    else {
                        cancel_direct_host_reply(
                            &reply,
                            "descendant host-call parent process no longer exists",
                        )?;
                        return Ok(Value::Null);
                    };
                    let Some(child) = parent.child_processes.get_mut(child_process_id) else {
                        cancel_direct_host_reply(
                            &reply,
                            "descendant host-call child process no longer exists",
                        )?;
                        return Ok(Value::Null);
                    };
                    let effects =
                        dispatch_host_operation(generation, kernel, child, operation, reply)?;
                    if effects.may_make_fd_readable {
                        Self::wake_ready_deferred_fd_reads(vm)?;
                    }
                    if effects.may_make_fd_writable {
                        Self::wake_ready_deferred_fd_writes(vm)?;
                    }
                    continue;
                }
                ActiveExecutionEvent::Common(other) => {
                    drop(reservation);
                    return Err(SidecarError::InvalidState(format!(
                        "unsupported common child event: {other:?}"
                    )));
                }
                ActiveExecutionEvent::ManagedStreamReadRecheck(pending) => {
                    drop(reservation);
                    pending
                        .reply
                        .fail(HostServiceError::new(
                            "ESTALE",
                            "stream read re-entry targeted descendant child routing",
                        ))
                        .map_err(SidecarError::from)?;
                    continue;
                }
                ActiveExecutionEvent::ManagedUdpPollRecheck(pending) => {
                    drop(reservation);
                    pending
                        .reply
                        .fail(HostServiceError::new(
                            "ESTALE",
                            "UDP poll re-entry targeted descendant child routing",
                        ))
                        .map_err(SidecarError::from)?;
                    continue;
                }
                ActiveExecutionEvent::Stdout(chunk) => {
                    if let Some(sink) = self.descendant_output_stream(
                        vm_id,
                        process_id,
                        current_process_path,
                        child_process_id,
                        InheritedOutputStream::Stdout,
                    )? {
                        let event = match sink {
                            InheritedOutputStream::Stdout => ActiveExecutionEvent::Stdout(chunk),
                            InheritedOutputStream::Stderr => ActiveExecutionEvent::Stderr(chunk),
                        };
                        self.route_descendant_output_to_parent(
                            vm_id,
                            process_id,
                            current_process_path,
                            child_process_id,
                            PolledExecutionEvent { event, reservation },
                        )?;
                        return Ok(Value::Null);
                    }
                    return Ok(json!({
                        "type": "stdout",
                        "data": host_bytes_value(&chunk),
                    }));
                }
                ActiveExecutionEvent::Stderr(chunk) => {
                    if let Some(sink) = self.descendant_output_stream(
                        vm_id,
                        process_id,
                        current_process_path,
                        child_process_id,
                        InheritedOutputStream::Stderr,
                    )? {
                        let event = match sink {
                            InheritedOutputStream::Stdout => ActiveExecutionEvent::Stdout(chunk),
                            InheritedOutputStream::Stderr => ActiveExecutionEvent::Stderr(chunk),
                        };
                        self.route_descendant_output_to_parent(
                            vm_id,
                            process_id,
                            current_process_path,
                            child_process_id,
                            PolledExecutionEvent { event, reservation },
                        )?;
                        return Ok(Value::Null);
                    }
                    return Ok(json!({
                        "type": "stderr",
                        "data": host_bytes_value(&chunk),
                    }));
                }
                ActiveExecutionEvent::Exited(mut exit_code) => {
                    let cleanup_start = Instant::now();
                    let had_trailing_events = {
                        let Some(vm) = self.vms.get_mut(vm_id) else {
                            return Ok(Value::Null);
                        };
                        let Some(parent) = Self::descendant_parent_process_mut(
                            vm,
                            process_id,
                            current_process_path,
                        ) else {
                            return Ok(Value::Null);
                        };
                        let Some(child) = parent.child_processes.get_mut(child_process_id) else {
                            return Ok(Value::Null);
                        };
                        child.discard_pending_exit_events();
                        loop {
                            let next = poll_child_execution_after_exit(child)?;
                            let Some(next) = next else {
                                break;
                            };
                            if matches!(next.event(), ActiveExecutionEvent::Exited(_)) {
                                continue;
                            }
                            child.queue_pending_polled_execution_event(next)?;
                        }
                        if !child.pending_execution_events.is_empty() {
                            // Preserve Node ordering: output and signal-state
                            // events already queued for the child must be
                            // observed before its terminal exit. Requeueing the
                            // exit at the front spins forever on the same exit
                            // while a trailing event remains behind it.
                            child.queue_pending_polled_execution_event(PolledExecutionEvent {
                                event: ActiveExecutionEvent::Exited(exit_code),
                                reservation,
                            })?;
                            true
                        } else {
                            false
                        }
                    };
                    if had_trailing_events {
                        internal_work += 1;
                        continue;
                    }

                    // The native wait status is authoritative for whether the
                    // runner exited normally or was terminated by a signal.
                    // Never infer a signal from 128+N: a program can
                    // legitimately call exit(137).
                    {
                        let Some(vm) = self.vms.get_mut(vm_id) else {
                            return Ok(Value::Null);
                        };
                        let Some(parent) = Self::descendant_parent_process_mut(
                            vm,
                            process_id,
                            current_process_path,
                        ) else {
                            return Ok(Value::Null);
                        };
                        let Some(child) = parent.child_processes.get_mut(child_process_id) else {
                            return Ok(Value::Null);
                        };
                        if let Some(runtime_pid) = child.execution.native_process_id() {
                            if let RuntimeChildStatusObservation::Exited(status) =
                                runtime_child_exit_status(runtime_pid)?
                            {
                                exit_code = status.status;
                                child.exit_signal = status.signal;
                                child.exit_core_dumped = status.core_dumped;
                            }
                        }
                    }

                    let bridge = self.bridge.clone();
                    let Some(vm) = self.vms.get_mut(vm_id) else {
                        return Ok(Value::Null);
                    };
                    let (exit_signal, signal_name, core_dumped) = {
                        let Some(parent) = Self::descendant_parent_process_mut(
                            vm,
                            process_id,
                            current_process_path,
                        ) else {
                            return Ok(Value::Null);
                        };
                        let Some(child) = parent.child_processes.get_mut(child_process_id) else {
                            return Ok(Value::Null);
                        };
                        let actual_signal = child.exit_signal.take();
                        (
                            actual_signal,
                            actual_signal
                                .and_then(canonical_signal_name)
                                .map(str::to_owned),
                            child.exit_core_dumped,
                        )
                    };
                    let terminating_kernel_pids = {
                        let Some(parent) = Self::descendant_parent_process_mut(
                            vm,
                            process_id,
                            current_process_path,
                        ) else {
                            return Ok(Value::Null);
                        };
                        let Some(child) = parent.child_processes.get(child_process_id) else {
                            return Ok(Value::Null);
                        };
                        Self::terminating_process_tree_kernel_pids(child)
                    };
                    for kernel_pid in terminating_kernel_pids {
                        retire_managed_process_routes(&bridge, vm_id, vm, kernel_pid)?;
                    }
                    let Some(parent) =
                        Self::descendant_parent_process_mut(vm, process_id, current_process_path)
                    else {
                        return Ok(Value::Null);
                    };
                    let guest_owns_kernel_wait = parent.execution.descendant_wait_ownership()
                        == DescendantWaitOwnership::Guest;
                    let Some(mut child) = parent.child_processes.remove(child_process_id) else {
                        return Ok(Value::Null);
                    };
                    let child_process_label =
                        Self::child_process_path_label(process_id, &child_path);
                    let detached_children =
                        Self::adopt_detached_child_processes(&child_process_label, &mut child);
                    release_inherited_child_raw_mode(&mut vm.kernel, &child)?;
                    let kernel_readiness = Arc::clone(&vm.kernel_socket_readiness);
                    let unix_address_registry = Arc::clone(&vm.unix_address_registry);
                    terminate_child_process_tree(
                        &mut vm.kernel,
                        &mut child,
                        &kernel_readiness,
                        &unix_address_registry,
                    );
                    finish_kernel_child_from_runtime_exit(
                        &child.kernel_handle,
                        exit_code,
                        exit_signal,
                        core_dumped,
                    );
                    if !guest_owns_kernel_wait {
                        vm.kernel
                            .wait_and_reap(child.kernel_pid)
                            .map_err(kernel_error)?;
                    }
                    for (detached_process_id, detached_child) in detached_children {
                        vm.detached_child_processes
                            .insert(detached_process_id.clone());
                        vm.active_processes
                            .insert(detached_process_id, detached_child);
                    }
                    let mut payload = Map::new();
                    payload.insert(String::from("type"), Value::String(String::from("exit")));
                    payload.insert(String::from("exitCode"), Value::from(exit_code));
                    payload.insert(String::from("coreDumped"), Value::from(core_dumped));
                    if let Some(signal_name) = signal_name {
                        payload.insert(String::from("signal"), Value::String(signal_name));
                    }
                    record_execute_phase("child_process_exit_cleanup", cleanup_start.elapsed());
                    return Ok(Value::Object(payload));
                }
                ActiveExecutionEvent::HostRpcRequest(request) => {
                    drop(reservation);
                    let mut current_child_path = current_process_path.to_vec();
                    current_child_path.push(child_process_id);
                    let kernel_wait_request = {
                        let Some(vm) = self.vms.get(vm_id) else {
                            return Ok(Value::Null);
                        };
                        let Some(root) = vm.active_processes.get(process_id) else {
                            return Ok(Value::Null);
                        };
                        let Some(parent) = Self::active_process_by_path(root, current_process_path)
                        else {
                            return Ok(Value::Null);
                        };
                        let Some(child) = parent.child_processes.get(child_process_id) else {
                            return Ok(Value::Null);
                        };
                        deferred_kernel_wait_request_for_process(&request, &vm.kernel, child)?
                    };
                    if let Some(kernel_wait_request) = kernel_wait_request {
                        let kernel_wait_call = ExecutionHostCall {
                            request: kernel_wait_request,
                            reply: request.reply.clone(),
                        };
                        if self.service_child_kernel_wait_rpc(
                            vm_id,
                            process_id,
                            current_process_path,
                            child_process_id,
                            &kernel_wait_call,
                        )? {
                            if javascript_sync_rpc_may_make_fd_writable(&kernel_wait_call) {
                                let Some(vm) = self.vms.get_mut(vm_id) else {
                                    return Ok(Value::Null);
                                };
                                Self::wake_ready_deferred_fd_writes(vm)?;
                            }
                            let parked = self
                                .vms
                                .get(vm_id)
                                .and_then(|vm| vm.active_processes.get(process_id))
                                .and_then(|root| {
                                    Self::active_process_by_path(root, current_process_path)
                                })
                                .and_then(|parent| parent.child_processes.get(child_process_id))
                                .and_then(|child| child.deferred_kernel_wait_rpc.as_ref())
                                .is_some_and(|(parked, _)| parked.id == kernel_wait_call.id);
                            if parked {
                                // The execution keeps exposing the unresolved
                                // sync request until it receives a reply. Yield
                                // the sidecar actor so capacity/deadline wakes
                                // can drive the next bounded recheck.
                                return Ok(Value::Null);
                            }
                            // An immediate response may have made a following
                            // execution event available in the same turn.
                            continue;
                        }
                    }
                    if request.method == "__kernel_stdio_write" {
                        let shared_tty = {
                            let Some(vm) = self.vms.get_mut(vm_id) else {
                                return Ok(Value::Null);
                            };
                            let Some(root) = vm.active_processes.get_mut(process_id) else {
                                return Ok(Value::Null);
                            };
                            let Some(parent) =
                                Self::active_process_by_path_mut(root, current_process_path)
                            else {
                                return Ok(Value::Null);
                            };
                            parent
                                .child_processes
                                .get(child_process_id)
                                .and_then(|child| {
                                    child
                                        .tty_master_owner
                                        .map(|owner| (child.kernel_pid, owner))
                                })
                        };
                        if let Some((child_kernel_pid, owner)) = shared_tty {
                            let response = self.service_shared_tty_stdio_write(
                                vm_id,
                                child_kernel_pid,
                                owner,
                                &request,
                            );
                            settle_execution_host_call(&request.reply, response.map(Into::into))?;
                            continue;
                        }
                    }
                    let response = if request.method == "process.signal_state" {
                        let (signal, registration) =
                            parse_process_signal_state_request(&request.args)
                                .map_err(SidecarError::from)?;
                        let Some(vm) = self.vms.get_mut(vm_id) else {
                            return Ok(Value::Null);
                        };
                        let Some(root) = vm.active_processes.get(process_id) else {
                            return Ok(Value::Null);
                        };
                        let Some(process) = Self::active_process_by_path(root, &current_child_path)
                        else {
                            return Ok(Value::Null);
                        };
                        apply_kernel_signal_registration(process, signal, &registration)?;
                        Ok(Value::Null.into())
                    } else if request.method == "process.kill" {
                        self.handle_descendant_process_kill_rpc(
                            vm_id,
                            process_id,
                            current_process_path,
                            child_process_id,
                            &request,
                        )
                        .map(Into::into)
                    } else if request.method.starts_with("child_process.") {
                        self.handle_descendant_javascript_child_process_rpc(
                            vm_id,
                            process_id,
                            &current_child_path,
                            &request,
                        )
                        .await
                    } else {
                        let Some(vm) = self.vms.get_mut(vm_id) else {
                            return Ok(Value::Null);
                        };
                        let socket_paths = build_socket_path_context(vm)?;
                        let kernel_readiness = Arc::clone(&vm.kernel_socket_readiness);
                        let capabilities = vm.capabilities.clone();
                        let Some(root) = vm.active_processes.get_mut(process_id) else {
                            return Ok(Value::Null);
                        };
                        let Some(parent) =
                            Self::active_process_by_path_mut(root, current_process_path)
                        else {
                            return Ok(Value::Null);
                        };
                        let Some(child) = parent.child_processes.get_mut(child_process_id) else {
                            return Ok(Value::Null);
                        };
                        service_javascript_sync_rpc(JavascriptSyncRpcServiceRequest {
                            bridge: &self.bridge,
                            vm_id,
                            dns: &vm.dns,
                            socket_paths: &socket_paths,
                            kernel: &mut vm.kernel,
                            kernel_readiness,
                            process: child,
                            sync_request: &request,
                            capabilities,
                            managed_descriptions: Some(Arc::clone(
                                &vm.managed_host_net_descriptions,
                            )),
                        })
                        .await
                    };

                    let response = match response {
                        Ok(HostServiceResponse::Deferred {
                            receiver,
                            timeout,
                            task_class,
                        }) => {
                            let Some(vm) = self.vms.get(vm_id) else {
                                return Ok(Value::Null);
                            };
                            let runtime = vm.runtime_context.clone();
                            let connection_id = vm.connection_id.clone();
                            let session_id = vm.session_id.clone();
                            let sender = self.process_event_sender.clone();
                            let event_notify = Arc::clone(&self.process_event_notify);
                            let envelope_vm_id = vm_id.to_owned();
                            let envelope_process_id =
                                Self::child_process_path_label(process_id, &current_child_path);
                            let reply = request.reply.clone();
                            let method = request.method.clone();
                            runtime
                                .spawn(task_class, async move {
                                    let receive = async {
                                        receiver.await.unwrap_or_else(|_| {
                                            Err(crate::state::DeferredRpcError {
                                                code: String::from(
                                                    "ERR_AGENTOS_DEFERRED_RPC_RESPONSE_CHANNEL_CLOSED",
                                                ),
                                                message: format!(
                                                    "deferred sync RPC response channel closed for {method}"
                                                ),
                                                details: None,
                                            })
                                        })
                                    };
                                    let result = match timeout {
                                        Some(timeout) => {
                                            match crate::execution::operation_deadline_timeout(
                                                &method,
                                                timeout,
                                                receive,
                                            )
                                            .await
                                            {
                                                Ok(result) => result,
                                                Err(_) => Err(crate::state::DeferredRpcError {
                                                    code: String::from(
                                                        "ERR_AGENTOS_DEFERRED_RPC_TIMEOUT",
                                                    ),
                                                    message: format!(
                                                        "deferred sync RPC {method} timed out after {} ms",
                                                        timeout.as_millis()
                                                    ),
                                                    details: None,
                                                }),
                                            }
                                        }
                                        None => receive.await,
                                    };
                                    let envelope = ProcessEventEnvelope {
                                        connection_id,
                                        session_id,
                                        vm_id: envelope_vm_id,
                                        process_id: envelope_process_id,
                                        event: ActiveExecutionEvent::HostCallCompletion(
                                            crate::state::HostCallCompletion {
                                                reply,
                                                result,
                                            },
                                        ),
                                    };
                                    if let Err(error) = sender.send(envelope).await {
                                        if let ActiveExecutionEvent::HostCallCompletion(
                                            completion,
                                        ) = error.0.event
                                        {
                                            if let Err(settlement_error) =
                                                cancel_host_call_completion(
                                                    &completion,
                                                    "nested deferred sync RPC completion lane closed",
                                                )
                                            {
                                                eprintln!(
                                                    "ERR_AGENTOS_NESTED_COMPLETION_SETTLEMENT: {settlement_error}"
                                                );
                                            }
                                        }
                                        eprintln!(
                                            "ERR_AGENTOS_PROCESS_EVENT_CHANNEL_CLOSED: nested deferred sync RPC completion could not be delivered"
                                        );
                                    } else {
                                        event_notify.notify_one();
                                    }
                                })
                                .map_err(SidecarError::from)?;
                            continue;
                        }
                        other => other,
                    };

                    if response.is_ok() && javascript_sync_rpc_may_make_fd_readable(&request) {
                        let Some(vm) = self.vms.get_mut(vm_id) else {
                            return Ok(Value::Null);
                        };
                        Self::wake_ready_deferred_fd_reads(vm)?;
                    }
                    if response.is_ok() && javascript_sync_rpc_may_make_fd_writable(&request) {
                        let Some(vm) = self.vms.get_mut(vm_id) else {
                            return Ok(Value::Null);
                        };
                        Self::wake_ready_deferred_fd_writes(vm)?;
                    }

                    let parent_signal_event = response
                        .as_ref()
                        .ok()
                        .and_then(HostServiceResponse::as_json)
                        .and_then(|result| {
                        let target_path_label =
                            Self::child_process_path_label(process_id, current_process_path);
                        if request.method != "process.kill"
                            || result.get("action").and_then(Value::as_str) != Some("user")
                            || result.get("targetProcessPath").and_then(Value::as_str)
                                != Some(target_path_label.as_str())
                        {
                            return None;
                        }
                        Some(json!({
                            "type": "signal",
                            "signal": result.get("signal").and_then(Value::as_str).unwrap_or_default(),
                            "number": result.get("number").and_then(Value::as_i64).unwrap_or_default(),
                        }))
                    });
                    settle_execution_host_call(&request.reply, response)?;
                    if let Some(event) = parent_signal_event {
                        return Ok(event);
                    }
                }
                ActiveExecutionEvent::HostCallCompletion(completion) => {
                    drop(reservation);
                    let Some(vm) = self.vms.get_mut(vm_id) else {
                        cancel_host_call_completion(
                            &completion,
                            "nested deferred sync RPC target VM no longer exists",
                        )?;
                        return Ok(Value::Null);
                    };
                    let kernel_readiness = Arc::clone(&vm.kernel_socket_readiness);
                    let unix_addresses = Arc::clone(&vm.unix_address_registry);
                    let managed_descriptions = Arc::clone(&vm.managed_host_net_descriptions);
                    let (kernel, active_processes) = (&mut vm.kernel, &mut vm.active_processes);
                    let Some(root) = active_processes.get_mut(process_id) else {
                        cancel_host_call_completion(
                            &completion,
                            "nested deferred sync RPC root process no longer exists",
                        )?;
                        return Ok(Value::Null);
                    };
                    let Some(parent) = Self::active_process_by_path_mut(root, current_process_path)
                    else {
                        cancel_host_call_completion(
                            &completion,
                            "nested deferred sync RPC parent process no longer exists",
                        )?;
                        return Ok(Value::Null);
                    };
                    let Some(child) = parent.child_processes.get_mut(child_process_id) else {
                        cancel_host_call_completion(
                            &completion,
                            "nested deferred sync RPC child process no longer exists",
                        )?;
                        return Ok(Value::Null);
                    };
                    settle_host_call_completion_for_process(
                        kernel,
                        &kernel_readiness,
                        &unix_addresses,
                        &managed_descriptions,
                        child,
                        completion,
                    )?;
                }
                ActiveExecutionEvent::SignalState {
                    signal,
                    registration,
                } => {
                    drop(reservation);
                    let Some(vm) = self.vms.get_mut(vm_id) else {
                        return Ok(Value::Null);
                    };
                    let Some(root) = vm.active_processes.get(process_id) else {
                        return Ok(Value::Null);
                    };
                    let Some(process) = Self::active_process_by_path(root, &child_path) else {
                        return Ok(Value::Null);
                    };
                    apply_kernel_signal_registration(process, signal, &registration)?;
                    return Ok(json!({
                        "type": "signal_state",
                        "signal": signal,
                        "registration": registration,
                    }));
                }
            }
        }
    }

    fn recover_descendant_runtime_child_process_event(
        &mut self,
        vm_id: &str,
        process_id: &str,
        current_process_path: &[&str],
        child_process_id: &str,
    ) -> Result<Option<ActiveExecutionEvent>, SidecarError> {
        let (parent_kernel_pid, child_kernel_pid, child_runtime_pid) = {
            let mut child_path = current_process_path.to_vec();
            child_path.push(child_process_id);
            let Some(vm) = self.vms.get_mut(vm_id) else {
                return Ok(None);
            };
            let Some(parent) =
                Self::descendant_parent_process_mut(vm, process_id, current_process_path)
            else {
                return Err(javascript_child_process_gone_error(process_id, &child_path));
            };
            let Some(child) = parent.child_processes.get_mut(child_process_id) else {
                return Err(javascript_child_process_gone_error(process_id, &child_path));
            };
            (
                parent.kernel_pid,
                child.kernel_pid,
                child.execution.native_process_id(),
            )
        };
        let Some(vm) = self.vms.get_mut(vm_id) else {
            return Ok(None);
        };
        if let Some(process_info) = vm.kernel.list_processes().get(&child_kernel_pid) {
            if process_info.status == ProcessStatus::Exited {
                return Ok(Some(ActiveExecutionEvent::Exited(
                    process_info.exit_code.unwrap_or(0),
                )));
            }
        }
        if let Some(wait_result) = vm
            .kernel
            .waitpid_with_options(
                EXECUTION_DRIVER_NAME,
                parent_kernel_pid,
                child_kernel_pid as i32,
                WaitPidFlags::WNOHANG,
            )
            .map_err(kernel_error)?
        {
            return Ok(Some(ActiveExecutionEvent::Exited(wait_result.status)));
        }

        if let Some(child_runtime_pid) = child_runtime_pid {
            match runtime_child_exit_status(child_runtime_pid)? {
                RuntimeChildStatusObservation::Exited(status) => {
                    let Some(root) = vm.active_processes.get_mut(process_id) else {
                        return Ok(None);
                    };
                    let Some(parent) = Self::active_process_by_path_mut(root, current_process_path)
                    else {
                        return Ok(None);
                    };
                    let Some(child) = parent.child_processes.get_mut(child_process_id) else {
                        return Ok(None);
                    };
                    child.exit_signal = status.signal;
                    child.exit_core_dumped = status.core_dumped;
                    return Ok(Some(ActiveExecutionEvent::Exited(status.status)));
                }
                RuntimeChildStatusObservation::Running => {}
                RuntimeChildStatusObservation::NotWaitable => {
                    return Err(SidecarError::host("ECHILD", format!("guest runtime process {child_runtime_pid} exited without an observable wait status"
                    )));
                }
            }
        }
        Ok(None)
    }

    fn write_descendant_process_stdin(
        &mut self,
        vm_id: &str,
        process_id: &str,
        current_process_path: &[&str],
        child_process_id: &str,
        chunk: &[u8],
    ) -> Result<(), SidecarError> {
        let mut child_path = current_process_path.to_vec();
        child_path.push(child_process_id);
        let Some(vm) = self.vms.get_mut(vm_id) else {
            return Err(javascript_child_process_gone_error(process_id, &child_path));
        };
        let Some(root) = vm.active_processes.get_mut(process_id) else {
            return Err(javascript_child_process_gone_error(process_id, &child_path));
        };
        let Some(parent) = Self::active_process_by_path_mut(root, current_process_path) else {
            return Err(javascript_child_process_gone_error(process_id, &child_path));
        };
        let Some(child) = parent.child_processes.get_mut(child_process_id) else {
            return Err(javascript_child_process_gone_error(process_id, &child_path));
        };
        write_kernel_process_stdin(&mut vm.kernel, child, chunk)?;
        self.process_event_notify.notify_one();
        Ok(())
    }

    fn close_descendant_process_stdin(
        &mut self,
        vm_id: &str,
        process_id: &str,
        current_process_path: &[&str],
        child_process_id: &str,
    ) -> Result<(), SidecarError> {
        let mut child_path = current_process_path.to_vec();
        child_path.push(child_process_id);
        let Some(vm) = self.vms.get_mut(vm_id) else {
            return Err(javascript_child_process_gone_error(process_id, &child_path));
        };
        let Some(root) = vm.active_processes.get_mut(process_id) else {
            return Err(javascript_child_process_gone_error(process_id, &child_path));
        };
        let Some(parent) = Self::active_process_by_path_mut(root, current_process_path) else {
            return Err(javascript_child_process_gone_error(process_id, &child_path));
        };
        let Some(child) = parent.child_processes.get_mut(child_process_id) else {
            return missing_javascript_child_cleanup_result(
                parent.next_child_process_id,
                child_process_id,
                "stdin close",
            );
        };
        close_kernel_process_stdin(&mut vm.kernel, child)?;
        self.process_event_notify.notify_one();
        Ok(())
    }

    fn kill_descendant_javascript_child_process(
        &mut self,
        vm_id: &str,
        process_id: &str,
        current_process_path: &[&str],
        child_process_id: &str,
        signal: &str,
    ) -> Result<(), SidecarError> {
        let signal_name = signal.to_owned();
        let signal = parse_signal(signal)?;
        let Some(vm) = self.vms.get_mut(vm_id) else {
            return Ok(());
        };
        let Some(root) = vm.active_processes.get_mut(process_id) else {
            return Ok(());
        };
        let Some(parent) = Self::active_process_by_path_mut(root, current_process_path) else {
            return Ok(());
        };
        let source_pid = parent.kernel_pid;
        let Some(child) = parent.child_processes.get_mut(child_process_id) else {
            return Ok(());
        };
        terminate_tracked_child_process_for_signal(&mut vm.kernel, child, signal, None)?;
        let child_process_label = if current_process_path.is_empty() {
            child_process_id.to_owned()
        } else {
            format!("{}/{}", current_process_path.join("/"), child_process_id)
        };
        emit_security_audit_event(
            &self.bridge,
            vm_id,
            "security.process.kill",
            audit_fields([
                (String::from("source"), String::from("guest_child_process")),
                (String::from("source_pid"), source_pid.to_string()),
                (String::from("target_pid"), child.kernel_pid.to_string()),
                (String::from("process_id"), process_id.to_owned()),
                (String::from("child_process_id"), child_process_label),
                (String::from("signal"), signal_name),
            ]),
        );
        self.process_event_notify.notify_one();
        Ok(())
    }

    fn handle_descendant_process_kill_rpc(
        &mut self,
        vm_id: &str,
        process_id: &str,
        current_process_path: &[&str],
        child_process_id: &str,
        request: &HostRpcRequest,
    ) -> Result<Value, SidecarError> {
        let target_pid = javascript_sync_rpc_arg_i32(&request.args, 0, "process.kill target pid")?;
        let signal_name = javascript_sync_rpc_arg_str(&request.args, 1, "process.kill signal")?;
        let signal = parse_signal(signal_name)?;

        let mut source_path = current_process_path.to_vec();
        source_path.push(child_process_id);

        if signal != 0 && target_pid < 0 {
            let pgid = target_pid.unsigned_abs();
            let caller_kernel_pid = {
                let Some(vm) = self.vms.get(vm_id) else {
                    return Err(SidecarError::host(
                        "ESRCH",
                        String::from("unknown VM during process.kill"),
                    ));
                };
                let Some(root) = vm.active_processes.get(process_id) else {
                    return Err(SidecarError::host(
                        "ESRCH",
                        format!("unknown process {process_id} during process.kill",),
                    ));
                };
                let Some(source) = Self::active_process_by_path(root, &source_path) else {
                    return Err(SidecarError::host(
                        "ESRCH",
                        format!("unknown child process {child_process_id} during process.kill",),
                    ));
                };
                source.kernel_pid
            };
            let caller_is_member =
                self.signal_vm_process_group(vm_id, caller_kernel_pid, pgid, signal_name)?;
            if !caller_is_member {
                return Ok(Value::Null);
            }
            let Some(vm) = self.vms.get_mut(vm_id) else {
                return Ok(Value::Null);
            };
            let Some(root) = vm.active_processes.get_mut(process_id) else {
                return Ok(Value::Null);
            };
            let Some(source) = Self::active_process_by_path_mut(root, &source_path) else {
                return Ok(Value::Null);
            };
            let action = protocol_signal_registration(
                source
                    .kernel_handle
                    .signal_action(signal, None)
                    .map_err(kernel_error)?,
            )
            .action;
            terminate_tracked_child_process_for_signal(&mut vm.kernel, source, signal, None)?;
            return Ok(json!({
                "self": true,
                "action": match action {
                    SignalDispositionAction::Default => "default",
                    SignalDispositionAction::Ignore => "ignore",
                    SignalDispositionAction::User => "user",
                },
            }));
        }

        let Some(vm) = self.vms.get_mut(vm_id) else {
            return Err(SidecarError::host(
                "ESRCH",
                String::from("unknown VM during process.kill"),
            ));
        };

        if signal == 0 {
            vm.kernel
                .signal_process(EXECUTION_DRIVER_NAME, target_pid, signal)
                .map_err(kernel_error)?;
            return Ok(Value::Null);
        }

        let target_kernel_pid = u32::try_from(target_pid).map_err(|_| {
            SidecarError::host("EINVAL", format!("invalid process pid {target_pid}"))
        })?;
        let (source_pid, located_target_path) = {
            let Some(root) = vm.active_processes.get(process_id) else {
                return Err(SidecarError::host(
                    "ESRCH",
                    format!("unknown process {process_id} during process.kill",),
                ));
            };
            let Some(source) = Self::active_process_by_path(root, &source_path) else {
                return Err(SidecarError::host(
                    "ESRCH",
                    format!("unknown child process {child_process_id} during process.kill",),
                ));
            };
            vm.kernel
                .signal_process(EXECUTION_DRIVER_NAME, target_pid, 0)
                .map_err(kernel_error)?;
            (
                source.kernel_pid,
                Self::active_process_path_by_kernel_pid(root, target_kernel_pid),
            )
        };
        let Some(target_path) = located_target_path else {
            // The target is alive but not part of this root's process tree.
            // Resolve it VM-wide so cross-tree pids and untracked kernel
            // processes still receive the signal.
            self.signal_vm_kernel_pid(vm_id, target_kernel_pid, signal_name)?;
            return Ok(Value::Null);
        };
        let Some(vm) = self.vms.get_mut(vm_id) else {
            return Err(SidecarError::host(
                "ESRCH",
                String::from("unknown VM during process.kill"),
            ));
        };

        let self_signal = source_pid == target_kernel_pid;
        let action = {
            let Some(root) = vm.active_processes.get(process_id) else {
                return Ok(Value::Null);
            };
            let target_path_refs = target_path.iter().map(String::as_str).collect::<Vec<_>>();
            let Some(target) = Self::active_process_by_path(root, &target_path_refs) else {
                return Err(SidecarError::host(
                    "ESRCH",
                    format!("unknown process pid {target_pid}"),
                ));
            };
            let action = if signal == 0 {
                SignalDispositionAction::Default
            } else {
                protocol_signal_registration(
                    target
                        .kernel_handle
                        .signal_action(signal, None)
                        .map_err(kernel_error)?,
                )
                .action
            };
            action
        };

        let Some(root) = vm.active_processes.get_mut(process_id) else {
            return Ok(Value::Null);
        };
        let Some(target) = Self::active_process_by_owned_path_mut(root, &target_path) else {
            return Err(SidecarError::host(
                "ESRCH",
                format!("unknown process pid {target_pid}"),
            ));
        };
        terminate_tracked_child_process_for_signal(&mut vm.kernel, target, signal, None)?;

        let action = match action {
            SignalDispositionAction::Default => "default",
            SignalDispositionAction::Ignore => "ignore",
            SignalDispositionAction::User => "user",
        };

        let target_path_label = Self::child_process_path_label(
            process_id,
            &target_path.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        emit_security_audit_event(
            &self.bridge,
            vm_id,
            "security.process.kill",
            audit_fields([
                (String::from("source"), String::from("guest_process")),
                (String::from("source_pid"), source_pid.to_string()),
                (String::from("target_pid"), target_pid.to_string()),
                (String::from("process_id"), process_id.to_owned()),
                (
                    String::from("target_process_path"),
                    target_path_label.clone(),
                ),
                (String::from("signal"), signal_name.to_owned()),
            ]),
        );

        Ok(json!({
            "self": self_signal,
            "action": action,
            "signal": signal_name,
            "number": signal,
            "targetProcessPath": target_path_label,
        }))
    }

    pub(crate) async fn poll_child_process(
        &mut self,
        vm_id: &str,
        process_id: &str,
        child_process_id: &str,
        wait_ms: u64,
    ) -> Result<Value, SidecarError> {
        self.poll_descendant_process(vm_id, process_id, &[], child_process_id, wait_ms)
            .await
    }

    pub(crate) fn validate_child_poll_target(
        &self,
        vm_id: &str,
        root_process_id: &str,
        caller_process_path: &[&str],
        child_process_id: &str,
    ) -> Result<(), SidecarError> {
        let vm = self.vms.get(vm_id).ok_or_else(|| missing_vm_error(vm_id))?;
        let root = vm
            .active_processes
            .get(root_process_id)
            .ok_or_else(|| missing_process_error(vm_id, root_process_id))?;
        let caller = Self::active_process_by_path(root, caller_process_path).ok_or_else(|| {
            javascript_child_process_gone_error(root_process_id, caller_process_path)
        })?;
        if !caller.child_processes.contains_key(child_process_id) {
            return Err(javascript_child_process_gone_error(
                root_process_id,
                &[child_process_id],
            ));
        }
        Ok(())
    }

    pub(crate) fn write_child_process_stdin(
        &mut self,
        vm_id: &str,
        process_id: &str,
        child_process_id: &str,
        chunk: &[u8],
    ) -> Result<(), SidecarError> {
        let Some(vm) = self.vms.get_mut(vm_id) else {
            return Err(javascript_child_process_gone_error(
                process_id,
                &[child_process_id],
            ));
        };
        let Some(child) = vm
            .active_processes
            .get_mut(process_id)
            .ok_or_else(|| missing_process_error(vm_id, process_id))?
            .child_processes
            .get_mut(child_process_id)
        else {
            return Err(javascript_child_process_gone_error(
                process_id,
                &[child_process_id],
            ));
        };
        write_kernel_process_stdin(&mut vm.kernel, child, chunk)?;
        self.process_event_notify.notify_one();
        Ok(())
    }

    pub(crate) fn close_child_process_stdin(
        &mut self,
        vm_id: &str,
        process_id: &str,
        child_process_id: &str,
    ) -> Result<(), SidecarError> {
        let Some(vm) = self.vms.get_mut(vm_id) else {
            return Err(javascript_child_process_gone_error(
                process_id,
                &[child_process_id],
            ));
        };
        let Some(child) = vm
            .active_processes
            .get_mut(process_id)
            .ok_or_else(|| missing_process_error(vm_id, process_id))?
            .child_processes
            .get_mut(child_process_id)
        else {
            return Err(javascript_child_process_gone_error(
                process_id,
                &[child_process_id],
            ));
        };
        close_kernel_process_stdin(&mut vm.kernel, child)?;
        self.process_event_notify.notify_one();
        Ok(())
    }

    pub(crate) fn kill_javascript_child_process(
        &mut self,
        vm_id: &str,
        process_id: &str,
        child_process_id: &str,
        signal: &str,
    ) -> Result<(), SidecarError> {
        let signal_name = signal.to_owned();
        let signal = parse_signal(signal)?;
        let Some(vm) = self.vms.get_mut(vm_id) else {
            return Ok(());
        };
        let process = vm
            .active_processes
            .get_mut(process_id)
            .ok_or_else(|| missing_process_error(vm_id, process_id))?;
        let source_pid = process.kernel_pid;
        let Some(child) = process.child_processes.get_mut(child_process_id) else {
            // Child IDs are monotonically allocated per parent. An allocated
            // ID that is no longer present was already reaped (or rolled back),
            // so cleanup kills are idempotent without hiding never-issued IDs.
            return missing_javascript_child_cleanup_result(
                process.next_child_process_id,
                child_process_id,
                "kill",
            );
        };
        terminate_tracked_child_process_for_signal(&mut vm.kernel, child, signal, None)?;
        emit_security_audit_event(
            &self.bridge,
            vm_id,
            "security.process.kill",
            audit_fields([
                (String::from("source"), String::from("guest_child_process")),
                (String::from("source_pid"), source_pid.to_string()),
                (String::from("target_pid"), child.kernel_pid.to_string()),
                (String::from("process_id"), process_id.to_owned()),
                (
                    String::from("child_process_id"),
                    child_process_id.to_owned(),
                ),
                (String::from("signal"), signal_name),
            ]),
        );
        self.process_event_notify.notify_one();
        Ok(())
    }
}
