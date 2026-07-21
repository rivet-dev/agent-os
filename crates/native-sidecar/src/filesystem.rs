//! Guest filesystem and VFS dispatch extracted from service.rs.

use crate::protocol::{GuestFilesystemCallRequest, RequestFrame, ResponsePayload};
use crate::service::{
    host_bytes_value, javascript_sync_rpc_arg_str, javascript_sync_rpc_arg_u32,
    javascript_sync_rpc_arg_u32_optional, javascript_sync_rpc_arg_u64,
    javascript_sync_rpc_arg_u64_optional, javascript_sync_rpc_bytes_arg,
    javascript_sync_rpc_encoding, javascript_sync_rpc_option_bool, javascript_sync_rpc_option_u32,
    kernel_error, normalize_path,
};
use crate::state::{
    ActiveExecutionEvent, ActiveProcess, BridgeError, SidecarKernel, EXECUTION_DRIVER_NAME,
};
use crate::{DispatchResult, NativeSidecar, NativeSidecarBridge, SidecarError};

use agentos_execution::{
    backend::HostServiceError, HostRpcRequest, LocalResolvedModuleFormat, ModuleFsReader,
    ModuleResolveMode, ModuleResolver,
};
use agentos_kernel::kernel::is_internal_unnamed_file_name;
use agentos_kernel::vfs::{VirtualStat, VirtualTimeSpec, VirtualUtimeSpec};
use agentos_native_sidecar_core::handle_guest_filesystem_call as core_guest_filesystem_call;
use nix::libc;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::env;
use std::fmt;
use std::fs;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

fn kernel_path_error(
    operation: &str,
    path: &str,
    error: impl Into<agentos_kernel::kernel::KernelError>,
) -> SidecarError {
    let error = error.into();
    let base = SidecarError::host(
        error.code(),
        format!("{operation} {path}: {}", error.message()),
    );
    if std::env::var_os("AGENTOS_TRACE_FS_ERRORS").is_some() {
        eprintln!("[agent-os-fs-error] operation={operation} path={path} error={base}");
    }
    base
}

fn classify_fiemap_ranges(
    allocated: Vec<(u64, u64)>,
    unwritten: &[(u64, u64)],
) -> Vec<(u64, u64, bool)> {
    let mut classified = Vec::new();
    for (start, end) in allocated {
        let mut cursor = start;
        for &(unwritten_start, unwritten_end) in unwritten {
            if unwritten_end <= cursor || unwritten_start >= end {
                continue;
            }
            if cursor < unwritten_start {
                classified.push((cursor, unwritten_start.min(end), false));
            }
            let overlap_start = cursor.max(unwritten_start);
            let overlap_end = end.min(unwritten_end);
            if overlap_start < overlap_end {
                classified.push((overlap_start, overlap_end, true));
                cursor = overlap_end;
            }
            if cursor == end {
                break;
            }
        }
        if cursor < end {
            classified.push((cursor, end, false));
        }
    }
    classified
}

const UTIME_NOW_NSEC: i64 = libc::UTIME_NOW;
const UTIME_OMIT_NSEC: i64 = libc::UTIME_OMIT;

fn parse_timespec_seconds(value: f64, label: &str) -> Result<VirtualTimeSpec, SidecarError> {
    if !value.is_finite() {
        return Err(SidecarError::InvalidState(format!(
            "{label} must be a finite numeric value"
        )));
    }
    let seconds = value.floor();
    let mut sec = seconds as i64;
    let mut nanos = ((value - seconds) * 1_000_000_000.0).round() as i64;
    if nanos >= 1_000_000_000 {
        sec = sec.saturating_add(1);
        nanos -= 1_000_000_000;
    }
    VirtualTimeSpec::new(sec, nanos as u32)
        .map_err(|error| SidecarError::InvalidState(format!("{label}: {error}")))
}

fn parse_timespec_integer(value: &Value, label: &str) -> Result<i64, SidecarError> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .ok_or_else(|| SidecarError::InvalidState(format!("{label} must be an integer")))
}

fn parse_utime_spec_value(value: &Value, label: &str) -> Result<VirtualUtimeSpec, SidecarError> {
    if let Some(number) = value.as_f64() {
        return parse_timespec_seconds(number, label).map(VirtualUtimeSpec::Set);
    }

    let Some(object) = value.as_object() else {
        return Err(SidecarError::InvalidState(format!(
            "{label} must be a numeric seconds value or {{ sec, nsec }}"
        )));
    };

    if let Some(kind) = object.get("kind").and_then(Value::as_str) {
        return match kind {
            "now" | "UTIME_NOW" => Ok(VirtualUtimeSpec::Now),
            "omit" | "UTIME_OMIT" => Ok(VirtualUtimeSpec::Omit),
            other => Err(SidecarError::InvalidState(format!(
                "{label} kind must be 'now' or 'omit', got {other}"
            ))),
        };
    }

    let Some(nsec_value) = object.get("nsec") else {
        return Err(SidecarError::InvalidState(format!(
            "{label} timespec requires nsec"
        )));
    };
    if let Some(text) = nsec_value.as_str() {
        return match text {
            "UTIME_NOW" => Ok(VirtualUtimeSpec::Now),
            "UTIME_OMIT" => Ok(VirtualUtimeSpec::Omit),
            _ => Err(SidecarError::InvalidState(format!(
                "{label} nsec must be numeric, UTIME_NOW, or UTIME_OMIT"
            ))),
        };
    }
    if let Some(integer) = nsec_value.as_i64().or_else(|| {
        nsec_value
            .as_u64()
            .and_then(|value| i64::try_from(value).ok())
    }) {
        if integer == UTIME_NOW_NSEC {
            return Ok(VirtualUtimeSpec::Now);
        }
        if integer == UTIME_OMIT_NSEC {
            return Ok(VirtualUtimeSpec::Omit);
        }
    }

    let sec_value = object
        .get("sec")
        .ok_or_else(|| SidecarError::InvalidState(format!("{label} timespec requires sec")))?;
    let sec = parse_timespec_integer(sec_value, &format!("{label}.sec"))?;
    let nsec = u32::try_from(parse_timespec_integer(
        nsec_value,
        &format!("{label}.nsec"),
    )?)
    .map_err(|_| SidecarError::InvalidState(format!("{label}.nsec must fit within u32")))?;
    VirtualTimeSpec::new(sec, nsec)
        .map(VirtualUtimeSpec::Set)
        .map_err(|error| SidecarError::InvalidState(format!("{label}: {error}")))
}

fn parse_utime_arg(
    args: &[Value],
    index: usize,
    label: &str,
) -> Result<VirtualUtimeSpec, SidecarError> {
    let value = args
        .get(index)
        .ok_or_else(|| SidecarError::InvalidState(format!("{label} is required")))?;
    parse_utime_spec_value(value, label)
}

pub(crate) async fn guest_filesystem_call<B>(
    sidecar: &mut NativeSidecar<B>,
    request: &RequestFrame,
    payload: GuestFilesystemCallRequest,
) -> Result<DispatchResult, SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let (connection_id, session_id, vm_id) = sidecar.vm_scope_for(&request.ownership)?;
    sidecar.require_owned_vm(&connection_id, &session_id, &vm_id)?;

    let response = {
        let vm = match sidecar.vms.get_mut(&vm_id) {
            Some(vm) => vm,
            None => {
                return Err(stale_filesystem_request_error(
                    sidecar,
                    &vm_id,
                    None,
                    "guest filesystem dispatch",
                ));
            }
        };
        core_guest_filesystem_call(&mut vm.kernel, payload)
            .map_err(native_guest_filesystem_core_error)?
    };

    Ok(DispatchResult {
        response: sidecar.respond(request, ResponsePayload::GuestFilesystemResult(response)),
        events: Vec::new(),
    })
}

fn native_guest_filesystem_core_error(
    error: agentos_native_sidecar_core::SidecarCoreError,
) -> SidecarError {
    match error.code() {
        Some(code) => SidecarError::Host(agentos_execution::backend::HostServiceError::new(
            code,
            error.message(),
        )),
        None => SidecarError::InvalidState(error.to_string()),
    }
}

fn stale_filesystem_request_error<B>(
    sidecar: &NativeSidecar<B>,
    vm_id: &str,
    process_id: Option<&str>,
    context: &str,
) -> SidecarError
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let message = match process_id {
        Some(process_id) => format!(
            "Ignoring stale filesystem request during {context}: VM {vm_id} process {process_id} was already reaped"
        ),
        None => format!(
            "Ignoring stale filesystem request during {context}: VM {vm_id} was already reaped"
        ),
    };
    if let Err(error) = sidecar.bridge.emit_log(vm_id, message.clone()) {
        eprintln!(
            "ERR_AGENTOS_DIAGNOSTIC_EMIT: failed to emit stale filesystem request diagnostic for VM {vm_id}: {error:?}"
        );
    }
    SidecarError::InvalidState(message)
}

/// Kernel-VFS-backed reader for resolver unit tests and kernel-only callers.
#[cfg(test)]
struct KernelModuleFsReader<'a> {
    kernel: &'a mut SidecarKernel,
}

#[cfg(test)]
impl ModuleFsReader for KernelModuleFsReader<'_> {
    fn canonical_guest_path(
        &mut self,
        guest_path: &str,
    ) -> Result<Option<String>, HostServiceError> {
        module_kernel_optional(self.kernel.realpath(guest_path))
    }

    fn read_to_string(&mut self, guest_path: &str) -> Result<Option<String>, HostServiceError> {
        let Some(bytes) = module_kernel_optional(self.kernel.read_file(guest_path))? else {
            return Ok(None);
        };
        module_utf8(guest_path, bytes).map(Some)
    }

    fn path_is_dir(&mut self, guest_path: &str) -> Result<Option<bool>, HostServiceError> {
        Ok(module_kernel_optional(self.kernel.stat(guest_path))?.map(|stat| stat.is_directory))
    }

    fn path_exists(&mut self, guest_path: &str) -> Result<bool, HostServiceError> {
        Ok(self.path_is_dir(guest_path)?.is_some())
    }
}

/// Module reader for live JavaScript processes. Kernel VFS state is the sole
/// mutable guest filesystem authority, so module resolution observes exactly
/// the same files and metadata as embedded `fs` and standalone WASM.
struct ProcessModuleFsReader<'a> {
    kernel: &'a mut SidecarKernel,
    process: &'a ActiveProcess,
}

impl ProcessModuleFsReader<'_> {
    fn normalize_guest_path(&self, guest_path: &str) -> String {
        normalize_process_filesystem_rpc_path(self.process, guest_path)
    }
}

impl ModuleFsReader for ProcessModuleFsReader<'_> {
    fn canonical_guest_path(
        &mut self,
        guest_path: &str,
    ) -> Result<Option<String>, HostServiceError> {
        let normalized = self.normalize_guest_path(guest_path);
        module_kernel_optional(self.kernel.realpath_for_process(
            EXECUTION_DRIVER_NAME,
            self.process.kernel_pid,
            &normalized,
        ))
    }

    fn read_to_string(&mut self, guest_path: &str) -> Result<Option<String>, HostServiceError> {
        let normalized = self.normalize_guest_path(guest_path);
        let Some(bytes) = module_kernel_optional(self.kernel.read_file_for_process(
            EXECUTION_DRIVER_NAME,
            self.process.kernel_pid,
            &normalized,
        ))?
        else {
            return Ok(None);
        };
        module_utf8(&normalized, bytes).map(Some)
    }

    fn path_is_dir(&mut self, guest_path: &str) -> Result<Option<bool>, HostServiceError> {
        let normalized = self.normalize_guest_path(guest_path);
        Ok(module_kernel_optional(self.kernel.stat_for_process(
            EXECUTION_DRIVER_NAME,
            self.process.kernel_pid,
            &normalized,
        ))?
        .map(|stat| stat.is_directory))
    }

    fn path_exists(&mut self, guest_path: &str) -> Result<bool, HostServiceError> {
        Ok(self.path_is_dir(guest_path)?.is_some())
    }
}

fn module_kernel_optional<T>(
    result: Result<T, agentos_kernel::kernel::KernelError>,
) -> Result<Option<T>, HostServiceError> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if matches!(error.code(), "ENOENT" | "ENOTDIR") => Ok(None),
        Err(error) => Err(HostServiceError::new(error.code(), error.to_string())),
    }
}

fn module_utf8(path: &str, bytes: Vec<u8>) -> Result<String, HostServiceError> {
    String::from_utf8(bytes).map_err(|error| {
        HostServiceError::new(
            "EILSEQ",
            format!("module filesystem file {path} is not valid UTF-8: {error}"),
        )
    })
}

/// Resolve / load / format / batch-resolve module requests against the kernel
/// VFS. Routed here from `service_javascript_sync_rpc` for the
/// `__resolve_module` / `__load_file` / `__module_format` /
/// `__batch_resolve_modules` methods (mapped from the guest bridge's
/// `_resolveModule` / `_loadFile` / `_moduleFormat` / `_batchResolveModules`).
/// The `/opt/agentos/pkgs/<name>/<version>` root containing `guest_entrypoint`,
/// when the entrypoint lives inside a projected package. `current` is a valid
/// version segment here — the resolver canonicalizes it through the kernel.
fn agentos_package_version_root(guest_entrypoint: &str) -> Option<String> {
    let rest = guest_entrypoint.strip_prefix("/opt/agentos/pkgs/")?;
    let mut parts = rest.split('/');
    let name = parts.next().filter(|part| !part.is_empty())?;
    let version = parts.next().filter(|part| !part.is_empty())?;
    Some(format!("/opt/agentos/pkgs/{name}/{version}"))
}

fn is_bare_module_specifier(specifier: &str) -> bool {
    !(specifier.starts_with('/')
        || specifier.starts_with("./")
        || specifier.starts_with("../")
        || specifier == "."
        || specifier == ".."
        || specifier.starts_with('#')
        || specifier.starts_with("file:"))
}

pub(crate) fn service_javascript_module_sync_rpc(
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    request: &HostRpcRequest,
) -> Result<Value, SidecarError> {
    // Self-contained package processes (agent adapters, packed JS commands)
    // carry their whole dependency closure inside the package mount. A bare
    // specifier that misses from an unpackaged context (a parent module path
    // like `/root` from cwd-based requires) retries from the package's own
    // version root, so packed packages resolve exactly what they shipped.
    let package_fallback_from = process
        .env
        .get("AGENTOS_GUEST_ENTRYPOINT")
        .and_then(|entrypoint| agentos_package_version_root(entrypoint));
    // Resolution must observe live kernel mount/VFS state. Until the kernel
    // exposes a generation token suitable for a keyed immutable cache, keep
    // this cache request-local so a rename, symlink change, or mount update can
    // never leave the V8 adapter with stale authority.
    let mut cache = agentos_execution::LocalModuleResolutionCache::default();
    let value = {
        let reader = ProcessModuleFsReader {
            kernel,
            process: &*process,
        };
        let mut resolver = ModuleResolver::new(reader, &mut cache);

        match request.method.as_str() {
            "__resolve_module" | "_resolveModule" | "_resolveModuleSync" => {
                let specifier =
                    javascript_sync_rpc_arg_str(&request.args, 0, "module resolve specifier")?;
                let parent = request.args.get(1).and_then(Value::as_str).unwrap_or("/");
                let mode = match request.args.get(2).and_then(Value::as_str) {
                    Some("import") => ModuleResolveMode::Import,
                    Some("require") => ModuleResolveMode::Require,
                    // `_resolveModule` defaults to import; `_resolveModuleSync` to require.
                    _ if request.method == "_resolveModuleSync" => ModuleResolveMode::Require,
                    _ => ModuleResolveMode::Import,
                };
                let mut resolved = resolver
                    .resolve_module(specifier, parent, mode)
                    .map_err(SidecarError::Host)?;
                if resolved.is_none() && is_bare_module_specifier(specifier) {
                    if let Some(fallback_from) = package_fallback_from
                        .as_deref()
                        .filter(|fallback| *fallback != parent)
                    {
                        resolved = resolver
                            .resolve_module(specifier, fallback_from, mode)
                            .map_err(SidecarError::Host)?;
                    }
                }
                if resolved.is_none() && std::env::var("AGENTOS_MODULE_READER_TRACE").is_ok() {
                    eprintln!("kernel-resolve MISS: {specifier} from {parent} mode={mode:?}");
                }
                resolved.map(Value::String).unwrap_or(Value::Null)
            }
            "__load_file" | "_loadFile" | "_loadFileSync" => {
                let path = javascript_sync_rpc_arg_str(&request.args, 0, "module load path")?;
                resolver
                    .load_file(path)
                    .map_err(SidecarError::Host)?
                    .map(Value::String)
                    .unwrap_or(Value::Null)
            }
            "__module_format" | "_moduleFormat" => {
                let path = javascript_sync_rpc_arg_str(&request.args, 0, "module format path")?;
                resolver
                    .module_format(path)
                    .map_err(SidecarError::Host)?
                    .map(|format: LocalResolvedModuleFormat| {
                        Value::String(String::from(format.as_str()))
                    })
                    .unwrap_or(Value::Null)
            }
            "__batch_resolve_modules" | "_batchResolveModules" => resolver
                .batch_resolve_modules(&request.args)
                .map_err(SidecarError::Host)?,
            other => {
                return Err(SidecarError::InvalidState(format!(
                    "unsupported JavaScript module sync RPC method {other}"
                )));
            }
        }
    };
    Ok(value)
}

#[derive(Clone, Copy, Default)]
struct FsSyncPhaseStats {
    calls: u64,
    total_ns: u128,
    max_ns: u128,
}

static FS_SYNC_PHASES: OnceLock<Mutex<BTreeMap<String, FsSyncPhaseStats>>> = OnceLock::new();

struct FsSyncPhaseTimer<'a> {
    method: &'a str,
    start: Option<Instant>,
}

impl<'a> FsSyncPhaseTimer<'a> {
    fn start(method: &'a str) -> Self {
        let start = fs_sync_phases_enabled().then(Instant::now);
        Self { method, start }
    }
}

impl Drop for FsSyncPhaseTimer<'_> {
    fn drop(&mut self) {
        let Some(start) = self.start else { return };
        record_fs_sync_phase(self.method, start.elapsed().as_nanos());
    }
}

fn record_fs_sync_subphase(method: &str, stage: &str, start: Instant) {
    if !fs_sync_phases_enabled() {
        return;
    }
    record_fs_sync_phase(&format!("{method}:{stage}"), start.elapsed().as_nanos());
}

fn fs_sync_phases_enabled() -> bool {
    matches!(env::var("AGENTOS_FS_SYNC_PHASES").as_deref(), Ok("1"))
}

fn record_fs_sync_phase(method: &str, elapsed_ns: u128) {
    let phases = FS_SYNC_PHASES.get_or_init(|| Mutex::new(BTreeMap::new()));
    let Ok(mut phases) = phases.lock() else {
        eprintln!("ERR_AGENTOS_DIAGNOSTIC_STATE: filesystem sync statistics lock is poisoned");
        return;
    };
    let stats = phases.entry(method.to_string()).or_default();
    stats.calls += 1;
    stats.total_ns += elapsed_ns;
    stats.max_ns = stats.max_ns.max(elapsed_ns);

    let Some(path) = env::var_os("AGENTOS_FS_SYNC_PHASES_FILE") else {
        return;
    };
    let mut output = String::new();
    for (method, stats) in phases.iter() {
        let total_us = stats.total_ns / 1_000;
        let avg_us = if stats.calls == 0 {
            0
        } else {
            total_us / u128::from(stats.calls)
        };
        let max_us = stats.max_ns / 1_000;
        output.push_str(&format!(
            "method={method} calls={} total_us={total_us} avg_us={avg_us} max_us={max_us}\n",
            stats.calls
        ));
    }
    if let Err(error) = fs::write(&path, output) {
        eprintln!(
            "ERR_AGENTOS_DIAGNOSTIC_WRITE: failed to write filesystem sync statistics to {}: {error}",
            path.to_string_lossy()
        );
    }
}

pub(crate) fn service_javascript_fs_read_sync_rpc(
    kernel: &mut SidecarKernel,
    _process: &mut ActiveProcess,
    kernel_pid: u32,
    request: &HostRpcRequest,
) -> Result<Vec<u8>, SidecarError> {
    let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "filesystem read fd")?;
    let length = usize::try_from(javascript_sync_rpc_arg_u64(
        &request.args,
        1,
        "filesystem read length",
    )?)
    .map_err(|_| {
        SidecarError::InvalidState("filesystem read length must fit within usize".to_string())
    })?;
    let position =
        javascript_sync_rpc_arg_u64_optional(&request.args, 2, "filesystem read position")?;
    match position {
        Some(offset) => kernel.fd_pread(EXECUTION_DRIVER_NAME, kernel_pid, fd, length, offset),
        None => kernel.fd_read(EXECUTION_DRIVER_NAME, kernel_pid, fd, length),
    }
    .map_err(kernel_error)
}

pub(crate) fn service_javascript_fs_sync_rpc(
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    kernel_pid: u32,
    request: &HostRpcRequest,
) -> Result<Value, SidecarError> {
    let _phase_timer = FsSyncPhaseTimer::start(request.method.as_str());
    match request.method.as_str() {
        "fs.open" | "fs.openSync" => {
            let phase_start = Instant::now();
            let path =
                javascript_sync_rpc_path_arg(process, &request.args, 0, "filesystem open path")?;
            let path = path.as_str();
            let flags = javascript_sync_rpc_arg_u32(&request.args, 1, "filesystem open flags")?;
            let mode =
                javascript_sync_rpc_arg_u32_optional(&request.args, 2, "filesystem open mode")?;
            record_fs_sync_subphase(request.method.as_str(), "parse", phase_start);
            let phase_start = Instant::now();
            kernel
                .fd_open(EXECUTION_DRIVER_NAME, kernel_pid, path, flags, mode)
                .map(|fd| json!(fd))
                .map_err(|error| kernel_path_error("fs.open", path, error))
                .inspect(|_| {
                    record_fs_sync_subphase(request.method.as_str(), "kernel_fd_open", phase_start);
                })
        }
        "fs.namedFifoPeerReadySync" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "named FIFO fd")?;
            kernel
                .fd_named_pipe_peer_ready(EXECUTION_DRIVER_NAME, kernel_pid, fd)
                .map(|ready| json!(ready))
                .map_err(kernel_error)
        }
        "fs.blockingIoTimeoutMsSync" => Ok(json!(kernel.resource_limits().max_blocking_read_ms)),
        "fs.read" | "fs.readSync" => {
            service_javascript_fs_read_sync_rpc(kernel, process, kernel_pid, request)
                .map(|bytes| host_bytes_value(&bytes))
        }
        "fs.write" | "fs.writeSync" => {
            let phase_start = Instant::now();
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "filesystem write fd")?;
            let contents = if let Some(bytes) = request.raw_bytes_args.get(&1) {
                bytes.clone()
            } else {
                javascript_sync_rpc_bytes_arg(&request.args, 1, "filesystem write contents")?
            };
            let position = javascript_sync_rpc_arg_u64_optional(
                &request.args,
                2,
                "filesystem write position",
            )?;
            record_fs_sync_subphase(request.method.as_str(), "parse", phase_start);
            let phase_start = Instant::now();
            let written = match position {
                Some(offset) => kernel
                    .fd_pwrite(EXECUTION_DRIVER_NAME, kernel_pid, fd, &contents, offset)
                    .map_err(kernel_error)?,
                None => kernel
                    .fd_write(EXECUTION_DRIVER_NAME, kernel_pid, fd, &contents)
                    .map_err(kernel_error)?,
            };
            record_fs_sync_subphase(request.method.as_str(), "kernel_fd_write", phase_start);
            let phase_start = Instant::now();
            let surfaces_stdio =
                position.is_none() && kernel_fd_surfaces_stdio_event(kernel, kernel_pid, fd)?;
            record_fs_sync_subphase(request.method.as_str(), "stdio_check", phase_start);
            if surfaces_stdio {
                let phase_start = Instant::now();
                let event = if fd == 1 {
                    ActiveExecutionEvent::Stdout(contents)
                } else {
                    ActiveExecutionEvent::Stderr(contents)
                };
                process.queue_pending_execution_event(event)?;
                record_fs_sync_subphase(request.method.as_str(), "queue_stdio_event", phase_start);
            }
            Ok(json!(written))
        }
        "fs.writevSync" => {
            let phase_start = Instant::now();
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "filesystem writev fd")?;
            let contents = request.raw_bytes_args.get(&1).ok_or_else(|| {
                SidecarError::InvalidState(String::from(
                    "filesystem writev requires raw byte payload",
                ))
            })?;
            let position = javascript_sync_rpc_arg_u64_optional(
                &request.args,
                2,
                "filesystem writev position",
            )?;
            let buffers = decode_javascript_writev_raw_payload(contents)?;
            record_fs_sync_subphase(request.method.as_str(), "parse", phase_start);

            let mut total_written = 0usize;
            let surfaces_stdio =
                position.is_none() && kernel_fd_surfaces_stdio_event(kernel, kernel_pid, fd)?;
            let mut next_position = position;
            let mut combined_stdio = Vec::new();
            for buffer in buffers {
                let mut offset = 0usize;
                while offset < buffer.len() {
                    let slice = &buffer[offset..];
                    let written = match next_position {
                        Some(position) => kernel
                            .fd_pwrite(EXECUTION_DRIVER_NAME, kernel_pid, fd, slice, position)
                            .map_err(kernel_error)?,
                        None => kernel
                            .fd_write(EXECUTION_DRIVER_NAME, kernel_pid, fd, slice)
                            .map_err(kernel_error)?,
                    };
                    if written == 0 {
                        return Err(SidecarError::host(
                            "EIO",
                            format!("filesystem writev made no progress on fd {fd}"),
                        ));
                    }
                    offset += written;
                    total_written = total_written.saturating_add(written);
                    if let Some(position) = &mut next_position {
                        *position = position.saturating_add(written as u64);
                    }
                }
                if surfaces_stdio {
                    combined_stdio.extend_from_slice(buffer);
                }
            }
            record_fs_sync_subphase(request.method.as_str(), "kernel_fd_write", phase_start);
            if surfaces_stdio && !combined_stdio.is_empty() {
                let event = if fd == 1 {
                    ActiveExecutionEvent::Stdout(combined_stdio)
                } else {
                    ActiveExecutionEvent::Stderr(combined_stdio)
                };
                process.queue_pending_execution_event(event)?;
            }
            Ok(json!(total_written))
        }
        "fs.close" | "fs.closeSync" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "filesystem close fd")?;
            kernel
                .fd_close(EXECUTION_DRIVER_NAME, kernel_pid, fd)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.openTmpfileSync" => {
            let directory =
                javascript_sync_rpc_path_arg(process, &request.args, 0, "unnamed-file directory")?;
            let flags = javascript_sync_rpc_arg_u32(&request.args, 1, "unnamed-file open flags")?;
            let mode = javascript_sync_rpc_arg_u32(&request.args, 2, "unnamed-file mode")?;
            let linkable =
                javascript_sync_rpc_option_bool(&request.args, 3, "linkable").unwrap_or(true);
            kernel
                .fd_open_tmpfile(
                    EXECUTION_DRIVER_NAME,
                    kernel_pid,
                    &directory,
                    flags,
                    mode,
                    linkable,
                )
                .map(|fd| Value::from(u64::from(fd)))
                .map_err(kernel_error)
        }
        "fs.linkFdSync" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "unnamed-file fd")?;
            let destination = javascript_sync_rpc_path_arg(
                process,
                &request.args,
                1,
                "unnamed-file link destination",
            )?;
            kernel
                .fd_link_tmpfile_for_process(EXECUTION_DRIVER_NAME, kernel_pid, fd, &destination)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.fstat" | "fs.fstatSync" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "filesystem fstat fd")?;
            kernel
                .fd_stat(EXECUTION_DRIVER_NAME, kernel_pid, fd)
                .map_err(kernel_error)?;
            kernel
                .dev_fd_stat(EXECUTION_DRIVER_NAME, kernel_pid, fd)
                .map(javascript_sync_rpc_stat_value)
                .map_err(kernel_error)
        }
        "fs.fsyncSync" | "fs.fdatasyncSync" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "filesystem sync fd")?;
            kernel
                .fd_sync(EXECUTION_DRIVER_NAME, kernel_pid, fd)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.truncateSync" | "fs.truncateForProcessSync" => {
            let path = javascript_sync_rpc_path_arg(
                process,
                &request.args,
                0,
                "filesystem truncate path",
            )?;
            let length = javascript_sync_rpc_arg_u64_optional(
                &request.args,
                1,
                "filesystem truncate length",
            )?
            .unwrap_or(0);
            kernel
                .truncate_for_process(EXECUTION_DRIVER_NAME, kernel_pid, &path, length)
                .map(|()| Value::Null)
                .map_err(|error| kernel_path_error("fs.truncate", &path, error))
        }
        "fs.fallocateSync" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "filesystem fallocate fd")?;
            let offset =
                javascript_sync_rpc_arg_u64(&request.args, 1, "filesystem fallocate offset")?;
            let length =
                javascript_sync_rpc_arg_u64(&request.args, 2, "filesystem fallocate length")?;
            kernel
                .fd_allocate(EXECUTION_DRIVER_NAME, kernel_pid, fd, offset, length)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.insertRangeSync" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "filesystem insert-range fd")?;
            let offset =
                javascript_sync_rpc_arg_u64(&request.args, 1, "filesystem insert-range offset")?;
            let length =
                javascript_sync_rpc_arg_u64(&request.args, 2, "filesystem insert-range length")?;
            kernel
                .fd_insert_range(EXECUTION_DRIVER_NAME, kernel_pid, fd, offset, length)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.collapseRangeSync" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "filesystem collapse-range fd")?;
            let offset =
                javascript_sync_rpc_arg_u64(&request.args, 1, "filesystem collapse-range offset")?;
            let length =
                javascript_sync_rpc_arg_u64(&request.args, 2, "filesystem collapse-range length")?;
            kernel
                .fd_collapse_range(EXECUTION_DRIVER_NAME, kernel_pid, fd, offset, length)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.punchHoleSync" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "filesystem punch-hole fd")?;
            let offset =
                javascript_sync_rpc_arg_u64(&request.args, 1, "filesystem punch-hole offset")?;
            let length =
                javascript_sync_rpc_arg_u64(&request.args, 2, "filesystem punch-hole length")?;
            kernel
                .fd_punch_hole(EXECUTION_DRIVER_NAME, kernel_pid, fd, offset, length)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.zeroRangeSync" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "filesystem zero-range fd")?;
            let offset =
                javascript_sync_rpc_arg_u64(&request.args, 1, "filesystem zero-range offset")?;
            let length =
                javascript_sync_rpc_arg_u64(&request.args, 2, "filesystem zero-range length")?;
            let keep_size =
                javascript_sync_rpc_arg_u32(&request.args, 3, "filesystem zero-range keep-size")?
                    != 0;
            kernel
                .fd_zero_range(
                    EXECUTION_DRIVER_NAME,
                    kernel_pid,
                    fd,
                    offset,
                    length,
                    keep_size,
                )
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.fiemapSync" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "filesystem fiemap fd")?;
            let path = kernel
                .fd_path(EXECUTION_DRIVER_NAME, kernel_pid, fd)
                .map_err(kernel_error)?;
            let ranges = kernel
                .fd_allocated_ranges(EXECUTION_DRIVER_NAME, kernel_pid, fd)
                .map_err(|error| kernel_path_error("fs.fiemap", &path, error))?;
            let unwritten = kernel
                .fd_unwritten_ranges(EXECUTION_DRIVER_NAME, kernel_pid, fd)
                .map_err(|error| kernel_path_error("fs.fiemap", &path, error))?;
            Ok(json!(classify_fiemap_ranges(ranges, &unwritten)
                .into_iter()
                .map(|(start, end, unwritten)| {
                    json!({ "start": start, "end": end, "unwritten": unwritten })
                })
                .collect::<Vec<_>>()))
        }
        "fs.chmodForProcessSync" => {
            let path =
                javascript_sync_rpc_path_arg(process, &request.args, 0, "filesystem chmod path")?;
            let mode = javascript_sync_rpc_arg_u32(&request.args, 1, "filesystem chmod mode")?;
            kernel
                .chmod_for_process(EXECUTION_DRIVER_NAME, kernel_pid, &path, mode)
                .map(|()| Value::Null)
                .map_err(|error| kernel_path_error("fs.chmod", &path, error))
        }
        "fs.ftruncateSync" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "filesystem ftruncate fd")?;
            let length = javascript_sync_rpc_arg_u64_optional(
                &request.args,
                1,
                "filesystem ftruncate length",
            )?
            .unwrap_or(0);
            let fd_stat = kernel
                .fd_stat(EXECUTION_DRIVER_NAME, kernel_pid, fd)
                .map_err(kernel_error)?;
            if (fd_stat.flags & libc::O_ACCMODE as u32) == libc::O_RDONLY as u32 {
                return Err(SidecarError::host(
                    "EBADF",
                    format!("file descriptor {fd} is not open for writing"),
                ));
            }
            kernel
                .fd_truncate(EXECUTION_DRIVER_NAME, kernel_pid, fd, length)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.readFileSync" | "fs.promises.readFile" => {
            let path = javascript_sync_rpc_path_arg(
                process,
                &request.args,
                0,
                "filesystem readFile path",
            )?;
            let path = path.as_str();
            let encoding = javascript_sync_rpc_encoding(&request.args);
            kernel
                .read_file_for_process(EXECUTION_DRIVER_NAME, kernel_pid, path)
                .map(|content| match encoding.as_deref() {
                    Some("utf8") | Some("utf-8") => {
                        Value::String(String::from_utf8_lossy(&content).into_owned())
                    }
                    _ => host_bytes_value(&content),
                })
                .map_err(kernel_error)
        }
        "fs.writeFileSync" | "fs.promises.writeFile" => {
            let path = javascript_sync_rpc_path_arg(
                process,
                &request.args,
                0,
                "filesystem writeFile path",
            )?;
            let path = path.as_str();
            let contents = if let Some(bytes) = request.raw_bytes_args.get(&1) {
                bytes.clone()
            } else {
                javascript_sync_rpc_bytes_arg(&request.args, 1, "filesystem writeFile contents")?
            };
            kernel
                .write_file_for_process(
                    EXECUTION_DRIVER_NAME,
                    kernel_pid,
                    path,
                    contents,
                    javascript_sync_rpc_option_u32(&request.args, 2, "mode")?,
                )
                .map(|()| Value::Null)
                .map_err(|error| kernel_path_error("fs.writeFile", path, error))
        }
        "fs.statfsSync" => {
            let path =
                javascript_sync_rpc_path_arg(process, &request.args, 0, "filesystem statfs path")?;
            let stats = kernel
                .filesystem_stats_for_process(EXECUTION_DRIVER_NAME, kernel_pid, path.as_str())
                .map_err(kernel_error)?;
            Ok(json!({
                "totalBytes": stats.total_bytes,
                "usedBytes": stats.used_bytes,
                "availableBytes": stats.available_bytes,
                "totalInodes": stats.total_inodes,
                "freeInodes": stats.free_inodes,
            }))
        }
        "fs.statSync" | "fs.promises.stat" => {
            let path =
                javascript_sync_rpc_path_arg(process, &request.args, 0, "filesystem stat path")?;
            let path = path.as_str();
            kernel
                .stat_for_process(EXECUTION_DRIVER_NAME, kernel_pid, path)
                .map(javascript_sync_rpc_stat_value)
                .map_err(kernel_error)
        }
        "fs.lstatSync" | "fs.promises.lstat" => {
            let path =
                javascript_sync_rpc_path_arg(process, &request.args, 0, "filesystem lstat path")?;
            let path = path.as_str();
            kernel
                .lstat_for_process(EXECUTION_DRIVER_NAME, kernel_pid, path)
                .map(javascript_sync_rpc_stat_value)
                .map_err(kernel_error)
        }
        "fs.readdirSync" | "fs.promises.readdir" => {
            let path =
                javascript_sync_rpc_path_arg(process, &request.args, 0, "filesystem readdir path")?;
            let path = path.as_str();
            service_javascript_fs_readdir_entries(kernel, process, kernel_pid, path)
                .map(javascript_sync_rpc_readdir_typed_value)
        }
        "fs.mkdirSync" | "fs.promises.mkdir" => {
            let path =
                javascript_sync_rpc_path_arg(process, &request.args, 0, "filesystem mkdir path")?;
            let path = path.as_str();
            let recursive =
                javascript_sync_rpc_option_bool(&request.args, 1, "recursive").unwrap_or(false);
            kernel
                .mkdir_for_process(
                    EXECUTION_DRIVER_NAME,
                    kernel_pid,
                    path,
                    recursive,
                    javascript_sync_rpc_option_u32(&request.args, 1, "mode")?,
                )
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.mknodSync" => {
            let path =
                javascript_sync_rpc_path_arg(process, &request.args, 0, "filesystem mknod path")?;
            let mode = javascript_sync_rpc_arg_u32(&request.args, 1, "filesystem mknod mode")?;
            let rdev = javascript_sync_rpc_arg_u64(&request.args, 2, "filesystem mknod device")?;
            kernel
                .mknod_for_process(EXECUTION_DRIVER_NAME, kernel_pid, path.as_str(), mode, rdev)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.remountSync" => {
            let path =
                javascript_sync_rpc_path_arg(process, &request.args, 0, "filesystem remount path")?;
            let options = request.args.get(1).and_then(Value::as_str).ok_or_else(|| {
                SidecarError::InvalidState(String::from(
                    "filesystem remount options must be a string",
                ))
            })?;
            kernel
                .remount_filesystem_for_process(
                    EXECUTION_DRIVER_NAME,
                    kernel_pid,
                    path.as_str(),
                    options,
                )
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.accessSync" | "fs.promises.access" => {
            let path =
                javascript_sync_rpc_path_arg(process, &request.args, 0, "filesystem access path")?;
            let path = path.as_str();
            let mode =
                javascript_sync_rpc_arg_u32_optional(&request.args, 1, "filesystem access mode")?
                    .unwrap_or(0);
            let effective_ids =
                javascript_sync_rpc_option_bool(&request.args, 2, "effective IDs").unwrap_or(false);
            let valid_mask = libc::R_OK as u32 | libc::W_OK as u32 | libc::X_OK as u32;
            if mode & !valid_mask != 0 {
                return Err(SidecarError::host(
                    "EINVAL",
                    format!("invalid filesystem access mode {mode:o}"),
                ));
            }
            kernel
                .access_for_process(EXECUTION_DRIVER_NAME, kernel_pid, path, mode, effective_ids)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.copyFileSync" | "fs.promises.copyFile" => {
            let source = javascript_sync_rpc_path_arg(
                process,
                &request.args,
                0,
                "filesystem copyFile source",
            )?;
            let source = source.as_str();
            let destination = javascript_sync_rpc_path_arg(
                process,
                &request.args,
                1,
                "filesystem copyFile destination",
            )?;
            let destination = destination.as_str();
            let contents = kernel
                .read_file_for_process(EXECUTION_DRIVER_NAME, kernel_pid, source)
                .map_err(kernel_error)?;
            kernel
                .write_file_for_process(
                    EXECUTION_DRIVER_NAME,
                    kernel_pid,
                    destination,
                    contents,
                    None,
                )
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.existsSync" => {
            let path =
                javascript_sync_rpc_path_arg(process, &request.args, 0, "filesystem exists path")?;
            let path = path.as_str();
            kernel
                .exists_for_process(EXECUTION_DRIVER_NAME, kernel_pid, path)
                .map(Value::Bool)
                .map_err(kernel_error)
        }
        "fs.readlinkSync" | "fs.promises.readlink" => {
            let path = javascript_sync_rpc_path_arg(
                process,
                &request.args,
                0,
                "filesystem readlink path",
            )?;
            let path = path.as_str();
            kernel
                .read_link_for_process(EXECUTION_DRIVER_NAME, kernel_pid, path)
                .map(Value::String)
                .map_err(kernel_error)
        }
        "fs.symlinkSync" | "fs.promises.symlink" => {
            let target =
                javascript_sync_rpc_arg_str(&request.args, 0, "filesystem symlink target")?;
            let link_path =
                javascript_sync_rpc_path_arg(process, &request.args, 1, "filesystem symlink path")?;
            let link_path = link_path.as_str();
            kernel
                .symlink_for_process(EXECUTION_DRIVER_NAME, kernel_pid, target, link_path)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.linkSync" | "fs.promises.link" => {
            let source =
                javascript_sync_rpc_path_arg(process, &request.args, 0, "filesystem link source")?;
            let source = source.as_str();
            let destination =
                javascript_sync_rpc_path_arg(process, &request.args, 1, "filesystem link path")?;
            let destination = destination.as_str();
            kernel
                .link_for_process(EXECUTION_DRIVER_NAME, kernel_pid, source, destination)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.renameSync" | "fs.promises.rename" => {
            let source = javascript_sync_rpc_path_arg(
                process,
                &request.args,
                0,
                "filesystem rename source",
            )?;
            let source = source.as_str();
            let destination = javascript_sync_rpc_path_arg(
                process,
                &request.args,
                1,
                "filesystem rename destination",
            )?;
            let destination = destination.as_str();
            kernel
                .rename_for_process(EXECUTION_DRIVER_NAME, kernel_pid, source, destination)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.renameAt2Sync" => {
            let source = javascript_sync_rpc_path_arg(
                process,
                &request.args,
                0,
                "filesystem renameat2 source",
            )?;
            let source = source.as_str();
            let destination = javascript_sync_rpc_path_arg(
                process,
                &request.args,
                1,
                "filesystem renameat2 destination",
            )?;
            let destination = destination.as_str();
            let flags =
                javascript_sync_rpc_arg_u32(&request.args, 2, "filesystem renameat2 flags")?;
            kernel
                .rename_at2_for_process(
                    EXECUTION_DRIVER_NAME,
                    kernel_pid,
                    source,
                    destination,
                    flags,
                )
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.rmdirSync" | "fs.promises.rmdir" => {
            let path =
                javascript_sync_rpc_path_arg(process, &request.args, 0, "filesystem rmdir path")?;
            let path = path.as_str();
            kernel
                .remove_dir_for_process(EXECUTION_DRIVER_NAME, kernel_pid, path)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.unlinkSync" | "fs.promises.unlink" => {
            let path =
                javascript_sync_rpc_path_arg(process, &request.args, 0, "filesystem unlink path")?;
            let path = path.as_str();
            kernel
                .remove_file_for_process(EXECUTION_DRIVER_NAME, kernel_pid, path)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.chmodSync" | "fs.promises.chmod" => {
            let path =
                javascript_sync_rpc_path_arg(process, &request.args, 0, "filesystem chmod path")?;
            let path = path.as_str();
            let mode = javascript_sync_rpc_arg_u32(&request.args, 1, "filesystem chmod mode")?;
            kernel
                .chmod_for_process(EXECUTION_DRIVER_NAME, kernel_pid, path, mode)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.chownSync" | "fs.promises.chown" | "fs.lchownSync" | "fs.promises.lchown" => {
            let path =
                javascript_sync_rpc_path_arg(process, &request.args, 0, "filesystem chown path")?;
            let path = path.as_str();
            let uid = javascript_sync_rpc_arg_u32(&request.args, 1, "filesystem chown uid")?;
            let gid = javascript_sync_rpc_arg_u32(&request.args, 2, "filesystem chown gid")?;
            let is_lchown = matches!(
                request.method.as_str(),
                "fs.lchownSync" | "fs.promises.lchown"
            );
            let result = if is_lchown {
                kernel.lchown_for_process(EXECUTION_DRIVER_NAME, kernel_pid, path, uid, gid)
            } else {
                kernel.chown_for_process(EXECUTION_DRIVER_NAME, kernel_pid, path, uid, gid, true)
            };
            result.map(|()| Value::Null).map_err(kernel_error)
        }
        "fs.getxattrSync" => {
            let path = javascript_sync_rpc_path_arg(
                process,
                &request.args,
                0,
                "filesystem getxattr path",
            )?;
            let name = javascript_sync_rpc_arg_str(&request.args, 1, "filesystem xattr name")?;
            let follow_symlinks =
                javascript_sync_rpc_option_bool(&request.args, 2, "follow symlinks")
                    .unwrap_or(true);
            kernel
                .get_xattr_for_process(
                    EXECUTION_DRIVER_NAME,
                    kernel_pid,
                    path.as_str(),
                    name,
                    follow_symlinks,
                )
                .map(|bytes| host_bytes_value(&bytes))
                .map_err(kernel_error)
        }
        "fs.fgetxattrSync" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "filesystem fgetxattr fd")?;
            let name = javascript_sync_rpc_arg_str(&request.args, 1, "filesystem xattr name")?;
            kernel
                .fd_get_xattr_for_process(EXECUTION_DRIVER_NAME, kernel_pid, fd, name)
                .map(|bytes| host_bytes_value(&bytes))
                .map_err(kernel_error)
        }
        "fs.listxattrSync" => {
            let path = javascript_sync_rpc_path_arg(
                process,
                &request.args,
                0,
                "filesystem listxattr path",
            )?;
            let follow_symlinks =
                javascript_sync_rpc_option_bool(&request.args, 1, "follow symlinks")
                    .unwrap_or(true);
            kernel
                .list_xattrs_for_process(
                    EXECUTION_DRIVER_NAME,
                    kernel_pid,
                    path.as_str(),
                    follow_symlinks,
                )
                .map(|names| json!(names))
                .map_err(kernel_error)
        }
        "fs.flistxattrSync" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "filesystem flistxattr fd")?;
            kernel
                .fd_list_xattrs_for_process(EXECUTION_DRIVER_NAME, kernel_pid, fd)
                .map(|names| json!(names))
                .map_err(kernel_error)
        }
        "fs.setxattrSync" => {
            let path = javascript_sync_rpc_path_arg(
                process,
                &request.args,
                0,
                "filesystem setxattr path",
            )?;
            let name = javascript_sync_rpc_arg_str(&request.args, 1, "filesystem xattr name")?;
            let value = javascript_sync_rpc_bytes_arg(&request.args, 2, "filesystem xattr value")?;
            let flags = javascript_sync_rpc_arg_u32(&request.args, 3, "filesystem xattr flags")?;
            let follow_symlinks =
                javascript_sync_rpc_option_bool(&request.args, 4, "follow symlinks")
                    .unwrap_or(true);
            kernel
                .set_xattr_for_process(
                    EXECUTION_DRIVER_NAME,
                    kernel_pid,
                    path.as_str(),
                    name,
                    value,
                    flags,
                    follow_symlinks,
                )
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.fsetxattrSync" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "filesystem fsetxattr fd")?;
            let name = javascript_sync_rpc_arg_str(&request.args, 1, "filesystem xattr name")?;
            let value = javascript_sync_rpc_bytes_arg(&request.args, 2, "filesystem xattr value")?;
            let flags = javascript_sync_rpc_arg_u32(&request.args, 3, "filesystem xattr flags")?;
            kernel
                .fd_set_xattr_for_process(EXECUTION_DRIVER_NAME, kernel_pid, fd, name, value, flags)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.removexattrSync" => {
            let path = javascript_sync_rpc_path_arg(
                process,
                &request.args,
                0,
                "filesystem removexattr path",
            )?;
            let name = javascript_sync_rpc_arg_str(&request.args, 1, "filesystem xattr name")?;
            let follow_symlinks =
                javascript_sync_rpc_option_bool(&request.args, 2, "follow symlinks")
                    .unwrap_or(true);
            kernel
                .remove_xattr_for_process(
                    EXECUTION_DRIVER_NAME,
                    kernel_pid,
                    path.as_str(),
                    name,
                    follow_symlinks,
                )
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.fremovexattrSync" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "filesystem fremovexattr fd")?;
            let name = javascript_sync_rpc_arg_str(&request.args, 1, "filesystem xattr name")?;
            kernel
                .fd_remove_xattr_for_process(EXECUTION_DRIVER_NAME, kernel_pid, fd, name)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.utimesSync" | "fs.promises.utimes" | "fs.lutimesSync" | "fs.promises.lutimes" => {
            let path =
                javascript_sync_rpc_path_arg(process, &request.args, 0, "filesystem utimes path")?;
            let path = path.as_str();
            let atime = parse_utime_arg(&request.args, 1, "filesystem utimes atime")?;
            let mtime = parse_utime_arg(&request.args, 2, "filesystem utimes mtime")?;
            let follow_symlinks = !matches!(
                request.method.as_str(),
                "fs.lutimesSync" | "fs.promises.lutimes"
            );
            kernel
                .utimes_spec_for_process(
                    EXECUTION_DRIVER_NAME,
                    kernel_pid,
                    path,
                    atime,
                    mtime,
                    follow_symlinks,
                )
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.futimesSync" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "filesystem futimes fd")?;
            let atime = parse_utime_arg(&request.args, 1, "filesystem futimes atime")?;
            let mtime = parse_utime_arg(&request.args, 2, "filesystem futimes mtime")?;
            kernel
                .futimes(EXECUTION_DRIVER_NAME, kernel_pid, fd, atime, mtime)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        _ => Err(SidecarError::InvalidState(format!(
            "unsupported JavaScript sync RPC method {}",
            request.method
        ))),
    }
}

fn kernel_fd_surfaces_stdio_event(
    kernel: &SidecarKernel,
    kernel_pid: u32,
    fd: u32,
) -> Result<bool, SidecarError> {
    let path = match fd {
        1 | 2 => kernel
            .fd_path(EXECUTION_DRIVER_NAME, kernel_pid, fd)
            .map_err(kernel_error)?,
        _ => return Ok(false),
    };
    Ok(matches!(
        (fd, path.as_str()),
        (1, "/dev/stdout") | (2, "/dev/stderr")
    ))
}

pub(crate) fn javascript_sync_rpc_path_arg(
    process: &ActiveProcess,
    args: &[Value],
    index: usize,
    label: &str,
) -> Result<String, SidecarError> {
    let path = javascript_sync_rpc_arg_str(args, index, label)?;
    let path = normalize_process_filesystem_rpc_path(process, path);
    if path.split('/').any(is_internal_unnamed_file_name) {
        return Err(SidecarError::host(
            "ENOENT",
            format!("no such file or directory: {path}"),
        ));
    }
    Ok(path)
}

fn normalize_process_filesystem_rpc_path(process: &ActiveProcess, path: &str) -> String {
    if path.starts_with('/') {
        normalize_path(path)
    } else {
        normalize_path(&format!(
            "{}/{}",
            process.guest_cwd.trim_end_matches('/'),
            path
        ))
    }
}

fn javascript_sync_rpc_stat_value(stat: VirtualStat) -> Value {
    let mut value = Map::with_capacity(18);
    value.insert("mode".to_string(), Value::from(stat.mode));
    value.insert("size".to_string(), Value::from(stat.size));
    value.insert("blocks".to_string(), Value::from(stat.blocks));
    value.insert("dev".to_string(), Value::from(stat.dev));
    value.insert("rdev".to_string(), Value::from(stat.rdev));
    value.insert("isDirectory".to_string(), Value::from(stat.is_directory));
    value.insert(
        "isSymbolicLink".to_string(),
        Value::from(stat.is_symbolic_link),
    );
    value.insert("atimeMs".to_string(), Value::from(stat.atime_ms));
    value.insert("atimeNsec".to_string(), Value::from(stat.atime_nsec));
    value.insert("mtimeMs".to_string(), Value::from(stat.mtime_ms));
    value.insert("mtimeNsec".to_string(), Value::from(stat.mtime_nsec));
    value.insert("ctimeMs".to_string(), Value::from(stat.ctime_ms));
    value.insert("ctimeNsec".to_string(), Value::from(stat.ctime_nsec));
    value.insert("birthtimeMs".to_string(), Value::from(stat.birthtime_ms));
    value.insert("ino".to_string(), Value::from(stat.ino));
    value.insert("nlink".to_string(), Value::from(stat.nlink));
    value.insert("uid".to_string(), Value::from(stat.uid));
    value.insert("gid".to_string(), Value::from(stat.gid));
    Value::Object(value)
}

fn read_le_u32(payload: &[u8], offset: &mut usize, label: &str) -> Result<u32, SidecarError> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| SidecarError::InvalidState(format!("filesystem {label} offset overflow")))?;
    let bytes = payload.get(*offset..end).ok_or_else(|| {
        SidecarError::InvalidState(format!("truncated filesystem {label} payload"))
    })?;
    *offset = end;
    Ok(u32::from_le_bytes(
        bytes.try_into().expect("slice length checked"),
    ))
}

fn decode_javascript_writev_raw_payload(payload: &[u8]) -> Result<Vec<&[u8]>, SidecarError> {
    let mut offset = 0usize;
    let count = read_le_u32(payload, &mut offset, "writev count")? as usize;
    let mut buffers = Vec::with_capacity(count);
    for _ in 0..count {
        let len = read_le_u32(payload, &mut offset, "writev buffer length")? as usize;
        let end = offset.checked_add(len).ok_or_else(|| {
            SidecarError::InvalidState(String::from("filesystem writev payload length overflow"))
        })?;
        let buffer = payload.get(offset..end).ok_or_else(|| {
            SidecarError::InvalidState(String::from("truncated filesystem writev payload"))
        })?;
        buffers.push(buffer);
        offset = end;
    }
    if offset != payload.len() {
        return Err(SidecarError::InvalidState(String::from(
            "filesystem writev payload has trailing bytes",
        )));
    }
    Ok(buffers)
}

pub(crate) fn service_javascript_fs_readdir_entries(
    kernel: &mut SidecarKernel,
    _process: &ActiveProcess,
    kernel_pid: u32,
    path: &str,
) -> Result<BTreeMap<String, bool>, SidecarError> {
    let entries = kernel
        .read_dir_with_types_for_process(EXECUTION_DRIVER_NAME, kernel_pid, path)
        .map_err(kernel_error)?;
    let mut typed = BTreeMap::new();
    for entry in entries {
        // The existing Node compatibility surface reports a symlink to a
        // directory as a directory. Preserve that behavior while keeping the
        // kernel filesystem authoritative; only symlinks require a followed
        // stat, so ordinary entries retain the one-pass typed readdir path.
        let is_directory = if entry.is_symbolic_link {
            let child_path = if path == "/" {
                format!("/{name}", name = entry.name)
            } else {
                format!(
                    "{parent}/{name}",
                    parent = path.trim_end_matches('/'),
                    name = entry.name
                )
            };
            match kernel.stat_for_process(EXECUTION_DRIVER_NAME, kernel_pid, &child_path) {
                Ok(stat) => stat.is_directory,
                Err(error) if error.code() == "ENOENT" => false,
                Err(error) => return Err(kernel_error(error)),
            }
        } else {
            entry.is_directory
        };
        typed.insert(entry.name, is_directory);
    }
    Ok(typed)
}

pub(crate) fn service_javascript_fs_readdir_raw_sync_rpc(
    kernel: &mut SidecarKernel,
    process: &ActiveProcess,
    kernel_pid: u32,
    request: &HostRpcRequest,
) -> Result<Vec<u8>, SidecarError> {
    let path = javascript_sync_rpc_path_arg(process, &request.args, 0, "filesystem readdir path")?;
    let entries =
        service_javascript_fs_readdir_entries(kernel, process, kernel_pid, path.as_str())?;
    encode_javascript_readdir_raw_payload(entries)
}

fn encode_javascript_readdir_raw_payload(
    entries: BTreeMap<String, bool>,
) -> Result<Vec<u8>, SidecarError> {
    let mut payload = Vec::new();
    for (name, is_dir) in entries
        .into_iter()
        .filter(|(name, _)| name != "." && name != "..")
    {
        let name = name.into_bytes();
        let name_len = u32::try_from(name.len()).map_err(|_| {
            SidecarError::InvalidState(String::from("filesystem readdir entry name too long"))
        })?;
        payload.push(u8::from(is_dir));
        payload.extend_from_slice(&name_len.to_le_bytes());
        payload.extend_from_slice(&name);
    }
    Ok(payload)
}

/// Like `javascript_sync_rpc_readdir_value` but carries each entry's
/// directory-ness as `{name, isDirectory}`. The guest's `normalizeReaddirEntries`
/// consumes these objects directly for `withFileTypes`, avoiding a per-entry stat
/// RPC, and extracts `.name` for the plain string form.
fn javascript_sync_rpc_readdir_typed_value(entries: BTreeMap<String, bool>) -> Value {
    json!(entries
        .into_iter()
        .filter(|(name, _)| name != "." && name != "..")
        .map(|(name, is_dir)| json!({ "name": name, "isDirectory": is_dir }))
        .collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::classify_fiemap_ranges;
    use crate::state::SidecarKernel;
    use agentos_kernel::kernel::KernelVmConfig;
    use agentos_kernel::mount_table::MountTable;
    use agentos_kernel::permissions::Permissions;
    use agentos_kernel::vfs::MemoryFileSystem;
    use std::fs;

    #[test]
    fn fiemap_ranges_split_data_and_unwritten_allocations() {
        assert_eq!(
            classify_fiemap_ranges(vec![(0, 2048), (3072, 4096)], &[(512, 1536), (3072, 4096)]),
            vec![
                (0, 512, false),
                (512, 1536, true),
                (1536, 2048, false),
                (3072, 4096, true),
            ]
        );
    }
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(prefix: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "{prefix}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time before unix epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    // Companion to the execution-crate `faithful_pnpm_symlink_layout_*` host
    // test, but resolving through the *kernel VFS* via a read-only `host_dir`
    // mount at `/root/node_modules` — the real VM path. A faithful pnpm tree
    // (every package in its own `.pnpm/<pkg>@<ver>/node_modules/<pkg>` entry,
    // dependencies wired by symlink) must resolve purely by the standard
    // ancestor walk + realpath, with NO `.pnpm` store scanning, and must pick
    // the version the symlink points at — not an alphabetically-earlier decoy.
    #[test]
    fn faithful_pnpm_symlink_layout_resolves_through_kernel_vfs() {
        use super::{KernelModuleFsReader, ModuleResolveMode};
        use agentos_execution::{LocalModuleResolutionCache, ModuleResolver};
        use agentos_kernel::mount_table::{MountOptions, MountedVirtualFileSystem};
        use std::os::unix::fs::symlink;

        let node_modules = temp_dir("pnpm-vfs-node-modules").join("node_modules");
        let write = |relative: &str, contents: &str| {
            let path = node_modules.join(relative);
            fs::create_dir_all(path.parent().expect("parent")).expect("create dirs");
            fs::write(path, contents).expect("write fixture");
        };
        // pnpm always writes *relative* symlinks; the VFS mount follows them
        // with RESOLVE_BENEATH (absolute targets are treated as escaping, which
        // is also why pnpm never uses them). `relative_target` is the target
        // expressed relative to the link's own directory.
        let link = |relative_target: &str, link_relative: &str| {
            let link_path = node_modules.join(link_relative);
            fs::create_dir_all(link_path.parent().expect("link parent")).expect("create dirs");
            symlink(relative_target, link_path).expect("create symlink");
        };

        // consumer@1.0.0 in its store entry; imports `dep`.
        write(
            ".pnpm/consumer@1.0.0/node_modules/consumer/index.mjs",
            "import { wanted } from 'dep';\nexport default wanted;",
        );
        write(
            ".pnpm/consumer@1.0.0/node_modules/consumer/package.json",
            r#"{ "version": "1.0.0", "type": "module", "exports": { ".": "./index.mjs" } }"#,
        );
        // dep@2.0.0 — the correct version — in its own store entry.
        write(
            ".pnpm/dep@2.0.0/node_modules/dep/index.mjs",
            "export const wanted = 2;",
        );
        write(
            ".pnpm/dep@2.0.0/node_modules/dep/package.json",
            r#"{ "version": "2.0.0", "type": "module", "exports": { ".": "./index.mjs" } }"#,
        );
        // Decoy: an alphabetically-earlier store entry holding an incompatible dep@1.
        write(
            ".pnpm/aaa-other@1.0.0/node_modules/dep/index.js",
            "module.exports = 1;",
        );
        write(
            ".pnpm/aaa-other@1.0.0/node_modules/dep/package.json",
            r#"{ "version": "1.0.0", "main": "index.js" }"#,
        );
        // pnpm's sibling symlink: consumer's `dep` -> dep@2.0.0's store entry,
        // expressed relative to `.pnpm/consumer@1.0.0/node_modules/`.
        link(
            "../../dep@2.0.0/node_modules/dep",
            ".pnpm/consumer@1.0.0/node_modules/dep",
        );
        // Top-level symlink: node_modules/consumer -> consumer's store entry,
        // expressed relative to `node_modules/`.
        link(".pnpm/consumer@1.0.0/node_modules/consumer", "consumer");

        // Mount the tree read-only at /root/node_modules, exactly like the live VM.
        let mut config = KernelVmConfig::new("vm-pnpm-vfs");
        config.permissions = Permissions::allow_all();
        let mut kernel = SidecarKernel::new(MountTable::new(MemoryFileSystem::new()), config);
        let host_dir = crate::plugins::host_dir::HostDirFilesystem::new(&node_modules)
            .expect("create host_dir over node_modules");
        kernel
            .mount_boxed_filesystem(
                "/root/node_modules",
                Box::new(MountedVirtualFileSystem::new(host_dir)),
                MountOptions::new("host_dir").read_only(true),
            )
            .expect("mount node_modules read-only");

        let mut cache = LocalModuleResolutionCache::default();
        let mut resolver = ModuleResolver::new(
            KernelModuleFsReader {
                kernel: &mut kernel,
            },
            &mut cache,
        );

        // Importer is the top-level symlink path. The ancestor walk finds `dep`
        // via pnpm's sibling symlink in consumer's store dir (pointing at
        // dep@2.0.0) — no `.pnpm` scan. Resolution reads entirely through the VFS.
        let resolved = resolver
            .resolve_module(
                "dep",
                "/root/node_modules/consumer/index.mjs",
                ModuleResolveMode::Import,
            )
            .expect("resolve dep through kernel VFS");
        assert_eq!(
            resolved.as_deref(),
            Some("/root/node_modules/.pnpm/consumer@1.0.0/node_modules/dep/index.mjs"),
            "must resolve dep@2.0.0 via the sibling symlink, not the aaa-other decoy",
        );

        // And the resolved source loads through the VFS too.
        let source = resolver
            .load_file("/root/node_modules/.pnpm/consumer@1.0.0/node_modules/dep/index.mjs")
            .expect("read resolved dep source via kernel VFS")
            .expect("load resolved dep source via kernel VFS");
        assert_eq!(source, "export const wanted = 2;");

        fs::remove_dir_all(node_modules.parent().expect("temp parent")).expect("remove temp tree");
    }

    // Companion to the kernel-VFS test above, but resolving through the
    // `HostDirModuleReader` — the bridge-thread reader the live VM uses so module
    // resolution runs concurrently with the service loop instead of serializing
    // behind it. It reads the SAME read-only `host_dir` mount (anchored
    // resolve-beneath, escaping-symlink refusal) and must resolve the identical pnpm layout to the
    // identical guest path, with no `.pnpm` scanning and the symlink-pointed
    // version winning over the decoy.
    #[test]
    fn faithful_pnpm_symlink_layout_resolves_through_host_dir_module_reader() {
        use crate::plugins::host_dir::HostDirModuleReader;
        use agentos_execution::{LocalModuleResolutionCache, ModuleResolveMode, ModuleResolver};
        use std::os::unix::fs::symlink;

        let node_modules = temp_dir("pnpm-reader-node-modules").join("node_modules");
        let write = |relative: &str, contents: &str| {
            let path = node_modules.join(relative);
            fs::create_dir_all(path.parent().expect("parent")).expect("create dirs");
            fs::write(path, contents).expect("write fixture");
        };
        let link = |relative_target: &str, link_relative: &str| {
            let link_path = node_modules.join(link_relative);
            fs::create_dir_all(link_path.parent().expect("link parent")).expect("create dirs");
            symlink(relative_target, link_path).expect("create symlink");
        };

        write(
            ".pnpm/consumer@1.0.0/node_modules/consumer/index.mjs",
            "import { wanted } from 'dep';\nexport default wanted;",
        );
        write(
            ".pnpm/consumer@1.0.0/node_modules/consumer/package.json",
            r#"{ "version": "1.0.0", "type": "module", "exports": { ".": "./index.mjs" } }"#,
        );
        write(
            ".pnpm/dep@2.0.0/node_modules/dep/index.mjs",
            "export const wanted = 2;",
        );
        write(
            ".pnpm/dep@2.0.0/node_modules/dep/package.json",
            r#"{ "version": "2.0.0", "type": "module", "exports": { ".": "./index.mjs" } }"#,
        );
        write(
            ".pnpm/aaa-other@1.0.0/node_modules/dep/index.js",
            "module.exports = 1;",
        );
        write(
            ".pnpm/aaa-other@1.0.0/node_modules/dep/package.json",
            r#"{ "version": "1.0.0", "main": "index.js" }"#,
        );
        link(
            "../../dep@2.0.0/node_modules/dep",
            ".pnpm/consumer@1.0.0/node_modules/dep",
        );
        link(".pnpm/consumer@1.0.0/node_modules/consumer", "consumer");

        // The reader is anchored at the node_modules host root, mounted at the
        // guest convention `/root/node_modules` — exactly what build_module_reader
        // derives for the live VM.
        let reader = HostDirModuleReader::from_mounts([("/root/node_modules", &node_modules)])
            .expect("build host_dir module reader");
        let mut cache = LocalModuleResolutionCache::default();
        let mut resolver = ModuleResolver::new(reader, &mut cache);

        let resolved = resolver
            .resolve_module(
                "dep",
                "/root/node_modules/consumer/index.mjs",
                ModuleResolveMode::Import,
            )
            .expect("resolve dep through host-dir module reader");
        assert_eq!(
            resolved.as_deref(),
            Some("/root/node_modules/.pnpm/consumer@1.0.0/node_modules/dep/index.mjs"),
            "reader must resolve dep@2.0.0 via the sibling symlink, not the aaa-other decoy",
        );

        let source = resolver
            .load_file("/root/node_modules/.pnpm/consumer@1.0.0/node_modules/dep/index.mjs")
            .expect("read resolved dep source via host_dir reader")
            .expect("load resolved dep source via host_dir reader");
        assert_eq!(source, "export const wanted = 2;");

        // Escaping-symlink refusal is preserved by the mount: a link pointing
        // outside the node_modules root must not read through it.
        let outside = temp_dir("pnpm-reader-outside");
        fs::create_dir_all(&outside).expect("create outside dir");
        fs::write(outside.join("escaped.js"), "module.exports = 'escaped';")
            .expect("write escape target");
        symlink(&outside, node_modules.join("escape-link")).expect("create escaping symlink");
        let escape_reader =
            HostDirModuleReader::from_mounts([("/root/node_modules", &node_modules)])
                .expect("build host_dir module reader");
        let mut escape_cache = LocalModuleResolutionCache::default();
        let mut escape_resolver = ModuleResolver::new(escape_reader, &mut escape_cache);
        let escaped = escape_resolver
            .load_file("/root/node_modules/escape-link/escaped.js")
            .expect_err("escaping symlink must produce a typed confinement error");
        assert_eq!(escaped.code, "EACCES");

        fs::remove_dir_all(node_modules.parent().expect("temp parent")).expect("remove temp tree");
        fs::remove_dir_all(&outside).ok();
    }

    // Phase 0 perf gate: compare cold-start module resolution cost of the new
    // kernel-VFS path against the legacy host-direct path over a representative
    // node_modules closure. Run with:
    //   cargo test -p agentos-native-sidecar --lib module_resolution_vfs_vs_host_cold_start_perf -- --nocapture --ignored
    #[test]
    #[ignore = "perf microbenchmark; run explicitly with --ignored --nocapture"]
    fn module_resolution_vfs_vs_host_cold_start_perf() {
        use super::KernelModuleFsReader;
        use agentos_execution::javascript::ModuleResolutionTestHarness;
        use agentos_execution::{LocalModuleResolutionCache, ModuleResolveMode, ModuleResolver};
        use agentos_kernel::mount_table::{MountOptions, MountedVirtualFileSystem};
        use std::time::Instant;

        // Build a representative closure: a root entry that imports N packages,
        // each a scoped/unscoped package with its own package.json + nested dep.
        const PACKAGES: usize = 40;
        let root = temp_dir("perf-closure");
        let write = |relative: &str, contents: &str| {
            let path = root.join(relative);
            fs::create_dir_all(path.parent().expect("parent")).expect("create dirs");
            fs::write(path, contents).expect("write");
        };

        let mut imports = Vec::new();
        for i in 0..PACKAGES {
            let pkg = format!("pkg{i}");
            write(
                &format!("node_modules/{pkg}/package.json"),
                &format!(r#"{{ "name": "{pkg}", "version": "1.0.0", "main": "lib/index.js" }}"#),
            );
            write(
                &format!("node_modules/{pkg}/lib/index.js"),
                "module.exports = require('./helper');",
            );
            write(
                &format!("node_modules/{pkg}/lib/helper.js"),
                "module.exports = 1;",
            );
            // a nested transitive dependency
            write(
                &format!("node_modules/{pkg}/node_modules/dep{i}/package.json"),
                &format!(r#"{{ "name": "dep{i}", "version": "1.0.0" }}"#),
            );
            write(
                &format!("node_modules/{pkg}/node_modules/dep{i}/index.js"),
                "module.exports = 2;",
            );
            imports.push(pkg);
        }
        write("index.js", "// root entry\n");

        let from = "/root/index.js";
        let iterations = 50usize;

        // --- Host-direct path (legacy) ---
        let host_start = Instant::now();
        for _ in 0..iterations {
            let mut harness = ModuleResolutionTestHarness::new(&root);
            for pkg in &imports {
                harness
                    .resolve_require(pkg, from)
                    .expect("host resolver perf fixture must resolve package");
            }
        }
        let host_elapsed = host_start.elapsed();

        // --- Kernel-VFS path (new) ---
        // Mount the whole closure root so /root resolves through the VFS.
        let build_kernel = || {
            let mut config = KernelVmConfig::new("vm-perf");
            config.permissions = Permissions::allow_all();
            let mut kernel = SidecarKernel::new(MountTable::new(MemoryFileSystem::new()), config);
            let host_dir = crate::plugins::host_dir::HostDirFilesystem::new(&root)
                .expect("host_dir over closure root");
            kernel
                .mount_boxed_filesystem(
                    "/root",
                    Box::new(MountedVirtualFileSystem::new(host_dir)),
                    MountOptions::new("host_dir").read_only(true),
                )
                .expect("mount /root");
            kernel
        };

        let vfs_start = Instant::now();
        for _ in 0..iterations {
            let mut kernel = build_kernel();
            let mut cache = LocalModuleResolutionCache::default();
            let mut resolver = ModuleResolver::new(
                KernelModuleFsReader {
                    kernel: &mut kernel,
                },
                &mut cache,
            );
            for pkg in &imports {
                resolver
                    .resolve_module(pkg, from, ModuleResolveMode::Require)
                    .expect("kernel resolver perf fixture must not fail")
                    .expect("kernel resolver perf fixture must resolve package");
            }
        }
        let vfs_elapsed = vfs_start.elapsed();

        // Exclude kernel-build cost from the VFS resolution figure by measuring
        // it separately, so the comparison is resolution-vs-resolution.
        let build_start = Instant::now();
        for _ in 0..iterations {
            let _kernel = build_kernel();
        }
        let build_elapsed = build_start.elapsed();
        let vfs_resolve_only = vfs_elapsed.saturating_sub(build_elapsed);

        let per_closure_host = host_elapsed / iterations as u32;
        let per_closure_vfs = vfs_elapsed / iterations as u32;
        let per_closure_vfs_resolve = vfs_resolve_only / iterations as u32;

        eprintln!("\n=== Phase 0 module-resolution cold-start perf ===");
        eprintln!("closure: {PACKAGES} packages, {iterations} cold iterations");
        eprintln!("host-direct : {host_elapsed:?} total | {per_closure_host:?} / closure");
        eprintln!(
            "kernel-VFS  : {vfs_elapsed:?} total | {per_closure_vfs:?} / closure (incl. mount build)"
        );
        eprintln!(
            "kernel-VFS  : {vfs_resolve_only:?} total | {per_closure_vfs_resolve:?} / closure (resolution only)"
        );
        eprintln!(
            "kernel build: {build_elapsed:?} total | {:?} / closure",
            build_elapsed / iterations as u32
        );
        let ratio = vfs_resolve_only.as_secs_f64() / host_elapsed.as_secs_f64().max(1e-9);
        eprintln!("ratio (vfs-resolve / host): {ratio:.2}x");

        fs::remove_dir_all(&root).expect("remove perf tree");
    }
}
