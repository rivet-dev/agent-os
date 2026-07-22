use super::super::*;
use crate::filesystem::javascript_sync_rpc_path_arg;
use crate::state::ManagedHostNetRoute;
use agentos_kernel::vfs::{VirtualTimeSpec, VirtualUtimeSpec};

const ALLOWED_WASM_PROCESS_SYNC_RPCS: &[&str] = &[
    "process.umask",
    "process.exec_image_open",
    "process.exec_image_open_fd",
    "process.exec_image_read",
    "process.exec_image_close",
    "process.image",
    "__kernel_tcgetattr",
    "__kernel_tcsetattr",
    "__kernel_tcgetpgrp",
    "__kernel_tcsetpgrp",
    "__kernel_tcgetsid",
    "__kernel_tty_set_size",
    "process.getrlimit",
    "process.setrlimit",
    "process.getuid",
    "process.getgid",
    "process.geteuid",
    "process.getegid",
    "process.getresuid",
    "process.getresgid",
    "process.getgroups",
    "process.getpwuid",
    "process.getpwnam",
    "process.getpwent",
    "process.getgrgid",
    "process.getgrnam",
    "process.getgrent",
    "process.setuid",
    "process.seteuid",
    "process.setreuid",
    "process.setresuid",
    "process.setgid",
    "process.setegid",
    "process.setregid",
    "process.setresgid",
    "process.setgroups",
    "fs.accessSync",
    "fs.blockingIoTimeoutMsSync",
    "fs.chmodForProcessSync",
    "fs.chownSync",
    "fs.collapseRangeSync",
    "fs.fallocateSync",
    "fs.fiemapSync",
    "fs.fiemapAtSync",
    "fs.fgetxattrSync",
    "fs.flistxattrSync",
    "fs.fsetxattrSync",
    "fs.fremovexattrSync",
    "fs.getxattrSync",
    "fs.insertRangeSync",
    "fs.lchownSync",
    "fs.linkFdSync",
    "fs.listxattrSync",
    "fs.mknodSync",
    "fs.namedFifoPeerReadySync",
    "fs.openTmpfileSync",
    "fs.punchHoleSync",
    "fs.remountSync",
    "fs.removexattrSync",
    "fs.renameAt2Sync",
    "fs.setxattrSync",
    "fs.statfsSync",
    "fs.truncateForProcessSync",
    "fs.zeroRangeSync",
    "process.getpgid",
    "process.setpgid",
    "process.waitpid_transition",
    "process.waitpid",
    "process.itimer_real",
    "process.signal_begin",
    "process.signal_end",
    "process.signal_mask",
    "process.signal_mask_scope_begin",
    "process.signal_mask_scope_end",
    "process.fd_pipe",
    "process.fd_open",
    "process.path_open_at",
    "process.path_mkdir_at",
    "process.path_stat_at",
    "process.path_statfs_at",
    "process.path_chmod_at",
    "process.path_chown_at",
    "process.path_utimes_at",
    "process.path_link_at",
    "process.path_readlink_at",
    "process.path_remove_dir_at",
    "process.path_rename_at",
    "process.path_symlink_at",
    "process.path_unlink_at",
    "process.random_get",
    "process.clock_time",
    "process.clock_resolution",
    "process.sleep",
    "process.system_identity",
    "process.fd_snapshot",
    "process.hostnet_fd_open",
    "process.hostnet_bind",
    "process.hostnet_connect",
    "process.hostnet_listen",
    "process.hostnet_accept",
    "process.hostnet_validate",
    "process.hostnet_recv",
    "process.hostnet_send",
    "process.hostnet_local_address",
    "process.hostnet_peer_address",
    "process.hostnet_get_option",
    "process.hostnet_set_option",
    "process.hostnet_poll",
    "process.hostnet_tls_connect",
    "process.posix_poll",
    "process.fd_description_identity",
    "process.fd_description_alias_count",
    "process.fd_preopens",
    "process.fd_preopen",
    "process.fd_read",
    "process.fd_pread",
    "process.fd_write",
    "process.fd_pwrite",
    "process.fd_sync",
    "process.fd_datasync",
    "process.fd_readdir",
    "process.fd_close",
    "process.fd_closefrom",
    "process.fd_stat",
    "process.fd_filestat",
    "process.fd_chmod",
    "process.fd_chown",
    "process.fd_truncate",
    "process.fd_utimes",
    "process.fd_set_flags",
    "process.fd_getfd",
    "process.fd_setfd",
    "process.fd_flock",
    "process.fd_record_lock",
    "process.fd_record_lock_cancel",
    "process.fd_dup",
    "process.fd_dup2",
    "process.fd_dup_min",
    "process.fd_move",
    "process.fd_seek",
    "process.fd_path",
    "process.fd_chdir_path",
    "process.fd_socketpair",
    "process.pty_open",
    "process.fd_sendmsg_rights",
    "process.fd_recvmsg_rights",
    "process.fd_socket_shutdown",
    "dns.resolveRawRr",
];

fn decode_javascript_dgram_operation(
    request: &HostRpcRequest,
) -> Result<DgramOperation, SidecarError> {
    let string =
        |index, label| javascript_sync_rpc_arg_str(&request.args, index, label).map(str::to_owned);
    match request.method.as_str() {
        "dgram.createSocket" => {
            let payload: DgramCreateSocketOptions =
                serde_json::from_value(request.args.first().cloned().ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "dgram.createSocket requires a request payload",
                    ))
                })?)
                .map_err(|error| {
                    SidecarError::InvalidState(format!(
                        "invalid dgram.createSocket payload: {error}"
                    ))
                })?;
            Ok(DgramOperation::Create {
                family: UdpFamily::from_socket_type(&payload.socket_type)?,
            })
        }
        "dgram.bind" => {
            let payload: DgramBindOptions =
                serde_json::from_value(request.args.get(1).cloned().ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "dgram.bind requires a request payload",
                    ))
                })?)
                .map_err(|error| {
                    SidecarError::InvalidState(format!("invalid dgram.bind payload: {error}"))
                })?;
            Ok(DgramOperation::Bind {
                socket_id: string(0, "dgram.bind socket id")?,
                address: payload.address,
                port: payload.port,
            })
        }
        "dgram.send" => {
            let payload: DgramSendOptions =
                serde_json::from_value(request.args.get(2).cloned().ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "dgram.send requires a request payload",
                    ))
                })?)
                .map_err(|error| {
                    SidecarError::InvalidState(format!("invalid dgram.send payload: {error}"))
                })?;
            Ok(DgramOperation::Send {
                socket_id: string(0, "dgram.send socket id")?,
                bytes: javascript_sync_rpc_bytes_arg(&request.args, 1, "dgram.send payload")?,
                address: payload.address,
                port: payload.port,
            })
        }
        "dgram.connect" => {
            let payload: DgramConnectOptions =
                serde_json::from_value(request.args.get(1).cloned().ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "dgram.connect requires a request payload",
                    ))
                })?)
                .map_err(|error| {
                    SidecarError::InvalidState(format!("invalid dgram.connect payload: {error}"))
                })?;
            Ok(DgramOperation::Connect {
                socket_id: string(0, "dgram.connect socket id")?,
                address: payload.address,
                port: payload.port,
            })
        }
        "dgram.disconnect" => Ok(DgramOperation::Disconnect {
            socket_id: string(0, "dgram.disconnect socket id")?,
        }),
        "dgram.remoteAddress" => Ok(DgramOperation::RemoteAddress {
            socket_id: string(0, "dgram.remoteAddress socket id")?,
        }),
        "dgram.close" => Ok(DgramOperation::Close {
            socket_id: string(0, "dgram.close socket id")?,
        }),
        "dgram.address" => Ok(DgramOperation::Address {
            socket_id: string(0, "dgram.address socket id")?,
        }),
        "dgram.setOption" => Ok(DgramOperation::SetOption {
            socket_id: string(0, "dgram.setOption socket id")?,
            name: string(1, "dgram.setOption option name")?,
            payload: request.args.get(2).cloned().ok_or_else(|| {
                SidecarError::InvalidState(String::from(
                    "dgram.setOption requires an option payload",
                ))
            })?,
        }),
        "dgram.setBufferSize" => {
            let size = javascript_sync_rpc_arg_u64(&request.args, 2, "dgram.setBufferSize size")?;
            Ok(DgramOperation::SetBufferSize {
                socket_id: string(0, "dgram.setBufferSize socket id")?,
                which: string(1, "dgram.setBufferSize buffer kind")?,
                size: usize::try_from(size).map_err(|_| {
                    SidecarError::InvalidState(String::from(
                        "dgram.setBufferSize size must fit within usize",
                    ))
                })?,
            })
        }
        "dgram.getBufferSize" => Ok(DgramOperation::GetBufferSize {
            socket_id: string(0, "dgram.getBufferSize socket id")?,
            which: string(1, "dgram.getBufferSize buffer kind")?,
        }),
        other => Err(SidecarError::InvalidState(format!(
            "unsupported JavaScript dgram sync RPC method {other}"
        ))),
    }
}

fn decode_javascript_dns_operation(request: &HostRpcRequest) -> Result<DnsOperation, SidecarError> {
    match request.method.as_str() {
        "dns.lookup" => {
            let payload: JavascriptDnsLookupRequest =
                serde_json::from_value(request.args.first().cloned().ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "dns.lookup requires a request payload",
                    ))
                })?)
                .map_err(|error| {
                    SidecarError::InvalidState(format!("invalid dns.lookup payload: {error}"))
                })?;
            Ok(DnsOperation::Lookup {
                hostname: payload.hostname,
                family: payload.family,
            })
        }
        "dns.resolve" | "dns.resolve4" | "dns.resolve6" | "dns.resolveRawRr" => {
            let payload: JavascriptDnsResolveRequest =
                serde_json::from_value(request.args.first().cloned().ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "dns.resolve requires a request payload",
                    ))
                })?)
                .map_err(|error| {
                    SidecarError::InvalidState(format!("invalid dns.resolve payload: {error}"))
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
            Ok(DnsOperation::Resolve {
                hostname: payload.hostname,
                requested_type,
                raw_record: request.method == "dns.resolveRawRr",
            })
        }
        other => Err(SidecarError::InvalidState(format!(
            "unsupported JavaScript dns sync RPC method {other}"
        ))),
    }
}

pub(crate) fn remap_wasm_process_sync_rpc(
    request: &HostRpcRequest,
) -> Result<Option<HostRpcRequest>, SidecarError> {
    if request.method != "process.wasm_sync_rpc" {
        return Ok(None);
    }
    let method = javascript_sync_rpc_arg_str(&request.args, 0, "WASM process sync RPC method")?;
    if !ALLOWED_WASM_PROCESS_SYNC_RPCS.contains(&method) {
        return Err(SidecarError::InvalidState(format!(
            "unsupported WASM process sync RPC method {method}"
        )));
    }
    Ok(Some(HostRpcRequest {
        id: request.id,
        method: method.to_owned(),
        args: request.args[1..].to_vec(),
        raw_bytes_args: request
            .raw_bytes_args
            .iter()
            .filter(|(index, _)| **index > 0 && **index != usize::MAX)
            .map(|(index, bytes)| (*index - 1, bytes.clone()))
            .collect(),
    }))
}

/// Whether a successful sync RPC can transition a pipe/socket descriptor from
/// not-readable to readable (including EOF). Wrapped WASM RPCs carry the real
/// method name as argument zero.
pub(crate) fn javascript_sync_rpc_may_make_fd_readable(request: &HostRpcRequest) -> bool {
    let method = if request.method == "process.wasm_sync_rpc" {
        request
            .args
            .first()
            .and_then(Value::as_str)
            .unwrap_or_default()
    } else {
        request.method.as_str()
    };
    matches!(
        method,
        "process.fd_write"
            | "process.fd_close"
            | "process.fd_socket_shutdown"
            | "__kernel_stdio_write"
            | "child_process.write_stdin"
            | "child_process.close_stdin"
    )
}

/// Whether a successful sync RPC can free capacity in a pipe and therefore
/// make a parked writer runnable.
pub(crate) fn javascript_sync_rpc_may_make_fd_writable(request: &HostRpcRequest) -> bool {
    let method = if request.method == "process.wasm_sync_rpc" {
        request
            .args
            .first()
            .and_then(Value::as_str)
            .unwrap_or_default()
    } else {
        request.method.as_str()
    };
    matches!(method, "process.fd_read" | "__kernel_stdin_read")
}

pub(crate) fn deferred_child_kernel_wait_request(
    request: &HostRpcRequest,
) -> Result<Option<HostRpcRequest>, SidecarError> {
    if matches!(
        request.method.as_str(),
        "__kernel_stdin_read"
            | "__kernel_poll"
            | "__kernel_stdio_write"
            | "process.fd_read"
            | "process.fd_write"
    ) {
        return Ok(Some(request.clone()));
    }
    if request.method != "process.wasm_sync_rpc" {
        return Ok(None);
    }
    let method = request
        .args
        .first()
        .and_then(Value::as_str)
        .ok_or_else(|| {
            SidecarError::InvalidState(String::from(
                "WASM process sync RPC method must be a string",
            ))
        })?;
    if method != "process.fd_read" && method != "process.fd_write" {
        return Ok(None);
    }
    Ok(Some(HostRpcRequest {
        id: request.id,
        method: method.to_owned(),
        args: request.args[1..].to_vec(),
        raw_bytes_args: request
            .raw_bytes_args
            .iter()
            .filter(|(index, _)| **index > 0 && **index != usize::MAX)
            .map(|(index, bytes)| (*index - 1, bytes.clone()))
            .collect(),
    }))
}

/// Normalize embedded-Node `fs.write*` calls only when they target a kernel
/// pipe. Regular-file writes already use the kernel-backed filesystem service,
/// while a full pipe must never block the sidecar actor.
pub(crate) fn deferred_kernel_wait_request_for_process(
    request: &HostRpcRequest,
    kernel: &SidecarKernel,
    process: &ActiveProcess,
) -> Result<Option<HostRpcRequest>, SidecarError> {
    if let Some(request) = deferred_child_kernel_wait_request(request)? {
        return Ok(Some(request));
    }
    if request.method != "fs.write" && request.method != "fs.writeSync" {
        return Ok(None);
    }
    if javascript_sync_rpc_arg_u64_optional(&request.args, 2, "filesystem write position")?
        .is_some()
    {
        return Ok(None);
    }
    let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "filesystem write fd")?;
    let is_pipe = kernel
        .fd_is_pipe(EXECUTION_DRIVER_NAME, process.kernel_pid, fd)
        .map_err(kernel_error)?;
    if !is_pipe {
        return Ok(None);
    }
    Ok(Some(HostRpcRequest {
        id: request.id,
        method: String::from("process.fd_write"),
        args: vec![
            request.args.first().cloned().unwrap_or(Value::Null),
            request.args.get(1).cloned().unwrap_or(Value::Null),
        ],
        raw_bytes_args: request
            .raw_bytes_args
            .iter()
            .filter(|(index, _)| **index == 1 || **index == usize::MAX)
            .map(|(index, bytes)| (*index, bytes.clone()))
            .collect(),
    }))
}

pub(crate) struct JavascriptSyncRpcServiceRequest<'a, B> {
    pub(crate) bridge: &'a SharedBridge<B>,
    pub(crate) vm_id: &'a str,
    pub(crate) dns: &'a VmDnsConfig,
    pub(crate) socket_paths: &'a SocketPathContext,
    pub(crate) kernel: &'a mut SidecarKernel,
    pub(crate) kernel_readiness: KernelSocketReadinessRegistry,
    pub(crate) process: &'a mut ActiveProcess,
    pub(crate) sync_request: &'a HostRpcRequest,
    pub(crate) capabilities: CapabilityRegistry,
    pub(crate) managed_descriptions: Option<crate::state::ManagedHostNetDescriptionRegistry>,
}

pub(in crate::execution) fn descriptor_rights_compat_request(
    id: u64,
    operation: agentos_execution::host::NetworkOperation,
) -> Result<HostRpcRequest, SidecarError> {
    let mut raw_bytes_args = std::collections::HashMap::new();
    let (method, args) = match operation {
        agentos_execution::host::NetworkOperation::SendDescriptorRights {
            fd,
            bytes,
            rights,
            flags,
        } => {
            raw_bytes_args.insert(1, bytes.into_vec());
            (
                "process.fd_sendmsg_rights",
                vec![
                    json!(fd),
                    Value::Null,
                    json!(rights.into_vec()),
                    json!(flags),
                ],
            )
        }
        agentos_execution::host::NetworkOperation::ReceiveDescriptorRights {
            fd,
            max_bytes,
            max_rights,
            close_on_exec,
            peek,
            dontwait,
            waitall,
        } => (
            "process.fd_recvmsg_rights",
            vec![
                json!(fd),
                json!(max_bytes.get()),
                json!(max_rights.get()),
                json!(close_on_exec),
                json!(peek),
                json!(dontwait),
                json!(waitall),
            ],
        ),
        other => {
            return Err(SidecarError::host(
                "EINVAL",
                format!("descriptor-rights adapter received unsupported operation: {other:?}"),
            ))
        }
    };
    Ok(HostRpcRequest {
        id,
        method: method.to_owned(),
        args,
        raw_bytes_args,
    })
}

#[allow(clippy::too_many_arguments)]
pub(in crate::execution) async fn service_descriptor_rights_compat_operation<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    dns: &VmDnsConfig,
    socket_paths: &SocketPathContext,
    kernel: &mut SidecarKernel,
    kernel_readiness: KernelSocketReadinessRegistry,
    process: &mut ActiveProcess,
    capabilities: CapabilityRegistry,
    managed_descriptions: crate::state::ManagedHostNetDescriptionRegistry,
    call_id: u64,
    operation: agentos_execution::host::NetworkOperation,
) -> Result<HostServiceResponse, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let request = descriptor_rights_compat_request(call_id, operation)?;
    service_javascript_sync_rpc(JavascriptSyncRpcServiceRequest {
        bridge,
        vm_id,
        dns,
        socket_paths,
        kernel,
        kernel_readiness,
        process,
        sync_request: &request,
        capabilities,
        managed_descriptions: Some(managed_descriptions),
    })
    .await
}

pub(crate) enum HostServiceResponse {
    Json(Value),
    Deferred {
        receiver: tokio::sync::oneshot::Receiver<Result<Value, crate::state::DeferredRpcError>>,
        timeout: Option<Duration>,
        task_class: agentos_runtime::TaskClass,
    },
    Raw(Vec<u8>),
    SourceBackedJson {
        value: Value,
        source_reservations: Vec<SharedReservation>,
    },
    SourceBackedRaw {
        payload: Vec<u8>,
        source_reservations: Vec<SharedReservation>,
    },
}

impl From<Value> for HostServiceResponse {
    fn from(value: Value) -> Self {
        Self::Json(value)
    }
}

impl HostServiceResponse {
    pub(in crate::execution) fn as_json(&self) -> Option<&Value> {
        match self {
            Self::Json(value) => Some(value),
            Self::Raw(_)
            | Self::Deferred { .. }
            | Self::SourceBackedJson { .. }
            | Self::SourceBackedRaw { .. } => None,
        }
    }
}

pub(crate) struct NetServiceRequest<'a, B> {
    pub(crate) bridge: &'a SharedBridge<B>,
    pub(crate) vm_id: &'a str,
    pub(crate) dns: &'a VmDnsConfig,
    pub(crate) socket_paths: &'a SocketPathContext,
    pub(crate) kernel: &'a mut SidecarKernel,
    pub(crate) kernel_readiness: KernelSocketReadinessRegistry,
    pub(crate) process: &'a mut ActiveProcess,
    pub(crate) sync_request: &'a HostRpcRequest,
    pub(crate) capabilities: CapabilityRegistry,
}

pub(crate) fn javascript_sync_rpc_arg_str<'a>(
    args: &'a [Value],
    index: usize,
    label: &str,
) -> Result<&'a str, SidecarError> {
    args.get(index)
        .and_then(Value::as_str)
        .ok_or_else(|| SidecarError::InvalidState(format!("{label} must be a string argument")))
}

pub(crate) fn javascript_sync_rpc_arg_bool(
    args: &[Value],
    index: usize,
    label: &str,
) -> Result<bool, SidecarError> {
    args.get(index)
        .and_then(Value::as_bool)
        .ok_or_else(|| SidecarError::InvalidState(format!("{label} must be a boolean argument")))
}

pub(crate) fn javascript_sync_rpc_encoding(args: &[Value]) -> Option<String> {
    args.get(1).and_then(|value| {
        value.as_str().map(str::to_owned).or_else(|| {
            value
                .get("encoding")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
    })
}

pub(crate) fn javascript_sync_rpc_option_bool(
    args: &[Value],
    index: usize,
    key: &str,
) -> Option<bool> {
    let value = args.get(index)?;
    if let Some(boolean) = value.as_bool() {
        return Some(boolean);
    }
    value.get(key).and_then(Value::as_bool)
}

pub(crate) fn javascript_sync_rpc_option_u32(
    args: &[Value],
    index: usize,
    key: &str,
) -> Result<Option<u32>, SidecarError> {
    let Some(value) = args.get(index).and_then(|value| {
        if value.is_object() {
            value.get(key)
        } else if key == "mode" && value.is_number() {
            Some(value)
        } else {
            None
        }
    }) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }

    let numeric = value
        .as_u64()
        .or_else(|| {
            value
                .as_f64()
                .filter(|number| number.is_finite() && *number >= 0.0)
                .map(|number| number as u64)
        })
        .ok_or_else(|| SidecarError::InvalidState(format!("{key} must be numeric")))?;

    u32::try_from(numeric)
        .map(Some)
        .map_err(|_| SidecarError::InvalidState(format!("{key} must fit within u32")))
}

pub(crate) fn javascript_sync_rpc_arg_u32(
    args: &[Value],
    index: usize,
    label: &str,
) -> Result<u32, SidecarError> {
    let value = javascript_sync_rpc_arg_u64(args, index, label)?;
    u32::try_from(value)
        .map_err(|_| SidecarError::InvalidState(format!("{label} must fit within u32")))
}

pub(crate) fn javascript_sync_rpc_arg_i32(
    args: &[Value],
    index: usize,
    label: &str,
) -> Result<i32, SidecarError> {
    let Some(value) = args.get(index) else {
        return Err(SidecarError::InvalidState(format!("{label} is required")));
    };

    let numeric = value
        .as_i64()
        .or_else(|| {
            value
                .as_f64()
                .filter(|number| number.is_finite())
                .map(|number| number as i64)
        })
        .ok_or_else(|| SidecarError::InvalidState(format!("{label} must be a numeric argument")))?;

    i32::try_from(numeric)
        .map_err(|_| SidecarError::InvalidState(format!("{label} must fit within i32")))
}

pub(crate) fn javascript_sync_rpc_arg_u32_optional(
    args: &[Value],
    index: usize,
    label: &str,
) -> Result<Option<u32>, SidecarError> {
    javascript_sync_rpc_arg_u64_optional(args, index, label)?
        .map(|value| {
            u32::try_from(value)
                .map_err(|_| SidecarError::InvalidState(format!("{label} must fit within u32")))
        })
        .transpose()
}

pub(crate) fn javascript_sync_rpc_arg_u64(
    args: &[Value],
    index: usize,
    label: &str,
) -> Result<u64, SidecarError> {
    let Some(value) = args.get(index) else {
        return Err(SidecarError::InvalidState(format!("{label} is required")));
    };

    if let Some(text) = value.as_str() {
        return text
            .parse::<u64>()
            .map_err(|_| SidecarError::InvalidState(format!("{label} must be a u64")));
    }

    value
        .as_u64()
        .or_else(|| {
            value
                .as_f64()
                .filter(|number| number.is_finite() && *number >= 0.0)
                .map(|number| number as u64)
        })
        .ok_or_else(|| SidecarError::InvalidState(format!("{label} must be a numeric argument")))
}

fn javascript_sync_rpc_arg_rlim(
    args: &[Value],
    index: usize,
    label: &str,
) -> Result<u64, SidecarError> {
    let Some(value) = args.get(index) else {
        return Err(SidecarError::InvalidState(format!("{label} is required")));
    };
    if let Some(text) = value.as_str() {
        return text
            .parse::<u64>()
            .map_err(|_| SidecarError::InvalidState(format!("{label} must be a u64")));
    }
    javascript_sync_rpc_arg_u64(args, index, label)
}

fn process_resource_limit_kind(
    resource: u32,
) -> Result<agentos_kernel::kernel::ProcessResourceLimitKind, SidecarError> {
    use agentos_kernel::kernel::ProcessResourceLimitKind;
    match resource {
        0 => Ok(ProcessResourceLimitKind::Cpu),
        1 => Ok(ProcessResourceLimitKind::FileSize),
        2 => Ok(ProcessResourceLimitKind::Data),
        3 => Ok(ProcessResourceLimitKind::Stack),
        4 => Ok(ProcessResourceLimitKind::Core),
        5 => Ok(ProcessResourceLimitKind::ResidentSet),
        6 => Ok(ProcessResourceLimitKind::Processes),
        7 => Ok(ProcessResourceLimitKind::OpenFiles),
        8 => Ok(ProcessResourceLimitKind::LockedMemory),
        9 => Ok(ProcessResourceLimitKind::AddressSpace),
        _ => Err(SidecarError::host(
            "EINVAL",
            format!("unknown resource limit {resource}"),
        )),
    }
}

pub(crate) fn javascript_sync_rpc_arg_u64_optional(
    args: &[Value],
    index: usize,
    label: &str,
) -> Result<Option<u64>, SidecarError> {
    let Some(value) = args.get(index) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    javascript_sync_rpc_arg_u64(args, index, label).map(Some)
}

pub(crate) fn javascript_sync_rpc_bytes_arg(
    args: &[Value],
    index: usize,
    label: &str,
) -> Result<Vec<u8>, SidecarError> {
    let Some(value) = args.get(index) else {
        return Err(SidecarError::InvalidState(format!("{label} is required")));
    };

    if let Some(text) = value.as_str() {
        return Ok(text.as_bytes().to_vec());
    }

    decode_encoded_bytes_value(value)
        .map_err(|error| SidecarError::host("EINVAL", format!("{label} {error}")))
}

/// Decode an owned byte argument from either the bridge's lossless CBOR byte
/// lane or its JSON compatibility projection. V8 removes byte strings from
/// `args` and records them in `raw_bytes_args`; engine-neutral decoders must
/// prefer that lane so a Buffer does not turn into a null/opaque placeholder.
pub(crate) fn javascript_sync_rpc_request_bytes_arg(
    request: &HostRpcRequest,
    index: usize,
    label: &str,
) -> Result<Vec<u8>, SidecarError> {
    if let Some(bytes) = request.raw_bytes_args.get(&index) {
        return Ok(bytes.clone());
    }
    let Some(value) = request.args.get(index) else {
        return Err(SidecarError::InvalidState(format!("{label} is required")));
    };
    if let Some(text) = value.as_str() {
        return Ok(text.as_bytes().to_vec());
    }
    decode_encoded_bytes_value(value)
        .or_else(|_| decode_bridge_buffer_value(value))
        .map_err(|error| SidecarError::host("EINVAL", format!("{label} {error}")))
}

pub(crate) fn host_bytes_value(bytes: &[u8]) -> Value {
    encoded_bytes_value(bytes)
}

#[derive(Debug, Deserialize)]
pub(crate) struct KernelPollFdRequest {
    pub(in crate::execution) fd: u32,
    pub(in crate::execution) events: u16,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub(in crate::execution) struct KernelPollFdResponse {
    pub(in crate::execution) fd: u32,
    pub(in crate::execution) events: u16,
    pub(in crate::execution) revents: u16,
}

pub(in crate::execution) fn javascript_sync_rpc_base64_arg(
    args: &[Value],
    index: usize,
    label: &str,
) -> Result<Vec<u8>, SidecarError> {
    let value = javascript_sync_rpc_arg_str(args, index, label)?;
    decode_base64(value).map_err(|error| SidecarError::InvalidState(format!("{label} {error}")))
}

// ── Sync-RPC round-trip counting (opt-in via AGENTOS_SYNC_RPC_TRACE=1) ──
// Each guest fs/module/net sync RPC funnels through service_javascript_sync_rpc,
// so this is the one place to measure the kernel-VFS "syscall storm" that makes
// metadata-heavy phases (resourceLoader.reload, createAgentSession) 40-90x slower
// in the VM than on bare node. Emits a perf log line every 200 calls with the
// running per-method breakdown.

fn wasm_process_resolve_at_path(
    kernel: &mut SidecarKernel,
    pid: u32,
    dir_fd: u32,
    path: &str,
) -> Result<String, SidecarError> {
    if path.starts_with('/') {
        let root_path = normalize_path(path);
        if dir_fd == 0 {
            let root_missing = kernel
                .lstat_for_process(EXECUTION_DRIVER_NAME, pid, &root_path)
                .is_err_and(|error| matches!(error.code(), "ENOENT" | "ENOTDIR"));
            if root_missing {
                let guest_cwd = kernel
                    .read_link_for_process(EXECUTION_DRIVER_NAME, pid, "/proc/self/cwd")
                    .map_err(kernel_error)?;
                let cwd_path = normalize_path(&format!(
                    "{}/{}",
                    guest_cwd.trim_end_matches('/'),
                    root_path.trim_start_matches('/')
                ));
                if kernel
                    .lstat_for_process(EXECUTION_DRIVER_NAME, pid, &cwd_path)
                    .is_ok()
                {
                    return Ok(cwd_path);
                }
            }
        }
        return Ok(root_path);
    }
    let stat = kernel
        .fd_stat(EXECUTION_DRIVER_NAME, pid, dir_fd)
        .map_err(kernel_error)?;
    if stat.filetype != agentos_kernel::fd_table::FILETYPE_DIRECTORY {
        return Err(SidecarError::host(
            "ENOTDIR",
            format!("file descriptor {dir_fd} is not a directory"),
        ));
    }
    let base = kernel
        .fd_path(EXECUTION_DRIVER_NAME, pid, dir_fd)
        .map_err(kernel_error)?;
    Ok(normalize_path(&format!("{base}/{path}")))
}

fn wasm_process_path_stat_value(stat: agentos_kernel::vfs::VirtualStat) -> Value {
    let filetype = if stat.is_directory {
        agentos_kernel::fd_table::FILETYPE_DIRECTORY
    } else if stat.is_symbolic_link {
        agentos_kernel::fd_table::FILETYPE_SYMBOLIC_LINK
    } else {
        agentos_kernel::fd_table::FILETYPE_REGULAR_FILE
    };
    json!({
        "dev": stat.dev,
        "ino": stat.ino,
        "filetype": filetype,
        "nlink": stat.nlink,
        "mode": stat.mode,
        "uid": stat.uid,
        "gid": stat.gid,
        "size": stat.size,
        "blocks": stat.blocks,
        "rdev": stat.rdev,
        "atimeMs": stat.atime_ms,
        "mtimeMs": stat.mtime_ms,
        "ctimeMs": stat.ctime_ms,
    })
}

fn wasm_process_utime_spec(
    nanoseconds: &str,
    explicit: bool,
    now: bool,
) -> Result<VirtualUtimeSpec, SidecarError> {
    if now {
        return Ok(VirtualUtimeSpec::Now);
    }
    if !explicit {
        return Ok(VirtualUtimeSpec::Omit);
    }
    let nanoseconds = nanoseconds
        .parse::<u64>()
        .map_err(|_| SidecarError::host("EINVAL", "timestamp must be u64 nanoseconds"))?;
    let seconds = i64::try_from(nanoseconds / 1_000_000_000)
        .map_err(|_| SidecarError::host("EINVAL", "timestamp exceeds i64 seconds"))?;
    VirtualTimeSpec::new(seconds, (nanoseconds % 1_000_000_000) as u32)
        .map(VirtualUtimeSpec::Set)
        .map_err(|error| SidecarError::host("EINVAL", error.to_string()))
}
pub(crate) async fn service_javascript_sync_rpc<B>(
    request: JavascriptSyncRpcServiceRequest<'_, B>,
) -> Result<HostServiceResponse, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let JavascriptSyncRpcServiceRequest {
        bridge,
        vm_id,
        dns,
        socket_paths,
        kernel,
        kernel_readiness,
        process,
        sync_request: original_request,
        capabilities,
        managed_descriptions,
    } = request;
    let remapped_request = remap_wasm_process_sync_rpc(original_request)?;
    let request = remapped_request.as_ref().unwrap_or(original_request);
    if sync_rpc_trace_enabled() {
        record_sync_rpc(request.method.as_str());
    }
    validate_guest_network_capability_alias(process, request)?;
    if request.raw_bytes_args.contains_key(&usize::MAX) && request.method == "fs.readSync" {
        let kernel_pid = process.kernel_pid;
        let bytes = service_javascript_fs_read_sync_rpc(kernel, process, kernel_pid, request)?;
        return Ok(HostServiceResponse::Raw(bytes));
    }
    if request.raw_bytes_args.contains_key(&usize::MAX) && request.method == "fs.readFileRangeSync"
    {
        let path =
            javascript_sync_rpc_path_arg(process, &request.args, 0, "filesystem ranged read path")?;
        let offset =
            javascript_sync_rpc_arg_u64(&request.args, 1, "filesystem ranged read offset")?;
        let length = usize::try_from(javascript_sync_rpc_arg_u64(
            &request.args,
            2,
            "filesystem ranged read length",
        )?)
        .map_err(|_| {
            SidecarError::InvalidState(
                "filesystem ranged read length must fit within usize".to_string(),
            )
        })?;
        let bytes = kernel
            .pread_file_for_process(
                EXECUTION_DRIVER_NAME,
                process.kernel_pid,
                path.as_str(),
                offset,
                length,
            )
            .map_err(kernel_error)?;
        return Ok(HostServiceResponse::Raw(bytes));
    }
    if request.method == "fs.readdirSync" {
        let kernel_pid = process.kernel_pid;
        let bytes =
            service_javascript_fs_readdir_raw_sync_rpc(kernel, process, kernel_pid, request)?;
        return Ok(HostServiceResponse::Raw(bytes));
    }
    let response = match request.method.as_str() {
        "__bench.noop" => Ok(Value::Null),
        "__bench.net_tcp_metrics_reset" => {
            net_tcp_trace_reset();
            Ok(Value::Null)
        }
        "__bench.net_tcp_metrics_snapshot" => Ok(net_tcp_trace_snapshot()),
        // Module resolution / loading / format detection read the kernel VFS so
        // the resolver sees exactly what the guest and `kernel.readFile()` see.
        "_resolveModule"
        | "_resolveModuleSync"
        | "__resolve_module"
        | "_batchResolveModules"
        | "__batch_resolve_modules"
        | "_loadFile"
        | "_loadFileSync"
        | "__load_file"
        | "_moduleFormat"
        | "__module_format" => service_javascript_module_sync_rpc(kernel, process, request),
        // Polyfills are static guest expressions, not VFS reads.
        "_loadPolyfill" | "__load_polyfill" => {
            service_javascript_internal_bridge_sync_rpc(process, request)
        }
        "__kernel_stdin_read" => {
            service_javascript_kernel_stdin_sync_rpc(kernel, process, request)
        }
        "__kernel_stdio_write" => {
            service_javascript_kernel_stdio_write_sync_rpc(kernel, process, request)
        }
        "__kernel_isatty" => service_javascript_kernel_isatty_sync_rpc(kernel, process, request),
        "__kernel_tty_size" => {
            service_javascript_kernel_tty_size_sync_rpc(kernel, process, request)
        }
        "__kernel_tty_set_size" => {
            service_javascript_kernel_tty_set_size_sync_rpc(kernel, process, request)
        }
        "__kernel_tcgetattr" => {
            service_javascript_kernel_tcgetattr_sync_rpc(kernel, process, request)
        }
        "__kernel_tcsetattr" => {
            service_javascript_kernel_tcsetattr_sync_rpc(kernel, process, request)
        }
        "__kernel_tcgetpgrp" => {
            service_javascript_kernel_tcgetpgrp_sync_rpc(kernel, process, request)
        }
        "__kernel_tcsetpgrp" => {
            service_javascript_kernel_tcsetpgrp_sync_rpc(kernel, process, request)
        }
        "__kernel_tcgetsid" => {
            service_javascript_kernel_tcgetsid_sync_rpc(kernel, process, request)
        }
        "__kernel_poll" => service_javascript_kernel_poll_sync_rpc(kernel, process, request),
        "__pty_set_raw_mode" => {
            service_javascript_pty_set_raw_mode_sync_rpc(kernel, process, request)
        }
        "crypto.hashDigest"
        | "crypto.hashCreate"
        | "crypto.hashUpdate"
        | "crypto.hashFinal"
        | "crypto.hashDestroy"
        | "crypto.hmacDigest"
        | "crypto.pbkdf2"
        | "crypto.scrypt"
        | "crypto.cipheriv"
        | "crypto.decipheriv"
        | "crypto.cipherivCreate"
        | "crypto.cipherivUpdate"
        | "crypto.cipherivFinal"
        | "crypto.sign"
        | "crypto.verify"
        | "crypto.asymmetricOp"
        | "crypto.createKeyObject"
        | "crypto.generateKeyPairSync"
        | "crypto.generateKeySync"
        | "crypto.generatePrimeSync"
        | "crypto.diffieHellman"
        | "crypto.diffieHellmanGroup"
        | "crypto.diffieHellmanSessionCreate"
        | "crypto.diffieHellmanSessionCall"
        | "crypto.diffieHellmanSessionDestroy"
        | "crypto.subtle" => service_javascript_crypto_sync_rpc(process, request),
        "dns.lookup" | "dns.resolve" | "dns.resolve4" | "dns.resolve6" | "dns.resolveRawRr" => {
            service_dns_operation(
                bridge,
                kernel,
                vm_id,
                dns,
                decode_javascript_dns_operation(request)?,
            )
        }
        "net.http_listen" | "net.http_close" | "net.http_wait" | "net.http_respond" => {
            return service_javascript_net_sync_rpc_response(NetServiceRequest {
                bridge,
                vm_id,
                dns,
                socket_paths,
                kernel,
                kernel_readiness: Arc::clone(&kernel_readiness),
                process,
                sync_request: request,
                capabilities: capabilities.clone(),
            })
        }
        "net.http2_server_listen"
        | "net.http2_server_poll"
        | "net.http2_server_close"
        | "net.http2_server_respond"
        | "net.http2_server_wait"
        | "net.http2_session_connect"
        | "net.http2_session_request"
        | "net.http2_session_settings"
        | "net.http2_session_set_local_window_size"
        | "net.http2_session_goaway"
        | "net.http2_session_close"
        | "net.http2_session_destroy"
        | "net.http2_session_poll"
        | "net.http2_session_wait"
        | "net.http2_stream_respond"
        | "net.http2_stream_push_stream"
        | "net.http2_stream_write"
        | "net.http2_stream_end"
        | "net.http2_stream_close"
        | "net.http2_stream_pause"
        | "net.http2_stream_resume"
        | "net.http2_stream_respond_with_file" => {
            return service_javascript_http2_sync_rpc(Http2ServiceRequest {
                bridge,
                kernel,
                vm_id,
                dns,
                socket_paths,
                process,
                sync_request: request,
                capabilities: capabilities.clone(),
            });
        }
        "net.bind_unix"
        | "net.bind_connected_unix"
        | "net.connect"
        | "net.reserve_tcp_port"
        | "net.release_tcp_port"
        | "net.listen"
        | "net.poll"
        | "net.socket_wait_connect"
        | "net.socket_read"
        | "net.socket_set_read_interest"
        | "net.socket_set_no_delay"
        | "net.socket_set_keep_alive"
        | "net.socket_upgrade_tls"
        | "net.socket_get_tls_client_hello"
        | "net.socket_tls_query"
        | "net.server_poll"
        | "net.server_accept"
        | "net.server_connections"
        | "net.upgrade_socket_write"
        | "net.upgrade_socket_end"
        | "net.upgrade_socket_destroy"
        | "net.write"
        | "net.shutdown"
        | "net.destroy"
        | "net.server_close"
        | "tls.get_ciphers" => {
            return service_javascript_net_sync_rpc_response(NetServiceRequest {
                bridge,
                vm_id,
                dns,
                socket_paths,
                kernel,
                kernel_readiness: Arc::clone(&kernel_readiness),
                process,
                sync_request: request,
                capabilities: capabilities.clone(),
            })
        }
        "dgram.poll" => {
            return service_javascript_dgram_poll_response(socket_paths, kernel, process, request)
                .await;
        }
        "dgram.createSocket"
        | "dgram.bind"
        | "dgram.send"
        | "dgram.connect"
        | "dgram.disconnect"
        | "dgram.remoteAddress"
        | "dgram.close"
        | "dgram.address"
        | "dgram.setOption"
        | "dgram.setBufferSize"
        | "dgram.getBufferSize" => {
            let operation = decode_javascript_dgram_operation(request)?;
            return service_dgram_operation(DgramServiceRequest {
                bridge,
                kernel,
                vm_id,
                dns,
                socket_paths,
                process,
                kernel_readiness,
                operation,
                capabilities,
            });
        }
        "sqlite.constants"
        | "sqlite.open"
        | "sqlite.close"
        | "sqlite.exec"
        | "sqlite.query"
        | "sqlite.prepare"
        | "sqlite.location"
        | "sqlite.checkpoint"
        | "sqlite.statement.run"
        | "sqlite.statement.get"
        | "sqlite.statement.all"
        | "sqlite.statement.iterate"
        | "sqlite.statement.columns"
        | "sqlite.statement.setReturnArrays"
        | "sqlite.statement.setReadBigInts"
        | "sqlite.statement.setAllowBareNamedParameters"
        | "sqlite.statement.setAllowUnknownNamedParameters"
        | "sqlite.statement.finalize" => {
            service_javascript_sqlite_sync_rpc(kernel, process, request)
        }
        "process.take_signal" | "process.signal_begin" => {
            if process.real_interval_timer.take_expiry() {
                process.kernel_handle.kill(libc::SIGALRM);
            }
            let delivery = process
                .kernel_handle
                .begin_signal_delivery()
                .map_err(kernel_error)?;
            Ok(delivery
                .map(|delivery| {
                    json!({
                        "signal": delivery.signal,
                        "token": delivery.token,
                        "flags": delivery.action.flags,
                    })
                })
                .unwrap_or(Value::Null))
        }
        "process.signal_end" => {
            let token = javascript_sync_rpc_arg_u64(&request.args, 0, "signal token")?;
            process
                .kernel_handle
                .end_signal_delivery(token)
                .map_err(kernel_error)?;
            Ok(Value::Null)
        }
        "process.signal_mask" => {
            let operation = javascript_sync_rpc_arg_u32(&request.args, 0, "signal-mask operation")?;
            let signals = request
                .args
                .get(1)
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    SidecarError::host("EINVAL", String::from("signal-mask set must be an array",
                    ))
                })?
                .iter()
                .map(|value| {
                    value
                        .as_i64()
                        .and_then(|signal| i32::try_from(signal).ok())
                        .ok_or_else(|| {
                            SidecarError::host("EINVAL", String::from("signal-mask entries must be 32-bit integers",
                            ))
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let set = SignalSet::from_signals(signals)
                .map_err(|error| SidecarError::host(error.code(), error.to_string()))?;
            let how = match operation {
                0 => SigmaskHow::Block,
                1 => SigmaskHow::Unblock,
                2 => SigmaskHow::SetMask,
                3 if set.is_empty() => SigmaskHow::Block,
                _ => {
                    return Err(SidecarError::host("EINVAL", format!("invalid signal-mask operation {operation}"
                    )))
                }
            };
            let previous = process
                .kernel_handle
                .sigprocmask(how, set)
                .map_err(kernel_error)?;
            Ok(json!({ "signals": previous.signals() }))
        }
        "process.signal_mask_scope_begin" => {
            let signals = request
                .args
                .first()
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    SidecarError::host("EINVAL", String::from("temporary signal mask must be an array",
                    ))
                })?
                .iter()
                .map(|value| {
                    value
                        .as_i64()
                        .and_then(|signal| i32::try_from(signal).ok())
                        .ok_or_else(|| {
                            SidecarError::host("EINVAL", String::from("temporary signal-mask entries must be 32-bit integers",
                            ))
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let mask = SignalSet::from_signals(signals)
                .map_err(|error| SidecarError::host(error.code(), error.to_string()))?;
            let token = process
                .kernel_handle
                .begin_temporary_signal_mask(mask)
                .map_err(kernel_error)?;
            Ok(Value::from(token))
        }
        "process.signal_mask_scope_end" => {
            let token =
                javascript_sync_rpc_arg_u64(&request.args, 0, "temporary signal-mask token")?;
            process
                .kernel_handle
                .end_temporary_signal_mask(token)
                .map_err(kernel_error)?;
            Ok(Value::Null)
        }
        "process.itimer_real" => {
            let operation = javascript_sync_rpc_arg_u32(&request.args, 0, "ITIMER_REAL operation")?;
            let values = match operation {
                0 => process.real_interval_timer.get(),
                1 => {
                    let value_us = javascript_sync_rpc_arg_u64(
                        &request.args,
                        1,
                        "ITIMER_REAL value microseconds",
                    )?;
                    let interval_us = javascript_sync_rpc_arg_u64(
                        &request.args,
                        2,
                        "ITIMER_REAL interval microseconds",
                    )?;
                    let values = process.real_interval_timer.set(value_us, interval_us);
                    if values.2 {
                        process.kernel_handle.kill(libc::SIGALRM);
                    }
                    (values.0, values.1)
                }
                other => {
                    return Err(SidecarError::host("EINVAL", format!("invalid ITIMER_REAL operation {other}"
                    )))
                }
            };
            Ok(json!({
                "remainingUs": values.0,
                "intervalUs": values.1,
            }))
        }
        "process.waitpid_transition" => {
            let selector = javascript_sync_rpc_arg_i32(&request.args, 0, "waitpid selector")?;
            let options = javascript_sync_rpc_arg_u32(&request.args, 1, "waitpid options")?;
            if options & !(1 | 2 | 8) != 0 {
                return Err(SidecarError::host("EINVAL", format!("invalid waitpid option bits {:#x}",
                    options & !(1 | 2 | 8)
                )));
            }
            let mut flags = WaitPidFlags::WNOHANG;
            if options & 2 != 0 {
                flags |= WaitPidFlags::WUNTRACED;
            }
            if options & 8 != 0 {
                flags |= WaitPidFlags::WCONTINUED;
            }
            let transition = kernel
                .take_nonterminal_wait_event(
                    EXECUTION_DRIVER_NAME,
                    process.kernel_pid,
                    selector,
                    flags,
                )
                .map_err(kernel_error)?;
            match transition {
                Some(event) => {
                    let status = match event.event {
                        agentos_kernel::kernel::WaitPidEvent::Stopped => {
                            ((event.status as u32 & 0xff) << 8) | 0x7f
                        }
                        agentos_kernel::kernel::WaitPidEvent::Continued => 0xffff,
                        agentos_kernel::kernel::WaitPidEvent::Exited => {
                            return Err(SidecarError::InvalidState(String::from(
                                "terminal wait event escaped nonterminal query",
                            )))
                        }
                    };
                    Ok(json!({ "pid": event.pid, "status": status }))
                }
                None => Ok(Value::Null),
            }
        }
        "process.waitpid" => {
            let selector = javascript_sync_rpc_arg_i32(&request.args, 0, "waitpid selector")?;
            let options = javascript_sync_rpc_arg_u32(&request.args, 1, "waitpid options")?;
            if options & !(1 | 2 | 8) != 0 {
                return Err(SidecarError::host(
                    "EINVAL",
                    format!("invalid waitpid option bits {:#x}", options & !(1 | 2 | 8)),
                ));
            }
            // The synchronous runner bridge must never park the sidecar
            // dispatcher. Managed runners cooperatively service child I/O and
            // retry; WNOHANG here describes bridge admission, not guest policy.
            let mut flags = WaitPidFlags::WNOHANG;
            if options & 2 != 0 {
                flags |= WaitPidFlags::WUNTRACED;
            }
            if options & 8 != 0 {
                flags |= WaitPidFlags::WCONTINUED;
            }
            let transition = kernel
                .waitpid_detailed_with_options(
                    EXECUTION_DRIVER_NAME,
                    process.kernel_pid,
                    selector,
                    flags,
                )
                .map_err(kernel_error)?;
            Ok(match transition {
                None => Value::Null,
                Some(transition) => {
                    let (event, raw_status, exit_code, signal, core_dumped) =
                        match (transition.event, transition.termination) {
                            (
                                agentos_kernel::kernel::WaitPidEvent::Exited,
                                Some(agentos_kernel::process_runtime::ProcessExit::Exited(code)),
                            ) => (
                                "exit",
                                ((code as u32 & 0xff) << 8),
                                code as u32 & 0xff,
                                0,
                                false,
                            ),
                            (
                                agentos_kernel::kernel::WaitPidEvent::Exited,
                                Some(agentos_kernel::process_runtime::ProcessExit::Signaled {
                                    signal,
                                    core_dumped,
                                }),
                            ) => (
                                "exit",
                                (signal as u32 & 0x7f) | if core_dumped { 0x80 } else { 0 },
                                0,
                                signal as u32 & 0x7f,
                                core_dumped,
                            ),
                            (agentos_kernel::kernel::WaitPidEvent::Stopped, _) => (
                                "stopped",
                                ((transition.status as u32 & 0xff) << 8) | 0x7f,
                                0,
                                transition.status as u32 & 0xff,
                                false,
                            ),
                            (agentos_kernel::kernel::WaitPidEvent::Continued, _) => {
                                ("continued", 0xffff, 0, 0, false)
                            }
                            (agentos_kernel::kernel::WaitPidEvent::Exited, None) => {
                                return Err(SidecarError::InvalidState(String::from(
                                    "kernel terminal wait transition omitted exact termination",
                                )))
                            }
                        };
                    json!({
                        "pid": transition.pid,
                        "event": event,
                        "status": transition.status,
                        "rawStatus": raw_status,
                        "exitCode": exit_code,
                        "signal": signal,
                        "coreDumped": core_dumped,
                    })
                }
            })
        }
        "process.fd_pipe" => kernel
            .open_pipe(EXECUTION_DRIVER_NAME, process.kernel_pid)
            .map(|(read_fd, write_fd)| json!({ "readFd": read_fd, "writeFd": write_fd }))
            .map_err(kernel_error),
        "process.fd_open" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "fd_open path")?;
            let flags = javascript_sync_rpc_arg_u32(&request.args, 1, "fd_open flags")?;
            let mode = javascript_sync_rpc_arg_u32_optional(&request.args, 2, "fd_open mode")?;
            let rights_base = javascript_sync_rpc_arg_str(&request.args, 3, "fd_open base rights")?
                .parse::<u64>()
                .map_err(|_| SidecarError::host("EINVAL", "fd_open base rights must be u64"))?;
            let rights_inheriting = javascript_sync_rpc_arg_str(
                &request.args,
                4,
                "fd_open inheriting rights",
            )?
            .parse::<u64>()
            .map_err(|_| SidecarError::host("EINVAL", "fd_open inheriting rights must be u64"))?;
            let path = wasm_process_resolve_at_path(kernel, process.kernel_pid, 0, path)?;
            kernel
                .fd_open_with_rights(
                    EXECUTION_DRIVER_NAME,
                    process.kernel_pid,
                    None,
                    &path,
                    flags,
                    mode,
                    Some((rights_base, rights_inheriting)),
                )
                .map(Value::from)
                .map_err(kernel_error)
        }
        "process.path_open_at" => {
            let dir_fd = javascript_sync_rpc_arg_u32(&request.args, 0, "path_open_at dir fd")?;
            let path = javascript_sync_rpc_arg_str(&request.args, 1, "path_open_at path")?;
            let flags = javascript_sync_rpc_arg_u32(&request.args, 2, "path_open_at flags")?;
            let mode = javascript_sync_rpc_arg_u32_optional(&request.args, 3, "path_open_at mode")?;
            let rights_base = javascript_sync_rpc_arg_str(
                &request.args,
                4,
                "path_open_at base rights",
            )?
            .parse::<u64>()
            .map_err(|_| SidecarError::host("EINVAL", "path_open_at base rights must be u64"))?;
            let rights_inheriting = javascript_sync_rpc_arg_str(
                &request.args,
                5,
                "path_open_at inheriting rights",
            )?
            .parse::<u64>()
            .map_err(|_| {
                SidecarError::host("EINVAL", "path_open_at inheriting rights must be u64")
            })?;
            let path = wasm_process_resolve_at_path(kernel, process.kernel_pid, dir_fd, path)?;
            kernel
                .fd_open_with_rights(
                    EXECUTION_DRIVER_NAME,
                    process.kernel_pid,
                    Some(dir_fd),
                    &path,
                    flags,
                    mode,
                    Some((rights_base, rights_inheriting)),
                )
                .map(Value::from)
                .map_err(kernel_error)
        }
        "process.path_mkdir_at" => {
            let dir_fd = javascript_sync_rpc_arg_u32(&request.args, 0, "path_mkdir_at dir fd")?;
            let path = javascript_sync_rpc_arg_str(&request.args, 1, "path_mkdir_at path")?;
            let path = wasm_process_resolve_at_path(kernel, process.kernel_pid, dir_fd, path)?;
            kernel
                .mkdir_for_process(
                    EXECUTION_DRIVER_NAME,
                    process.kernel_pid,
                    &path,
                    false,
                    None,
                )
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "process.path_stat_at" => {
            let dir_fd = javascript_sync_rpc_arg_u32(&request.args, 0, "path_stat_at dir fd")?;
            let path = javascript_sync_rpc_arg_str(&request.args, 1, "path_stat_at path")?;
            let follow = javascript_sync_rpc_arg_bool(&request.args, 2, "path_stat_at follow")?;
            let path = wasm_process_resolve_at_path(kernel, process.kernel_pid, dir_fd, path)?;
            let stat = if follow {
                kernel.stat_for_process(EXECUTION_DRIVER_NAME, process.kernel_pid, &path)
            } else {
                kernel.lstat_for_process(EXECUTION_DRIVER_NAME, process.kernel_pid, &path)
            }
            .map_err(kernel_error)?;
            Ok(wasm_process_path_stat_value(stat))
        }
        "process.path_chmod_at" => {
            let dir_fd = javascript_sync_rpc_arg_u32(&request.args, 0, "path_chmod_at dir fd")?;
            let path = javascript_sync_rpc_arg_str(&request.args, 1, "path_chmod_at path")?;
            let mode = javascript_sync_rpc_arg_u32(&request.args, 2, "path_chmod_at mode")?;
            let path = wasm_process_resolve_at_path(kernel, process.kernel_pid, dir_fd, path)?;
            kernel
                .chmod_for_process(EXECUTION_DRIVER_NAME, process.kernel_pid, &path, mode)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "process.path_chown_at" => {
            let dir_fd = javascript_sync_rpc_arg_u32(&request.args, 0, "path_chown_at dir fd")?;
            let path = javascript_sync_rpc_arg_str(&request.args, 1, "path_chown_at path")?;
            let uid = javascript_sync_rpc_arg_u32(&request.args, 2, "path_chown_at uid")?;
            let gid = javascript_sync_rpc_arg_u32(&request.args, 3, "path_chown_at gid")?;
            let follow = javascript_sync_rpc_arg_bool(&request.args, 4, "path_chown_at follow")?;
            let path = wasm_process_resolve_at_path(kernel, process.kernel_pid, dir_fd, path)?;
            kernel
                .chown_for_process(
                    EXECUTION_DRIVER_NAME,
                    process.kernel_pid,
                    &path,
                    uid,
                    gid,
                    follow,
                )
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "process.path_utimes_at" => {
            let dir_fd = javascript_sync_rpc_arg_u32(&request.args, 0, "path_utimes_at dir fd")?;
            let path = javascript_sync_rpc_arg_str(&request.args, 1, "path_utimes_at path")?;
            let follow = javascript_sync_rpc_arg_bool(&request.args, 2, "path_utimes_at follow")?;
            let atime_ns = javascript_sync_rpc_arg_str(&request.args, 3, "path_utimes_at atime")?;
            let mtime_ns = javascript_sync_rpc_arg_str(&request.args, 4, "path_utimes_at mtime")?;
            let fst_flags = javascript_sync_rpc_arg_u32(&request.args, 5, "path_utimes_at flags")?;
            let path = wasm_process_resolve_at_path(kernel, process.kernel_pid, dir_fd, path)?;
            let atime = wasm_process_utime_spec(atime_ns, fst_flags & 1 != 0, fst_flags & 2 != 0)?;
            let mtime = wasm_process_utime_spec(mtime_ns, fst_flags & 4 != 0, fst_flags & 8 != 0)?;
            kernel.utimes_spec_for_process(
                EXECUTION_DRIVER_NAME,
                process.kernel_pid,
                &path,
                atime,
                mtime,
                follow,
            )
            .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "process.random_get" => {
            let length = usize::try_from(javascript_sync_rpc_arg_u64(
                &request.args,
                0,
                "random_get length",
            )?)
            .map_err(|_| SidecarError::InvalidState("random_get length is too large".into()))?;
            let maximum = process.limits.wasm.sync_read_limit_bytes;
            if length > maximum {
                return Err(SidecarError::Host(
                    HostServiceError::limit(
                        "E2BIG",
                        "limits.wasm.syncReadLimitBytes",
                        maximum as u64,
                        length as u64,
                    )
                    .with_details(json!({
                        "limitName": "limits.wasm.syncReadLimitBytes",
                        "limit": maximum,
                        "observed": length,
                        "hint": "raise limits.wasm.syncReadLimitBytes if needed",
                    })),
                ));
            }
            let mut bytes = vec![0_u8; length];
            getrandom::getrandom(&mut bytes).map_err(|error| {
                SidecarError::Io(format!("failed to read system random bytes: {error}"))
            })?;
            Ok(host_bytes_value(&bytes))
        }
        "process.clock_time" => {
            let clock_id = javascript_sync_rpc_arg_u32(&request.args, 0, "clock id")?;
            let clock = match clock_id {
                0 => KernelClockId::Realtime,
                1 => KernelClockId::Monotonic,
                2 => KernelClockId::ProcessCpu,
                3 => KernelClockId::ThreadCpu,
                _ => {
                    return Err(SidecarError::host(
                        "EINVAL",
                        format!("unsupported clock id {clock_id}"),
                    ))
                }
            };
            let deterministic_realtime_ns = if clock == KernelClockId::Realtime {
                request
                    .args
                    .get(2)
                    .and_then(Value::as_str)
                    .map(|value| {
                        value.parse::<u64>().map_err(|_| {
                            SidecarError::host(
                                "EINVAL",
                                "deterministic realtime must be u64 nanoseconds",
                            )
                        })
                    })
                    .transpose()?
            } else {
                None
            };
            kernel
                .clock_time_ns(clock, deterministic_realtime_ns)
                .map(|nanoseconds| json!(nanoseconds.to_string()))
                .map_err(kernel_error)
        }
        "process.clock_resolution" => {
            let clock_id = javascript_sync_rpc_arg_u32(&request.args, 0, "clock id")?;
            let clock = match clock_id {
                0 => KernelClockId::Realtime,
                1 => KernelClockId::Monotonic,
                2 => KernelClockId::ProcessCpu,
                3 => KernelClockId::ThreadCpu,
                _ => {
                    return Err(SidecarError::host(
                        "EINVAL",
                        format!("unsupported clock id {clock_id}"),
                    ))
                }
            };
            kernel
                .clock_resolution_ns(clock)
                .map(|nanoseconds| json!(nanoseconds.to_string()))
                .map_err(kernel_error)
        }
        "process.system_identity" => {
            let identity = kernel.system_identity();
            Ok(json!({
                "hostname": identity.hostname,
                "type": identity.os_type,
                "release": identity.os_release,
                "version": identity.os_version,
                "machine": identity.machine,
                "domainName": identity.domain_name,
            }))
        }
        "process.path_link_at" => {
            let old_fd = javascript_sync_rpc_arg_u32(&request.args, 0, "path_link_at old fd")?;
            let old_path = javascript_sync_rpc_arg_str(&request.args, 1, "path_link_at old path")?;
            let new_fd = javascript_sync_rpc_arg_u32(&request.args, 2, "path_link_at new fd")?;
            let new_path = javascript_sync_rpc_arg_str(&request.args, 3, "path_link_at new path")?;
            let follow = javascript_sync_rpc_arg_bool(&request.args, 4, "path_link_at follow")?;
            let mut old_path =
                wasm_process_resolve_at_path(kernel, process.kernel_pid, old_fd, old_path)?;
            let new_path =
                wasm_process_resolve_at_path(kernel, process.kernel_pid, new_fd, new_path)?;
            if follow {
                old_path = kernel
                    .realpath_for_process(EXECUTION_DRIVER_NAME, process.kernel_pid, &old_path)
                    .map_err(kernel_error)?;
            }
            kernel
                .link_for_process(
                    EXECUTION_DRIVER_NAME,
                    process.kernel_pid,
                    &old_path,
                    &new_path,
                )
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "process.path_readlink_at" => {
            let dir_fd = javascript_sync_rpc_arg_u32(&request.args, 0, "path_readlink_at dir fd")?;
            let path = javascript_sync_rpc_arg_str(&request.args, 1, "path_readlink_at path")?;
            let path = wasm_process_resolve_at_path(kernel, process.kernel_pid, dir_fd, path)?;
            kernel
                .read_link_for_process(EXECUTION_DRIVER_NAME, process.kernel_pid, &path)
                .map(Value::String)
                .map_err(kernel_error)
        }
        "process.path_remove_dir_at" => {
            let dir_fd =
                javascript_sync_rpc_arg_u32(&request.args, 0, "path_remove_dir_at dir fd")?;
            let path = javascript_sync_rpc_arg_str(&request.args, 1, "path_remove_dir_at path")?;
            kernel
                .validate_remove_directory_pathname(path)
                .map_err(kernel_error)?;
            let path = wasm_process_resolve_at_path(kernel, process.kernel_pid, dir_fd, path)?;
            kernel
                .remove_dir_for_process(EXECUTION_DRIVER_NAME, process.kernel_pid, &path)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "process.path_rename_at" => {
            let old_fd = javascript_sync_rpc_arg_u32(&request.args, 0, "path_rename_at old fd")?;
            let old_path =
                javascript_sync_rpc_arg_str(&request.args, 1, "path_rename_at old path")?;
            let new_fd = javascript_sync_rpc_arg_u32(&request.args, 2, "path_rename_at new fd")?;
            let new_path =
                javascript_sync_rpc_arg_str(&request.args, 3, "path_rename_at new path")?;
            let old_path =
                wasm_process_resolve_at_path(kernel, process.kernel_pid, old_fd, old_path)?;
            let new_path =
                wasm_process_resolve_at_path(kernel, process.kernel_pid, new_fd, new_path)?;
            kernel
                .rename_for_process(
                    EXECUTION_DRIVER_NAME,
                    process.kernel_pid,
                    &old_path,
                    &new_path,
                )
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "process.path_symlink_at" => {
            let target = javascript_sync_rpc_arg_str(&request.args, 0, "path_symlink_at target")?;
            let dir_fd = javascript_sync_rpc_arg_u32(&request.args, 1, "path_symlink_at dir fd")?;
            let path = javascript_sync_rpc_arg_str(&request.args, 2, "path_symlink_at path")?;
            let path = wasm_process_resolve_at_path(kernel, process.kernel_pid, dir_fd, path)?;
            kernel
                .symlink_for_process(
                    EXECUTION_DRIVER_NAME,
                    process.kernel_pid,
                    target,
                    &path,
                )
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "process.path_unlink_at" => {
            let dir_fd = javascript_sync_rpc_arg_u32(&request.args, 0, "path_unlink_at dir fd")?;
            let path = javascript_sync_rpc_arg_str(&request.args, 1, "path_unlink_at path")?;
            let path = wasm_process_resolve_at_path(kernel, process.kernel_pid, dir_fd, path)?;
            kernel
                .remove_file_for_process(EXECUTION_DRIVER_NAME, process.kernel_pid, &path)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "process.fd_preopens" => kernel
            .initialize_wasi_preopens(EXECUTION_DRIVER_NAME, process.kernel_pid)
            .map(|preopens| {
                Value::Array(
                    preopens
                        .into_iter()
                        .map(|preopen| {
                            json!({
                                "fd": preopen.fd,
                                "guestPath": preopen.guest_path,
                                "rightsBase": preopen.rights_base,
                                "rightsInheriting": preopen.rights_inheriting,
                            })
                        })
                        .collect(),
                )
            })
            .map_err(kernel_error),
        "process.fd_preopen" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "fd_preopen fd")?;
            kernel
                .wasi_preopen(EXECUTION_DRIVER_NAME, process.kernel_pid, fd)
                .map(|preopen| {
                    preopen
                        .map(|preopen| {
                            json!({
                                "fd": preopen.fd,
                                "guestPath": preopen.guest_path,
                                "rightsBase": preopen.rights_base,
                                "rightsInheriting": preopen.rights_inheriting,
                            })
                        })
                        .unwrap_or(Value::Null)
                })
                .map_err(kernel_error)
        }
        "process.fd_snapshot" => {
            let entries = kernel
                .fd_snapshot(EXECUTION_DRIVER_NAME, process.kernel_pid)
                .map_err(kernel_error)?;
            let managed_ids = if let Some(registry) = managed_descriptions.as_ref() {
                registry
                    .lock()
                    .map_err(|_| {
                        SidecarError::host("EIO", "managed description registry lock poisoned")
                    })?
                    .iter()
                    .filter_map(|(description_id, description)| {
                        description
                            .route_for(process.kernel_pid)
                            .is_some()
                            .then_some(*description_id)
                    })
                    .collect::<BTreeSet<_>>()
            } else {
                BTreeSet::new()
            };
            Ok(Value::Array(
                entries
                    .into_iter()
                    .map(|entry| {
                        json!({
                            "fd": entry.fd,
                            "descriptionId": entry.description_id.to_string(),
                            "managedHostNet": managed_ids.contains(&entry.description_id),
                            "fdFlags": entry.fd_flags,
                            "statusFlags": entry.status_flags,
                            "filetype": entry.filetype,
                            "rightsBase": entry.rights_base,
                            "rightsInheriting": entry.rights_inheriting,
                            "kind": if entry.is_socket {
                                "socket"
                            } else if entry.is_pipe {
                                "pipe"
                            } else if entry.is_pty {
                                "pty"
                            } else {
                                "file"
                            },
                        })
                    })
                    .collect(),
            ))
        }
        "process.hostnet_fd_open" => {
            let datagram = javascript_sync_rpc_arg_bool(
                &request.args,
                0,
                "host-network datagram flag",
            )?;
            let nonblocking = javascript_sync_rpc_arg_bool(
                &request.args,
                1,
                "host-network nonblocking flag",
            )?;
            let close_on_exec = javascript_sync_rpc_arg_bool(
                &request.args,
                2,
                "host-network close-on-exec flag",
            )?;
            kernel
                .fd_open_external_socket(
                    EXECUTION_DRIVER_NAME,
                    process.kernel_pid,
                    datagram,
                    nonblocking,
                    close_on_exec,
                )
                .map(|(fd, description_id)| {
                    json!({ "fd": fd, "descriptionId": description_id.to_string() })
                })
                .map_err(kernel_error)
        }
        "process.fd_description_identity" => {
            let fd = javascript_sync_rpc_arg_u32(
                &request.args,
                0,
                "fd description identity fd",
            )?;
            kernel
                .fd_description_identity(EXECUTION_DRIVER_NAME, process.kernel_pid, fd)
                .map(|(description_id, aliases)| {
                    json!({
                        "descriptionId": description_id.to_string(),
                        "aliases": aliases,
                    })
                })
                .map_err(kernel_error)
        }
        "process.fd_description_alias_count" => {
            let description_id = javascript_sync_rpc_arg_str(
                &request.args,
                0,
                "fd description id",
            )?
            .parse::<u64>()
            .map_err(|_| {
                SidecarError::host("EINVAL", "fd description id must be a u64 decimal string")
            })?;
            kernel
                .fd_description_alias_count(
                    EXECUTION_DRIVER_NAME,
                    process.kernel_pid,
                    description_id,
                )
                .map(Value::from)
                .map_err(kernel_error)
        }
        "process.fd_read" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "fd_read fd")?;
            // A previous read may have freed capacity in fd 0's pipe. Refill
            // it before the next blocking read so large run-to-completion
            // stdin payloads continue draining and the deferred EOF is
            // delivered after the queued tail.
            if fd == 0 {
                flush_pending_kernel_stdin(kernel, process)?;
            }
            let length = usize::try_from(javascript_sync_rpc_arg_u64(
                &request.args,
                1,
                "fd_read length",
            )?)
            .map_err(|_| SidecarError::InvalidState("fd_read length is too large".into()))?;
            let timeout_ms =
                javascript_sync_rpc_arg_u64_optional(&request.args, 2, "fd_read timeout ms")?;
            match timeout_ms {
                Some(timeout_ms) => kernel
                    .fd_read_with_timeout_result(
                        EXECUTION_DRIVER_NAME,
                        process.kernel_pid,
                        fd,
                        length,
                        Some(Duration::from_millis(timeout_ms)),
                    )
                    .map(Option::unwrap_or_default),
                None => kernel.fd_read(EXECUTION_DRIVER_NAME, process.kernel_pid, fd, length),
            }
            .map(|bytes| host_bytes_value(&bytes))
            .map_err(kernel_error)
        }
        "process.fd_pread" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "fd_pread fd")?;
            let length = usize::try_from(javascript_sync_rpc_arg_u64(
                &request.args,
                1,
                "fd_pread length",
            )?)
            .map_err(|_| SidecarError::InvalidState("fd_pread length is too large".into()))?;
            let offset = javascript_sync_rpc_arg_str(&request.args, 2, "fd_pread offset")?
                .parse::<u64>()
                .map_err(|_| SidecarError::InvalidState("fd_pread offset must be u64".into()))?;
            kernel
                .fd_pread(
                    EXECUTION_DRIVER_NAME,
                    process.kernel_pid,
                    fd,
                    length,
                    offset,
                )
                .map(|bytes| host_bytes_value(&bytes))
                .map_err(kernel_error)
        }
        "process.fd_write" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "fd_write fd")?;
            let data = javascript_sync_rpc_bytes_arg(&request.args, 1, "fd_write data")?;
            // A synchronous WASM RPC cannot park this dispatcher in a
            // blocking pipe write: the reader's RPC must be serviced here as
            // well. The runner polls and retries when a logically blocking fd
            // reports EAGAIN; genuinely nonblocking fds surface EAGAIN.
            let written = match process.execution.synchronous_fd_write_policy() {
                SynchronousFdWritePolicy::NonblockingRetry => kernel.fd_write_nonblocking(
                    EXECUTION_DRIVER_NAME,
                    process.kernel_pid,
                    fd,
                    &data,
                ),
                SynchronousFdWritePolicy::Blocking => {
                    kernel.fd_write(EXECUTION_DRIVER_NAME, process.kernel_pid, fd, &data)
                }
            }
            .map_err(kernel_error)?;
            Ok(Value::from(written))
        }
        "process.fd_pwrite" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "fd_pwrite fd")?;
            let data = javascript_sync_rpc_bytes_arg(&request.args, 1, "fd_pwrite data")?;
            let offset = javascript_sync_rpc_arg_str(&request.args, 2, "fd_pwrite offset")?
                .parse::<u64>()
                .map_err(|_| SidecarError::InvalidState("fd_pwrite offset must be u64".into()))?;
            let written = kernel
                .fd_pwrite(EXECUTION_DRIVER_NAME, process.kernel_pid, fd, &data, offset)
                .map_err(kernel_error)?;
            Ok(Value::from(written))
        }
        "process.fd_sync" | "process.fd_datasync" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "fd_sync fd")?;
            kernel
                .fd_sync(EXECUTION_DRIVER_NAME, process.kernel_pid, fd)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "process.fd_readdir" => {
            const MAX_READDIR_ENTRIES_PER_CALL: usize = 4096;
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "fd_readdir fd")?;
            let cookie = javascript_sync_rpc_arg_str(&request.args, 1, "fd_readdir cookie")?
                .parse::<usize>()
                .map_err(|_| {
                    SidecarError::InvalidState("fd_readdir cookie must be usize".into())
                })?;
            let max_entries = usize::try_from(javascript_sync_rpc_arg_u64(
                &request.args,
                2,
                "fd_readdir max entries",
            )?)
            .unwrap_or(usize::MAX)
            .min(MAX_READDIR_ENTRIES_PER_CALL);
            kernel
                .fd_read_dir_with_types(EXECUTION_DRIVER_NAME, process.kernel_pid, fd)
                .map(|entries| {
                    Value::Array(
                        entries
                            .into_iter()
                            .enumerate()
                            .skip(cookie)
                            .take(max_entries)
                            .map(|(index, entry)| {
                                json!({
                                    "name": entry.name,
                                    "ino": entry.ino.to_string(),
                                    "filetype": entry.filetype,
                                    "next": index.saturating_add(1).to_string(),
                                })
                            })
                            .collect(),
                    )
                })
                .map_err(kernel_error)
        }
        "process.fd_close" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "fd_close fd")?;
            kernel
                .fd_close(EXECUTION_DRIVER_NAME, process.kernel_pid, fd)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "process.fd_stat" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "fd_stat fd")?;
            kernel
                .fd_stat(EXECUTION_DRIVER_NAME, process.kernel_pid, fd)
                .map(|stat| {
                    json!({
                        "filetype": stat.filetype,
                        "flags": stat.flags,
                        "rightsBase": stat.rights,
                        "rightsInheriting": stat.rights_inheriting,
                        "preopenPath": stat.wasi_preopen_path,
                    })
                })
                .map_err(kernel_error)
        }
        "process.fd_filestat" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "fd_filestat fd")?;
            let fd_stat = kernel
                .fd_stat(EXECUTION_DRIVER_NAME, process.kernel_pid, fd)
                .map_err(kernel_error)?;
            kernel
                .dev_fd_stat(EXECUTION_DRIVER_NAME, process.kernel_pid, fd)
                .map(|stat| {
                    json!({
                        "dev": stat.dev,
                        "ino": stat.ino,
                        "filetype": fd_stat.filetype,
                        "nlink": stat.nlink,
                        "mode": stat.mode,
                        "uid": stat.uid,
                        "gid": stat.gid,
                        "size": stat.size,
                        "blocks": stat.blocks,
                        "rdev": stat.rdev,
                        "atimeMs": stat.atime_ms,
                        "mtimeMs": stat.mtime_ms,
                        "ctimeMs": stat.ctime_ms,
                    })
                })
                .map_err(kernel_error)
        }
        "process.fd_chown" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "fd_chown fd")?;
            let uid = javascript_sync_rpc_arg_u32(&request.args, 1, "fd_chown uid")?;
            let gid = javascript_sync_rpc_arg_u32(&request.args, 2, "fd_chown gid")?;
            kernel
                .fd_chown_for_process(EXECUTION_DRIVER_NAME, process.kernel_pid, fd, uid, gid)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "process.fd_chmod" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "fd_chmod fd")?;
            let mode = javascript_sync_rpc_arg_u32(&request.args, 1, "fd_chmod mode")?;
            kernel
                .fd_chmod_for_process(EXECUTION_DRIVER_NAME, process.kernel_pid, fd, mode)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "process.fd_truncate" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "fd_truncate fd")?;
            let length = javascript_sync_rpc_arg_str(&request.args, 1, "fd_truncate length")?
                .parse::<u64>()
                .map_err(|_| SidecarError::InvalidState("fd_truncate length must be u64".into()))?;
            kernel
                .fd_truncate(EXECUTION_DRIVER_NAME, process.kernel_pid, fd, length)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "process.fd_utimes" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "fd_utimes fd")?;
            let atime_ns = javascript_sync_rpc_arg_str(&request.args, 1, "fd_utimes atime")?;
            let mtime_ns = javascript_sync_rpc_arg_str(&request.args, 2, "fd_utimes mtime")?;
            let fst_flags = javascript_sync_rpc_arg_u32(&request.args, 3, "fd_utimes flags")?;
            let atime =
                wasm_process_utime_spec(atime_ns, fst_flags & 1 != 0, fst_flags & 2 != 0)?;
            let mtime =
                wasm_process_utime_spec(mtime_ns, fst_flags & 4 != 0, fst_flags & 8 != 0)?;
            kernel
                .futimes(
                    EXECUTION_DRIVER_NAME,
                    process.kernel_pid,
                    fd,
                    atime,
                    mtime,
                )
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "process.fd_set_flags" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "fd_set_flags fd")?;
            let flags = javascript_sync_rpc_arg_u32(&request.args, 1, "fd_set_flags flags")?;
            kernel
                .fd_fcntl(
                    EXECUTION_DRIVER_NAME,
                    process.kernel_pid,
                    fd,
                    agentos_kernel::fd_table::F_SETFL,
                    flags,
                )
                .map(Value::from)
                .map_err(kernel_error)
        }
        "process.fd_getfd" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "fd_getfd fd")?;
            kernel
                .fd_fcntl(
                    EXECUTION_DRIVER_NAME,
                    process.kernel_pid,
                    fd,
                    agentos_kernel::fd_table::F_GETFD,
                    0,
                )
                .map(Value::from)
                .map_err(kernel_error)
        }
        "process.fd_setfd" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "fd_setfd fd")?;
            let flags = javascript_sync_rpc_arg_u32(&request.args, 1, "fd_setfd flags")?;
            kernel
                .fd_fcntl(
                    EXECUTION_DRIVER_NAME,
                    process.kernel_pid,
                    fd,
                    agentos_kernel::fd_table::F_SETFD,
                    flags,
                )
                .map(Value::from)
                .map_err(kernel_error)
        }
        "process.fd_flock" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "fd_flock fd")?;
            let operation = javascript_sync_rpc_arg_u32(&request.args, 1, "fd_flock operation")?;
            kernel
                .fd_flock(EXECUTION_DRIVER_NAME, process.kernel_pid, fd, operation)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "process.fd_record_lock" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "fd_record_lock fd")?;
            let command = javascript_sync_rpc_arg_u32(&request.args, 1, "fd_record_lock command")?;
            let raw_lock_type =
                javascript_sync_rpc_arg_u32(&request.args, 2, "fd_record_lock type")?;
            let start = javascript_sync_rpc_arg_str(&request.args, 3, "fd_record_lock start")?
                .parse::<u64>()
                .map_err(|_| {
                    SidecarError::host("EINVAL", "fd_record_lock start must be u64")
                })?;
            let length = javascript_sync_rpc_arg_str(&request.args, 4, "fd_record_lock length")?
                .parse::<u64>()
                .map_err(|_| {
                    SidecarError::host("EINVAL", "fd_record_lock length must be u64")
                })?;
            let lock_type = match raw_lock_type {
                0 => agentos_kernel::fd_table::RecordLockType::Read,
                1 => agentos_kernel::fd_table::RecordLockType::Write,
                2 => agentos_kernel::fd_table::RecordLockType::Unlock,
                _ => {
                    return Err(SidecarError::host("EINVAL", "fd_record_lock type must be F_RDLCK, F_WRLCK, or F_UNLCK",
                    ))
                }
            };
            let conflict = match command {
                12 => kernel.fd_record_lock(
                    EXECUTION_DRIVER_NAME,
                    process.kernel_pid,
                    fd,
                    lock_type,
                    start,
                    length,
                    true,
                ),
                13 => kernel.fd_record_lock(
                    EXECUTION_DRIVER_NAME,
                    process.kernel_pid,
                    fd,
                    lock_type,
                    start,
                    length,
                    false,
                ),
                14 => kernel
                    .fd_record_lock_wait(
                        EXECUTION_DRIVER_NAME,
                        process.kernel_pid,
                        fd,
                        lock_type,
                        start,
                        length,
                    )
                    .map(|()| None),
                _ => {
                    return Err(SidecarError::host("EINVAL", format!("unsupported fd_record_lock command {command}"
                    )))
                }
            }
            .map_err(kernel_error)?;
            let response = conflict.map_or_else(
                || json!({ "type": 2, "pid": 0, "start": start.to_string(), "length": length.to_string() }),
                |lock| {
                    let lock_type = match lock.lock_type {
                        agentos_kernel::fd_table::RecordLockType::Read => 0,
                        agentos_kernel::fd_table::RecordLockType::Write => 1,
                        agentos_kernel::fd_table::RecordLockType::Unlock => 2,
                    };
                    json!({
                        "type": lock_type,
                        "pid": lock.pid,
                        "start": lock.start.to_string(),
                        "length": lock.length().to_string(),
                    })
                },
            );
            Ok(response)
        }
        "process.fd_record_lock_cancel" => kernel
            .fd_record_lock_cancel(EXECUTION_DRIVER_NAME, process.kernel_pid)
            .map(|()| Value::Null)
            .map_err(kernel_error),
        "process.fd_dup" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "fd_dup fd")?;
            kernel
                .fd_dup(EXECUTION_DRIVER_NAME, process.kernel_pid, fd)
                .map(Value::from)
                .map_err(kernel_error)
        }
        "process.fd_dup2" => {
            let old_fd = javascript_sync_rpc_arg_u32(&request.args, 0, "fd_dup2 old fd")?;
            let new_fd = javascript_sync_rpc_arg_u32(&request.args, 1, "fd_dup2 new fd")?;
            kernel
                .fd_dup2(EXECUTION_DRIVER_NAME, process.kernel_pid, old_fd, new_fd)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "process.fd_dup_min" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "fd_dup_min fd")?;
            let min_fd = javascript_sync_rpc_arg_u32(&request.args, 1, "fd_dup_min minimum")?;
            kernel
                .fd_fcntl(
                    EXECUTION_DRIVER_NAME,
                    process.kernel_pid,
                    fd,
                    agentos_kernel::fd_table::F_DUPFD,
                    min_fd,
                )
                .map(Value::from)
                .map_err(kernel_error)
        }
        "process.fd_move" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "fd_move fd")?;
            let replaced_fd = javascript_sync_rpc_arg_u32_optional(
                &request.args,
                1,
                "fd_move replaced fd",
            )?;
            kernel
                .fd_renumber_projection(
                    EXECUTION_DRIVER_NAME,
                    process.kernel_pid,
                    fd,
                    replaced_fd,
                )
                .map(Value::from)
                .map_err(kernel_error)
        }
        "process.fd_seek" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "fd_seek fd")?;
            let offset = javascript_sync_rpc_arg_str(&request.args, 1, "fd_seek offset")?
                .parse::<i64>()
                .map_err(|_| SidecarError::InvalidState("fd_seek offset must be i64".into()))?;
            let whence = u8::try_from(javascript_sync_rpc_arg_u32(
                &request.args,
                2,
                "fd_seek whence",
            )?)
            .map_err(|_| SidecarError::InvalidState("fd_seek whence is invalid".into()))?;
            kernel
                .fd_seek(
                    EXECUTION_DRIVER_NAME,
                    process.kernel_pid,
                    fd,
                    offset,
                    whence,
                )
                .map(|next| Value::String(next.to_string()))
                .map_err(kernel_error)
        }
        "process.fd_path" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "fd_path fd")?;
            kernel
                .fd_path(EXECUTION_DRIVER_NAME, process.kernel_pid, fd)
                .map(Value::String)
                .map_err(kernel_error)
        }
        "process.fd_chdir_path" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "fchdir fd")?;
            let stat = kernel
                .fd_stat(EXECUTION_DRIVER_NAME, process.kernel_pid, fd)
                .map_err(kernel_error)?;
            if stat.filetype != agentos_kernel::fd_table::FILETYPE_DIRECTORY {
                return Err(SidecarError::host("ENOTDIR", format!("file descriptor {fd} is not a directory"
                )));
            }
            kernel
                .fd_path(EXECUTION_DRIVER_NAME, process.kernel_pid, fd)
                .map(Value::String)
                .map_err(kernel_error)
        }
        "process.fd_socketpair" => {
            let socket_kind = javascript_sync_rpc_arg_u32(&request.args, 0, "socketpair kind")?;
            let nonblocking =
                javascript_sync_rpc_arg_bool(&request.args, 1, "socketpair nonblocking")?;
            let close_on_exec =
                javascript_sync_rpc_arg_bool(&request.args, 2, "socketpair close-on-exec")?;
            let socket_type = match socket_kind {
                1 => SocketType::Stream,
                2 => SocketType::Datagram,
                3 => SocketType::SeqPacket,
                _ => {
                    return Err(SidecarError::InvalidState(format!(
                        "unsupported socketpair kind {socket_kind}"
                    )))
                }
            };
            kernel
                .fd_socketpair(
                    EXECUTION_DRIVER_NAME,
                    process.kernel_pid,
                    socket_type,
                    nonblocking,
                    close_on_exec,
                )
                .map(|(first_fd, second_fd)| json!({ "firstFd": first_fd, "secondFd": second_fd }))
                .map_err(kernel_error)
        }
        "process.pty_open" => kernel
            .open_pty(EXECUTION_DRIVER_NAME, process.kernel_pid)
            .map(|(master_fd, slave_fd, path)| {
                json!({ "masterFd": master_fd, "slaveFd": slave_fd, "path": path })
            })
            .map_err(kernel_error),
        "process.fd_sendmsg_rights" => {
            let socket_fd = javascript_sync_rpc_arg_u32(&request.args, 0, "sendmsg socket fd")?;
            let data = javascript_sync_rpc_request_bytes_arg(request, 1, "sendmsg data")?;
            let raw_rights = request
                .args
                .get(2)
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    SidecarError::InvalidState(
                        "sendmsg rights must be an array of file descriptors".into(),
                    )
                })?;
            if raw_rights.len() > LINUX_SCM_MAX_FD {
                return Err(SidecarError::host("EINVAL", format!("SCM_RIGHTS accepts at most {LINUX_SCM_MAX_FD} descriptors"
                )));
            }
            if let Some(limit) = kernel.resource_limits().max_open_fds {
                if raw_rights.len() > limit {
                    return Err(SidecarError::host("EMFILE", format!("SCM_RIGHTS descriptor list has {} entries, exceeding limits.resources.maxOpenFds ({limit}); raise limits.resources.maxOpenFds",
                        raw_rights.len()
                    )));
                }
            }

            // Snapshot before constructing new pending descriptions. Existing
            // transferred aliases are de-duplicated by their open-description
            // identity; only a metadata-only pending socket adds a description.
            let network_counts = process_network_resource_counts_with_transfers(
                kernel,
                process,
                &socket_paths.host_net_transfer_descriptions,
            );
            let mut rights = Vec::with_capacity(raw_rights.len());
            let mut pending_host_net_count = 0usize;
            for value in raw_rights {
                if let Some(fd) = value.as_u64().and_then(|fd| u32::try_from(fd).ok()) {
                    rights.push(FdTransferRequest::Fd(fd));
                    continue;
                }
                if value.get("kind").and_then(Value::as_str) != Some("hostNet") {
                    return Err(SidecarError::InvalidState(
                        "sendmsg rights entries must be kernel fds or hostNet descriptions".into(),
                    ));
                }
                let managed_fd = value
                    .get("fd")
                    .and_then(Value::as_u64)
                    .and_then(|fd| u32::try_from(fd).ok());
                let managed_description_id = value
                    .get("descriptionId")
                    .and_then(Value::as_str)
                    .map(|description_id| {
                        description_id.parse::<u64>().map_err(|_| {
                            SidecarError::host(
                                "EINVAL",
                                "SCM_RIGHTS host-network descriptionId must be a u64 decimal string",
                            )
                        })
                    })
                    .transpose()?;
                if let (Some(fd), Some(description_id)) =
                    (managed_fd, managed_description_id)
                {
                    let (actual_description_id, _) = kernel
                        .fd_description_identity(
                            EXECUTION_DRIVER_NAME,
                            process.kernel_pid,
                            fd,
                        )
                        .map_err(kernel_error)?;
                    if actual_description_id != description_id {
                        return Err(SidecarError::host(
                            "EINVAL",
                            "SCM_RIGHTS host-network fd and descriptionId disagree",
                        ));
                    }
                    let description = managed_descriptions
                        .as_ref()
                        .ok_or_else(|| {
                            SidecarError::host(
                                "ENOTSOCK",
                                "managed SCM_RIGHTS registry is unavailable",
                            )
                        })?
                        .lock()
                        .map_err(|_| {
                            SidecarError::host(
                                "EIO",
                                "managed description registry lock poisoned",
                            )
                        })?
                        .get(&description_id)
                        .cloned()
                        .ok_or_else(|| {
                            SidecarError::host(
                                "ENOTSOCK",
                                "managed SCM_RIGHTS description is unknown",
                            )
                        })?;
                    let transferred = prepare_managed_transferred_host_net_resource(
                        kernel,
                        process,
                        description_id,
                        fd,
                        &description,
                        "managed SCM_RIGHTS host-network",
                    )?;
                    register_host_net_transfer_description(
                        &socket_paths.host_net_transfer_descriptions,
                        &transferred,
                    )?;
                    let transfer = kernel
                        .fd_transfer(EXECUTION_DRIVER_NAME, process.kernel_pid, fd)
                        .map_err(kernel_error)?;
                    rights.push(FdTransferRequest::Opaque(Arc::new(
                        ManagedTransferredHostNetSocket {
                            resource: transferred,
                            transfer,
                        },
                    )));
                    continue;
                }
                if managed_fd.is_some() || managed_description_id.is_some() {
                    return Err(SidecarError::host(
                        "EINVAL",
                        "SCM_RIGHTS host-network fd and descriptionId must be provided together",
                    ));
                }
                let source = scm_rights_host_net_source(value)?;
                let transferred = if let Some(source) = source {
                    prepare_transferred_host_net_resource(
                        kernel,
                        process,
                        &source,
                        value,
                        "SCM_RIGHTS host-network",
                    )?
                } else {
                    let options =
                        host_net_open_description_options(value, "SCM_RIGHTS pending socket")?;
                    let metadata = TransferredHostNetMetadata::pending(
                        value,
                        options,
                        "SCM_RIGHTS pending socket",
                    )?;
                    pending_host_net_count = pending_host_net_count.saturating_add(1);
                    TransferredHostNetSocket::Pending {
                        metadata,
                        description_handles: Arc::new(()),
                        tcp_reservation: None,
                    }
                };
                register_host_net_transfer_description(
                    &socket_paths.host_net_transfer_descriptions,
                    &transferred,
                )?;
                rights.push(FdTransferRequest::Opaque(Arc::new(transferred)));
            }
            check_spawn_host_net_resource_limit(
                kernel.resource_limits().max_sockets,
                network_counts.sockets,
                pending_host_net_count,
                "EMFILE",
                "SCM_RIGHTS socket descriptions",
                "maxSockets",
            )?;
            check_spawn_host_net_resource_limit(
                kernel.resource_limits().max_connections,
                network_counts.connections,
                0,
                "EAGAIN",
                "SCM_RIGHTS connected socket descriptions",
                "maxConnections",
            )?;
            kernel
                .fd_socket_sendmsg_transfers(
                    EXECUTION_DRIVER_NAME,
                    process.kernel_pid,
                    socket_fd,
                    &data,
                    &rights,
                )
                .map(Value::from)
                .map_err(kernel_error)
        }
        "process.fd_recvmsg_rights" => {
            let socket_fd = javascript_sync_rpc_arg_u32(&request.args, 0, "recvmsg socket fd")?;
            let max_bytes = usize::try_from(javascript_sync_rpc_arg_u64(
                &request.args,
                1,
                "recvmsg maximum bytes",
            )?)
            .map_err(|_| SidecarError::InvalidState("recvmsg byte limit is too large".into()))?;
            let max_rights = usize::try_from(javascript_sync_rpc_arg_u64(
                &request.args,
                2,
                "recvmsg maximum rights",
            )?)
            .map_err(|_| SidecarError::InvalidState("recvmsg rights limit is too large".into()))?;
            let close_on_exec =
                javascript_sync_rpc_arg_bool(&request.args, 3, "recvmsg close-on-exec")?;
            let peek = request
                .args
                .get(4)
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let dontwait = request
                .args
                .get(5)
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let waitall = request
                .args
                .get(6)
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let message = kernel
                .fd_socket_recvmsg(
                    EXECUTION_DRIVER_NAME,
                    process.kernel_pid,
                    socket_fd,
                    max_bytes,
                    max_rights,
                    close_on_exec,
                    peek,
                    dontwait,
                    waitall,
                )
                .map_err(kernel_error)?;
            Ok(if let Some(message) = message {
                let mut rights = Vec::with_capacity(message.rights.len());
                for right in message.rights {
                    match right {
                        ReceivedFdRight::Fd(fd) => {
                            rights.push(json!({ "kind": "kernel", "fd": fd }));
                        }
                        ReceivedFdRight::Opaque(resource) => {
                            let (transferred, managed_transfer) = match Arc::downcast::<
                                ManagedTransferredHostNetSocket,
                            >(resource)
                            {
                                Ok(managed) => {
                                    let managed = match Arc::try_unwrap(managed) {
                                        Ok(managed) => managed,
                                        Err(shared) => shared.clone_for_fd_transfer()?,
                                    };
                                    (managed.resource, Some(managed.transfer))
                                }
                                Err(resource) => {
                                    let transferred = Arc::downcast::<TransferredHostNetSocket>(
                                        resource,
                                    )
                                    .map_err(|_| {
                                        SidecarError::InvalidState(
                                            "received unknown SCM_RIGHTS resource type".into(),
                                        )
                                    })?;
                                    let transferred = match Arc::try_unwrap(transferred) {
                                        Ok(transferred) => transferred,
                                        Err(shared) => shared.clone_for_fd_transfer()?,
                                    };
                                    (transferred, None)
                                }
                            };
                            let managed_transfer = managed_transfer
                                .or_else(|| transferred.kernel_transfer_guard());
                            let managed_description_id = managed_transfer
                                .as_ref()
                                .map(TransferredFd::description_id);
                            let mut managed_registry = if let Some(description_id) = managed_description_id {
                                let registry = managed_descriptions.as_ref().ok_or_else(|| {
                                    SidecarError::host(
                                        "ENOTSOCK",
                                        "managed SCM_RIGHTS registry is unavailable",
                                    )
                                })?;
                                let descriptions = registry.lock().map_err(|_| {
                                    SidecarError::host(
                                        "EIO",
                                        "managed description registry lock poisoned",
                                    )
                                })?;
                                if !descriptions.contains_key(&description_id) {
                                    return Err(SidecarError::host(
                                        "ESTALE",
                                        "managed SCM_RIGHTS description disappeared in transit",
                                    ));
                                }
                                Some(descriptions)
                            } else {
                                None
                            };
                            let installed_managed_fd = managed_transfer
                                .as_ref()
                                .map(|transfer| {
                                    kernel
                                        .fd_install_transfer(
                                            EXECUTION_DRIVER_NAME,
                                            process.kernel_pid,
                                            transfer,
                                            close_on_exec,
                                        )
                                        .map_err(kernel_error)
                                })
                                .transpose()?;
                            let mut installed_managed_route = None;
                            let install_result = (|| -> Result<(), SidecarError> {
                                match transferred {
                                TransferredHostNetSocket::Tcp {
                                    mut socket,
                                    metadata,
                                } => {
                                    let pending = reserve_capability(
                                        &capabilities,
                                        CapabilityKind::TcpSocket,
                                    )?;
                                    let socket_id = process.allocate_tcp_socket_id();
                                    socket.listener_id = None;
                                    let capability_key =
                                        NativeCapabilityKey::TcpSocket(socket_id.clone());
                                    let identity = commit_process_capability(
                                        process,
                                        pending,
                                        capability_key.clone(),
                                        socket_id.clone(),
                                        socket.kernel_socket_id,
                                    )?;
                                    socket.set_event_pusher(
                                        process.execution.execution_wake_handle(
                                            process.kernel_handle.runtime_identity(),
                                        ),
                                        Some(identity),
                                        Arc::clone(&process.process_event_notify),
                                    );
                                    register_kernel_readiness_target(
                                        &kernel_readiness,
                                        socket.kernel_socket_id,
                                        process.execution.execution_wake_handle(
                                            process.kernel_handle.runtime_identity(),
                                        ),
                                        Some(Arc::clone(&socket.read_event_notify)),
                                        process.capability_readiness_identity(&capability_key),
                                        socket_id.clone(),
                                        KernelSocketReadinessEvent::Data,
                                    );
                                    let local = socket.guest_local_addr;
                                    let remote = socket.guest_remote_addr;
                                    process.tcp_sockets.insert(socket_id.clone(), *socket);
                                    installed_managed_route =
                                        Some(ManagedHostNetRoute::TcpSocket(socket_id.clone()));
                                    rights.push(transferred_hostnet_value(
                                        "tcp",
                                        metadata,
                                        Some(("socketId", socket_id)),
                                        Some(identity),
                                        Some(local),
                                        Some(remote),
                                    ));
                                }
                                TransferredHostNetSocket::TcpListener { listener, metadata } => {
                                    let pending = reserve_capability(
                                        &capabilities,
                                        CapabilityKind::TcpListener,
                                    )?;
                                    let listener_id = process.allocate_tcp_listener_id();
                                    let local = listener.guest_local_addr();
                                    let capability_key =
                                        NativeCapabilityKey::TcpListener(listener_id.clone());
                                    let identity = commit_process_capability(
                                        process,
                                        pending,
                                        capability_key.clone(),
                                        listener_id.clone(),
                                        listener.kernel_socket_id,
                                    )?;
                                    register_kernel_readiness_target(
                                        &kernel_readiness,
                                        listener.kernel_socket_id,
                                        process.execution.execution_wake_handle(
                                            process.kernel_handle.runtime_identity(),
                                        ),
                                        None,
                                        process.capability_readiness_identity(&capability_key),
                                        listener_id.clone(),
                                        KernelSocketReadinessEvent::Accept,
                                    );
                                    process.tcp_listeners.insert(listener_id.clone(), listener);
                                    installed_managed_route = Some(
                                        ManagedHostNetRoute::TcpListener(listener_id.clone()),
                                    );
                                    rights.push(transferred_hostnet_value(
                                        "listener",
                                        metadata,
                                        Some(("serverId", listener_id)),
                                        Some(identity),
                                        Some(local),
                                        None,
                                    ));
                                }
                                TransferredHostNetSocket::Udp { socket, metadata } => {
                                    let pending = reserve_capability(
                                        &capabilities,
                                        CapabilityKind::UdpSocket,
                                    )?;
                                    let socket_id = process.allocate_udp_socket_id();
                                    let local = socket.guest_local_addr;
                                    let capability_key =
                                        NativeCapabilityKey::UdpSocket(socket_id.clone());
                                    let identity = commit_process_capability(
                                        process,
                                        pending,
                                        capability_key.clone(),
                                        socket_id.clone(),
                                        socket.kernel_socket_id,
                                    )?;
                                    socket.set_event_pusher(
                                        process.execution.execution_wake_handle(
                                            process.kernel_handle.runtime_identity(),
                                        ),
                                        Some(identity),
                                        Arc::clone(&process.process_event_notify),
                                    );
                                    register_kernel_readiness_target(
                                        &kernel_readiness,
                                        socket.kernel_socket_id,
                                        process.execution.execution_wake_handle(
                                            process.kernel_handle.runtime_identity(),
                                        ),
                                        Some(Arc::clone(&socket.read_event_notify)),
                                        process.capability_readiness_identity(&capability_key),
                                        socket_id.clone(),
                                        KernelSocketReadinessEvent::Datagram,
                                    );
                                    process.udp_sockets.insert(socket_id.clone(), socket);
                                    installed_managed_route =
                                        Some(ManagedHostNetRoute::UdpSocket(socket_id.clone()));
                                    rights.push(transferred_hostnet_value(
                                        "udp",
                                        metadata,
                                        Some(("udpSocketId", socket_id)),
                                        Some(identity),
                                        local,
                                        None,
                                    ));
                                }
                                TransferredHostNetSocket::Unix {
                                    mut socket,
                                    metadata,
                                } => {
                                    let pending = reserve_capability(
                                        &capabilities,
                                        CapabilityKind::UnixSocket,
                                    )?;
                                    let socket_id = process.allocate_unix_socket_id();
                                    socket.listener_id = None;
                                    let capability_key =
                                        NativeCapabilityKey::UnixSocket(socket_id.clone());
                                    let identity = commit_process_capability(
                                        process,
                                        pending,
                                        capability_key,
                                        socket_id.clone(),
                                        None,
                                    )?;
                                    socket.set_event_pusher(
                                        process.execution.execution_wake_handle(
                                            process.kernel_handle.runtime_identity(),
                                        ),
                                        Some(identity),
                                        Arc::clone(&process.process_event_notify),
                                    );
                                    process.unix_sockets.insert(socket_id.clone(), socket);
                                    installed_managed_route =
                                        Some(ManagedHostNetRoute::UnixSocket(socket_id.clone()));
                                    rights.push(transferred_hostnet_value(
                                        "unix",
                                        metadata,
                                        Some(("socketId", socket_id)),
                                        Some(identity),
                                        None,
                                        None,
                                    ));
                                }
                                TransferredHostNetSocket::UnixListener { listener, metadata } => {
                                    let pending = reserve_capability(
                                        &capabilities,
                                        CapabilityKind::UnixListener,
                                    )?;
                                    let listener_id = process.allocate_unix_listener_id();
                                    let capability_key =
                                        NativeCapabilityKey::UnixListener(listener_id.clone());
                                    let identity = commit_process_capability(
                                        process,
                                        pending,
                                        capability_key,
                                        listener_id.clone(),
                                        None,
                                    )?;
                                    listener.set_event_pusher(
                                        process.execution.execution_wake_handle(
                                            process.kernel_handle.runtime_identity(),
                                        ),
                                        Some(identity),
                                        Arc::clone(&process.process_event_notify),
                                    );
                                    process.unix_listeners.insert(listener_id.clone(), listener);
                                    installed_managed_route = if metadata.listening {
                                        Some(ManagedHostNetRoute::UnixListener(listener_id.clone()))
                                    } else {
                                        Some(ManagedHostNetRoute::UnixBound {
                                            listener_id: listener_id.clone(),
                                        })
                                    };
                                    rights.push(transferred_hostnet_value(
                                        "unix-listener",
                                        metadata,
                                        Some(("serverId", listener_id)),
                                        Some(identity),
                                        None,
                                        None,
                                    ));
                                }
                                TransferredHostNetSocket::Pending {
                                    metadata,
                                    tcp_reservation,
                                    ..
                                } => {
                                    installed_managed_route = if let Some(reservation) = tcp_reservation {
                                        let reservation_id = process.allocate_tcp_port_reservation_id();
                                        process.tcp_port_reservations.insert(
                                            reservation_id.clone(),
                                            reservation,
                                        );
                                        Some(ManagedHostNetRoute::TcpBound { reservation_id })
                                    } else {
                                        Some(ManagedHostNetRoute::Unbound)
                                    };
                                    rights.push(transferred_hostnet_value(
                                        "pending", metadata, None, None, None, None,
                                    ));
                                }
                                }
                                Ok(())
                            })();
                            if let Err(error) = install_result {
                                if let Some(fd) = installed_managed_fd {
                                    if let Err(close_error) = kernel.fd_close(
                                        EXECUTION_DRIVER_NAME,
                                        process.kernel_pid,
                                        fd,
                                    ) {
                                        eprintln!(
                                            "[agentos] failed to roll back received managed host-network fd {fd}: {close_error}"
                                        );
                                    }
                                }
                                return Err(error);
                            }
                            if let Some(fd) = installed_managed_fd {
                                let description_id = managed_description_id.expect(
                                    "managed fd installation requires a canonical description",
                                );
                                if let Some(route) = installed_managed_route {
                                    managed_registry
                                        .as_mut()
                                        .expect("managed registry was prevalidated")
                                        .get_mut(&description_id)
                                        .expect("managed description remains locked")
                                        .routes
                                        .insert(process.kernel_pid, route);
                                }
                                let value = rights.last_mut().expect(
                                    "host-network receive must append one bootstrap right",
                                );
                                let object = value
                                    .as_object_mut()
                                    .expect("host-network receive metadata is constructed as an object");
                                object.insert("fd".into(), Value::from(fd));
                                object.insert(
                                    "descriptionId".into(),
                                    Value::String(description_id.to_string()),
                                );
                            }
                        }
                    }
                }
                json!({
                    "data": host_bytes_value(&message.payload),
                    "rights": rights,
                    "payloadTruncated": message.payload_truncated,
                    "controlTruncated": message.control_truncated,
                    "fullLength": message.full_length,
                })
            } else {
                json!({
                    "data": host_bytes_value(&[]),
                    "rights": [],
                    "payloadTruncated": false,
                    "controlTruncated": false,
                    "fullLength": 0,
                })
            })
        }
        "process.fd_socket_shutdown" => {
            let socket_fd = javascript_sync_rpc_arg_u32(&request.args, 0, "shutdown socket fd")?;
            let how = match javascript_sync_rpc_arg_u32(&request.args, 1, "shutdown mode")? {
                0 => KernelSocketShutdown::Read,
                1 => KernelSocketShutdown::Write,
                2 => KernelSocketShutdown::Both,
                other => {
                    return Err(SidecarError::InvalidState(format!(
                        "invalid shutdown mode {other}"
                    )))
                }
            };
            kernel
                .fd_socket_shutdown(EXECUTION_DRIVER_NAME, process.kernel_pid, socket_fd, how)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "process.kill" => {
            let target_pid =
                javascript_sync_rpc_arg_i32(&request.args, 0, "process.kill target pid")?;
            let signal = javascript_sync_rpc_arg_str(&request.args, 1, "process.kill signal")?;
            let parsed_signal = parse_signal(signal)?;
            if parsed_signal == 0 {
                kernel
                    .signal_process(EXECUTION_DRIVER_NAME, target_pid, parsed_signal)
                    .map_err(kernel_error)?;
                return Ok(Value::Null.into());
            }
            let process_pid = i32::try_from(process.kernel_pid)
                .map_err(|_| SidecarError::InvalidState("process pid exceeds i32".into()))?;
            if target_pid != process_pid {
                return Err(SidecarError::InvalidState(format!(
                    "unknown process pid {target_pid}"
                )));
            }
            kernel
                .signal_process(EXECUTION_DRIVER_NAME, target_pid, parsed_signal)
                .map_err(kernel_error)?;
            process.apply_runtime_controls()?;
            Ok(Value::Null)
        }
        "process.umask" => {
            let new_mask = javascript_sync_rpc_arg_u32_optional(&request.args, 0, "process umask")?;
            kernel
                .umask(EXECUTION_DRIVER_NAME, process.kernel_pid, new_mask)
                .map(|mask| json!(mask))
                .map_err(kernel_error)
        }
        "process.getrlimit" => {
            let resource = javascript_sync_rpc_arg_u32(
                &request.args,
                0,
                "process.getrlimit resource",
            )?;
            let kind = process_resource_limit_kind(resource)?;
            kernel
                .get_resource_limit(EXECUTION_DRIVER_NAME, process.kernel_pid, kind)
                .map(|limit| {
                    json!({
                        "soft": limit.soft.unwrap_or(u64::MAX).to_string(),
                        "hard": limit.hard.unwrap_or(u64::MAX).to_string(),
                    })
                })
                .map_err(kernel_error)
        }
        "process.setrlimit" => {
            let resource = javascript_sync_rpc_arg_u32(
                &request.args,
                0,
                "process.setrlimit resource",
            )?;
            let soft = javascript_sync_rpc_arg_rlim(
                &request.args,
                1,
                "process.setrlimit soft value",
            )?;
            let hard = javascript_sync_rpc_arg_rlim(
                &request.args,
                2,
                "process.setrlimit hard value",
            )?;
            kernel
                .set_resource_limit(
                    EXECUTION_DRIVER_NAME,
                    process.kernel_pid,
                    process_resource_limit_kind(resource)?,
                    agentos_kernel::kernel::ProcessResourceLimit {
                        soft: (soft != u64::MAX).then_some(soft),
                        hard: (hard != u64::MAX).then_some(hard),
                    },
                )
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "process.getuid" => kernel
            .getuid(EXECUTION_DRIVER_NAME, process.kernel_pid)
            .map(|value| json!(value))
            .map_err(kernel_error),
        "process.getgid" => kernel
            .getgid(EXECUTION_DRIVER_NAME, process.kernel_pid)
            .map(|value| json!(value))
            .map_err(kernel_error),
        "process.geteuid" => kernel
            .geteuid(EXECUTION_DRIVER_NAME, process.kernel_pid)
            .map(|value| json!(value))
            .map_err(kernel_error),
        "process.getegid" => kernel
            .getegid(EXECUTION_DRIVER_NAME, process.kernel_pid)
            .map(|value| json!(value))
            .map_err(kernel_error),
        "process.getresuid" => kernel
            .getresuid(EXECUTION_DRIVER_NAME, process.kernel_pid)
            .map(|(uid, euid, suid)| json!([uid, euid, suid]))
            .map_err(kernel_error),
        "process.getresgid" => kernel
            .getresgid(EXECUTION_DRIVER_NAME, process.kernel_pid)
            .map(|(gid, egid, sgid)| json!([gid, egid, sgid]))
            .map_err(kernel_error),
        "process.getgroups" => kernel
            .getgroups(EXECUTION_DRIVER_NAME, process.kernel_pid)
            .map(|groups| json!(groups))
            .map_err(kernel_error),
        "process.getpwuid" => {
            let uid = javascript_sync_rpc_arg_u32(&request.args, 0, "passwd uid")?;
            kernel
                .getpwuid_for_process(EXECUTION_DRIVER_NAME, process.kernel_pid, uid)
                .map(|entry| json!(entry))
                .map_err(kernel_error)
        }
        "process.getpwnam" => {
            let name = javascript_sync_rpc_arg_str(&request.args, 0, "passwd name")?;
            kernel
                .getpwnam_for_process(EXECUTION_DRIVER_NAME, process.kernel_pid, name)
                .map(|entry| json!(entry))
                .map_err(kernel_error)
        }
        "process.getpwent" => {
            let index = javascript_sync_rpc_arg_u32(&request.args, 0, "passwd index")?;
            kernel
                .getpwent_for_process(EXECUTION_DRIVER_NAME, process.kernel_pid, index as usize)
                .map(|entry| json!(entry))
                .map_err(kernel_error)
        }
        "process.getgrgid" => {
            let gid = javascript_sync_rpc_arg_u32(&request.args, 0, "group gid")?;
            kernel
                .getgrgid_for_process(EXECUTION_DRIVER_NAME, process.kernel_pid, gid)
                .map(|entry| json!(entry))
                .map_err(kernel_error)
        }
        "process.getgrnam" => {
            let name = javascript_sync_rpc_arg_str(&request.args, 0, "group name")?;
            kernel
                .getgrnam_for_process(EXECUTION_DRIVER_NAME, process.kernel_pid, name)
                .map(|entry| json!(entry))
                .map_err(kernel_error)
        }
        "process.getgrent" => {
            let index = javascript_sync_rpc_arg_u32(&request.args, 0, "group index")?;
            kernel
                .getgrent_for_process(EXECUTION_DRIVER_NAME, process.kernel_pid, index as usize)
                .map(|entry| json!(entry))
                .map_err(kernel_error)
        }
        "process.setuid" => {
            let uid = javascript_sync_rpc_arg_u32(&request.args, 0, "setuid uid")?;
            kernel
                .setuid(EXECUTION_DRIVER_NAME, process.kernel_pid, uid)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "process.seteuid" => {
            let uid = javascript_sync_rpc_arg_u32(&request.args, 0, "seteuid uid")?;
            kernel
                .seteuid(EXECUTION_DRIVER_NAME, process.kernel_pid, uid)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "process.setreuid" => {
            let uid = javascript_sync_rpc_arg_u32_optional(&request.args, 0, "setreuid uid")?;
            let euid = javascript_sync_rpc_arg_u32_optional(&request.args, 1, "setreuid euid")?;
            kernel
                .setreuid(EXECUTION_DRIVER_NAME, process.kernel_pid, uid, euid)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "process.setresuid" => {
            let uid = javascript_sync_rpc_arg_u32_optional(&request.args, 0, "setresuid uid")?;
            let euid = javascript_sync_rpc_arg_u32_optional(&request.args, 1, "setresuid euid")?;
            let suid = javascript_sync_rpc_arg_u32_optional(&request.args, 2, "setresuid suid")?;
            kernel
                .setresuid(EXECUTION_DRIVER_NAME, process.kernel_pid, uid, euid, suid)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "process.setgid" => {
            let gid = javascript_sync_rpc_arg_u32(&request.args, 0, "setgid gid")?;
            kernel
                .setgid(EXECUTION_DRIVER_NAME, process.kernel_pid, gid)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "process.setegid" => {
            let gid = javascript_sync_rpc_arg_u32(&request.args, 0, "setegid gid")?;
            kernel
                .setegid(EXECUTION_DRIVER_NAME, process.kernel_pid, gid)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "process.setregid" => {
            let gid = javascript_sync_rpc_arg_u32_optional(&request.args, 0, "setregid gid")?;
            let egid = javascript_sync_rpc_arg_u32_optional(&request.args, 1, "setregid egid")?;
            kernel
                .setregid(EXECUTION_DRIVER_NAME, process.kernel_pid, gid, egid)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "process.setresgid" => {
            let gid = javascript_sync_rpc_arg_u32_optional(&request.args, 0, "setresgid gid")?;
            let egid = javascript_sync_rpc_arg_u32_optional(&request.args, 1, "setresgid egid")?;
            let sgid = javascript_sync_rpc_arg_u32_optional(&request.args, 2, "setresgid sgid")?;
            kernel
                .setresgid(EXECUTION_DRIVER_NAME, process.kernel_pid, gid, egid, sgid)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "process.setgroups" => {
            let groups = request
                .args
                .first()
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    SidecarError::InvalidState(
                        "process setgroups requires an array argument".into(),
                    )
                })?
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    let raw = value.as_u64().ok_or_else(|| {
                        SidecarError::InvalidState(format!(
                            "process setgroups entry {index} must be a non-negative integer"
                        ))
                    })?;
                    u32::try_from(raw).map_err(|_| {
                        SidecarError::InvalidState(format!(
                            "process setgroups entry {index} exceeds u32"
                        ))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            kernel
                .setgroups(EXECUTION_DRIVER_NAME, process.kernel_pid, groups)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "process.getpgid" => {
            let requested_pid =
                javascript_sync_rpc_arg_u32(&request.args, 0, "process getpgid pid")?;
            let target_pid = if requested_pid == 0 {
                process.kernel_pid
            } else {
                requested_pid
            };
            kernel
                .getpgid(EXECUTION_DRIVER_NAME, target_pid)
                .map(|pgid| json!(pgid))
                .map_err(kernel_error)
        }
        "process.setpgid" => {
            let requested_pid =
                javascript_sync_rpc_arg_u32(&request.args, 0, "process setpgid pid")?;
            let pgid =
                javascript_sync_rpc_arg_u32(&request.args, 1, "process setpgid process group")?;
            let target_pid = if requested_pid == 0 {
                process.kernel_pid
            } else {
                requested_pid
            };
            kernel
                .setpgid(EXECUTION_DRIVER_NAME, target_pid, pgid)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        _ => service_javascript_fs_sync_rpc(kernel, process, process.kernel_pid, request),
    }?;
    Ok(response.into())
}

fn service_javascript_internal_bridge_sync_rpc(
    process: &ActiveProcess,
    request: &HostRpcRequest,
) -> Result<Value, SidecarError> {
    // Module resolution / loading / format now reads the kernel VFS via
    // `service_javascript_module_sync_rpc`. This host-context path only handles
    // polyfills, which are static guest expressions independent of the FS.
    let method = match request.method.as_str() {
        "_loadPolyfill" | "__load_polyfill" => "_loadPolyfill",
        other => {
            return Err(SidecarError::InvalidState(format!(
                "unsupported JavaScript internal bridge method {other}"
            )));
        }
    };

    handle_internal_bridge_call_from_host_context(
        &process.host_cwd,
        &process.guest_cwd,
        &process.env,
        method,
        &request.args,
    )
    .map_err(SidecarError::Host)?
    .ok_or_else(|| {
        SidecarError::InvalidState(format!(
            "JavaScript internal bridge method {method} returned no value"
        ))
    })
}

const JAVASCRIPT_NET_POLL_MAX_WAIT: Duration = Duration::from_millis(50);
pub(in crate::execution) const EXITED_PROCESS_SNAPSHOT_RETENTION: Duration = Duration::from_secs(2);

pub(in crate::execution) fn resolve_http2_file_response_guest_path(
    process: &ActiveProcess,
    path: &str,
) -> String {
    if Path::new(path).is_absolute() {
        normalize_path(path)
    } else {
        normalize_path(&format!("{}/{}", process.guest_cwd, path))
    }
}

pub(crate) fn clamp_javascript_net_poll_wait(wait_ms: u64) -> Duration {
    // WASM net.poll runs on the sidecar's sync-RPC main thread. Guest-controlled waits
    // must stay bounded so one VM cannot stall dispose/shutdown or unrelated VM work.
    if wait_ms == 0 {
        Duration::ZERO
    } else {
        Duration::from_millis(wait_ms).min(JAVASCRIPT_NET_POLL_MAX_WAIT)
    }
}

fn service_javascript_tls_deferred_rpc(
    vm_id: &str,
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    request: &HostRpcRequest,
    capabilities: &CapabilityRegistry,
) -> Result<Option<HostServiceResponse>, SidecarError> {
    let operation_deadline = reactor_io_limits(&process.limits).operation_deadline;
    let deferred = |receiver| HostServiceResponse::Deferred {
        receiver,
        timeout: Some(operation_deadline),
        task_class: agentos_runtime::TaskClass::Tls,
    };
    match request.method.as_str() {
        "net.socket_upgrade_tls" => {
            let socket_id =
                javascript_sync_rpc_arg_str(&request.args, 0, "net.socket_upgrade_tls socket id")?;
            let options_json =
                javascript_sync_rpc_arg_str(&request.args, 1, "net.socket_upgrade_tls options")?;
            let options: TlsBridgeOptions =
                serde_json::from_str(options_json).map_err(|error| {
                    SidecarError::InvalidState(format!(
                        "net.socket_upgrade_tls options must be valid JSON: {error}"
                    ))
                })?;
            if process
                .capability_leases
                .contains_key(&NativeCapabilityKey::TlsSocket(socket_id.to_owned()))
            {
                return Err(SidecarError::host(
                    "EALREADY",
                    format!("TCP socket {socket_id} is already upgraded to TLS"),
                ));
            }
            let pending = reserve_capability(capabilities, CapabilityKind::TlsTransport)?;
            let socket = process.tcp_sockets.get(socket_id).ok_or_else(|| {
                SidecarError::InvalidState(format!(
                    "unknown TCP socket {socket_id} for TLS upgrade"
                ))
            })?;
            let receiver = socket.upgrade_tls(vm_id, kernel, process.kernel_pid, options)?;
            let kernel_socket_id = socket.kernel_socket_id;
            commit_process_capability(
                process,
                pending,
                NativeCapabilityKey::TlsSocket(socket_id.to_owned()),
                format!("tls-{socket_id}"),
                kernel_socket_id,
            )?;
            Ok(Some(deferred(receiver)))
        }
        "net.upgrade_socket_write" | "net.write" => {
            let socket_id =
                javascript_sync_rpc_arg_str(&request.args, 0, "deferred TLS write socket id")?;
            let Some(socket) = process.tcp_sockets.get(socket_id) else {
                return Ok(None);
            };
            if !socket.tls_mode.load(Ordering::SeqCst) {
                return Ok(None);
            }
            let chunk = if request.method == "net.upgrade_socket_write" {
                javascript_sync_rpc_base64_arg(&request.args, 1, "net.upgrade_socket_write chunk")?
            } else if let Some(bytes) = request.raw_bytes_args.get(&1) {
                bytes.clone()
            } else {
                javascript_sync_rpc_bytes_arg(&request.args, 1, "net.write chunk")?
            };
            Ok(Some(deferred(socket.begin_tls_write(&chunk)?)))
        }
        "net.upgrade_socket_end" | "net.shutdown" => {
            let socket_id =
                javascript_sync_rpc_arg_str(&request.args, 0, "deferred TLS shutdown socket id")?;
            let Some(socket) = process.tcp_sockets.get(socket_id) else {
                return Ok(None);
            };
            if !socket.tls_mode.load(Ordering::SeqCst) {
                return Ok(None);
            }
            Ok(Some(deferred(socket.begin_tls_shutdown()?)))
        }
        _ => Ok(None),
    }
}

fn service_javascript_plain_socket_deferred_rpc(
    process: &ActiveProcess,
    request: &HostRpcRequest,
) -> Result<Option<HostServiceResponse>, SidecarError> {
    let deferred = |receiver| HostServiceResponse::Deferred {
        receiver,
        timeout: None,
        task_class: agentos_runtime::TaskClass::Socket,
    };
    match request.method.as_str() {
        "net.write" => {
            let socket_id = javascript_sync_rpc_arg_str(&request.args, 0, "net.write socket id")?;
            let chunk = if let Some(bytes) = request.raw_bytes_args.get(&1) {
                bytes.clone()
            } else {
                javascript_sync_rpc_bytes_arg(&request.args, 1, "net.write chunk")?
            };
            if let Some(socket) = process.tcp_sockets.get(socket_id) {
                if socket.kernel_socket_id.is_some() {
                    return Ok(None);
                }
                return Ok(Some(deferred(socket.begin_plain_write(&chunk)?)));
            }
            let socket = process.unix_sockets.get(socket_id).ok_or_else(|| {
                SidecarError::InvalidState(format!("unknown net socket {socket_id} for net.write"))
            })?;
            Ok(Some(deferred(socket.begin_plain_write(&chunk)?)))
        }
        "net.shutdown" => {
            let socket_id =
                javascript_sync_rpc_arg_str(&request.args, 0, "net.shutdown socket id")?;
            if let Some(socket) = process.tcp_sockets.get(socket_id) {
                if socket.kernel_socket_id.is_some() {
                    return Ok(None);
                }
                return Ok(Some(deferred(socket.begin_plain_shutdown()?)));
            }
            let socket = process.unix_sockets.get(socket_id).ok_or_else(|| {
                SidecarError::InvalidState(format!(
                    "unknown net socket {socket_id} for net.shutdown"
                ))
            })?;
            Ok(Some(deferred(socket.begin_plain_shutdown()?)))
        }
        _ => Ok(None),
    }
}

pub(in crate::execution) fn service_javascript_net_sync_rpc_response<B>(
    request: NetServiceRequest<'_, B>,
) -> Result<HostServiceResponse, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    if request.sync_request.method == "net.server_close" {
        let listener_id = javascript_sync_rpc_arg_str(
            &request.sync_request.args,
            0,
            "net.server_close listener id",
        )?
        .to_owned();
        if let Some(listener) = request.process.tcp_listeners.remove(&listener_id) {
            release_tcp_listener_handle(
                request.process,
                &listener_id,
                listener,
                request.kernel,
                &request.kernel_readiness,
            )?;
            return Ok(HostServiceResponse::Json(Value::Null));
        }

        let listener = request
            .process
            .unix_listeners
            .remove(&listener_id)
            .ok_or_else(|| {
                SidecarError::InvalidState(format!("unknown net listener {listener_id}"))
            })?;
        release_unix_listener_capability(request.process, &listener_id, &listener)?;
        if !listener.is_final_description_handle() {
            return Ok(HostServiceResponse::Json(Value::Null));
        }
        for socket in request
            .process
            .unix_sockets
            .values_mut()
            .filter(|socket| socket.listener_id.as_deref() == Some(listener_id.as_str()))
        {
            socket.cache_remote_peer_metadata(&request.socket_paths.unix_bound_addresses)?;
        }
        close_pending_guest_unix_connections(
            &request.socket_paths.unix_bound_addresses,
            &listener.registry_binding_id,
        )?;
        release_guest_unix_binding(
            &request.socket_paths.unix_bound_addresses,
            &listener.registry_binding_id,
        )?;
        purge_guest_unix_target(
            &request.socket_paths.unix_bound_addresses,
            &listener.registry_binding_id,
        )?;
        let unlink_node_path = request
            .sync_request
            .args
            .get(1)
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if unlink_node_path {
            if let Some(path) = listener.guest_node_path.as_deref() {
                match request.kernel.remove_file(path) {
                    Ok(()) => {}
                    Err(error) if error.code() == "ENOENT" => {}
                    Err(error) => return Err(kernel_error(error)),
                }
            }
        }
        let close_completion = listener.close();

        let operation_deadline = reactor_io_limits(&request.process.limits).operation_deadline;
        let (respond_to, receiver) = tokio::sync::oneshot::channel();
        request
            .process
            .runtime_context
            .spawn(agentos_runtime::TaskClass::Listener, async move {
                let result = match crate::execution::operation_deadline_timeout(
                    "JavaScript Unix listener close",
                    operation_deadline,
                    close_completion,
                )
                .await
                {
                    Ok(Ok(())) => Ok(Value::Null),
                    Ok(Err(_)) => Err(crate::state::DeferredRpcError {
                        code: String::from("ERR_AGENTOS_LISTENER_CLOSE"),
                        message: format!(
                            "Unix listener {listener_id} close task ended without acknowledgement"
                        ),
                        details: None,
                    }),
                    Err(_) => Err(crate::state::DeferredRpcError {
                        code: String::from("ETIMEDOUT"),
                        message: format!(
                            "Unix listener {listener_id} close exceeded {}ms; raise limits.reactor.operationDeadlineMs",
                            operation_deadline.as_millis()
                        ),
                        details: None,
                    }),
                };
                if respond_to.send(result).is_err() {
                    eprintln!(
                        "ERR_AGENTOS_LISTENER_CLOSE_COMPLETION_DROPPED: caller stopped waiting for Unix listener {listener_id}"
                    );
                }
            })
            .map_err(SidecarError::from)?;
        return Ok(HostServiceResponse::Deferred {
            receiver,
            timeout: None,
            task_class: agentos_runtime::TaskClass::Listener,
        });
    }
    if request.sync_request.method == "net.connect" {
        let payload = request
            .sync_request
            .args
            .first()
            .cloned()
            .ok_or_else(|| {
                SidecarError::InvalidState(String::from("net.connect requires a request payload"))
            })
            .and_then(|value| {
                serde_json::from_value::<JavascriptNetConnectRequest>(value).map_err(|error| {
                    SidecarError::InvalidState(format!("invalid net.connect payload: {error}"))
                })
            })?;
        if payload.path.is_some() || payload.abstract_path_hex.is_some() {
            if payload.path.is_some() && payload.abstract_path_hex.is_some() {
                return Err(SidecarError::InvalidState(String::from(
                    "net.connect accepts either path or abstractPathHex, not both",
                )));
            }
            request.bridge.require_network_access(
                request.vm_id,
                NetworkOperation::Http,
                format_unix_socket_resource(
                    payload.path.as_deref(),
                    payload.abstract_path_hex.as_deref(),
                    false,
                ),
            )?;
            let (target, target_binding_id, remote_address) = if let Some(hex) =
                payload.abstract_path_hex.as_deref()
            {
                let guest_name = decode_abstract_unix_name(hex)?;
                let host_name = host_abstract_unix_name(request.socket_paths, &guest_name);
                let target = guest_unix_binding_for_host_key(
                    &request.socket_paths.unix_bound_addresses,
                    &abstract_unix_host_address_key(&host_name),
                )?
                .ok_or_else(|| {
                    sidecar_net_error(std::io::Error::from_raw_os_error(libc::ECONNREFUSED))
                })?;
                (
                    NativeUnixConnectTarget::Abstract(host_name.to_vec()),
                    target.0,
                    target.1,
                )
            } else {
                let path = payload.path.as_deref().expect("validated Unix path");
                let (candidate_path, _) = resolve_guest_unix_path(request.process, path)?;
                reject_host_mounted_unix_socket_path(request.socket_paths, &candidate_path)?;
                let node = request
                    .kernel
                    .resolve_unix_socket_connect_target_for_process(
                        EXECUTION_DRIVER_NAME,
                        request.process.kernel_pid,
                        &request.process.guest_cwd,
                        path,
                    )
                    .map_err(kernel_error)?;
                reject_host_mounted_unix_socket_path(request.socket_paths, &node.canonical_path)?;
                let (host_path, binding_id, address) =
                    guest_unix_path_target(request.socket_paths, (node.stat.dev, node.stat.ino))?
                        .ok_or_else(|| {
                        sidecar_net_error(std::io::Error::from_raw_os_error(libc::ECONNREFUSED))
                    })?;
                (
                    NativeUnixConnectTarget::Path(host_path),
                    binding_id,
                    address,
                )
            };
            let pending = reserve_capability(&request.capabilities, CapabilityKind::UnixSocket)?;
            let bound_listener = if let Some(listener_id) = payload.bound_server_id.as_deref() {
                let listener = request
                    .process
                    .unix_listeners
                    .remove(listener_id)
                    .ok_or_else(|| {
                        SidecarError::InvalidState(format!(
                            "unknown bound Unix socket {listener_id}"
                        ))
                    })?;
                if listener.acceptor_started || listener.bound_socket.is_none() {
                    request
                        .process
                        .unix_listeners
                        .insert(listener_id.to_owned(), listener);
                    return Err(sidecar_net_error(std::io::Error::from_raw_os_error(
                        libc::EINVAL,
                    )));
                }
                Some((listener_id.to_owned(), listener))
            } else {
                None
            };
            return defer_native_unix_connect(
                request.process,
                request.sync_request.id,
                pending,
                target,
                remote_address,
                Arc::clone(&request.socket_paths.unix_bound_addresses),
                target_binding_id,
                bound_listener,
            );
        }

        let port = payload.port.ok_or_else(|| {
            SidecarError::InvalidState(String::from("net.connect requires either a path or port"))
        })?;
        let host = payload.host.as_deref().unwrap_or("localhost");
        let is_http_loopback_target = is_loopback_socket_host(host)
            && [SocketFamily::Ipv4, SocketFamily::Ipv6]
                .iter()
                .any(|family| {
                    let family_number = match family {
                        SocketFamily::Ipv4 => 4,
                        SocketFamily::Ipv6 => 6,
                    };
                    if payload
                        .family
                        .is_some_and(|requested| requested != family_number)
                    {
                        return false;
                    }
                    request
                        .socket_paths
                        .http_loopback_target(*family, port)
                        .is_some()
                });
        if !is_http_loopback_target {
            request.bridge.require_network_access(
                request.vm_id,
                NetworkOperation::Http,
                format_tcp_resource(host, port),
            )?;
            let resolved = resolve_tcp_connect_addr(
                request.bridge,
                request.kernel,
                request.vm_id,
                request.dns,
                host,
                port,
                payload.family,
                request.socket_paths,
            )?;
            if !resolved.use_kernel_loopback {
                let pending = reserve_capability(&request.capabilities, CapabilityKind::TcpSocket)?;
                return defer_native_tcp_connect(
                    request.process,
                    request.sync_request.id,
                    pending,
                    resolved,
                    payload.local_reservation,
                );
            }
        }
    }
    if let Some(response) = service_javascript_tls_deferred_rpc(
        request.vm_id,
        request.kernel,
        request.process,
        request.sync_request,
        &request.capabilities,
    )? {
        return Ok(response);
    }
    if let Some(response) =
        service_javascript_plain_socket_deferred_rpc(request.process, request.sync_request)?
    {
        return Ok(response);
    }
    if request.sync_request.method == "net.http_wait" {
        let server_id =
            javascript_sync_rpc_arg_u64(&request.sync_request.args, 0, "net.http_wait server id")?;
        let server = request
            .process
            .http_servers
            .get(&server_id)
            .ok_or_else(|| {
                SidecarError::InvalidState(format!("unknown HTTP server {server_id}"))
            })?;
        let closed = Arc::clone(&server.closed);
        let close_notify = Arc::clone(&server.close_notify);
        let (respond_to, receiver) = tokio::sync::oneshot::channel();
        request
            .process
            .runtime_context
            .spawn(agentos_runtime::TaskClass::Listener, async move {
                let notified = close_notify.notified();
                if !closed.load(Ordering::Acquire) {
                    notified.await;
                }
                respond_to.settle(Ok(json!({
                    "kind": "serverClose",
                    "id": server_id,
                })));
            })
            .map_err(SidecarError::from)?;
        return Ok(HostServiceResponse::Deferred {
            receiver,
            timeout: None,
            task_class: agentos_runtime::TaskClass::Listener,
        });
    }
    if request.sync_request.method == "net.poll" {
        let NetServiceRequest {
            kernel,
            kernel_readiness,
            process,
            socket_paths,
            sync_request: request,
            ..
        } = request;
        let socket_id = javascript_sync_rpc_arg_str(&request.args, 0, "net.poll socket id")?;
        let wait_ms = javascript_sync_rpc_arg_u64_optional(&request.args, 1, "net.poll wait ms")?
            .unwrap_or_default();
        let trace_enabled = net_tcp_trace_enabled(&process.env);
        let event = if let Some(socket) = process.tcp_sockets.get_mut(socket_id) {
            socket.set_application_read_interest(true)?;
            socket.poll(
                kernel,
                process.kernel_pid,
                clamp_javascript_net_poll_wait(wait_ms),
                trace_enabled,
            )?
        } else if let Some(socket) = process.unix_sockets.get_mut(socket_id) {
            socket.set_application_read_interest(true)?;
            socket.poll(clamp_javascript_net_poll_wait(wait_ms))?
        } else {
            return Err(SidecarError::host(
                "EBADF",
                format!("unknown net socket {socket_id}"),
            ));
        };
        return match event {
            Some(TcpSocketEvent::Data {
                bytes,
                reservation,
                mut source_reservations,
            }) => {
                source_reservations.push(reservation);
                Ok(HostServiceResponse::SourceBackedJson {
                    value: json!({
                        "type": "data",
                        "data": host_bytes_value(&bytes),
                    }),
                    source_reservations,
                })
            }
            Some(TcpSocketEvent::End) => Ok(json!({ "type": "end" }).into()),
            Some(TcpSocketEvent::Error { code, message }) => Ok(json!({
                "type": "error",
                "code": code,
                "message": message,
            })
            .into()),
            Some(TcpSocketEvent::Close { had_error }) => {
                if let Some(socket) = process.tcp_sockets.remove(socket_id) {
                    release_tcp_socket_handle(
                        process,
                        socket_id,
                        socket,
                        kernel,
                        &kernel_readiness,
                    );
                } else if let Some(socket) = process.unix_sockets.remove(socket_id) {
                    release_unix_socket_handle(
                        process,
                        socket_id,
                        socket,
                        &socket_paths.unix_bound_addresses,
                    );
                }
                Ok(json!({ "type": "close", "hadError": had_error }).into())
            }
            None => Ok(Value::Null.into()),
        };
    }
    if request.sync_request.method != "net.socket_read" {
        return service_javascript_net_sync_rpc(request).map(Into::into);
    }

    let NetServiceRequest {
        kernel,
        process,
        sync_request: request,
        ..
    } = request;
    let trace_enabled = net_tcp_trace_enabled(&process.env);
    let socket_id = javascript_sync_rpc_arg_str(&request.args, 0, "net.socket_read socket id")?;
    let max_bytes = javascript_sync_rpc_arg_u64_optional(
        &request.args,
        1,
        "net.socket_read maximum byte count",
    )?
    .map(|value| usize::try_from(value).unwrap_or(usize::MAX))
    .unwrap_or(64 * 1024);
    if trace_enabled {
        NET_TCP_TRACE_COUNTERS
            .socket_read_calls
            .fetch_add(1, Ordering::Relaxed);
        NET_TCP_TRACE_COUNTERS
            .socket_read_zero_wait_calls
            .fetch_add(1, Ordering::Relaxed);
    }

    let event = if let Some(socket) = process.tcp_sockets.get_mut(socket_id) {
        socket.set_application_read_interest(true)?;
        socket.poll_limited(
            kernel,
            process.kernel_pid,
            Duration::ZERO,
            trace_enabled,
            max_bytes,
        )?
    } else {
        let socket = process
            .unix_sockets
            .get_mut(socket_id)
            .ok_or_else(|| SidecarError::InvalidState(format!("unknown net socket {socket_id}")))?;
        socket.set_application_read_interest(true)?;
        socket.poll_limited(Duration::ZERO, max_bytes)?
    };

    match event {
        Some(TcpSocketEvent::Data {
            bytes,
            reservation,
            mut source_reservations,
        }) => {
            // The bridge registry already reserved this call's declared
            // response maximum before host visibility. Keep the transport
            // ownership live through handoff, but do not charge the response
            // bytes a second time.
            source_reservations.push(reservation);
            Ok(HostServiceResponse::SourceBackedRaw {
                payload: bytes,
                source_reservations,
            })
        }
        other => net_read_value(other).map(Into::into),
    }
}

async fn service_javascript_dgram_poll_response(
    socket_paths: &SocketPathContext,
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    request: &HostRpcRequest,
) -> Result<HostServiceResponse, SidecarError> {
    let socket_id = javascript_sync_rpc_arg_str(&request.args, 0, "dgram.poll socket id")?;
    let wait_ms = javascript_sync_rpc_arg_u64_optional(&request.args, 1, "dgram.poll wait ms")?
        .unwrap_or_default();
    let event = process
        .udp_sockets
        .get(socket_id)
        .ok_or_else(|| SidecarError::InvalidState(format!("unknown UDP socket {socket_id}")))?
        .poll(kernel, process.kernel_pid, Duration::from_millis(wait_ms))
        .await?;

    match event {
        Some(DatagramEvent::Message {
            data,
            remote_addr,
            _byte_reservation,
            _datagram_reservation,
            _udp_byte_reservation,
            _udp_datagram_reservation,
        }) => {
            let family = SocketFamily::from_ip(remote_addr.ip());
            let guest_remote_port = if is_loopback_ip(remote_addr.ip()) {
                socket_paths
                    .guest_udp_port_for_host_port(family, remote_addr.port())
                    .unwrap_or(remote_addr.port())
            } else {
                remote_addr.port()
            };
            // The bridge registry owns the declared response budget. These
            // source reservations only keep the datagram storage charged until
            // the encoded response has crossed into the V8 completion target.
            let mut response = remote_endpoint_value(&remote_addr, guest_remote_port);
            if let Value::Object(fields) = &mut response {
                fields.insert(String::from("type"), Value::String(String::from("message")));
                fields.insert(String::from("data"), host_bytes_value(&data));
            }
            Ok(HostServiceResponse::SourceBackedJson {
                value: response,
                source_reservations: vec![
                    _byte_reservation,
                    _datagram_reservation,
                    _udp_byte_reservation,
                    _udp_datagram_reservation,
                ],
            })
        }
        Some(DatagramEvent::Error { code, message }) => Ok(HostServiceResponse::Json(json!({
            "type": "error",
            "code": code,
            "message": message,
        }))),
        None => Ok(HostServiceResponse::Json(Value::Null)),
    }
}

pub(crate) fn service_javascript_net_sync_rpc<B>(
    request: NetServiceRequest<'_, B>,
) -> Result<Value, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let NetServiceRequest {
        bridge,
        vm_id,
        dns,
        socket_paths,
        kernel,
        kernel_readiness,
        process,
        sync_request: request,
        capabilities,
    } = request;
    let trace_enabled = net_tcp_trace_enabled(&process.env);
    match request.method.as_str() {
        "net.http_listen" => {
            let pending = reserve_capability(&capabilities, CapabilityKind::TcpListener)?;
            let payload_json =
                javascript_sync_rpc_arg_str(&request.args, 0, "net.http_listen payload")?;
            let payload: JavascriptHttpListenRequest =
                serde_json::from_str(payload_json).map_err(|error| {
                    SidecarError::InvalidState(format!(
                        "net.http_listen payload must be valid JSON: {error}"
                    ))
                })?;
            let (family, bind_host, guest_host) =
                normalize_tcp_listen_host(payload.hostname.as_deref())?;
            let requested_port = payload.port.unwrap_or(0);
            bridge.require_network_access(
                vm_id,
                NetworkOperation::Listen,
                format_tcp_resource(bind_host, requested_port),
            )?;
            let port = allocate_guest_listen_port(
                requested_port,
                family,
                &socket_paths.used_tcp_guest_ports,
                socket_paths.listen_policy,
            )?;
            let mut listener =
                ActiveTcpListener::bind(bind_host, guest_host, port, Some(DEFAULT_NET_BACKLOG))?;
            let guest_local_addr = listener.guest_local_addr();
            commit_process_capability(
                process,
                pending,
                NativeCapabilityKey::HttpServer(payload.server_id),
                format!("http-server-{}", payload.server_id),
                None,
            )?;
            process.http_servers.insert(
                payload.server_id,
                ActiveHttpServer {
                    listener: listener.listener.take().ok_or_else(|| {
                        SidecarError::InvalidState(String::from(
                            "HTTP listener missing host TCP socket",
                        ))
                    })?,
                    guest_local_addr,
                    next_request_id: 0,
                    closed: Arc::new(AtomicBool::new(false)),
                    close_notify: Arc::new(tokio::sync::Notify::new()),
                },
            );
            serde_json::to_string(&json!({
                "address": socket_address_value(&guest_local_addr)
            }))
            .map(Value::String)
            .map_err(|error| SidecarError::host("ERR_AGENTOS_NODE_SYNC_RPC", format!("{error}")))
        }
        "net.http_close" => {
            let server_id =
                javascript_sync_rpc_arg_u64(&request.args, 0, "net.http_close server id")?;
            let server = process.http_servers.remove(&server_id).ok_or_else(|| {
                SidecarError::InvalidState(format!("unknown HTTP server {server_id}"))
            })?;
            server.closed.store(true, Ordering::Release);
            server.close_notify.notify_waiters();
            drop(server.listener);
            process.release_capability(&NativeCapabilityKey::HttpServer(server_id))?;
            process
                .pending_http_requests
                .retain(|(pending_server_id, _), _| *pending_server_id != server_id);
            Ok(Value::Null)
        }
        "net.http_wait" => unreachable!("net.http_wait is deferred by the response wrapper"),
        "net.http_respond" => {
            let server_id =
                javascript_sync_rpc_arg_u64(&request.args, 0, "net.http_respond server id")?;
            let request_id =
                javascript_sync_rpc_arg_u64(&request.args, 1, "net.http_respond request id")?;
            let response_json =
                javascript_sync_rpc_arg_str(&request.args, 2, "net.http_respond payload")?;
            ensure_vm_fetch_response_within_limit(
                response_json,
                "net.http_respond",
                VM_FETCH_BUFFER_LIMIT_BYTES,
            )
            .map_err(sidecar_core_execution_error)?;
            serde_json::from_str::<Value>(response_json).map_err(|error| {
                SidecarError::Execution(format!(
                    "net.http_respond payload must be valid JSON: {error}"
                ))
            })?;
            complete_loopback_http_request(
                process,
                (server_id, request_id),
                response_json.to_owned(),
            )?;
            Ok(Value::Null)
        }
        "net.bind_unix" => {
            let payload = request
                .args
                .first()
                .cloned()
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "net.bind_unix requires a request payload",
                    ))
                })
                .and_then(|value| {
                    serde_json::from_value::<JavascriptNetListenRequest>(value).map_err(|error| {
                        SidecarError::InvalidState(format!(
                            "invalid net.bind_unix payload: {error}"
                        ))
                    })
                })?;
            let address_kinds = usize::from(payload.path.is_some())
                + usize::from(payload.abstract_path_hex.is_some())
                + usize::from(payload.autobind);
            if address_kinds != 1 || payload.bound_server_id.is_some() {
                return Err(SidecarError::InvalidState(String::from(
                    "net.bind_unix requires exactly one Unix address",
                )));
            }
            bridge.require_network_access(
                vm_id,
                NetworkOperation::Listen,
                format_unix_socket_resource(
                    payload.path.as_deref(),
                    payload.abstract_path_hex.as_deref(),
                    payload.autobind,
                ),
            )?;

            let pending = reserve_capability(&capabilities, CapabilityKind::UnixListener)?;
            let listener_id = process.allocate_unix_listener_id();
            let registry_binding_id = guest_unix_binding_id(process.kernel_pid, &listener_id);
            let mut listener = if payload.autobind {
                let mut bound = None;
                for nonce in 0..4096 {
                    let guest_name =
                        guest_autobind_unix_name(process.kernel_pid, &listener_id, nonce);
                    let host_name = host_abstract_unix_name(socket_paths, &guest_name);
                    register_guest_unix_binding(
                        &socket_paths.unix_bound_addresses,
                        &registry_binding_id,
                        &abstract_unix_host_address_key(&host_name),
                        GuestUnixAddress {
                            path: abstract_unix_node_path(&guest_name),
                            abstract_path_hex: Some(abstract_unix_name_hex(&guest_name)),
                        },
                        None,
                        None,
                    )?;
                    match ActiveUnixListener::bind_abstract_unlistened(
                        &host_name,
                        &guest_name,
                        registry_binding_id.clone(),
                        process.runtime_context.clone(),
                    ) {
                        Ok(listener) => {
                            bound = Some(listener);
                            break;
                        }
                        Err(error) => {
                            rollback_guest_unix_binding(
                                &socket_paths.unix_bound_addresses,
                                &registry_binding_id,
                            )?;
                            if guest_error_code(&error) != Some("EADDRINUSE") {
                                return Err(error);
                            }
                        }
                    }
                }
                bound.ok_or_else(|| {
                    SidecarError::host(
                        "EADDRINUSE",
                        String::from(
                            "Linux AF_UNIX autobind namespace exhausted after 4096 attempts",
                        ),
                    )
                })?
            } else if let Some(hex) = payload.abstract_path_hex.as_deref() {
                let guest_name = decode_abstract_unix_name(hex)?;
                let host_name = host_abstract_unix_name(socket_paths, &guest_name);
                register_guest_unix_binding(
                    &socket_paths.unix_bound_addresses,
                    &registry_binding_id,
                    &abstract_unix_host_address_key(&host_name),
                    GuestUnixAddress {
                        path: abstract_unix_node_path(&guest_name),
                        abstract_path_hex: Some(abstract_unix_name_hex(&guest_name)),
                    },
                    None,
                    None,
                )?;
                match ActiveUnixListener::bind_abstract_unlistened(
                    &host_name,
                    &guest_name,
                    registry_binding_id.clone(),
                    process.runtime_context.clone(),
                ) {
                    Ok(listener) => listener,
                    Err(error) => {
                        rollback_guest_unix_binding(
                            &socket_paths.unix_bound_addresses,
                            &registry_binding_id,
                        )?;
                        return Err(error);
                    }
                }
            } else {
                let path = payload.path.as_deref().expect("validated Unix path");
                let (candidate_path, reported_path) = resolve_guest_unix_path(process, path)?;
                reject_host_mounted_unix_socket_path(socket_paths, &candidate_path)?;
                let canonical_candidate = kernel
                    .resolve_unix_socket_bind_target_for_process(
                        EXECUTION_DRIVER_NAME,
                        process.kernel_pid,
                        &process.guest_cwd,
                        path,
                    )
                    .map_err(kernel_error)?;
                reject_host_mounted_unix_socket_path(socket_paths, &canonical_candidate)?;
                let node = kernel
                    .bind_unix_socket_path_for_process(
                        EXECUTION_DRIVER_NAME,
                        process.kernel_pid,
                        &process.guest_cwd,
                        path,
                    )
                    .map_err(kernel_error)?;
                let guest_path = node.canonical_path;
                let host_path = allocate_guest_socket_host_path(
                    socket_paths,
                    process.kernel_pid,
                    &listener_id,
                    &guest_path,
                );
                if let Err(error) = register_guest_unix_binding(
                    &socket_paths.unix_bound_addresses,
                    &registry_binding_id,
                    &pathname_unix_host_address_key(&host_path),
                    GuestUnixAddress {
                        path: reported_path.clone(),
                        abstract_path_hex: None,
                    },
                    Some((node.stat.dev, node.stat.ino)),
                    Some(host_path.clone()),
                ) {
                    if let Err(rollback_error) = kernel.remove_file(&guest_path) {
                        return Err(SidecarError::Execution(format!(
                            "{error}; failed to roll back Unix socket node {guest_path}: {}",
                            kernel_error(rollback_error)
                        )));
                    }
                    return Err(error);
                }
                match ActiveUnixListener::bind_unlistened(
                    &host_path,
                    &reported_path,
                    registry_binding_id.clone(),
                    process.runtime_context.clone(),
                ) {
                    Ok(mut listener) => {
                        listener.guest_node_path = Some(guest_path);
                        listener
                    }
                    Err(error) => {
                        rollback_guest_unix_path_binding(
                            &socket_paths.unix_bound_addresses,
                            &registry_binding_id,
                            kernel,
                            &guest_path,
                            &host_path,
                        )?;
                        return Err(error);
                    }
                }
            };
            listener
                .registry_binding_id
                .clone_from(&registry_binding_id);
            let local_path = listener.path.clone();
            let local_abstract_path_hex = listener.abstract_path_hex.clone();
            let capability_key = NativeCapabilityKey::UnixListener(listener_id.clone());
            let identity = commit_process_capability(
                process,
                pending,
                capability_key.clone(),
                listener_id.clone(),
                None,
            )?;
            listener.retain_description_lease(
                process
                    .shared_capability_lease(&capability_key)
                    .expect("committed Unix listener capability lease"),
            );
            listener.set_event_pusher(
                process
                    .execution
                    .execution_wake_handle(process.kernel_handle.runtime_identity()),
                Some(identity),
                Arc::clone(&process.process_event_notify),
            );
            process.unix_listeners.insert(listener_id.clone(), listener);
            Ok(json!({
                "serverId": listener_id,
                "capabilityId": identity.0,
                "capabilityGeneration": identity.1,
                "localPath": local_path,
                "localAbstractPathHex": local_abstract_path_hex,
            }))
        }
        "net.bind_connected_unix" => {
            let payload = request
                .args
                .first()
                .cloned()
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "net.bind_connected_unix requires a request payload",
                    ))
                })
                .and_then(|value| {
                    serde_json::from_value::<JavascriptNetBindConnectedUnixRequest>(value).map_err(
                        |error| {
                            SidecarError::InvalidState(format!(
                                "invalid net.bind_connected_unix payload: {error}"
                            ))
                        },
                    )
                })?;
            if usize::from(payload.path.is_some())
                + usize::from(payload.abstract_path_hex.is_some())
                + usize::from(payload.autobind)
                != 1
            {
                return Err(SidecarError::InvalidState(String::from(
                    "net.bind_connected_unix requires exactly one Unix address",
                )));
            }
            bridge.require_network_access(
                vm_id,
                NetworkOperation::Listen,
                format_unix_socket_resource(
                    payload.path.as_deref(),
                    payload.abstract_path_hex.as_deref(),
                    payload.autobind,
                ),
            )?;
            let binding_id = guest_unix_binding_id(
                process.kernel_pid,
                &format!("connected:{}", payload.socket_id),
            );
            let socket = process
                .unix_sockets
                .get(&payload.socket_id)
                .ok_or_else(|| {
                    SidecarError::InvalidState(format!("unknown Unix socket {}", payload.socket_id))
                })?;
            if socket.local_registry_binding_id.is_some() {
                return Err(sidecar_net_error(std::io::Error::from_raw_os_error(
                    libc::EINVAL,
                )));
            }
            let remote_registry_binding_id = socket.remote_registry_binding_id.clone();
            let peer_can_observe_late_bind =
                guest_unix_connection_peer_open(socket.connection_state.as_ref());

            if payload.autobind || payload.abstract_path_hex.is_some() {
                let explicit_name = payload
                    .abstract_path_hex
                    .as_deref()
                    .map(decode_abstract_unix_name)
                    .transpose()?;
                let attempts = if explicit_name.is_some() { 1 } else { 4096 };
                let mut bound_name = None;
                for nonce in 0..attempts {
                    let guest_name = explicit_name.clone().unwrap_or_else(|| {
                        guest_autobind_unix_name(process.kernel_pid, &binding_id, nonce).to_vec()
                    });
                    let host_name = host_abstract_unix_name(socket_paths, &guest_name);
                    register_guest_unix_binding(
                        &socket_paths.unix_bound_addresses,
                        &binding_id,
                        &abstract_unix_host_address_key(&host_name),
                        GuestUnixAddress {
                            path: abstract_unix_node_path(&guest_name),
                            abstract_path_hex: Some(abstract_unix_name_hex(&guest_name)),
                        },
                        None,
                        None,
                    )?;
                    if peer_can_observe_late_bind {
                        let target_binding_id = remote_registry_binding_id
                            .as_deref()
                            .expect("tracked Unix connection has a target binding");
                        if let Err(error) = queue_guest_unix_peer(
                            &socket_paths.unix_bound_addresses,
                            &binding_id,
                            target_binding_id,
                        ) {
                            rollback_guest_unix_binding(
                                &socket_paths.unix_bound_addresses,
                                &binding_id,
                            )?;
                            return Err(error);
                        }
                    }
                    let result = process
                        .unix_sockets
                        .get_mut(&payload.socket_id)
                        .expect("validated Unix socket remains registered")
                        .bind_abstract(&host_name, &guest_name, &binding_id);
                    match result {
                        Ok(()) => {
                            bound_name = Some(guest_name);
                            break;
                        }
                        Err(error) => {
                            rollback_guest_unix_binding(
                                &socket_paths.unix_bound_addresses,
                                &binding_id,
                            )?;
                            if explicit_name.is_some()
                                || guest_error_code(&error) != Some("EADDRINUSE")
                            {
                                return Err(error);
                            }
                        }
                    }
                }
                let guest_name = bound_name.ok_or_else(|| {
                    sidecar_net_error(std::io::Error::from_raw_os_error(libc::EADDRINUSE))
                })?;
                Ok(json!({
                    "localPath": abstract_unix_node_path(&guest_name),
                    "localAbstractPathHex": abstract_unix_name_hex(&guest_name),
                }))
            } else {
                let path = payload.path.as_deref().expect("validated Unix path");
                let (candidate_path, reported_path) = resolve_guest_unix_path(process, path)?;
                reject_host_mounted_unix_socket_path(socket_paths, &candidate_path)?;
                let canonical_candidate = kernel
                    .resolve_unix_socket_bind_target_for_process(
                        EXECUTION_DRIVER_NAME,
                        process.kernel_pid,
                        &process.guest_cwd,
                        path,
                    )
                    .map_err(kernel_error)?;
                reject_host_mounted_unix_socket_path(socket_paths, &canonical_candidate)?;
                let node = kernel
                    .bind_unix_socket_path_for_process(
                        EXECUTION_DRIVER_NAME,
                        process.kernel_pid,
                        &process.guest_cwd,
                        path,
                    )
                    .map_err(kernel_error)?;
                let guest_path = node.canonical_path;
                let host_path = allocate_guest_socket_host_path(
                    socket_paths,
                    process.kernel_pid,
                    &binding_id,
                    &guest_path,
                );
                if let Err(error) = register_guest_unix_binding(
                    &socket_paths.unix_bound_addresses,
                    &binding_id,
                    &pathname_unix_host_address_key(&host_path),
                    GuestUnixAddress {
                        path: reported_path.clone(),
                        abstract_path_hex: None,
                    },
                    Some((node.stat.dev, node.stat.ino)),
                    Some(host_path.clone()),
                ) {
                    if let Err(rollback_error) = kernel.remove_file(&guest_path) {
                        return Err(SidecarError::Execution(format!(
                            "{error}; failed to roll back Unix socket node {guest_path}: {}",
                            kernel_error(rollback_error)
                        )));
                    }
                    return Err(error);
                }
                if peer_can_observe_late_bind {
                    let target_binding_id = remote_registry_binding_id
                        .as_deref()
                        .expect("tracked Unix connection has a target binding");
                    if let Err(error) = queue_guest_unix_peer(
                        &socket_paths.unix_bound_addresses,
                        &binding_id,
                        target_binding_id,
                    ) {
                        rollback_guest_unix_path_binding(
                            &socket_paths.unix_bound_addresses,
                            &binding_id,
                            kernel,
                            &guest_path,
                            &host_path,
                        )?;
                        return Err(error);
                    }
                }
                if let Err(error) = process
                    .unix_sockets
                    .get_mut(&payload.socket_id)
                    .expect("validated Unix socket remains registered")
                    .bind_path(&host_path, &reported_path, &binding_id)
                {
                    rollback_guest_unix_path_binding(
                        &socket_paths.unix_bound_addresses,
                        &binding_id,
                        kernel,
                        &guest_path,
                        &host_path,
                    )?;
                    return Err(error);
                }
                Ok(json!({ "localPath": reported_path }))
            }
        }
        "net.reserve_tcp_port" => {
            let payload = request
                .args
                .first()
                .cloned()
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "net.reserve_tcp_port requires a request payload",
                    ))
                })
                .and_then(|value| {
                    serde_json::from_value::<JavascriptNetReserveTcpPortRequest>(value).map_err(
                        |error| {
                            SidecarError::InvalidState(format!(
                                "invalid net.reserve_tcp_port payload: {error}"
                            ))
                        },
                    )
                })?;
            let (family, _bind_host, guest_host) =
                normalize_tcp_listen_host(payload.host.as_deref())?;
            let requested_port = payload.port.unwrap_or(0);
            let port = allocate_guest_listen_port(
                requested_port,
                family,
                &socket_paths.used_tcp_guest_ports,
                socket_paths.listen_policy,
            )?;
            let reservation_id = process.allocate_tcp_port_reservation_id();
            process
                .tcp_port_reservations
                .insert(reservation_id.clone(), (family, port));
            Ok(json!({
                "reservationId": reservation_id,
                "localAddress": guest_host,
                "localPort": port,
                "family": match family {
                    SocketFamily::Ipv4 => "IPv4",
                    SocketFamily::Ipv6 => "IPv6",
                },
            }))
        }
        "net.release_tcp_port" => {
            let reservation_id =
                javascript_sync_rpc_arg_str(&request.args, 0, "net.release_tcp_port reservation")?;
            process.tcp_port_reservations.remove(reservation_id);
            Ok(Value::Null)
        }
        "net.connect" => {
            let payload = request
                .args
                .first()
                .cloned()
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "net.connect requires a request payload",
                    ))
                })
                .and_then(|value| {
                    serde_json::from_value::<JavascriptNetConnectRequest>(value).map_err(|error| {
                        SidecarError::InvalidState(format!("invalid net.connect payload: {error}"))
                    })
                })?;
            let pending = reserve_capability(
                &capabilities,
                if payload.path.is_some() {
                    CapabilityKind::UnixSocket
                } else {
                    CapabilityKind::TcpSocket
                },
            )?;
            if let Some(path) = payload.path.as_deref() {
                let guest_path = normalize_path(path);
                let host_path = resolve_guest_socket_host_path(socket_paths, &guest_path);
                let socket = ActiveUnixSocket::connect(
                    &host_path,
                    &guest_path,
                    capabilities.resources(),
                    process.runtime_context.clone(),
                    reactor_io_limits(&process.limits),
                )?;
                let socket_id = process.allocate_unix_socket_id();
                let capability_key = NativeCapabilityKey::UnixSocket(socket_id.clone());
                let identity = commit_process_capability(
                    process,
                    pending,
                    capability_key.clone(),
                    socket_id.clone(),
                    None,
                )?;
                socket.set_event_pusher(
                    process
                        .execution
                        .execution_wake_handle(process.kernel_handle.runtime_identity()),
                    Some(identity),
                    Arc::clone(&process.process_event_notify),
                );
                socket
                    .set_fairness_identity(process.capability_fairness_identity(&capability_key))?;
                socket.retain_description_lease(
                    process
                        .shared_capability_lease(&capability_key)
                        .expect("committed socket capability lease"),
                );
                process.unix_sockets.insert(socket_id.clone(), socket);
                Ok(json!({
                    "socketId": socket_id,
                    "capabilityId": identity.0,
                    "capabilityGeneration": identity.1,
                    "remotePath": guest_path,
                }))
            } else {
                let port = payload.port.ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "net.connect requires either a path or port",
                    ))
                })?;
                let host = payload.host.as_deref().unwrap_or("localhost");
                let local_reservation = payload.local_reservation.as_deref().and_then(|id| {
                    process
                        .tcp_port_reservations
                        .remove(id)
                        .map(|reservation| (id.to_owned(), reservation))
                });
                bridge.require_network_access(
                    vm_id,
                    NetworkOperation::Http,
                    format_tcp_resource(host, port),
                )?;
                if is_loopback_socket_host(host) {
                    let families = [SocketFamily::Ipv4, SocketFamily::Ipv6];
                    if let Some((family, target)) = families.iter().find_map(|family| {
                        let family_number = match family {
                            SocketFamily::Ipv4 => 4,
                            SocketFamily::Ipv6 => 6,
                        };
                        if payload
                            .family
                            .is_some_and(|requested| requested != family_number)
                        {
                            return None;
                        }
                        socket_paths
                            .http_loopback_target(*family, port)
                            .map(|target| (*family, target))
                    }) {
                        if let Some((reservation_id, reservation)) = local_reservation {
                            process
                                .tcp_port_reservations
                                .insert(reservation_id, reservation);
                        }
                        let remote_address = match family {
                            SocketFamily::Ipv4 => "127.0.0.1",
                            SocketFamily::Ipv6 => "::1",
                        };
                        return Ok(json!({
                            "loopbackHttpTarget": {
                                "processId": target.process_id.clone(),
                                "serverId": target.server_id,
                                "host": remote_address,
                                "port": port,
                            },
                            "localAddress": match family {
                                SocketFamily::Ipv4 => "127.0.0.1",
                                SocketFamily::Ipv6 => "::1",
                            },
                            "localPort": payload.local_port.unwrap_or(0),
                            "remoteAddress": remote_address,
                            "remotePort": port,
                            "remoteFamily": match family {
                                SocketFamily::Ipv4 => "IPv4",
                                SocketFamily::Ipv6 => "IPv6",
                            },
                        }));
                    }
                }
                let connect_result = ActiveTcpSocket::connect(ActiveTcpConnectRequest {
                    bridge,
                    kernel,
                    kernel_pid: process.kernel_pid,
                    vm_id,
                    dns,
                    host,
                    port,
                    family: payload.family,
                    local_address: payload.local_address.as_deref(),
                    local_port: payload.local_port,
                    local_reservation: local_reservation
                        .as_ref()
                        .map(|(_, reservation)| *reservation),
                    context: socket_paths,
                    resources: capabilities.resources(),
                    runtime_context: process.runtime_context.clone(),
                    reactor_limits: reactor_io_limits(&process.limits),
                });
                if let Err(error) = connect_result {
                    if let Some((reservation_id, reservation)) = local_reservation {
                        process
                            .tcp_port_reservations
                            .insert(reservation_id, reservation);
                    }
                    return Err(error);
                }
                let socket = connect_result?;
                let socket_id = process.allocate_tcp_socket_id();
                let local_addr = socket.guest_local_addr;
                let remote_addr = socket.guest_remote_addr;
                let capability_key = NativeCapabilityKey::TcpSocket(socket_id.clone());
                let identity = match commit_process_capability(
                    process,
                    pending,
                    capability_key.clone(),
                    socket_id.clone(),
                    socket.kernel_socket_id,
                ) {
                    Ok(identity) => identity,
                    Err(error) => {
                        if let Err(close_error) = socket.close(kernel, process.kernel_pid) {
                            eprintln!(
                                "ERR_AGENTOS_TCP_ROLLBACK: failed to close rejected TCP socket: {close_error}"
                            );
                        }
                        return Err(error);
                    }
                };
                socket.set_event_pusher(
                    process
                        .execution
                        .execution_wake_handle(process.kernel_handle.runtime_identity()),
                    Some(identity),
                    Arc::clone(&process.process_event_notify),
                );
                socket
                    .set_fairness_identity(process.capability_fairness_identity(&capability_key))?;
                socket.retain_description_lease(
                    process
                        .shared_capability_lease(&capability_key)
                        .expect("committed socket capability lease"),
                );
                register_kernel_readiness_target(
                    &kernel_readiness,
                    socket.kernel_socket_id,
                    process
                        .execution
                        .execution_wake_handle(process.kernel_handle.runtime_identity()),
                    Some(Arc::clone(&socket.read_event_notify)),
                    process.capability_readiness_identity(&capability_key),
                    socket_id.clone(),
                    KernelSocketReadinessEvent::Data,
                );
                process.tcp_sockets.insert(socket_id.clone(), socket);
                Ok(json!({
                    "socketId": socket_id,
                    "capabilityId": identity.0,
                    "capabilityGeneration": identity.1,
                    "localAddress": local_addr.ip().to_string(),
                    "localPort": local_addr.port(),
                    "remoteAddress": remote_addr.ip().to_string(),
                    "remotePort": remote_addr.port(),
                    "remoteFamily": socket_addr_family(&remote_addr),
                }))
            }
        }
        "net.listen" => {
            let payload = request
                .args
                .first()
                .cloned()
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "net.listen requires a request payload",
                    ))
                })
                .and_then(|value| match value {
                    Value::String(json) => {
                        serde_json::from_str::<JavascriptNetListenRequest>(&json).map_err(|error| {
                            SidecarError::InvalidState(format!(
                                "invalid net.listen payload: {error}"
                            ))
                        })
                    }
                    other => serde_json::from_value::<JavascriptNetListenRequest>(other).map_err(
                        |error| {
                            SidecarError::InvalidState(format!(
                                "invalid net.listen payload: {error}"
                            ))
                        },
                    ),
                })?;
            if let Some(listener_id) = payload.bound_server_id.as_deref() {
                if payload.path.is_some() || payload.abstract_path_hex.is_some() || payload.autobind
                {
                    return Err(SidecarError::InvalidState(String::from(
                        "net.listen boundServerId cannot be combined with an address",
                    )));
                }
                let listener = process.unix_listeners.remove(listener_id).ok_or_else(|| {
                    SidecarError::InvalidState(format!("unknown bound Unix socket {listener_id}"))
                })?;
                let local_path = listener.path.clone();
                let local_abstract_path_hex = listener.abstract_path_hex.clone();
                let listener = match listener.listen_bound(
                    socket_paths.clone(),
                    payload.backlog,
                    capabilities.clone(),
                    process.runtime_context.clone(),
                    reactor_io_limits(&process.limits),
                ) {
                    Ok(listener) => listener,
                    Err(error) => {
                        process.release_capability_if_present(&NativeCapabilityKey::UnixListener(
                            listener_id.to_owned(),
                        ));
                        return Err(error);
                    }
                };
                let capability_key = NativeCapabilityKey::UnixListener(listener_id.to_owned());
                let identity = process
                    .capability_readiness_identity(&capability_key)
                    .ok_or_else(|| {
                        SidecarError::InvalidState(format!(
                            "missing capability for bound Unix socket {listener_id}"
                        ))
                    })?;
                listener.set_event_pusher(
                    process
                        .execution
                        .execution_wake_handle(process.kernel_handle.runtime_identity()),
                    Some(identity),
                    Arc::clone(&process.process_event_notify),
                );
                process
                    .unix_listeners
                    .insert(listener_id.to_owned(), listener);
                return Ok(json!({
                    "serverId": listener_id,
                    "capabilityId": identity.0,
                    "capabilityGeneration": identity.1,
                    "localPath": local_path,
                    "localAbstractPathHex": local_abstract_path_hex,
                }));
            }
            if payload.path.is_some() || payload.abstract_path_hex.is_some() || payload.autobind {
                let pending = reserve_capability(&capabilities, CapabilityKind::UnixListener)?;
                if usize::from(payload.path.is_some())
                    + usize::from(payload.abstract_path_hex.is_some())
                    + usize::from(payload.autobind)
                    != 1
                {
                    return Err(SidecarError::InvalidState(String::from(
                        "net.listen accepts exactly one Unix address",
                    )));
                }
                let listener_id = process.allocate_unix_listener_id();
                let registry_binding_id = guest_unix_binding_id(process.kernel_pid, &listener_id);
                let (listener, local_path, local_abstract_path_hex) = if payload.autobind {
                    bridge.require_network_access(
                        vm_id,
                        NetworkOperation::Listen,
                        format_unix_socket_resource(None, None, true),
                    )?;
                    let mut bound = None;
                    for nonce in 0..4096 {
                        let guest_name =
                            guest_autobind_unix_name(process.kernel_pid, &listener_id, nonce);
                        let host_name = host_abstract_unix_name(socket_paths, &guest_name);
                        let local_path = abstract_unix_node_path(&guest_name);
                        let local_hex = abstract_unix_name_hex(&guest_name);
                        register_guest_unix_binding(
                            &socket_paths.unix_bound_addresses,
                            &registry_binding_id,
                            &abstract_unix_host_address_key(&host_name),
                            GuestUnixAddress {
                                path: local_path.clone(),
                                abstract_path_hex: Some(local_hex.clone()),
                            },
                            None,
                            None,
                        )?;
                        match ActiveUnixListener::bind_abstract(
                            &host_name,
                            &guest_name,
                            registry_binding_id.clone(),
                            socket_paths.clone(),
                            payload.backlog,
                            capabilities.clone(),
                            process.runtime_context.clone(),
                            reactor_io_limits(&process.limits),
                        ) {
                            Ok(listener) => {
                                bound = Some((listener, local_path, Some(local_hex)));
                                break;
                            }
                            Err(error) => {
                                rollback_guest_unix_binding(
                                    &socket_paths.unix_bound_addresses,
                                    &registry_binding_id,
                                )?;
                                if guest_error_code(&error) != Some("EADDRINUSE") {
                                    return Err(error);
                                }
                            }
                        }
                    }
                    bound.ok_or_else(|| {
                        SidecarError::host(
                            "EADDRINUSE",
                            String::from(
                                "Linux AF_UNIX autobind namespace exhausted after 4096 attempts",
                            ),
                        )
                    })?
                } else if let Some(hex) = payload.abstract_path_hex.as_deref() {
                    bridge.require_network_access(
                        vm_id,
                        NetworkOperation::Listen,
                        format_unix_socket_resource(None, Some(hex), false),
                    )?;
                    let guest_name = decode_abstract_unix_name(hex)?;
                    let host_name = host_abstract_unix_name(socket_paths, &guest_name);
                    let local_path = abstract_unix_node_path(&guest_name);
                    let local_hex = abstract_unix_name_hex(&guest_name);
                    register_guest_unix_binding(
                        &socket_paths.unix_bound_addresses,
                        &registry_binding_id,
                        &abstract_unix_host_address_key(&host_name),
                        GuestUnixAddress {
                            path: local_path.clone(),
                            abstract_path_hex: Some(local_hex.clone()),
                        },
                        None,
                        None,
                    )?;
                    let listener = match ActiveUnixListener::bind_abstract(
                        &host_name,
                        &guest_name,
                        registry_binding_id.clone(),
                        socket_paths.clone(),
                        payload.backlog,
                        capabilities.clone(),
                        process.runtime_context.clone(),
                        reactor_io_limits(&process.limits),
                    ) {
                        Ok(listener) => listener,
                        Err(error) => {
                            rollback_guest_unix_binding(
                                &socket_paths.unix_bound_addresses,
                                &registry_binding_id,
                            )?;
                            return Err(error);
                        }
                    };
                    (listener, local_path, Some(local_hex))
                } else {
                    let path = payload.path.as_deref().expect("validated Unix path");
                    bridge.require_network_access(
                        vm_id,
                        NetworkOperation::Listen,
                        format_unix_socket_resource(Some(path), None, false),
                    )?;
                    let (candidate_path, reported_path) = resolve_guest_unix_path(process, path)?;
                    reject_host_mounted_unix_socket_path(socket_paths, &candidate_path)?;
                    let canonical_candidate = kernel
                        .resolve_unix_socket_bind_target_for_process(
                            EXECUTION_DRIVER_NAME,
                            process.kernel_pid,
                            &process.guest_cwd,
                            path,
                        )
                        .map_err(kernel_error)?;
                    reject_host_mounted_unix_socket_path(socket_paths, &canonical_candidate)?;
                    let node = kernel
                        .bind_unix_socket_path_for_process(
                            EXECUTION_DRIVER_NAME,
                            process.kernel_pid,
                            &process.guest_cwd,
                            path,
                        )
                        .map_err(kernel_error)?;
                    let guest_path = node.canonical_path;
                    let host_path = allocate_guest_socket_host_path(
                        socket_paths,
                        process.kernel_pid,
                        &listener_id,
                        &guest_path,
                    );
                    register_guest_unix_binding(
                        &socket_paths.unix_bound_addresses,
                        &registry_binding_id,
                        &pathname_unix_host_address_key(&host_path),
                        GuestUnixAddress {
                            path: reported_path.clone(),
                            abstract_path_hex: None,
                        },
                        Some((node.stat.dev, node.stat.ino)),
                        Some(host_path.clone()),
                    )?;
                    let listener = match ActiveUnixListener::bind(
                        &host_path,
                        &reported_path,
                        registry_binding_id.clone(),
                        socket_paths.clone(),
                        payload.backlog,
                        capabilities.clone(),
                        process.runtime_context.clone(),
                        reactor_io_limits(&process.limits),
                    ) {
                        Ok(listener) => listener,
                        Err(error) => {
                            rollback_guest_unix_path_binding(
                                &socket_paths.unix_bound_addresses,
                                &registry_binding_id,
                                kernel,
                                &guest_path,
                                &host_path,
                            )?;
                            return Err(error);
                        }
                    };
                    (listener, reported_path, None)
                };
                let capability_key = NativeCapabilityKey::UnixListener(listener_id.clone());
                let identity = commit_process_capability(
                    process,
                    pending,
                    capability_key.clone(),
                    listener_id.clone(),
                    None,
                )?;
                listener.retain_description_lease(
                    process
                        .shared_capability_lease(&capability_key)
                        .expect("committed Unix listener capability lease"),
                );
                listener.set_event_pusher(
                    process
                        .execution
                        .execution_wake_handle(process.kernel_handle.runtime_identity()),
                    Some(identity),
                    Arc::clone(&process.process_event_notify),
                );
                process.unix_listeners.insert(listener_id.clone(), listener);
                Ok(json!({
                    "serverId": listener_id,
                    "capabilityId": identity.0,
                    "capabilityGeneration": identity.1,
                    "path": local_path,
                    "localPath": local_path,
                    "localAbstractPathHex": local_abstract_path_hex,
                }))
            } else {
                let pending = reserve_capability(&capabilities, CapabilityKind::TcpListener)?;
                let (family, bind_host, guest_host) =
                    normalize_tcp_listen_host(payload.host.as_deref())?;
                let requested_port = payload.port.unwrap_or(0);
                bridge.require_network_access(
                    vm_id,
                    NetworkOperation::Listen,
                    format_tcp_resource(bind_host, requested_port),
                )?;
                let local_reservation = payload.local_reservation.as_deref().and_then(|id| {
                    process
                        .tcp_port_reservations
                        .remove(id)
                        .map(|reservation| (id.to_owned(), reservation))
                });
                let port = if requested_port != 0
                    && local_reservation
                        .as_ref()
                        .map(|(_, reservation)| *reservation)
                        == Some((family, requested_port))
                {
                    requested_port
                } else {
                    allocate_guest_listen_port(
                        requested_port,
                        family,
                        &socket_paths.used_tcp_guest_ports,
                        socket_paths.listen_policy,
                    )?
                };
                let listener_result = ActiveTcpListener::bind_kernel(
                    kernel,
                    process.kernel_pid,
                    guest_host,
                    port,
                    payload.backlog,
                );
                if let Err(error) = listener_result {
                    if let Some((reservation_id, reservation)) = local_reservation {
                        process
                            .tcp_port_reservations
                            .insert(reservation_id, reservation);
                    }
                    return Err(error);
                }
                let listener = listener_result?;
                let listener_id = process.allocate_tcp_listener_id();
                let local_addr = listener.guest_local_addr();
                let capability_key = NativeCapabilityKey::TcpListener(listener_id.clone());
                let identity = match commit_process_capability(
                    process,
                    pending,
                    capability_key.clone(),
                    listener_id.clone(),
                    listener.kernel_socket_id,
                ) {
                    Ok(identity) => identity,
                    Err(error) => {
                        if let Err(close_error) = listener.close(kernel, process.kernel_pid) {
                            eprintln!(
                                "ERR_AGENTOS_TCP_ROLLBACK: failed to close rejected TCP listener: {close_error}"
                            );
                        }
                        return Err(error);
                    }
                };
                listener.retain_description_lease(
                    process
                        .shared_capability_lease(&capability_key)
                        .expect("committed TCP listener capability lease"),
                );
                register_kernel_readiness_target(
                    &kernel_readiness,
                    listener.kernel_socket_id,
                    process
                        .execution
                        .execution_wake_handle(process.kernel_handle.runtime_identity()),
                    None,
                    process.capability_readiness_identity(&capability_key),
                    listener_id.clone(),
                    KernelSocketReadinessEvent::Accept,
                );
                process.tcp_listeners.insert(listener_id.clone(), listener);
                Ok(json!({
                    "serverId": listener_id,
                    "capabilityId": identity.0,
                    "capabilityGeneration": identity.1,
                    "localAddress": local_addr.ip().to_string(),
                    "localPort": local_addr.port(),
                    "family": socket_addr_family(&local_addr),
                }))
            }
        }
        "net.poll" => {
            let socket_id = javascript_sync_rpc_arg_str(&request.args, 0, "net.poll socket id")?;
            let wait_ms =
                javascript_sync_rpc_arg_u64_optional(&request.args, 1, "net.poll wait ms")?
                    .unwrap_or_default();
            let wait = clamp_javascript_net_poll_wait(wait_ms);
            let event = if let Some(socket) = process.tcp_sockets.get_mut(socket_id) {
                socket.set_application_read_interest(true)?;
                socket.poll(kernel, process.kernel_pid, wait, trace_enabled)?
            } else if let Some(socket) = process.unix_sockets.get_mut(socket_id) {
                socket.set_application_read_interest(true)?;
                socket.poll(wait)?
            } else {
                return Err(SidecarError::InvalidState(format!(
                    "unknown net socket {socket_id}"
                )));
            };
            match event {
                Some(TcpSocketEvent::Data { bytes: chunk, .. }) => Ok(json!({
                    "type": "data",
                    "data": host_bytes_value(&chunk),
                })),
                Some(TcpSocketEvent::End) => Ok(json!({
                    "type": "end",
                })),
                Some(TcpSocketEvent::Error { code, message }) => Ok(json!({
                    "type": "error",
                    "code": code,
                    "message": message,
                })),
                Some(TcpSocketEvent::Close { had_error }) => {
                    if let Some(socket) = process.tcp_sockets.remove(socket_id) {
                        release_tcp_socket_handle(
                            process,
                            socket_id,
                            socket,
                            kernel,
                            &kernel_readiness,
                        );
                    } else if let Some(socket) = process.unix_sockets.remove(socket_id) {
                        release_unix_socket_handle(
                            process,
                            socket_id,
                            socket,
                            &socket_paths.unix_bound_addresses,
                        );
                    }
                    Ok(json!({
                        "type": "close",
                        "hadError": had_error,
                    }))
                }
                None => Ok(Value::Null),
            }
        }
        "net.socket_wait_connect" => {
            let socket_id =
                javascript_sync_rpc_arg_str(&request.args, 0, "net.socket_wait_connect socket id")?;
            if let Some(socket) = process.tcp_sockets.get(socket_id) {
                encode_net_json_string(socket.socket_info(), "net.socket_wait_connect")
            } else {
                let socket = process.unix_sockets.get(socket_id).ok_or_else(|| {
                    SidecarError::InvalidState(format!("unknown net socket {socket_id}"))
                })?;
                encode_net_json_string(socket.socket_info(), "net.socket_wait_connect")
            }
        }
        "net.socket_read" => {
            let socket_id =
                javascript_sync_rpc_arg_str(&request.args, 0, "net.socket_read socket id")?;
            if trace_enabled {
                NET_TCP_TRACE_COUNTERS
                    .socket_read_calls
                    .fetch_add(1, Ordering::Relaxed);
                NET_TCP_TRACE_COUNTERS
                    .socket_read_zero_wait_calls
                    .fetch_add(1, Ordering::Relaxed);
            }
            if let Some(socket) = process.tcp_sockets.get_mut(socket_id) {
                socket.set_application_read_interest(true)?;
                net_read_value(socket.poll(
                    kernel,
                    process.kernel_pid,
                    Duration::ZERO,
                    trace_enabled,
                )?)
            } else if let Some(socket) = process.unix_sockets.get_mut(socket_id) {
                socket.set_application_read_interest(true)?;
                net_read_value(socket.poll(Duration::ZERO)?)
            } else {
                // A data callback may synchronously destroy its socket while the
                // readiness-driven read pump still owns an admitted turn. Match
                // Node's teardown semantics by making that trailing read observe
                // EOF; mutating operations on a stale handle remain hard errors.
                Ok(Value::Null)
            }
        }
        "net.socket_set_read_interest" => {
            let socket_id = javascript_sync_rpc_arg_str(
                &request.args,
                0,
                "net.socket_set_read_interest socket id",
            )?;
            let enabled = javascript_sync_rpc_arg_bool(
                &request.args,
                1,
                "net.socket_set_read_interest enabled",
            )?;
            if let Some(socket) = process.tcp_sockets.get(socket_id) {
                socket.set_application_read_interest(enabled)?;
            } else if let Some(socket) = process.unix_sockets.get(socket_id) {
                socket.set_application_read_interest(enabled)?;
            } else {
                return Err(SidecarError::InvalidState(format!(
                    "unknown net socket {socket_id}"
                )));
            }
            Ok(Value::Null)
        }
        "net.socket_set_no_delay" => {
            let socket_id =
                javascript_sync_rpc_arg_str(&request.args, 0, "net.socket_set_no_delay socket id")?;
            let enable =
                javascript_sync_rpc_arg_bool(&request.args, 1, "net.socket_set_no_delay enabled")?;
            if let Some(socket) = process.tcp_sockets.get_mut(socket_id) {
                socket.set_no_delay(enable)?;
            } else if !process.unix_sockets.contains_key(socket_id) {
                return Err(SidecarError::InvalidState(format!(
                    "unknown net socket {socket_id}"
                )));
            }
            Ok(Value::Null)
        }
        "net.socket_set_keep_alive" => {
            let socket_id = javascript_sync_rpc_arg_str(
                &request.args,
                0,
                "net.socket_set_keep_alive socket id",
            )?;
            let enable = javascript_sync_rpc_arg_bool(
                &request.args,
                1,
                "net.socket_set_keep_alive enabled",
            )?;
            let initial_delay_secs = javascript_sync_rpc_arg_u64_optional(
                &request.args,
                2,
                "net.socket_set_keep_alive initial delay seconds",
            )?;
            if let Some(socket) = process.tcp_sockets.get_mut(socket_id) {
                socket.set_keep_alive(enable, initial_delay_secs)?;
            } else if !process.unix_sockets.contains_key(socket_id) {
                return Err(SidecarError::InvalidState(format!(
                    "unknown net socket {socket_id}"
                )));
            }
            Ok(Value::Null)
        }
        "net.socket_upgrade_tls" => Err(SidecarError::InvalidState(String::from(
            "TLS upgrade must use the deferred sidecar dispatcher response path",
        ))),
        "net.socket_get_tls_client_hello" => {
            let socket_id = javascript_sync_rpc_arg_str(
                &request.args,
                0,
                "net.socket_get_tls_client_hello socket id",
            )?;
            let socket = process.tcp_sockets.get(socket_id).ok_or_else(|| {
                SidecarError::InvalidState(format!(
                    "unknown TCP socket {socket_id} for TLS client hello query"
                ))
            })?;
            socket.tls_client_hello_json(vm_id, kernel)
        }
        "net.socket_tls_query" => {
            let socket_id =
                javascript_sync_rpc_arg_str(&request.args, 0, "net.socket_tls_query socket id")?;
            let query =
                javascript_sync_rpc_arg_str(&request.args, 1, "net.socket_tls_query query")?;
            let detailed = request
                .args
                .get(2)
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let socket = process.tcp_sockets.get(socket_id).ok_or_else(|| {
                SidecarError::InvalidState(format!("unknown TCP socket {socket_id} for TLS query"))
            })?;
            socket.tls_query(query, detailed)
        }
        "net.server_poll" => {
            let listener_id =
                javascript_sync_rpc_arg_str(&request.args, 0, "net.server_poll listener id")?;
            let wait_ms =
                javascript_sync_rpc_arg_u64_optional(&request.args, 1, "net.server_poll wait ms")?
                    .unwrap_or_default();
            let tcp_pending_capability = if process.tcp_listeners.contains_key(listener_id) {
                match reserve_capability(&capabilities, CapabilityKind::TcpSocket) {
                    Ok(pending) => Some(pending),
                    Err(error) => {
                        return Ok(json!({
                            "type": "error",
                            "code": host_service_error_code(&error),
                            "message": host_service_error_message(&error),
                        }));
                    }
                }
            } else {
                None
            };
            let tcp_event = if let Some(listener) = process.tcp_listeners.get_mut(listener_id) {
                Some(listener.poll(
                    kernel,
                    process.kernel_pid,
                    Duration::from_millis(wait_ms),
                    trace_enabled,
                )?)
            } else {
                None
            };

            if let Some(event) = tcp_event {
                return match event {
                    Some(TcpListenerEvent::Connection(pending)) => {
                        let PendingTcpSocket {
                            stream,
                            kernel_socket_id,
                            guest_local_addr,
                            guest_remote_addr,
                        } = pending;
                        let pending_capability = tcp_pending_capability
                            .expect("TCP capability reserved before listener accept");
                        let mut socket = if let Some(stream) = stream {
                            ActiveTcpSocket::from_stream(
                                stream,
                                Some(listener_id.to_string()),
                                guest_local_addr,
                                guest_remote_addr,
                                capabilities.resources(),
                                process.runtime_context.clone(),
                                reactor_io_limits(&process.limits),
                            )?
                        } else {
                            ActiveTcpSocket::from_kernel(
                                kernel_socket_id.ok_or_else(|| {
                                    SidecarError::InvalidState(String::from(
                                        "kernel TCP accept missing socket id",
                                    ))
                                })?,
                                Some(listener_id.to_string()),
                                guest_local_addr,
                                guest_remote_addr,
                                capabilities.resources(),
                                process.runtime_context.clone(),
                                reactor_io_limits(&process.limits),
                            )
                        };
                        let socket_id = process.allocate_tcp_socket_id();
                        let capability_key = NativeCapabilityKey::TcpSocket(socket_id.clone());
                        let identity = match commit_process_capability(
                            process,
                            pending_capability,
                            capability_key.clone(),
                            socket_id.clone(),
                            socket.kernel_socket_id,
                        ) {
                            Ok(identity) => identity,
                            Err(error) => {
                                if let Err(close_error) = socket.close(kernel, process.kernel_pid) {
                                    eprintln!(
                                        "ERR_AGENTOS_TCP_ROLLBACK: failed to close rejected TCP socket: {close_error}"
                                    );
                                }
                                return Err(error);
                            }
                        };
                        socket.set_event_pusher(
                            process
                                .execution
                                .execution_wake_handle(process.kernel_handle.runtime_identity()),
                            Some(identity),
                            Arc::clone(&process.process_event_notify),
                        );
                        socket.set_fairness_identity(
                            process.capability_fairness_identity(&capability_key),
                        )?;
                        socket.retain_description_lease(
                            process
                                .shared_capability_lease(&capability_key)
                                .expect("committed TCP capability lease"),
                        );
                        register_kernel_readiness_target(
                            &kernel_readiness,
                            socket.kernel_socket_id,
                            process
                                .execution
                                .execution_wake_handle(process.kernel_handle.runtime_identity()),
                            Some(Arc::clone(&socket.read_event_notify)),
                            process.capability_readiness_identity(&capability_key),
                            socket_id.clone(),
                            KernelSocketReadinessEvent::Data,
                        );
                        if let Some(listener) = process.tcp_listeners.get_mut(listener_id) {
                            socket.listener_connection_retirement =
                                Some(listener.register_connection(&socket_id));
                        }
                        process.tcp_sockets.insert(socket_id.clone(), socket);
                        Ok(json!({
                            "type": "connection",
                            "socketId": socket_id,
                            "capabilityId": identity.0,
                            "capabilityGeneration": identity.1,
                            "localAddress": guest_local_addr.ip().to_string(),
                            "localPort": guest_local_addr.port(),
                            "remoteAddress": guest_remote_addr.ip().to_string(),
                            "remotePort": guest_remote_addr.port(),
                            "remoteFamily": socket_addr_family(&guest_remote_addr),
                        }))
                    }
                    Some(TcpListenerEvent::Error { code, message }) => Ok(json!({
                        "type": "error",
                        "code": code,
                        "message": message,
                    })),
                    None => Ok(Value::Null),
                };
            }

            let event = {
                let listener = process.unix_listeners.get_mut(listener_id).ok_or_else(|| {
                    SidecarError::InvalidState(format!("unknown net listener {listener_id}"))
                })?;
                listener.poll(Duration::from_millis(wait_ms))?
            };

            match event {
                Some(UnixListenerEvent::Connection {
                    socket: mut pending,
                    capability: pending_capability,
                }) => {
                    let mut socket = ActiveUnixSocket::from_stream_with_metadata(
                        pending.stream,
                        Some(listener_id.to_string()),
                        pending.local_path.clone(),
                        pending.remote_path.clone(),
                        pending.local_abstract_path_hex.clone(),
                        pending.remote_abstract_path_hex.clone(),
                        None,
                        None,
                        capabilities.resources(),
                        process.runtime_context.clone(),
                        reactor_io_limits(&process.limits),
                    )?;
                    socket.connection_state = pending.connection_guard.state.take();
                    socket.remote_registry_binding_id = Some(
                        process
                            .unix_listeners
                            .get(listener_id)
                            .expect("Unix listener remains registered during accept")
                            .registry_binding_id
                            .clone(),
                    );
                    let socket_id = process.allocate_unix_socket_id();
                    let capability_key = NativeCapabilityKey::UnixSocket(socket_id.clone());
                    let identity = commit_process_capability(
                        process,
                        pending_capability,
                        capability_key.clone(),
                        socket_id.clone(),
                        None,
                    )?;
                    socket.set_event_pusher(
                        process
                            .execution
                            .execution_wake_handle(process.kernel_handle.runtime_identity()),
                        Some(identity),
                        Arc::clone(&process.process_event_notify),
                    );
                    socket.set_fairness_identity(
                        process.capability_fairness_identity(&capability_key),
                    )?;
                    socket.retain_description_lease(
                        process
                            .shared_capability_lease(&capability_key)
                            .expect("committed Unix capability lease"),
                    );
                    if let Some(listener) = process.unix_listeners.get_mut(listener_id) {
                        socket.listener_connection_retirement =
                            Some(listener.register_connection(&socket_id));
                    }
                    process.unix_sockets.insert(socket_id.clone(), socket);
                    Ok(json!({
                        "type": "connection",
                        "socketId": socket_id,
                        "capabilityId": identity.0,
                        "capabilityGeneration": identity.1,
                        "localPath": pending.local_path,
                        "remotePath": pending.remote_path,
                        "localAbstractPathHex": pending.local_abstract_path_hex,
                        "remoteAbstractPathHex": pending.remote_abstract_path_hex,
                    }))
                }
                Some(UnixListenerEvent::Error { code, message }) => Ok(json!({
                    "type": "error",
                    "code": code,
                    "message": message,
                })),
                None => Ok(Value::Null),
            }
        }
        "net.server_accept" => {
            let listener_id =
                javascript_sync_rpc_arg_str(&request.args, 0, "net.server_accept listener id")?;
            if trace_enabled {
                NET_TCP_TRACE_COUNTERS
                    .server_accept_calls
                    .fetch_add(1, Ordering::Relaxed);
                NET_TCP_TRACE_COUNTERS
                    .server_accept_zero_wait_calls
                    .fetch_add(1, Ordering::Relaxed);
            }
            if let Some(listener) = process.tcp_listeners.get_mut(listener_id) {
                let pending_capability =
                    reserve_capability(&capabilities, CapabilityKind::TcpSocket)?;
                return match listener.poll(
                    kernel,
                    process.kernel_pid,
                    Duration::ZERO,
                    trace_enabled,
                )? {
                    Some(TcpListenerEvent::Connection(pending)) => {
                        let PendingTcpSocket {
                            stream,
                            kernel_socket_id,
                            guest_local_addr,
                            guest_remote_addr,
                        } = pending;
                        let mut info = tcp_socket_info_value(&guest_local_addr, &guest_remote_addr);
                        let mut socket = if let Some(stream) = stream {
                            ActiveTcpSocket::from_stream(
                                stream,
                                Some(listener_id.to_string()),
                                guest_local_addr,
                                guest_remote_addr,
                                capabilities.resources(),
                                process.runtime_context.clone(),
                                reactor_io_limits(&process.limits),
                            )?
                        } else {
                            ActiveTcpSocket::from_kernel(
                                kernel_socket_id.ok_or_else(|| {
                                    SidecarError::InvalidState(String::from(
                                        "kernel TCP accept missing socket id",
                                    ))
                                })?,
                                Some(listener_id.to_string()),
                                guest_local_addr,
                                guest_remote_addr,
                                capabilities.resources(),
                                process.runtime_context.clone(),
                                reactor_io_limits(&process.limits),
                            )
                        };
                        let socket_id = process.allocate_tcp_socket_id();
                        let capability_key = NativeCapabilityKey::TcpSocket(socket_id.clone());
                        let identity = match commit_process_capability(
                            process,
                            pending_capability,
                            capability_key.clone(),
                            socket_id.clone(),
                            socket.kernel_socket_id,
                        ) {
                            Ok(identity) => identity,
                            Err(error) => {
                                if let Err(close_error) = socket.close(kernel, process.kernel_pid) {
                                    eprintln!(
                                        "ERR_AGENTOS_TCP_ROLLBACK: failed to close rejected TCP socket: {close_error}"
                                    );
                                }
                                return Err(error);
                            }
                        };
                        socket.set_event_pusher(
                            process
                                .execution
                                .execution_wake_handle(process.kernel_handle.runtime_identity()),
                            Some(identity),
                            Arc::clone(&process.process_event_notify),
                        );
                        if let Value::Object(fields) = &mut info {
                            fields.insert(String::from("capabilityId"), json!(identity.0));
                            fields.insert(String::from("capabilityGeneration"), json!(identity.1));
                        }
                        socket.set_fairness_identity(
                            process.capability_fairness_identity(&capability_key),
                        )?;
                        socket.retain_description_lease(
                            process
                                .shared_capability_lease(&capability_key)
                                .expect("committed TCP capability lease"),
                        );
                        register_kernel_readiness_target(
                            &kernel_readiness,
                            socket.kernel_socket_id,
                            process
                                .execution
                                .execution_wake_handle(process.kernel_handle.runtime_identity()),
                            Some(Arc::clone(&socket.read_event_notify)),
                            process.capability_readiness_identity(&capability_key),
                            socket_id.clone(),
                            KernelSocketReadinessEvent::Data,
                        );
                        if let Some(listener) = process.tcp_listeners.get_mut(listener_id) {
                            socket.listener_connection_retirement =
                                Some(listener.register_connection(&socket_id));
                        }
                        process.tcp_sockets.insert(socket_id.clone(), socket);
                        encode_net_json_string(
                            json!({
                                "socketId": socket_id,
                                "info": info,
                            }),
                            "net.server_accept",
                        )
                    }
                    Some(TcpListenerEvent::Error { code, message }) => Err(SidecarError::host(
                        code.as_deref().unwrap_or("EIO"),
                        message,
                    )),
                    None => Ok(net_timeout_value()),
                };
            }

            let target_binding_id = process
                .unix_listeners
                .get(listener_id)
                .ok_or_else(|| {
                    SidecarError::InvalidState(format!("unknown net listener {listener_id}"))
                })?
                .registry_binding_id
                .clone();
            let event = process
                .unix_listeners
                .get_mut(listener_id)
                .expect("validated Unix listener remains registered")
                .poll(Duration::ZERO)?;
            match event {
                Some(UnixListenerEvent::Connection {
                    socket: mut pending,
                    capability: pending_capability,
                }) => {
                    let mut info = json!({
                        "localPath": pending.local_path.clone(),
                        "remotePath": pending.remote_path.clone(),
                        "localAbstractPathHex": pending.local_abstract_path_hex.clone(),
                        "remoteAbstractPathHex": pending.remote_abstract_path_hex.clone(),
                    });
                    let mut socket = ActiveUnixSocket::from_stream_with_metadata(
                        pending.stream,
                        Some(listener_id.to_string()),
                        pending.local_path,
                        pending.remote_path,
                        pending.local_abstract_path_hex,
                        pending.remote_abstract_path_hex,
                        None,
                        None,
                        capabilities.resources(),
                        process.runtime_context.clone(),
                        reactor_io_limits(&process.limits),
                    )?;
                    socket.connection_state = pending.connection_guard.state.take();
                    socket.remote_registry_binding_id = Some(target_binding_id);
                    let socket_id = process.allocate_unix_socket_id();
                    let capability_key = NativeCapabilityKey::UnixSocket(socket_id.clone());
                    let identity = commit_process_capability(
                        process,
                        pending_capability,
                        capability_key.clone(),
                        socket_id.clone(),
                        None,
                    )?;
                    socket.set_event_pusher(
                        process
                            .execution
                            .execution_wake_handle(process.kernel_handle.runtime_identity()),
                        Some(identity),
                        Arc::clone(&process.process_event_notify),
                    );
                    socket.set_fairness_identity(
                        process.capability_fairness_identity(&capability_key),
                    )?;
                    socket.retain_description_lease(
                        process
                            .shared_capability_lease(&capability_key)
                            .expect("committed Unix capability lease"),
                    );
                    if let Value::Object(fields) = &mut info {
                        fields.insert(String::from("capabilityId"), json!(identity.0));
                        fields.insert(String::from("capabilityGeneration"), json!(identity.1));
                    }
                    if let Some(listener) = process.unix_listeners.get_mut(listener_id) {
                        socket.listener_connection_retirement =
                            Some(listener.register_connection(&socket_id));
                    }
                    process.unix_sockets.insert(socket_id.clone(), socket);
                    encode_net_json_string(
                        json!({
                            "socketId": socket_id,
                            "info": info,
                        }),
                        "net.server_accept",
                    )
                }
                Some(UnixListenerEvent::Error { code, message }) => Err(SidecarError::host(
                    code.as_deref().unwrap_or("EIO"),
                    message,
                )),
                None => Ok(net_timeout_value()),
            }
        }
        "net.server_connections" => {
            let listener_id = javascript_sync_rpc_arg_str(
                &request.args,
                0,
                "net.server_connections listener id",
            )?;
            if let Some(listener) = process.tcp_listeners.get(listener_id) {
                Ok(json!(listener.active_connection_count()))
            } else {
                let listener = process.unix_listeners.get(listener_id).ok_or_else(|| {
                    SidecarError::InvalidState(format!("unknown net listener {listener_id}"))
                })?;
                Ok(json!(listener.active_connection_count()))
            }
        }
        "net.upgrade_socket_write" => {
            let socket_id = javascript_sync_rpc_arg_str(
                &request.args,
                0,
                "net.upgrade_socket_write socket id",
            )?;
            let chunk =
                javascript_sync_rpc_base64_arg(&request.args, 1, "net.upgrade_socket_write chunk")?;
            let socket = process.tcp_sockets.get(socket_id).ok_or_else(|| {
                SidecarError::InvalidState(format!("unknown TCP socket {socket_id}"))
            })?;
            socket
                .write_all(kernel, process.kernel_pid, &chunk)
                .map(|written| json!(written))
        }
        "net.upgrade_socket_end" => {
            let socket_id =
                javascript_sync_rpc_arg_str(&request.args, 0, "net.upgrade_socket_end socket id")?;
            let socket = process.tcp_sockets.get(socket_id).ok_or_else(|| {
                SidecarError::InvalidState(format!("unknown TCP socket {socket_id}"))
            })?;
            socket.shutdown_write(kernel, process.kernel_pid)?;
            Ok(Value::Null)
        }
        "net.upgrade_socket_destroy" => {
            let socket_id = javascript_sync_rpc_arg_str(
                &request.args,
                0,
                "net.upgrade_socket_destroy socket id",
            )?;
            let socket = process.tcp_sockets.remove(socket_id).ok_or_else(|| {
                SidecarError::InvalidState(format!("unknown TCP socket {socket_id}"))
            })?;
            release_tcp_socket_handle(process, socket_id, socket, kernel, &kernel_readiness);
            Ok(Value::Null)
        }
        "net.write" => {
            let socket_id = javascript_sync_rpc_arg_str(&request.args, 0, "net.write socket id")?;
            let chunk = if let Some(bytes) = request.raw_bytes_args.get(&1) {
                bytes.clone()
            } else {
                javascript_sync_rpc_bytes_arg(&request.args, 1, "net.write chunk")?
            };
            if trace_enabled {
                NET_TCP_TRACE_COUNTERS
                    .socket_write_calls
                    .fetch_add(1, Ordering::Relaxed);
                NET_TCP_TRACE_COUNTERS.socket_write_bytes.fetch_add(
                    u64::try_from(chunk.len()).unwrap_or(u64::MAX),
                    Ordering::Relaxed,
                );
            }
            if let Some(socket) = process.tcp_sockets.get(socket_id) {
                let write_started = trace_enabled.then(Instant::now);
                let write_result = socket.write_all(kernel, process.kernel_pid, &chunk);
                if let Some(write_started) = write_started {
                    NET_TCP_TRACE_COUNTERS.socket_write_kernel_us.fetch_add(
                        duration_micros_u64(write_started.elapsed()),
                        Ordering::Relaxed,
                    );
                }
                match write_result {
                    Ok(written) => Ok(json!(written)),
                    Err(error) => {
                        if trace_enabled {
                            NET_TCP_TRACE_COUNTERS
                                .socket_write_errors
                                .fetch_add(1, Ordering::Relaxed);
                        }
                        Err(error)
                    }
                }
            } else {
                let socket = process.unix_sockets.get(socket_id).ok_or_else(|| {
                    SidecarError::InvalidState(format!("unknown net socket {socket_id}"))
                })?;
                socket.write_all(&chunk).map(|written| json!(written))
            }
        }
        "net.shutdown" => {
            let socket_id =
                javascript_sync_rpc_arg_str(&request.args, 0, "net.shutdown socket id")?;
            if let Some(socket) = process.tcp_sockets.get(socket_id) {
                socket.shutdown_write(kernel, process.kernel_pid)?;
            } else {
                let socket = process.unix_sockets.get(socket_id).ok_or_else(|| {
                    SidecarError::InvalidState(format!("unknown net socket {socket_id}"))
                })?;
                socket.shutdown_write()?;
            }
            Ok(Value::Null)
        }
        "net.destroy" => {
            let socket_id = javascript_sync_rpc_arg_str(&request.args, 0, "net.destroy socket id")?;
            if let Some(socket) = process.tcp_sockets.remove(socket_id) {
                release_tcp_socket_handle(process, socket_id, socket, kernel, &kernel_readiness);
                Ok(Value::Null)
            } else if let Some(socket) = process.unix_sockets.remove(socket_id) {
                release_unix_socket_handle(
                    process,
                    socket_id,
                    socket,
                    &socket_paths.unix_bound_addresses,
                );
                Ok(Value::Null)
            } else {
                Ok(Value::Null)
            }
        }
        "net.server_close" => {
            let listener_id =
                javascript_sync_rpc_arg_str(&request.args, 0, "net.server_close listener id")?;
            if let Some(listener) = process.tcp_listeners.remove(listener_id) {
                release_tcp_listener_handle(
                    process,
                    listener_id,
                    listener,
                    kernel,
                    &kernel_readiness,
                )?;
                Ok(Value::Null)
            } else {
                let listener = process.unix_listeners.remove(listener_id).ok_or_else(|| {
                    SidecarError::InvalidState(format!("unknown net listener {listener_id}"))
                })?;
                release_unix_listener_capability(process, listener_id, &listener)?;
                if listener.is_final_description_handle() {
                    drop(listener.close());
                }
                Ok(Value::Null)
            }
        }
        "tls.get_ciphers" => encode_net_json_string(
            Value::Array(
                tls_provider()
                    .cipher_suites
                    .iter()
                    .filter_map(|suite| {
                        suite
                            .suite()
                            .as_str()
                            .map(|value| Value::String(value.to_owned()))
                    })
                    .collect(),
            ),
            "tls.get_ciphers",
        ),
        _ => Err(SidecarError::InvalidState(format!(
            "unsupported JavaScript net sync RPC method {}",
            request.method
        ))),
    }
}

pub(in crate::execution) fn resolve_guest_unix_path(
    process: &ActiveProcess,
    path: &str,
) -> Result<(String, String), SidecarError> {
    if path.len() > 108 {
        return Err(sidecar_net_error(std::io::Error::from_raw_os_error(
            libc::ENAMETOOLONG,
        )));
    }
    let resolved = if Path::new(path).is_absolute() {
        normalize_path(path)
    } else {
        normalize_path(&format!("{}/{}", process.guest_cwd, path))
    };
    Ok((resolved, path.to_owned()))
}

fn host_mount_read_only_for_guest_path(
    mounts: &[crate::protocol::MountDescriptor],
    guest_path: &str,
) -> Option<bool> {
    let normalized = normalize_path(guest_path);
    mounts
        .iter()
        .filter(|mount| mount.plugin.id == "host_dir" || mount.plugin.id == "module_access")
        .filter(|mount| {
            normalized == mount.guest_path
                || normalized.starts_with(&format!("{}/", mount.guest_path.trim_end_matches('/')))
        })
        .max_by_key(|mount| mount.guest_path.len())
        .map(|mount| mount.read_only)
}

pub(in crate::execution) fn reject_host_mounted_unix_socket_path(
    context: &SocketPathContext,
    guest_path: &str,
) -> Result<(), SidecarError> {
    if let Some(read_only) = host_mount_read_only_for_guest_path(&context.mounts, guest_path) {
        let errno = if read_only {
            libc::EROFS
        } else {
            libc::ENOTSUP
        };
        return Err(sidecar_net_error(std::io::Error::from_raw_os_error(errno)));
    }
    Ok(())
}

pub(in crate::execution) fn allocate_guest_socket_host_path(
    context: &SocketPathContext,
    kernel_pid: u32,
    listener_id: &str,
    guest_path: &str,
) -> PathBuf {
    let mut digest = Sha256::new();
    digest.update(b"agentos-unix-path-v1\0");
    digest.update(kernel_pid.to_le_bytes());
    digest.update(listener_id.as_bytes());
    digest.update(b"\0");
    digest.update(guest_path.as_bytes());
    let leaf = abstract_unix_name_hex(&digest.finalize()[..16]);
    context.unix_socket_host_dir.join(leaf)
}

pub(in crate::execution) fn format_unix_socket_resource(
    path: Option<&str>,
    abstract_path_hex: Option<&str>,
    autobind: bool,
) -> String {
    if let Some(path) = path {
        format!("unix:{path}")
    } else if let Some(hex) = abstract_path_hex {
        format!("unix:abstract:{hex}")
    } else if autobind {
        String::from("unix:autobind")
    } else {
        String::from("unix:unnamed")
    }
}

pub(crate) fn error_code(error: &SidecarError) -> &str {
    match error {
        SidecarError::ResourceLimit(_) => "ERR_AGENTOS_RESOURCE_LIMIT",
        SidecarError::Host(error) => &error.code,
        SidecarError::InvalidState(_) => "invalid_state",
        SidecarError::ProtocolVersionMismatch(_) => "protocol_version_mismatch",
        SidecarError::BridgeVersionMismatch(_) => "bridge_version_mismatch",
        SidecarError::Conflict(_) => "conflict",
        SidecarError::Unauthorized(_) => "unauthorized",
        SidecarError::Unsupported(_) => "unsupported",
        SidecarError::FrameTooLarge(_) => "frame_too_large",
        SidecarError::Kernel(_) => "kernel_error",
        SidecarError::Plugin(_) => "plugin_error",
        SidecarError::Execution(_) => "execution_error",
        SidecarError::ExecutionEventChannelClosed { .. } => "execution_event_channel_closed",
        SidecarError::Bridge(_) => "bridge_error",
        SidecarError::Io(_) => "io_error",
    }
}

pub(in crate::execution) fn guest_error_code(error: &SidecarError) -> Option<&str> {
    error.code()
}

pub(crate) fn host_service_error_code(error: &SidecarError) -> String {
    error
        .code()
        .unwrap_or("ERR_AGENTOS_NODE_SYNC_RPC")
        .to_owned()
}

pub(in crate::execution) fn host_service_error_message(error: &SidecarError) -> String {
    match error {
        SidecarError::ResourceLimit(limit) => crate::state::guest_limit_message(limit),
        SidecarError::Host(error) => error.message.clone(),
        _ => error.to_string(),
    }
}

pub(crate) fn host_service_error(
    error: &SidecarError,
) -> agentos_execution::backend::HostServiceError {
    use agentos_execution::backend::HostServiceError;

    match error {
        SidecarError::Host(error) => error.clone(),
        SidecarError::ResourceLimit(limit) => {
            let guest_scope = if limit.scope.starts_with("vm=") {
                "vm"
            } else {
                "process"
            };
            let mut details = serde_json::json!({
                "limitName": limit.resource.name(),
                "limit": limit.limit,
                "requested": limit.requested,
                "configPath": limit.config_path,
                "scope": guest_scope,
            });
            if guest_scope == "vm" {
                details["used"] = serde_json::json!(limit.used);
                details["observed"] = serde_json::json!(limit.used.saturating_add(limit.requested));
            }
            HostServiceError::new(
                "ERR_AGENTOS_RESOURCE_LIMIT",
                crate::state::guest_limit_message(limit),
            )
            .with_details(details)
        }
        _ => HostServiceError::new(
            host_service_error_code(error),
            host_service_error_message(error),
        ),
    }
}

#[cfg(test)]
pub(crate) fn ignore_stale_javascript_sync_rpc_response(
    error: SidecarError,
) -> Result<(), SidecarError> {
    match error {
        SidecarError::Execution(message)
            if message.ends_with("is no longer pending")
                && message.starts_with("sync RPC request ") =>
        {
            Ok(())
        }
        SidecarError::Execution(message) => {
            let lower = message.to_ascii_lowercase();
            if message.contains("ERR_AGENTOS_BRIDGE_STALE_COMPLETION") {
                // The V8 registry only emits this after proving that the exact
                // session generation owned a host-visible route which teardown
                // canceled before this response arrived. Keep arbitrary unknown
                // call IDs and mismatched generations fatal.
                eprintln!("INFO_AGENTOS_STALE_BRIDGE_COMPLETION: {message}");
                Ok(())
            } else if lower.contains("sync rpc response")
                && (lower.contains("broken pipe") || lower.contains("channel closed unexpectedly"))
            {
                Ok(())
            } else {
                Err(SidecarError::Execution(message))
            }
        }
        other => Err(other),
    }
}

#[cfg(test)]
mod error_code_tests {
    use super::{
        host_service_error_code, host_service_error_message,
        ignore_stale_javascript_sync_rpc_response, javascript_sync_rpc_arg_rlim,
        javascript_sync_rpc_arg_u64, process_resource_limit_kind, SidecarError,
    };
    use agentos_kernel::kernel::ProcessResourceLimitKind;
    use agentos_runtime::accounting::{LimitError, ResourceClass};
    use serde_json::json;

    #[test]
    fn wasm_u64_arguments_accept_lossless_decimal_strings() {
        assert_eq!(
            javascript_sync_rpc_arg_u64(&[json!(u64::MAX.to_string())], 0, "offset")
                .expect("decode maximum u64"),
            u64::MAX
        );
        assert!(javascript_sync_rpc_arg_u64(&[json!("-1")], 0, "offset").is_err());
        assert!(javascript_sync_rpc_arg_u64(&[json!("1.5")], 0, "offset").is_err());
    }

    #[test]
    fn wasm_resource_limit_numbers_cover_the_linux_surface() {
        let expected = [
            ProcessResourceLimitKind::Cpu,
            ProcessResourceLimitKind::FileSize,
            ProcessResourceLimitKind::Data,
            ProcessResourceLimitKind::Stack,
            ProcessResourceLimitKind::Core,
            ProcessResourceLimitKind::ResidentSet,
            ProcessResourceLimitKind::Processes,
            ProcessResourceLimitKind::OpenFiles,
            ProcessResourceLimitKind::LockedMemory,
            ProcessResourceLimitKind::AddressSpace,
        ];
        for (resource, expected) in expected.into_iter().enumerate() {
            assert_eq!(
                process_resource_limit_kind(resource as u32).expect("known resource"),
                expected
            );
        }
        assert_eq!(
            process_resource_limit_kind(10)
                .expect_err("unknown resource")
                .code(),
            Some("EINVAL")
        );
    }

    #[test]
    fn wasm_resource_limit_values_preserve_full_u64_precision() {
        assert_eq!(
            javascript_sync_rpc_arg_rlim(&[json!(u64::MAX.to_string())], 0, "rlim")
                .expect("parse RLIM_INFINITY"),
            u64::MAX
        );
    }

    #[test]
    fn host_service_error_code_ignores_spoofed_errnos() {
        let error = SidecarError::Execution(String::from("user said 'EACCES: denied'"));
        assert_eq!(host_service_error_code(&error), "ERR_AGENTOS_NODE_SYNC_RPC");
    }

    #[test]
    fn host_service_error_code_preserves_real_sidecar_errnos() {
        let error = SidecarError::host("EACCES", "permission denied on /foo");
        assert_eq!(host_service_error_code(&error), "EACCES");
    }

    #[test]
    fn host_service_error_code_preserves_dgram_state_errors() {
        for code in [
            "ERR_SOCKET_BAD_PORT",
            "ERR_SOCKET_DGRAM_IS_CONNECTED",
            "ERR_SOCKET_DGRAM_NOT_CONNECTED",
            "ERR_SOCKET_DGRAM_NOT_RUNNING",
        ] {
            let error = SidecarError::host(code, "dgram state error");
            assert_eq!(host_service_error_code(&error), code);
        }
    }

    #[test]
    fn host_service_error_code_does_not_parse_diagnostic_messages() {
        let error = SidecarError::Io(String::from(
            "failed to create mapped guest directory /.next/server: File exists (os error 17)",
        ));
        assert_eq!(host_service_error_code(&error), "ERR_AGENTOS_NODE_SYNC_RPC");
    }

    #[test]
    fn host_service_error_code_preserves_native_binary_rejections() {
        let error = SidecarError::host(
            "ERR_NATIVE_BINARY_NOT_SUPPORTED",
            String::from(
                "refused to execute native ELF guest binary at /tmp/fake-rg inside the VM",
            ),
        );
        assert_eq!(
            host_service_error_code(&error),
            "ERR_NATIVE_BINARY_NOT_SUPPORTED"
        );
    }

    #[test]
    fn javascript_sync_rpc_error_hides_process_occupancy() {
        let error = SidecarError::ResourceLimit(LimitError {
            scope: String::from("sidecar-process"),
            resource: ResourceClass::BridgeResponseBytes,
            used: 65_535,
            requested: 1,
            limit: 65_536,
            config_path: String::from("runtime.resources.maxBridgeResponseBytes"),
        });
        let message = host_service_error_message(&error);
        assert!(!message.contains("used=65535"));
        assert!(message.contains("scope=process"));
        assert!(message.contains("requested=1 limit=65536"));
    }

    #[test]
    fn stale_bridge_filter_requires_registry_proof_of_cancellation() {
        let stale = SidecarError::Execution(String::from(
            "failed to reply to guest JavaScript sync RPC request: ERR_AGENTOS_BRIDGE_STALE_COMPLETION: response for canceled host-visible bridge call_id 17 in session vm-1 generation Some(3)",
        ));
        assert!(ignore_stale_javascript_sync_rpc_response(stale).is_ok());

        for hard_error in [
            "ERR_AGENTOS_BRIDGE_UNKNOWN_CALL_ID: response for unknown bridge call_id 17",
            "ERR_AGENTOS_BRIDGE_STALE_GENERATION: response call_id 17 generation Some(4), expected Some(3)",
        ] {
            let error = SidecarError::Execution(format!(
                "failed to reply to guest JavaScript sync RPC request: {hard_error}"
            ));
            assert!(
                ignore_stale_javascript_sync_rpc_response(error).is_err(),
                "must not suppress {hard_error}"
            );
        }
    }
}

#[cfg(test)]
mod wasm_sync_rpc_tests {
    use super::{
        deferred_child_kernel_wait_request, javascript_sync_rpc_request_bytes_arg,
        remap_wasm_process_sync_rpc, HostRpcRequest, ALLOWED_WASM_PROCESS_SYNC_RPCS,
    };
    use serde_json::json;
    use std::collections::{BTreeSet, HashMap};

    fn emitted_wasm_wrapped_sync_rpcs() -> BTreeSet<&'static str> {
        let source = include_str!("../../../../execution/src/wasm.rs");
        let start = source
            .find("case \"process.exec_image_open\":")
            .expect("WASM process sync-RPC switch must exist");
        let end = source[start..]
            .find("_processWasmSyncRpc.applySync")
            .map(|offset| start + offset)
            .expect("WASM process sync-RPC dispatch call must exist");
        source[start..end]
            .lines()
            .filter_map(|line| {
                line.trim()
                    .strip_prefix("case \"")
                    .and_then(|line| line.strip_suffix("\":"))
            })
            .collect()
    }

    #[test]
    fn every_emitted_wasm_wrapped_rpc_is_unwrapped_to_the_direct_handler_shape() {
        let emitted = emitted_wasm_wrapped_sync_rpcs();
        assert!(!emitted.is_empty(), "expected wrapped WASM RPC methods");
        let allowed = ALLOWED_WASM_PROCESS_SYNC_RPCS
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        assert_eq!(
            emitted, allowed,
            "the generic WASM wrapper switch and sidecar allowlist must remain exact"
        );

        let service_source = include_str!("rpc.rs");
        let service_start = service_source
            .find("pub(crate) async fn service_javascript_sync_rpc")
            .expect("sync RPC service must exist");
        let service_end = service_source[service_start..]
            .find("fn service_javascript_internal_bridge_sync_rpc")
            .map(|offset| service_start + offset)
            .expect("sync RPC service end must exist");
        let service_source = &service_source[service_start..service_end];
        let typed_dispatch_source = include_str!("../host_dispatch/mod.rs");
        let typed_filesystem_dispatch_source = include_str!("../host_dispatch/filesystem.rs");

        for method in emitted {
            if !crate::execution::host_dispatch::is_wasm_adapter_only_rpc(method) {
                assert!(
                    service_source.contains(&format!("\"{method}\""))
                        || typed_dispatch_source.contains(&format!("\"{method}\""))
                        || typed_filesystem_dispatch_source.contains(&format!("\"{method}\"")),
                    "WASM emits {method}, but the direct sync RPC dispatcher has no handler"
                );
            }
            let direct = HostRpcRequest {
                id: 17,
                method: method.to_owned(),
                args: vec![json!({ "marker": method })],
                raw_bytes_args: HashMap::from([(0, vec![1, 2, 3])]),
            };
            let wrapped = HostRpcRequest {
                id: direct.id,
                method: String::from("process.wasm_sync_rpc"),
                args: vec![json!(method), direct.args[0].clone()],
                raw_bytes_args: HashMap::from([(1, vec![1, 2, 3])]),
            };
            let remapped = remap_wasm_process_sync_rpc(&wrapped)
                .expect("emitted method must be accepted")
                .expect("wrapper method must be remapped");
            assert_eq!(remapped.id, direct.id, "request id for {method}");
            assert_eq!(
                remapped.method, direct.method,
                "handler method for {method}"
            );
            assert_eq!(remapped.args, direct.args, "handler args for {method}");
            assert_eq!(
                remapped.raw_bytes_args, direct.raw_bytes_args,
                "raw argument indexes for {method}"
            );
        }
    }

    #[test]
    fn wrapped_wasm_fd_read_is_normalized_for_descendant_deferral() {
        let request = HostRpcRequest {
            id: 41,
            method: String::from("process.wasm_sync_rpc"),
            args: vec![json!("process.fd_read"), json!(7), json!(4096), json!(5000)],
            raw_bytes_args: HashMap::new(),
        };

        let normalized = deferred_child_kernel_wait_request(&request)
            .expect("normalize wrapped request")
            .expect("wrapped fd_read must use descendant wait path");
        assert_eq!(normalized.id, request.id);
        assert_eq!(normalized.method, "process.fd_read");
        assert_eq!(normalized.args, request.args[1..]);
    }

    #[test]
    fn request_byte_decoder_prefers_lossless_cbor_lane_and_accepts_compat_shapes() {
        let raw = HostRpcRequest {
            id: 1,
            method: String::from("process.fd_sendmsg_rights"),
            args: vec![
                json!(4),
                json!({ "__agentOSType": "bytes", "base64": "d3Jvbmc=" }),
            ],
            raw_bytes_args: HashMap::from([(1, vec![0, 255, 1, 128])]),
        };
        assert_eq!(
            javascript_sync_rpc_request_bytes_arg(&raw, 1, "payload")
                .expect("decode raw CBOR byte lane"),
            vec![0, 255, 1, 128]
        );

        for (value, expected) in [
            (
                json!({ "__agentOSType": "bytes", "base64": "AP8BgA==" }),
                vec![0, 255, 1, 128],
            ),
            (
                json!({ "__type": "Buffer", "data": "AP8BgA==" }),
                vec![0, 255, 1, 128],
            ),
            (
                json!({ "__type": "buffer", "value": "AP8BgA==" }),
                vec![0, 255, 1, 128],
            ),
            (json!("text"), b"text".to_vec()),
        ] {
            let request = HostRpcRequest {
                id: 2,
                method: String::from("test"),
                args: vec![value],
                raw_bytes_args: HashMap::new(),
            };
            assert_eq!(
                javascript_sync_rpc_request_bytes_arg(&request, 0, "payload")
                    .expect("decode compatibility byte shape"),
                expected
            );
        }

        let invalid = HostRpcRequest {
            id: 3,
            method: String::from("test"),
            args: vec![json!([0, 255, 1, 128])],
            raw_bytes_args: HashMap::new(),
        };
        let error = javascript_sync_rpc_request_bytes_arg(&invalid, 0, "payload")
            .expect_err("numeric arrays are not a bridge byte encoding");
        assert!(error.to_string().contains("payload"));
    }
}
