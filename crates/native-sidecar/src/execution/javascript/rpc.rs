use super::super::*;
use crate::filesystem::{
    javascript_sync_rpc_path_arg, remove_process_shadow_path, rename_process_shadow_path,
};
use agentos_kernel::vfs::{VirtualTimeSpec, VirtualUtimeSpec};

const ALLOWED_WASM_PROCESS_SYNC_RPCS: &[&str] = &[
    "process.umask",
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
    "process.itimer_real",
    "process.fd_pipe",
    "process.fd_open",
    "process.path_open_at",
    "process.path_mkdir_at",
    "process.path_stat_at",
    "process.path_chown_at",
    "process.path_utimes_at",
    "process.path_link_at",
    "process.path_readlink_at",
    "process.path_remove_dir_at",
    "process.path_rename_at",
    "process.path_symlink_at",
    "process.path_unlink_at",
    "process.fd_snapshot",
    "process.fd_read",
    "process.fd_pread",
    "process.fd_write",
    "process.fd_pwrite",
    "process.fd_sync",
    "process.fd_datasync",
    "process.fd_readdir",
    "process.fd_close",
    "process.fd_stat",
    "process.fd_filestat",
    "process.fd_chmod",
    "process.fd_chown",
    "process.fd_truncate",
    "process.fd_set_flags",
    "process.fd_getfd",
    "process.fd_setfd",
    "process.fd_flock",
    "process.fd_record_lock",
    "process.fd_record_lock_cancel",
    "process.fd_dup",
    "process.fd_dup2",
    "process.fd_dup_min",
    "process.fd_seek",
    "process.fd_chdir_path",
    "process.fd_socketpair",
    "process.fd_sendmsg_rights",
    "process.fd_recvmsg_rights",
    "process.fd_socket_shutdown",
    "dns.resolveRawRr",
];

fn remap_wasm_process_sync_rpc(
    request: &JavascriptSyncRpcRequest,
) -> Result<Option<JavascriptSyncRpcRequest>, SidecarError> {
    if request.method != "process.wasm_sync_rpc" {
        return Ok(None);
    }
    let method = javascript_sync_rpc_arg_str(&request.args, 0, "WASM process sync RPC method")?;
    if !ALLOWED_WASM_PROCESS_SYNC_RPCS.contains(&method) {
        return Err(SidecarError::InvalidState(format!(
            "unsupported WASM process sync RPC method {method}"
        )));
    }
    Ok(Some(JavascriptSyncRpcRequest {
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
pub(crate) fn javascript_sync_rpc_may_make_fd_readable(request: &JavascriptSyncRpcRequest) -> bool {
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
pub(crate) fn javascript_sync_rpc_may_make_fd_writable(request: &JavascriptSyncRpcRequest) -> bool {
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
    request: &JavascriptSyncRpcRequest,
) -> Result<Option<JavascriptSyncRpcRequest>, SidecarError> {
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
    Ok(Some(JavascriptSyncRpcRequest {
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
/// pipe. Regular-file writes must retain the filesystem service's host-shadow
/// synchronization, while a full pipe must never block the sidecar actor.
pub(crate) fn deferred_kernel_wait_request_for_process(
    request: &JavascriptSyncRpcRequest,
    kernel: &SidecarKernel,
    process: &ActiveProcess,
) -> Result<Option<JavascriptSyncRpcRequest>, SidecarError> {
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
    // Projected host files live in the process-local mapped-fd table rather
    // than the kernel fd table. They are regular files and can never require
    // the nonblocking pipe-write path below.
    if process.mapped_host_fd(fd).is_some() {
        return Ok(None);
    }
    let stat = kernel
        .fd_stat(EXECUTION_DRIVER_NAME, process.kernel_pid, fd)
        .map_err(kernel_error)?;
    if stat.filetype != agentos_kernel::fd_table::FILETYPE_PIPE {
        return Ok(None);
    }
    Ok(Some(JavascriptSyncRpcRequest {
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
    pub(crate) socket_paths: &'a JavascriptSocketPathContext,
    pub(crate) kernel: &'a mut SidecarKernel,
    pub(crate) kernel_readiness: KernelSocketReadinessRegistry,
    pub(crate) process: &'a mut ActiveProcess,
    pub(crate) sync_request: &'a JavascriptSyncRpcRequest,
    pub(crate) capabilities: CapabilityRegistry,
}

pub(crate) enum JavascriptSyncRpcServiceResponse {
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

impl From<Value> for JavascriptSyncRpcServiceResponse {
    fn from(value: Value) -> Self {
        Self::Json(value)
    }
}

impl JavascriptSyncRpcServiceResponse {
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

pub(crate) struct JavascriptNetSyncRpcServiceRequest<'a, B> {
    pub(crate) bridge: &'a SharedBridge<B>,
    pub(crate) vm_id: &'a str,
    pub(crate) dns: &'a VmDnsConfig,
    pub(crate) socket_paths: &'a JavascriptSocketPathContext,
    pub(crate) kernel: &'a mut SidecarKernel,
    pub(crate) kernel_readiness: KernelSocketReadinessRegistry,
    pub(crate) process: &'a mut ActiveProcess,
    pub(crate) sync_request: &'a JavascriptSyncRpcRequest,
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
        .map_err(|error| SidecarError::InvalidState(format!("{label} {error}")))
}

pub(crate) fn javascript_sync_rpc_bytes_value(bytes: &[u8]) -> Value {
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
        return Err(SidecarError::InvalidState(format!(
            "ENOTDIR: file descriptor {dir_fd} is not a directory"
        )));
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
        "size": stat.size,
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
    let nanoseconds = nanoseconds.parse::<u64>().map_err(|_| {
        SidecarError::InvalidState("EINVAL: pathname timestamp must be u64 nanoseconds".into())
    })?;
    let seconds = i64::try_from(nanoseconds / 1_000_000_000).map_err(|_| {
        SidecarError::InvalidState("EINVAL: pathname timestamp exceeds i64 seconds".into())
    })?;
    VirtualTimeSpec::new(seconds, (nanoseconds % 1_000_000_000) as u32)
        .map(VirtualUtimeSpec::Set)
        .map_err(|error| SidecarError::InvalidState(format!("EINVAL: {error}")))
}
pub(crate) async fn service_javascript_sync_rpc<B>(
    request: JavascriptSyncRpcServiceRequest<'_, B>,
) -> Result<JavascriptSyncRpcServiceResponse, SidecarError>
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
        return Ok(JavascriptSyncRpcServiceResponse::Raw(bytes));
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
        return Ok(JavascriptSyncRpcServiceResponse::Raw(bytes));
    }
    if request.method == "fs.readdirSync" {
        let kernel_pid = process.kernel_pid;
        let bytes =
            service_javascript_fs_readdir_raw_sync_rpc(kernel, process, kernel_pid, request)?;
        return Ok(JavascriptSyncRpcServiceResponse::Raw(bytes));
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
            // A TTY (PTY-backed) JavaScript process must read its stdin from the
            // kernel PTY slave (fd 0) so cooked-mode line discipline (echo,
            // VERASE/VKILL/VWERASE, ICRNL, VEOF) applies exactly as it does for
            // wasm/python. Non-TTY JS keeps using the in-process local stdin
            // bridge (piped stdin fed via process.execution.write_stdin).
            let js_local_bridge = matches!(process.execution, ActiveExecution::Javascript(_))
                && process.tty_master_fd.is_none()
                && !process.direct_posix_stdin;
            if js_local_bridge {
                match &process.execution {
                    ActiveExecution::Javascript(execution) => execution
                        .read_kernel_stdin_sync_rpc(request)
                        .map_err(|error| SidecarError::Execution(error.to_string())),
                    _ => unreachable!("js_local_bridge implies a JavaScript execution"),
                }
            } else {
                service_javascript_kernel_stdin_sync_rpc(kernel, process, request)
            }
        }
        "__kernel_stdio_write" => {
            service_javascript_kernel_stdio_write_sync_rpc(kernel, process, request)
        }
        "__kernel_isatty" => service_javascript_kernel_isatty_sync_rpc(kernel, process, request),
        "__kernel_tty_size" => {
            service_javascript_kernel_tty_size_sync_rpc(kernel, process, request)
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
            service_javascript_dns_sync_rpc(bridge, kernel, vm_id, dns, request)
        }
        "net.http_listen" | "net.http_close" | "net.http_wait" | "net.http_respond" => {
            return service_javascript_net_sync_rpc_response(JavascriptNetSyncRpcServiceRequest {
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
            return service_javascript_http2_sync_rpc(JavascriptHttp2SyncRpcServiceRequest {
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
            return service_javascript_net_sync_rpc_response(JavascriptNetSyncRpcServiceRequest {
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
            return service_javascript_dgram_sync_rpc(JavascriptDgramSyncRpcServiceRequest {
                bridge,
                kernel,
                vm_id,
                dns,
                socket_paths,
                process,
                kernel_readiness,
                sync_request: request,
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
        "process.take_signal" => {
            let signal = if process.real_interval_timer.take_expiry() {
                Some(libc::SIGALRM)
            } else {
                process.pending_wasm_signals.pop_first()
            };
            process
                .pending_wasm_signals_gauge
                .observe_depth(process.pending_wasm_signals.len());
            Ok(signal.map(Value::from).unwrap_or(Value::Null))
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
                    process.real_interval_timer.set(value_us, interval_us)
                }
                other => {
                    return Err(SidecarError::InvalidState(format!(
                        "EINVAL: invalid ITIMER_REAL operation {other}"
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
                return Err(SidecarError::InvalidState(format!(
                    "EINVAL: invalid waitpid option bits {:#x}",
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
        "process.fd_pipe" => kernel
            .open_pipe(EXECUTION_DRIVER_NAME, process.kernel_pid)
            .map(|(read_fd, write_fd)| json!({ "readFd": read_fd, "writeFd": write_fd }))
            .map_err(kernel_error),
        "process.fd_open" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "fd_open path")?;
            let flags = javascript_sync_rpc_arg_u32(&request.args, 1, "fd_open flags")?;
            let mode = javascript_sync_rpc_arg_u32_optional(&request.args, 2, "fd_open mode")?;
            let path = wasm_process_resolve_at_path(kernel, process.kernel_pid, 0, path)?;
            kernel
                .fd_open(
                    EXECUTION_DRIVER_NAME,
                    process.kernel_pid,
                    &path,
                    flags,
                    mode,
                )
                .map(Value::from)
                .map_err(kernel_error)
        }
        "process.path_open_at" => {
            let dir_fd = javascript_sync_rpc_arg_u32(&request.args, 0, "path_open_at dir fd")?;
            let path = javascript_sync_rpc_arg_str(&request.args, 1, "path_open_at path")?;
            let flags = javascript_sync_rpc_arg_u32(&request.args, 2, "path_open_at flags")?;
            let mode = javascript_sync_rpc_arg_u32_optional(&request.args, 3, "path_open_at mode")?;
            let path = wasm_process_resolve_at_path(kernel, process.kernel_pid, dir_fd, path)?;
            kernel
                .fd_open(
                    EXECUTION_DRIVER_NAME,
                    process.kernel_pid,
                    &path,
                    flags,
                    mode,
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
            if follow {
                kernel.utimes_spec(&path, atime, mtime)
            } else {
                kernel.lutimes(&path, atime, mtime)
            }
            .map(|()| Value::Null)
            .map_err(kernel_error)
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
                .link(&old_path, &new_path)
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
            let path = wasm_process_resolve_at_path(kernel, process.kernel_pid, dir_fd, path)?;
            kernel.remove_dir(&path).map_err(kernel_error)?;
            remove_process_shadow_path(process, &path)?;
            Ok(Value::Null)
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
            kernel.rename(&old_path, &new_path).map_err(kernel_error)?;
            rename_process_shadow_path(process, &old_path, &new_path)?;
            Ok(Value::Null)
        }
        "process.path_symlink_at" => {
            let target = javascript_sync_rpc_arg_str(&request.args, 0, "path_symlink_at target")?;
            let dir_fd = javascript_sync_rpc_arg_u32(&request.args, 1, "path_symlink_at dir fd")?;
            let path = javascript_sync_rpc_arg_str(&request.args, 2, "path_symlink_at path")?;
            let path = wasm_process_resolve_at_path(kernel, process.kernel_pid, dir_fd, path)?;
            kernel
                .symlink(target, &path)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "process.path_unlink_at" => {
            let dir_fd = javascript_sync_rpc_arg_u32(&request.args, 0, "path_unlink_at dir fd")?;
            let path = javascript_sync_rpc_arg_str(&request.args, 1, "path_unlink_at path")?;
            let path = wasm_process_resolve_at_path(kernel, process.kernel_pid, dir_fd, path)?;
            kernel.remove_file(&path).map_err(kernel_error)?;
            remove_process_shadow_path(process, &path)?;
            Ok(Value::Null)
        }
        "process.fd_snapshot" => kernel
            .fd_snapshot(EXECUTION_DRIVER_NAME, process.kernel_pid)
            .map(|entries| {
                Value::Array(
                    entries
                        .into_iter()
                        .map(|entry| {
                            json!({
                                "fd": entry.fd,
                                "fdFlags": entry.fd_flags,
                                "statusFlags": entry.status_flags,
                                "filetype": entry.filetype,
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
                )
            })
            .map_err(kernel_error),
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
            .map(|bytes| javascript_sync_rpc_bytes_value(&bytes))
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
                .map(|bytes| javascript_sync_rpc_bytes_value(&bytes))
                .map_err(kernel_error)
        }
        "process.fd_write" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "fd_write fd")?;
            let data = javascript_sync_rpc_bytes_arg(&request.args, 1, "fd_write data")?;
            // A synchronous WASM RPC cannot park this dispatcher in a
            // blocking pipe write: the reader's RPC must be serviced here as
            // well. The runner polls and retries when a logically blocking fd
            // reports EAGAIN; genuinely nonblocking fds surface EAGAIN.
            let written = if process.runtime == GuestRuntimeKind::WebAssembly {
                kernel.fd_write_nonblocking(EXECUTION_DRIVER_NAME, process.kernel_pid, fd, &data)
            } else {
                kernel.fd_write(EXECUTION_DRIVER_NAME, process.kernel_pid, fd, &data)
            }
            .map_err(kernel_error)?;
            if kernel
                .fd_stat(EXECUTION_DRIVER_NAME, process.kernel_pid, fd)
                .map_err(kernel_error)?
                .filetype
                == agentos_kernel::fd_table::FILETYPE_REGULAR_FILE
            {
                crate::filesystem::mirror_kernel_fd_contents_to_process_shadow(
                    kernel,
                    process,
                    process.kernel_pid,
                    fd,
                )?;
            }
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
            if kernel
                .fd_stat(EXECUTION_DRIVER_NAME, process.kernel_pid, fd)
                .map_err(kernel_error)?
                .filetype
                == agentos_kernel::fd_table::FILETYPE_REGULAR_FILE
            {
                crate::filesystem::mirror_kernel_fd_contents_to_process_shadow(
                    kernel,
                    process,
                    process.kernel_pid,
                    fd,
                )?;
            }
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
                                    "filetype": if entry.is_directory {
                                        agentos_kernel::fd_table::FILETYPE_DIRECTORY
                                    } else if entry.is_symbolic_link {
                                        agentos_kernel::fd_table::FILETYPE_SYMBOLIC_LINK
                                    } else {
                                        agentos_kernel::fd_table::FILETYPE_REGULAR_FILE
                                    },
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
                        "rights": stat.rights,
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
                .map_err(kernel_error)?;
            crate::filesystem::mirror_kernel_fd_contents_to_process_shadow(
                kernel,
                process,
                process.kernel_pid,
                fd,
            )?;
            Ok(Value::Null)
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
                    SidecarError::InvalidState("EINVAL: fd_record_lock start must be u64".into())
                })?;
            let length = javascript_sync_rpc_arg_str(&request.args, 4, "fd_record_lock length")?
                .parse::<u64>()
                .map_err(|_| {
                    SidecarError::InvalidState("EINVAL: fd_record_lock length must be u64".into())
                })?;
            let lock_type = match raw_lock_type {
                0 => agentos_kernel::fd_table::RecordLockType::Read,
                1 => agentos_kernel::fd_table::RecordLockType::Write,
                2 => agentos_kernel::fd_table::RecordLockType::Unlock,
                _ => {
                    return Err(SidecarError::InvalidState(
                        "EINVAL: fd_record_lock type must be F_RDLCK, F_WRLCK, or F_UNLCK".into(),
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
                    return Err(SidecarError::InvalidState(format!(
                        "EINVAL: unsupported fd_record_lock command {command}"
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
        "process.fd_chdir_path" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "fchdir fd")?;
            let stat = kernel
                .fd_stat(EXECUTION_DRIVER_NAME, process.kernel_pid, fd)
                .map_err(kernel_error)?;
            if stat.filetype != agentos_kernel::fd_table::FILETYPE_DIRECTORY {
                return Err(SidecarError::InvalidState(format!(
                    "ENOTDIR: file descriptor {fd} is not a directory"
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
        "process.fd_sendmsg_rights" => {
            let socket_fd = javascript_sync_rpc_arg_u32(&request.args, 0, "sendmsg socket fd")?;
            let data = javascript_sync_rpc_bytes_arg(&request.args, 1, "sendmsg data")?;
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
                return Err(SidecarError::InvalidState(format!(
                    "EINVAL: SCM_RIGHTS accepts at most {LINUX_SCM_MAX_FD} descriptors"
                )));
            }
            if let Some(limit) = kernel.resource_limits().max_open_fds {
                if raw_rights.len() > limit {
                    return Err(SidecarError::InvalidState(format!(
                        "EMFILE: SCM_RIGHTS descriptor list has {} entries, exceeding limits.resources.maxOpenFds ({limit}); raise limits.resources.maxOpenFds",
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
                    }
                };
                register_host_net_transfer_description(
                    &socket_paths.host_net_transfer_descriptions,
                    &transferred,
                );
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
                            let transferred = Arc::downcast::<TransferredHostNetSocket>(resource)
                                .map_err(|_| {
                                SidecarError::InvalidState(
                                    "received unknown SCM_RIGHTS resource type".into(),
                                )
                            })?;
                            let transferred = match Arc::try_unwrap(transferred) {
                                Ok(transferred) => transferred,
                                Err(shared) => shared.clone_for_fd_transfer()?,
                            };
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
                                        process.execution.javascript_v8_session_handle(),
                                        Some(identity),
                                    );
                                    register_kernel_readiness_target(
                                        &kernel_readiness,
                                        socket.kernel_socket_id,
                                        process.execution.javascript_v8_session_handle(),
                                        Some(Arc::clone(&socket.read_event_notify)),
                                        process.capability_readiness_identity(&capability_key),
                                        socket_id.clone(),
                                        KernelSocketReadinessEvent::Data,
                                    );
                                    let local = socket.guest_local_addr;
                                    let remote = socket.guest_remote_addr;
                                    process.tcp_sockets.insert(socket_id.clone(), *socket);
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
                                        process.execution.javascript_v8_session_handle(),
                                        None,
                                        process.capability_readiness_identity(&capability_key),
                                        listener_id.clone(),
                                        KernelSocketReadinessEvent::Accept,
                                    );
                                    process.tcp_listeners.insert(listener_id.clone(), listener);
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
                                        process.execution.javascript_v8_session_handle(),
                                        Some(identity),
                                    );
                                    register_kernel_readiness_target(
                                        &kernel_readiness,
                                        socket.kernel_socket_id,
                                        process.execution.javascript_v8_session_handle(),
                                        Some(Arc::clone(&socket.read_event_notify)),
                                        process.capability_readiness_identity(&capability_key),
                                        socket_id.clone(),
                                        KernelSocketReadinessEvent::Datagram,
                                    );
                                    process.udp_sockets.insert(socket_id.clone(), socket);
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
                                        process.execution.javascript_v8_session_handle(),
                                        Some(identity),
                                    );
                                    process.unix_sockets.insert(socket_id.clone(), socket);
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
                                        process.execution.javascript_v8_session_handle(),
                                        Some(identity),
                                    );
                                    process.unix_listeners.insert(listener_id.clone(), listener);
                                    rights.push(transferred_hostnet_value(
                                        "unix-listener",
                                        metadata,
                                        Some(("serverId", listener_id)),
                                        Some(identity),
                                        None,
                                        None,
                                    ));
                                }
                                TransferredHostNetSocket::Pending { metadata, .. } => {
                                    rights.push(transferred_hostnet_value(
                                        "pending", metadata, None, None, None, None,
                                    ));
                                }
                            }
                        }
                    }
                }
                json!({
                    "data": javascript_sync_rpc_bytes_value(&message.payload),
                    "rights": rights,
                    "payloadTruncated": message.payload_truncated,
                    "controlTruncated": message.control_truncated,
                    "fullLength": message.full_length,
                })
            } else {
                json!({
                    "data": javascript_sync_rpc_bytes_value(&[]),
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
            if !matches!(
                canonical_signal_name(parsed_signal),
                Some("SIGWINCH" | "SIGCHLD" | "SIGCONT" | "SIGURG")
            ) {
                apply_active_process_default_signal(kernel, process, parsed_signal)?;
            }
            Ok(json!({
                "self": true,
                "action": "default",
            }))
        }
        "process.umask" => {
            let new_mask = javascript_sync_rpc_arg_u32_optional(&request.args, 0, "process umask")?;
            kernel
                .umask(EXECUTION_DRIVER_NAME, process.kernel_pid, new_mask)
                .map(|mask| json!(mask))
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
                .getpwuid(uid)
                .map(|entry| json!(entry))
                .map_err(kernel_error)
        }
        "process.getpwnam" => {
            let name = javascript_sync_rpc_arg_str(&request.args, 0, "passwd name")?;
            kernel
                .getpwnam(name)
                .map(|entry| json!(entry))
                .map_err(kernel_error)
        }
        "process.getpwent" => {
            let index = javascript_sync_rpc_arg_u32(&request.args, 0, "passwd index")?;
            kernel
                .getpwent(index as usize)
                .map(|entry| json!(entry))
                .map_err(kernel_error)
        }
        "process.getgrgid" => {
            let gid = javascript_sync_rpc_arg_u32(&request.args, 0, "group gid")?;
            kernel
                .getgrgid(gid)
                .map(|entry| json!(entry))
                .map_err(kernel_error)
        }
        "process.getgrnam" => {
            let name = javascript_sync_rpc_arg_str(&request.args, 0, "group name")?;
            kernel
                .getgrnam(name)
                .map(|entry| json!(entry))
                .map_err(kernel_error)
        }
        "process.getgrent" => {
            let index = javascript_sync_rpc_arg_u32(&request.args, 0, "group index")?;
            kernel
                .getgrent(index as usize)
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
        "fs.chmodSync" | "fs.promises.chmod" => {
            let response =
                service_javascript_fs_sync_rpc(kernel, process, process.kernel_pid, request)?;
            mirror_process_chmod_to_host(process, request)?;
            Ok(response)
        }
        _ => service_javascript_fs_sync_rpc(kernel, process, process.kernel_pid, request),
    }?;
    Ok(response.into())
}

fn service_javascript_internal_bridge_sync_rpc(
    process: &ActiveProcess,
    request: &JavascriptSyncRpcRequest,
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
    .ok_or_else(|| {
        SidecarError::InvalidState(format!(
            "JavaScript internal bridge method {method} returned no value"
        ))
    })
}

fn mirror_process_chmod_to_host(
    process: &ActiveProcess,
    request: &JavascriptSyncRpcRequest,
) -> Result<(), SidecarError> {
    let guest_path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem chmod path")?;
    let mode = javascript_sync_rpc_arg_u32(&request.args, 1, "filesystem chmod mode")? & 0o7777;
    let Some(host_path) = resolve_process_guest_path_to_host(process, guest_path) else {
        return Ok(());
    };
    if !host_path.exists() {
        return Ok(());
    }
    fs::set_permissions(&host_path, fs::Permissions::from_mode(mode)).map_err(|error| {
        SidecarError::Io(format!(
            "failed to mirror chmod to host path {}: {error}",
            host_path.display()
        ))
    })
}

fn resolve_process_guest_path_to_host(
    process: &ActiveProcess,
    guest_path: &str,
) -> Option<PathBuf> {
    let normalized_guest_path = if guest_path.starts_with('/') {
        normalize_path(guest_path)
    } else {
        normalize_path(&format!(
            "{}/{}",
            process.guest_cwd.trim_end_matches('/'),
            guest_path
        ))
    };
    if let Some(host_path) =
        host_path_from_runtime_guest_mappings(&process.env, &normalized_guest_path)
    {
        return Some(host_path);
    }
    let normalized_guest_cwd = normalize_path(&process.guest_cwd);
    let mut host_root = normalize_host_path(&process.host_cwd);
    for _ in normalized_guest_cwd
        .trim_start_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
    {
        host_root = host_root.parent()?.to_path_buf();
    }
    if normalized_guest_path == "/" {
        Some(host_root)
    } else {
        Some(host_root.join(normalized_guest_path.trim_start_matches('/')))
    }
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
    request: &JavascriptSyncRpcRequest,
    capabilities: &CapabilityRegistry,
) -> Result<Option<JavascriptSyncRpcServiceResponse>, SidecarError> {
    let operation_deadline = reactor_io_limits(&process.limits).operation_deadline;
    let deferred = |receiver| JavascriptSyncRpcServiceResponse::Deferred {
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
            let options: JavascriptTlsBridgeOptions =
                serde_json::from_str(options_json).map_err(|error| {
                    SidecarError::InvalidState(format!(
                        "net.socket_upgrade_tls options must be valid JSON: {error}"
                    ))
                })?;
            if process
                .capability_leases
                .contains_key(&NativeCapabilityKey::TlsSocket(socket_id.to_owned()))
            {
                return Err(SidecarError::Execution(format!(
                    "EALREADY: TCP socket {socket_id} is already upgraded to TLS"
                )));
            }
            let pending = reserve_capability(capabilities, CapabilityKind::TlsTransport)?;
            let socket = process.tcp_sockets.get(socket_id).ok_or_else(|| {
                SidecarError::InvalidState(format!(
                    "unknown TCP socket {socket_id} for TLS upgrade"
                ))
            })?;
            let receiver = socket.upgrade_tls(vm_id, kernel, options)?;
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
    request: &JavascriptSyncRpcRequest,
) -> Result<Option<JavascriptSyncRpcServiceResponse>, SidecarError> {
    let deferred = |receiver| JavascriptSyncRpcServiceResponse::Deferred {
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

fn service_javascript_net_sync_rpc_response<B>(
    request: JavascriptNetSyncRpcServiceRequest<'_, B>,
) -> Result<JavascriptSyncRpcServiceResponse, SidecarError>
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
            return Ok(JavascriptSyncRpcServiceResponse::Json(Value::Null));
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
            return Ok(JavascriptSyncRpcServiceResponse::Json(Value::Null));
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
                let result = match tokio::time::timeout(operation_deadline, close_completion).await {
                    Ok(Ok(())) => Ok(Value::Null),
                    Ok(Err(_)) => Err(crate::state::DeferredRpcError {
                        code: String::from("ERR_AGENTOS_LISTENER_CLOSE"),
                        message: format!(
                            "Unix listener {listener_id} close task ended without acknowledgement"
                        ),
                    }),
                    Err(_) => Err(crate::state::DeferredRpcError {
                        code: String::from("ETIMEDOUT"),
                        message: format!(
                            "Unix listener {listener_id} close exceeded {}ms; raise limits.reactor.operationDeadlineMs",
                            operation_deadline.as_millis()
                        ),
                    }),
                };
                if respond_to.send(result).is_err() {
                    eprintln!(
                        "ERR_AGENTOS_LISTENER_CLOSE_COMPLETION_DROPPED: caller stopped waiting for Unix listener {listener_id}"
                    );
                }
            })
            .map_err(SidecarError::from)?;
        return Ok(JavascriptSyncRpcServiceResponse::Deferred {
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
            && [JavascriptSocketFamily::Ipv4, JavascriptSocketFamily::Ipv6]
                .iter()
                .any(|family| {
                    let family_number = match family {
                        JavascriptSocketFamily::Ipv4 => 4,
                        JavascriptSocketFamily::Ipv6 => 6,
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
        return Ok(JavascriptSyncRpcServiceResponse::Deferred {
            receiver,
            timeout: None,
            task_class: agentos_runtime::TaskClass::Listener,
        });
    }
    if request.sync_request.method != "net.socket_read" {
        return service_javascript_net_sync_rpc(request).map(Into::into);
    }

    let JavascriptNetSyncRpcServiceRequest {
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
        Some(JavascriptTcpSocketEvent::Data {
            bytes,
            reservation,
            mut source_reservations,
        }) => {
            // The bridge registry already reserved this call's declared
            // response maximum before host visibility. Keep the transport
            // ownership live through handoff, but do not charge the response
            // bytes a second time.
            source_reservations.push(reservation);
            Ok(JavascriptSyncRpcServiceResponse::SourceBackedRaw {
                payload: bytes,
                source_reservations,
            })
        }
        other => javascript_net_read_value(other).map(Into::into),
    }
}

async fn service_javascript_dgram_poll_response(
    socket_paths: &JavascriptSocketPathContext,
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    request: &JavascriptSyncRpcRequest,
) -> Result<JavascriptSyncRpcServiceResponse, SidecarError> {
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
        Some(JavascriptUdpSocketEvent::Message {
            data,
            remote_addr,
            _byte_reservation,
            _datagram_reservation,
            _udp_byte_reservation,
            _udp_datagram_reservation,
        }) => {
            let family = JavascriptSocketFamily::from_ip(remote_addr.ip());
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
                fields.insert(String::from("data"), javascript_sync_rpc_bytes_value(&data));
            }
            Ok(JavascriptSyncRpcServiceResponse::SourceBackedJson {
                value: response,
                source_reservations: vec![
                    _byte_reservation,
                    _datagram_reservation,
                    _udp_byte_reservation,
                    _udp_datagram_reservation,
                ],
            })
        }
        Some(JavascriptUdpSocketEvent::Error { code, message }) => {
            Ok(JavascriptSyncRpcServiceResponse::Json(json!({
                "type": "error",
                "code": code,
                "message": message,
            })))
        }
        None => Ok(JavascriptSyncRpcServiceResponse::Json(Value::Null)),
    }
}

pub(crate) fn service_javascript_net_sync_rpc<B>(
    request: JavascriptNetSyncRpcServiceRequest<'_, B>,
) -> Result<Value, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let JavascriptNetSyncRpcServiceRequest {
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
            let mut listener = ActiveTcpListener::bind(
                bind_host,
                guest_host,
                port,
                Some(DEFAULT_JAVASCRIPT_NET_BACKLOG),
            )?;
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
            .map_err(|error| SidecarError::Execution(format!("ERR_AGENTOS_NODE_SYNC_RPC: {error}")))
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
                            if guest_errno_code(&error.to_string()) != Some("EADDRINUSE") {
                                return Err(error);
                            }
                        }
                    }
                }
                bound.ok_or_else(|| {
                    SidecarError::Execution(String::from(
                        "EADDRINUSE: Linux AF_UNIX autobind namespace exhausted after 4096 attempts",
                    ))
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
                process.execution.javascript_v8_session_handle(),
                Some(identity),
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
                                || guest_errno_code(&error.to_string()) != Some("EADDRINUSE")
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
                    JavascriptSocketFamily::Ipv4 => "IPv4",
                    JavascriptSocketFamily::Ipv6 => "IPv6",
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
                    process.execution.javascript_v8_session_handle(),
                    Some(identity),
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
                    let families = [JavascriptSocketFamily::Ipv4, JavascriptSocketFamily::Ipv6];
                    if let Some((family, target)) = families.iter().find_map(|family| {
                        let family_number = match family {
                            JavascriptSocketFamily::Ipv4 => 4,
                            JavascriptSocketFamily::Ipv6 => 6,
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
                            JavascriptSocketFamily::Ipv4 => "127.0.0.1",
                            JavascriptSocketFamily::Ipv6 => "::1",
                        };
                        return Ok(json!({
                            "loopbackHttpTarget": {
                                "processId": target.process_id.clone(),
                                "serverId": target.server_id,
                                "host": remote_address,
                                "port": port,
                            },
                            "localAddress": match family {
                                JavascriptSocketFamily::Ipv4 => "127.0.0.1",
                                JavascriptSocketFamily::Ipv6 => "::1",
                            },
                            "localPort": payload.local_port.unwrap_or(0),
                            "remoteAddress": remote_address,
                            "remotePort": port,
                            "remoteFamily": match family {
                                JavascriptSocketFamily::Ipv4 => "IPv4",
                                JavascriptSocketFamily::Ipv6 => "IPv6",
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
                        let _ = socket.close(kernel, process.kernel_pid);
                        return Err(error);
                    }
                };
                socket.set_event_pusher(
                    process.execution.javascript_v8_session_handle(),
                    Some(identity),
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
                    process.execution.javascript_v8_session_handle(),
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
                    process.execution.javascript_v8_session_handle(),
                    Some(identity),
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
                                if guest_errno_code(&error.to_string()) != Some("EADDRINUSE") {
                                    return Err(error);
                                }
                            }
                        }
                    }
                    bound.ok_or_else(|| {
                        SidecarError::Execution(String::from(
                            "EADDRINUSE: Linux AF_UNIX autobind namespace exhausted after 4096 attempts",
                        ))
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
                    process.execution.javascript_v8_session_handle(),
                    Some(identity),
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
                        let _ = listener.close(kernel, process.kernel_pid);
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
                    process.execution.javascript_v8_session_handle(),
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
                Some(JavascriptTcpSocketEvent::Data { bytes: chunk, .. }) => Ok(json!({
                    "type": "data",
                    "data": javascript_sync_rpc_bytes_value(&chunk),
                })),
                Some(JavascriptTcpSocketEvent::End) => Ok(json!({
                    "type": "end",
                })),
                Some(JavascriptTcpSocketEvent::Error { code, message }) => Ok(json!({
                    "type": "error",
                    "code": code,
                    "message": message,
                })),
                Some(JavascriptTcpSocketEvent::Close { had_error }) => {
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
                javascript_net_json_string(socket.socket_info(), "net.socket_wait_connect")
            } else {
                let socket = process.unix_sockets.get(socket_id).ok_or_else(|| {
                    SidecarError::InvalidState(format!("unknown net socket {socket_id}"))
                })?;
                javascript_net_json_string(socket.socket_info(), "net.socket_wait_connect")
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
                javascript_net_read_value(socket.poll(
                    kernel,
                    process.kernel_pid,
                    Duration::ZERO,
                    trace_enabled,
                )?)
            } else if let Some(socket) = process.unix_sockets.get_mut(socket_id) {
                socket.set_application_read_interest(true)?;
                javascript_net_read_value(socket.poll(Duration::ZERO)?)
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
                            "code": javascript_sync_rpc_error_code(&error),
                            "message": javascript_sync_rpc_error_message(&error),
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
                    Some(JavascriptTcpListenerEvent::Connection(pending)) => {
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
                                let _ = socket.close(kernel, process.kernel_pid);
                                return Err(error);
                            }
                        };
                        socket.set_event_pusher(
                            process.execution.javascript_v8_session_handle(),
                            Some(identity),
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
                            process.execution.javascript_v8_session_handle(),
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
                    Some(JavascriptTcpListenerEvent::Error { code, message }) => Ok(json!({
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
                Some(JavascriptUnixListenerEvent::Connection {
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
                        process.execution.javascript_v8_session_handle(),
                        Some(identity),
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
                Some(JavascriptUnixListenerEvent::Error { code, message }) => Ok(json!({
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
                    Some(JavascriptTcpListenerEvent::Connection(pending)) => {
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
                                let _ = socket.close(kernel, process.kernel_pid);
                                return Err(error);
                            }
                        };
                        socket.set_event_pusher(
                            process.execution.javascript_v8_session_handle(),
                            Some(identity),
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
                            process.execution.javascript_v8_session_handle(),
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
                        javascript_net_json_string(
                            json!({
                                "socketId": socket_id,
                                "info": info,
                            }),
                            "net.server_accept",
                        )
                    }
                    Some(JavascriptTcpListenerEvent::Error { code, message }) => {
                        let detail = code.unwrap_or_else(|| String::from("server accept"));
                        Err(SidecarError::Execution(format!("{detail}: {message}")))
                    }
                    None => Ok(javascript_net_timeout_value()),
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
                Some(JavascriptUnixListenerEvent::Connection {
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
                        process.execution.javascript_v8_session_handle(),
                        Some(identity),
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
                    javascript_net_json_string(
                        json!({
                            "socketId": socket_id,
                            "info": info,
                        }),
                        "net.server_accept",
                    )
                }
                Some(JavascriptUnixListenerEvent::Error { code, message }) => {
                    let detail = code.unwrap_or_else(|| String::from("server accept"));
                    Err(SidecarError::Execution(format!("{detail}: {message}")))
                }
                None => Ok(javascript_net_timeout_value()),
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
        "tls.get_ciphers" => javascript_net_json_string(
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

fn resolve_guest_unix_path(
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

fn reject_host_mounted_unix_socket_path(
    context: &JavascriptSocketPathContext,
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

fn allocate_guest_socket_host_path(
    context: &JavascriptSocketPathContext,
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

fn format_unix_socket_resource(
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

pub(crate) fn error_code(error: &SidecarError) -> &'static str {
    match error {
        SidecarError::ResourceLimit(_) => "ERR_AGENTOS_RESOURCE_LIMIT",
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
        SidecarError::Bridge(_) => "bridge_error",
        SidecarError::Io(_) => "io_error",
    }
}

pub(in crate::execution) fn guest_errno_code(message: &str) -> Option<&str> {
    const TRUSTED_PREFIXES: &[&str] = &[
        "ERR_AGENTOS_NODE_SYNC_RPC",
        "ERR_AGENTOS_PYTHON_VFS_RPC",
        "ERR_AGENTOS_BRIDGE",
    ];

    let mut segments = message.split(':').map(str::trim);
    let first = segments.next()?;
    if is_guest_errno_segment(first) {
        return Some(first);
    }

    if TRUSTED_PREFIXES.contains(&first) {
        let second = segments.next()?;
        if is_guest_errno_segment(second) {
            return Some(second);
        }
    }

    None
}

fn is_guest_errno_segment(segment: &str) -> bool {
    segment.len() >= 2
        && segment.starts_with('E')
        && !segment.starts_with("ERR_")
        && segment[1..]
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

pub(crate) fn javascript_sync_rpc_error_code(error: &SidecarError) -> String {
    let message = error.to_string();
    for code in [
        "ERR_SOCKET_BAD_PORT",
        "ERR_SOCKET_DGRAM_IS_CONNECTED",
        "ERR_SOCKET_DGRAM_NOT_CONNECTED",
        "ERR_SOCKET_DGRAM_NOT_RUNNING",
    ] {
        if message
            .strip_prefix(code)
            .is_some_and(|suffix| suffix.starts_with(':'))
        {
            return code.to_owned();
        }
    }
    if let Some(code) = guest_errno_code(&message) {
        return code.to_owned();
    }
    if message.starts_with("ERR_NATIVE_BINARY_NOT_SUPPORTED:") {
        return String::from("ERR_NATIVE_BINARY_NOT_SUPPORTED");
    }

    let lower = message.to_ascii_lowercase();
    if lower.contains("no such file or directory")
        || lower.contains("entry not found")
        || lower.contains("not found")
    {
        return String::from("ENOENT");
    }
    if lower.contains("permission denied") {
        return String::from("EACCES");
    }
    if lower.contains("already exists")
        || lower.contains("already registered")
        || lower.contains("file exists")
    {
        return String::from("EEXIST");
    }
    if lower.contains("invalid argument") {
        return String::from("EINVAL");
    }

    String::from("ERR_AGENTOS_NODE_SYNC_RPC")
}

pub(in crate::execution) fn javascript_sync_rpc_error_message(error: &SidecarError) -> String {
    match error {
        SidecarError::ResourceLimit(limit) => crate::state::guest_limit_message(limit),
        _ => error.to_string(),
    }
}

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
        guest_errno_code, ignore_stale_javascript_sync_rpc_response,
        javascript_sync_rpc_error_code, javascript_sync_rpc_error_message, SidecarError,
    };
    use agentos_runtime::accounting::{LimitError, ResourceClass};

    #[test]
    fn guest_errno_code_rejects_guest_controlled_errno_segments() {
        assert_eq!(guest_errno_code("user said 'EACCES: denied'"), None);
        assert_eq!(
            guest_errno_code("prefix: user said 'EPERM': more text"),
            None
        );
        assert_eq!(guest_errno_code("ERR_AGENTOS_FAKE: EACCES: denied"), None);
    }

    #[test]
    fn guest_errno_code_accepts_trusted_agentos_prefixes() {
        assert_eq!(
            guest_errno_code("ERR_AGENTOS_NODE_SYNC_RPC: EACCES: permission denied on /foo"),
            Some("EACCES")
        );
        assert_eq!(
            guest_errno_code("ERR_AGENTOS_PYTHON_VFS_RPC: ENOENT: missing file"),
            Some("ENOENT")
        );
        assert_eq!(guest_errno_code("EEXIST: already exists"), Some("EEXIST"));
    }

    #[test]
    fn javascript_sync_rpc_error_code_ignores_spoofed_errnos() {
        let error = SidecarError::Execution(String::from("user said 'EACCES: denied'"));
        assert_eq!(
            javascript_sync_rpc_error_code(&error),
            "ERR_AGENTOS_NODE_SYNC_RPC"
        );
    }

    #[test]
    fn javascript_sync_rpc_error_code_preserves_real_sidecar_errnos() {
        let error = SidecarError::Execution(String::from(
            "ERR_AGENTOS_NODE_SYNC_RPC: EACCES: permission denied on /foo",
        ));
        assert_eq!(javascript_sync_rpc_error_code(&error), "EACCES");
    }

    #[test]
    fn javascript_sync_rpc_error_code_preserves_dgram_state_errors() {
        for code in [
            "ERR_SOCKET_BAD_PORT",
            "ERR_SOCKET_DGRAM_IS_CONNECTED",
            "ERR_SOCKET_DGRAM_NOT_CONNECTED",
            "ERR_SOCKET_DGRAM_NOT_RUNNING",
        ] {
            let error = SidecarError::Execution(format!("{code}: dgram state error"));
            assert_eq!(javascript_sync_rpc_error_code(&error), code);
        }
    }

    #[test]
    fn javascript_sync_rpc_error_code_maps_file_exists_messages() {
        let error = SidecarError::Io(String::from(
            "failed to create mapped guest directory /.next/server: File exists (os error 17)",
        ));
        assert_eq!(javascript_sync_rpc_error_code(&error), "EEXIST");
    }

    #[test]
    fn javascript_sync_rpc_error_code_preserves_native_binary_rejections() {
        let error = SidecarError::Execution(String::from(
            "ERR_NATIVE_BINARY_NOT_SUPPORTED: refused to execute native ELF guest binary at /tmp/fake-rg inside the VM",
        ));
        assert_eq!(
            javascript_sync_rpc_error_code(&error),
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
        let message = javascript_sync_rpc_error_message(&error);
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
        deferred_child_kernel_wait_request, remap_wasm_process_sync_rpc, JavascriptSyncRpcRequest,
        ALLOWED_WASM_PROCESS_SYNC_RPCS,
    };
    use serde_json::json;
    use std::collections::{BTreeSet, HashMap};

    fn emitted_wasm_process_sync_rpcs() -> BTreeSet<&'static str> {
        let source = include_str!("../../../../execution/src/wasm.rs");
        let start = source
            .find("case \"process.getpgid\":")
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
                    .filter(|method| method.starts_with("process."))
            })
            .collect()
    }

    #[test]
    fn every_emitted_wasm_process_rpc_is_unwrapped_to_the_direct_handler_shape() {
        let emitted = emitted_wasm_process_sync_rpcs();
        assert!(!emitted.is_empty(), "expected WASM process RPC methods");
        let allowed = ALLOWED_WASM_PROCESS_SYNC_RPCS
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let missing = emitted.difference(&allowed).copied().collect::<Vec<_>>();
        assert!(
            missing.is_empty(),
            "WASM emits process RPCs the sidecar wrapper does not allow: {missing:?}"
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

        for method in emitted {
            assert!(
                service_source.contains(&format!("\"{method}\"")),
                "WASM emits {method}, but the direct sync RPC dispatcher has no handler"
            );
            let direct = JavascriptSyncRpcRequest {
                id: 17,
                method: method.to_owned(),
                args: vec![json!({ "marker": method })],
                raw_bytes_args: HashMap::from([(0, vec![1, 2, 3])]),
            };
            let wrapped = JavascriptSyncRpcRequest {
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
        let request = JavascriptSyncRpcRequest {
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
}
