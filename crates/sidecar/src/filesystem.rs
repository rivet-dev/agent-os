//! Guest filesystem and VFS dispatch extracted from service.rs.

use crate::execution::{
    host_path_from_runtime_guest_mappings, sync_active_process_host_writes_to_kernel,
};
use crate::protocol::{
    GuestFilesystemCallRequest, GuestFilesystemOperation, GuestFilesystemResultResponse,
    GuestFilesystemStat, RequestFrame, ResponsePayload, RootFilesystemEntryEncoding,
};
use crate::service::{
    javascript_sync_rpc_arg_str, javascript_sync_rpc_arg_u32, javascript_sync_rpc_arg_u32_optional,
    javascript_sync_rpc_arg_u64, javascript_sync_rpc_arg_u64_optional,
    javascript_sync_rpc_bytes_arg, javascript_sync_rpc_bytes_value, javascript_sync_rpc_encoding,
    javascript_sync_rpc_option_bool, javascript_sync_rpc_option_u32, kernel_error,
    log_stale_process_event, normalize_host_path, normalize_path, path_is_within_root,
};
use crate::state::{
    ActiveExecutionEvent, ActiveProcess, BridgeError, SidecarKernel, VmState,
    EXECUTION_DRIVER_NAME, PYTHON_VFS_RPC_GUEST_ROOT,
};
use crate::{DispatchResult, NativeSidecar, NativeSidecarBridge, SidecarError};

use agent_os_execution::{
    JavascriptSyncRpcRequest, PythonVfsRpcMethod, PythonVfsRpcRequest, PythonVfsRpcResponsePayload,
    PythonVfsRpcStat,
};
use agent_os_kernel::vfs::{VirtualStat, VirtualTimeSpec, VirtualUtimeSpec};
use base64::Engine;
use nix::errno::Errno;
use nix::fcntl::{open, openat2, OFlag, OpenHow, ResolveFlag};
use nix::libc;
use nix::sys::stat::{utimensat, Mode, UtimensatFlags};
use nix::sys::time::TimeSpec;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::{symlink, FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

const PYTHON_PYODIDE_GUEST_ROOT: &str = "/__agent_os_pyodide";

fn kernel_path_error(
    operation: &str,
    path: &str,
    error: impl Into<agent_os_kernel::kernel::KernelError>,
) -> SidecarError {
    let error = error.into();
    let base = kernel_error(error);
    match base {
        SidecarError::Kernel(message) => {
            SidecarError::Kernel(format!("{operation} {path}: {message}"))
        }
        other => other,
    }
}
const PYTHON_PYODIDE_CACHE_GUEST_ROOT: &str = "/__agent_os_pyodide_cache";
const UTIME_NOW_NSEC: i64 = libc::UTIME_NOW;
const UTIME_OMIT_NSEC: i64 = libc::UTIME_OMIT;

#[derive(Debug, Clone)]
struct MappedRuntimeHostPath {
    guest_path: String,
    host_root: PathBuf,
    host_path: PathBuf,
}

#[derive(Debug, Clone)]
enum MappedRuntimeHostAccess {
    Writable(MappedRuntimeHostPath),
    ReadOnly(MappedRuntimeHostPath),
}

#[derive(Debug)]
struct AnchoredFd {
    fd: RawFd,
}

impl AnchoredFd {
    fn proc_path(&self) -> PathBuf {
        PathBuf::from(format!("/proc/self/fd/{}", self.fd))
    }
}

impl AsRawFd for AnchoredFd {
    fn as_raw_fd(&self) -> RawFd {
        self.fd
    }
}

impl Drop for AnchoredFd {
    fn drop(&mut self) {
        let _ = nix::unistd::close(self.fd);
    }
}

#[derive(Debug)]
struct MappedRuntimeOpenedPath {
    handle: AnchoredFd,
    host_path: PathBuf,
}

#[derive(Debug)]
struct MappedRuntimeParentPath {
    directory: AnchoredFd,
    host_path: PathBuf,
    child_name: OsString,
}

#[derive(Debug, Deserialize)]
struct RuntimeGuestPathMappingWire {
    #[serde(rename = "guestPath")]
    guest_path: String,
    #[serde(rename = "hostPath")]
    host_path: String,
}

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

fn metadata_timespec(
    metadata: &fs::Metadata,
    access_time: bool,
) -> Result<VirtualTimeSpec, SidecarError> {
    let (sec, nsec) = if access_time {
        (metadata.atime(), metadata.atime_nsec())
    } else {
        (metadata.mtime(), metadata.mtime_nsec())
    };
    VirtualTimeSpec::new(sec, nsec.clamp(0, 999_999_999) as u32)
        .map_err(|error| SidecarError::InvalidState(format!("invalid host metadata time: {error}")))
}

fn resolve_host_utime(spec: VirtualUtimeSpec, existing: VirtualTimeSpec) -> TimeSpec {
    match spec {
        VirtualUtimeSpec::Set(spec) => TimeSpec::new(spec.sec, spec.nsec as libc::c_long),
        VirtualUtimeSpec::Now => TimeSpec::new(0, libc::UTIME_NOW),
        VirtualUtimeSpec::Omit => TimeSpec::new(existing.sec, libc::UTIME_OMIT),
    }
}

fn apply_host_path_utimens(
    host_path: &Path,
    atime: VirtualUtimeSpec,
    mtime: VirtualUtimeSpec,
    follow_symlinks: bool,
    context: &str,
) -> Result<(), SidecarError> {
    let existing = match (atime, mtime) {
        (VirtualUtimeSpec::Omit, _) | (_, VirtualUtimeSpec::Omit) => {
            let metadata = if follow_symlinks {
                fs::metadata(host_path)
            } else {
                fs::symlink_metadata(host_path)
            }
            .map_err(|error| {
                SidecarError::Io(format!(
                    "{context}: failed to stat {}: {error}",
                    host_path.display()
                ))
            })?;
            Some((
                metadata_timespec(&metadata, true)?,
                metadata_timespec(&metadata, false)?,
            ))
        }
        _ => None,
    };
    let existing_atime = existing
        .as_ref()
        .map(|(atime, _)| *atime)
        .unwrap_or(VirtualTimeSpec { sec: 0, nsec: 0 });
    let existing_mtime = existing
        .as_ref()
        .map(|(_, mtime)| *mtime)
        .unwrap_or(VirtualTimeSpec { sec: 0, nsec: 0 });
    let times = [
        resolve_host_utime(atime, existing_atime),
        resolve_host_utime(mtime, existing_mtime),
    ];
    let flags = if follow_symlinks {
        UtimensatFlags::FollowSymlink
    } else {
        UtimensatFlags::NoFollowSymlink
    };
    utimensat(None, host_path, &times[0], &times[1], flags).map_err(|error| {
        SidecarError::Io(format!(
            "{context}: failed to update {}: {error}",
            host_path.display()
        ))
    })
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
    let response = match payload.operation {
        GuestFilesystemOperation::ReadFile => {
            sync_active_shadow_path_to_kernel(vm, &payload.path)?;
            let bytes = vm.kernel.read_file(&payload.path).map_err(kernel_error)?;
            let (content, encoding) = encode_guest_filesystem_content(bytes);
            GuestFilesystemResultResponse {
                operation: payload.operation,
                path: payload.path,
                content: Some(content),
                encoding: Some(encoding),
                entries: None,
                stat: None,
                exists: None,
                target: None,
            }
        }
        GuestFilesystemOperation::WriteFile => {
            let bytes = decode_guest_filesystem_content(
                &payload.path,
                payload.content.as_deref(),
                payload.encoding,
            )?;
            vm.kernel
                .write_file(&payload.path, bytes.clone())
                .map_err(kernel_error)?;
            mirror_guest_file_write_to_shadow(vm, &payload.path, &bytes)?;
            GuestFilesystemResultResponse {
                operation: payload.operation,
                path: payload.path,
                content: None,
                encoding: None,
                entries: None,
                stat: None,
                exists: None,
                target: None,
            }
        }
        GuestFilesystemOperation::CreateDir => {
            vm.kernel.create_dir(&payload.path).map_err(kernel_error)?;
            mirror_guest_directory_write_to_shadow(vm, &payload.path)?;
            GuestFilesystemResultResponse {
                operation: payload.operation,
                path: payload.path,
                content: None,
                encoding: None,
                entries: None,
                stat: None,
                exists: None,
                target: None,
            }
        }
        GuestFilesystemOperation::Mkdir => {
            vm.kernel
                .mkdir(&payload.path, payload.recursive)
                .map_err(kernel_error)?;
            mirror_guest_directory_write_to_shadow(vm, &payload.path)?;
            GuestFilesystemResultResponse {
                operation: payload.operation,
                path: payload.path,
                content: None,
                encoding: None,
                entries: None,
                stat: None,
                exists: None,
                target: None,
            }
        }
        GuestFilesystemOperation::Exists => {
            sync_active_shadow_path_to_kernel(vm, &payload.path)?;
            GuestFilesystemResultResponse {
                operation: payload.operation,
                path: payload.path.clone(),
                content: None,
                encoding: None,
                entries: None,
                stat: None,
                exists: Some(vm.kernel.exists(&payload.path).map_err(kernel_error)?),
                target: None,
            }
        }
        GuestFilesystemOperation::Stat => {
            sync_active_shadow_path_to_kernel(vm, &payload.path)?;
            GuestFilesystemResultResponse {
                operation: payload.operation,
                path: payload.path.clone(),
                content: None,
                encoding: None,
                entries: None,
                stat: Some(guest_filesystem_stat(
                    vm.kernel.stat(&payload.path).map_err(kernel_error)?,
                )),
                exists: None,
                target: None,
            }
        }
        GuestFilesystemOperation::Lstat => {
            sync_active_shadow_path_to_kernel(vm, &payload.path)?;
            GuestFilesystemResultResponse {
                operation: payload.operation,
                path: payload.path.clone(),
                content: None,
                encoding: None,
                entries: None,
                stat: Some(guest_filesystem_stat(
                    vm.kernel.lstat(&payload.path).map_err(kernel_error)?,
                )),
                exists: None,
                target: None,
            }
        }
        GuestFilesystemOperation::ReadDir => GuestFilesystemResultResponse {
            operation: payload.operation,
            path: payload.path.clone(),
            content: None,
            encoding: None,
            entries: Some(vm.kernel.read_dir(&payload.path).map_err(kernel_error)?),
            stat: None,
            exists: None,
            target: None,
        },
        GuestFilesystemOperation::RemoveFile => {
            vm.kernel.remove_file(&payload.path).map_err(kernel_error)?;
            remove_guest_shadow_path(vm, &payload.path)?;
            GuestFilesystemResultResponse {
                operation: payload.operation,
                path: payload.path,
                content: None,
                encoding: None,
                entries: None,
                stat: None,
                exists: None,
                target: None,
            }
        }
        GuestFilesystemOperation::RemoveDir => {
            vm.kernel.remove_dir(&payload.path).map_err(kernel_error)?;
            remove_guest_shadow_path(vm, &payload.path)?;
            GuestFilesystemResultResponse {
                operation: payload.operation,
                path: payload.path,
                content: None,
                encoding: None,
                entries: None,
                stat: None,
                exists: None,
                target: None,
            }
        }
        GuestFilesystemOperation::Rename => {
            let destination = payload.destination_path.ok_or_else(|| {
                SidecarError::InvalidState(String::from(
                    "guest filesystem rename requires a destination_path",
                ))
            })?;
            vm.kernel
                .rename(&payload.path, &destination)
                .map_err(kernel_error)?;
            rename_guest_shadow_path(vm, &payload.path, &destination)?;
            GuestFilesystemResultResponse {
                operation: payload.operation,
                path: payload.path,
                content: None,
                encoding: None,
                entries: None,
                stat: None,
                exists: None,
                target: Some(destination),
            }
        }
        GuestFilesystemOperation::Realpath => GuestFilesystemResultResponse {
            operation: payload.operation,
            path: payload.path.clone(),
            content: None,
            encoding: None,
            entries: None,
            stat: None,
            exists: None,
            target: Some(vm.kernel.realpath(&payload.path).map_err(kernel_error)?),
        },
        GuestFilesystemOperation::Symlink => {
            let target = payload.target.ok_or_else(|| {
                SidecarError::InvalidState(String::from(
                    "guest filesystem symlink requires a target",
                ))
            })?;
            vm.kernel
                .symlink(&target, &payload.path)
                .map_err(kernel_error)?;
            mirror_guest_symlink_to_shadow(vm, &payload.path, &target)?;
            GuestFilesystemResultResponse {
                operation: payload.operation,
                path: payload.path,
                content: None,
                encoding: None,
                entries: None,
                stat: None,
                exists: None,
                target: Some(target),
            }
        }
        GuestFilesystemOperation::ReadLink => GuestFilesystemResultResponse {
            operation: payload.operation,
            path: payload.path.clone(),
            content: None,
            encoding: None,
            entries: None,
            stat: None,
            exists: None,
            target: Some(vm.kernel.read_link(&payload.path).map_err(kernel_error)?),
        },
        GuestFilesystemOperation::Link => {
            let destination = payload.destination_path.ok_or_else(|| {
                SidecarError::InvalidState(String::from(
                    "guest filesystem link requires a destination_path",
                ))
            })?;
            vm.kernel
                .link(&payload.path, &destination)
                .map_err(kernel_error)?;
            mirror_guest_link_to_shadow(vm, &payload.path, &destination)?;
            GuestFilesystemResultResponse {
                operation: payload.operation,
                path: payload.path,
                content: None,
                encoding: None,
                entries: None,
                stat: None,
                exists: None,
                target: Some(destination),
            }
        }
        GuestFilesystemOperation::Chmod => {
            let mode = payload.mode.ok_or_else(|| {
                SidecarError::InvalidState(String::from("guest filesystem chmod requires a mode"))
            })?;
            vm.kernel.chmod(&payload.path, mode).map_err(kernel_error)?;
            mirror_guest_chmod_to_shadow(vm, &payload.path, mode)?;
            GuestFilesystemResultResponse {
                operation: payload.operation,
                path: payload.path,
                content: None,
                encoding: None,
                entries: None,
                stat: None,
                exists: None,
                target: None,
            }
        }
        GuestFilesystemOperation::Chown => {
            let uid = payload.uid.ok_or_else(|| {
                SidecarError::InvalidState(String::from("guest filesystem chown requires a uid"))
            })?;
            let gid = payload.gid.ok_or_else(|| {
                SidecarError::InvalidState(String::from("guest filesystem chown requires a gid"))
            })?;
            vm.kernel
                .chown(&payload.path, uid, gid)
                .map_err(kernel_error)?;
            GuestFilesystemResultResponse {
                operation: payload.operation,
                path: payload.path,
                content: None,
                encoding: None,
                entries: None,
                stat: None,
                exists: None,
                target: None,
            }
        }
        GuestFilesystemOperation::Utimes => {
            let atime_ms = payload.atime_ms.ok_or_else(|| {
                SidecarError::InvalidState(String::from(
                    "guest filesystem utimes requires atime_ms",
                ))
            })?;
            let mtime_ms = payload.mtime_ms.ok_or_else(|| {
                SidecarError::InvalidState(String::from(
                    "guest filesystem utimes requires mtime_ms",
                ))
            })?;
            vm.kernel
                .utimes(&payload.path, atime_ms, mtime_ms)
                .map_err(kernel_error)?;
            mirror_guest_utimes_to_shadow(
                vm,
                &payload.path,
                VirtualUtimeSpec::Set(VirtualTimeSpec::from_millis(atime_ms)),
                VirtualUtimeSpec::Set(VirtualTimeSpec::from_millis(mtime_ms)),
                true,
            )?;
            GuestFilesystemResultResponse {
                operation: payload.operation,
                path: payload.path,
                content: None,
                encoding: None,
                entries: None,
                stat: None,
                exists: None,
                target: None,
            }
        }
        GuestFilesystemOperation::Truncate => {
            let len = payload.len.ok_or_else(|| {
                SidecarError::InvalidState(String::from("guest filesystem truncate requires len"))
            })?;
            vm.kernel
                .truncate(&payload.path, len)
                .map_err(kernel_error)?;
            mirror_guest_truncate_to_shadow(vm, &payload.path, len)?;
            GuestFilesystemResultResponse {
                operation: payload.operation,
                path: payload.path,
                content: None,
                encoding: None,
                entries: None,
                stat: None,
                exists: None,
                target: None,
            }
        }
    };

    Ok(DispatchResult {
        response: sidecar.respond(request, ResponsePayload::GuestFilesystemResult(response)),
        events: Vec::new(),
    })
}

pub(crate) fn handle_python_vfs_rpc_request<B>(
    sidecar: &mut NativeSidecar<B>,
    vm_id: &str,
    process_id: &str,
    request: PythonVfsRpcRequest,
) -> Result<(), SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let Some(vm) = sidecar.vms.get(vm_id) else {
        log_stale_process_event(&sidecar.bridge, vm_id, process_id, "python VFS RPC");
        return Ok(());
    };
    if !vm.active_processes.contains_key(process_id) {
        log_stale_process_event(&sidecar.bridge, vm_id, process_id, "python VFS RPC");
        return Ok(());
    }

    let response = match normalize_python_vfs_rpc_path(&request.path) {
        Ok(path) => {
            let Some(vm) = sidecar.vms.get_mut(vm_id) else {
                log_stale_process_event(&sidecar.bridge, vm_id, process_id, "python VFS RPC");
                return Ok(());
            };
            match request.method {
                PythonVfsRpcMethod::Read => vm
                    .kernel
                    .read_file(&path)
                    .map(|content| PythonVfsRpcResponsePayload::Read {
                        content_base64: base64::engine::general_purpose::STANDARD.encode(content),
                    })
                    .map_err(kernel_error),
                PythonVfsRpcMethod::Write => {
                    let content_base64 = request.content_base64.as_deref().ok_or_else(|| {
                        SidecarError::InvalidState(format!(
                            "python VFS fsWrite for {} requires contentBase64",
                            path
                        ))
                    })?;
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(content_base64)
                        .map_err(|error| {
                            SidecarError::InvalidState(format!(
                                "invalid base64 python VFS content for {}: {error}",
                                path
                            ))
                        })?;
                    vm.kernel
                        .write_file(&path, bytes)
                        .map(|()| PythonVfsRpcResponsePayload::Empty)
                        .map_err(kernel_error)
                }
                PythonVfsRpcMethod::Stat => vm
                    .kernel
                    .stat(&path)
                    .map(|stat| PythonVfsRpcResponsePayload::Stat {
                        stat: PythonVfsRpcStat {
                            mode: stat.mode,
                            size: stat.size,
                            is_directory: stat.is_directory,
                            is_symbolic_link: stat.is_symbolic_link,
                        },
                    })
                    .map_err(kernel_error),
                PythonVfsRpcMethod::ReadDir => vm
                    .kernel
                    .read_dir(&path)
                    .map(|entries| PythonVfsRpcResponsePayload::ReadDir { entries })
                    .map_err(kernel_error),
                PythonVfsRpcMethod::Mkdir => vm
                    .kernel
                    .mkdir(&path, request.recursive)
                    .map(|()| PythonVfsRpcResponsePayload::Empty)
                    .map_err(kernel_error),
                PythonVfsRpcMethod::HttpRequest
                | PythonVfsRpcMethod::DnsLookup
                | PythonVfsRpcMethod::SubprocessRun => {
                    Err(SidecarError::InvalidState(String::from(
                        "python non-filesystem RPC reached filesystem dispatcher unexpectedly",
                    )))
                }
            }
        }
        Err(error) => Err(error),
    };

    let Some(vm) = sidecar.vms.get_mut(vm_id) else {
        log_stale_process_event(&sidecar.bridge, vm_id, process_id, "python VFS RPC");
        return Ok(());
    };
    let Some(process) = vm.active_processes.get_mut(process_id) else {
        log_stale_process_event(&sidecar.bridge, vm_id, process_id, "python VFS RPC");
        return Ok(());
    };

    match response {
        Ok(payload) => process
            .execution
            .respond_python_vfs_rpc_success(request.id, payload),
        Err(error) => process.execution.respond_python_vfs_rpc_error(
            request.id,
            "ERR_AGENT_OS_PYTHON_VFS_RPC",
            error.to_string(),
        ),
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
    let _ = sidecar.bridge.emit_log(vm_id, message.clone());
    SidecarError::InvalidState(message)
}

pub(crate) fn encode_guest_filesystem_content(
    content: Vec<u8>,
) -> (String, RootFilesystemEntryEncoding) {
    match String::from_utf8(content) {
        Ok(text) => (text, RootFilesystemEntryEncoding::Utf8),
        Err(error) => (
            base64::engine::general_purpose::STANDARD.encode(error.into_bytes()),
            RootFilesystemEntryEncoding::Base64,
        ),
    }
}

pub(crate) fn normalize_python_vfs_rpc_path(path: &str) -> Result<String, SidecarError> {
    if !path.starts_with('/') {
        return Err(SidecarError::InvalidState(format!(
            "python VFS RPC path {path} must be absolute within {PYTHON_VFS_RPC_GUEST_ROOT}"
        )));
    }

    let normalized = normalize_path(path);
    if normalized == PYTHON_VFS_RPC_GUEST_ROOT
        || normalized.starts_with(&format!("{PYTHON_VFS_RPC_GUEST_ROOT}/"))
    {
        Ok(normalized)
    } else {
        Err(SidecarError::InvalidState(format!(
            "python VFS RPC path {normalized} escapes guest workspace root {PYTHON_VFS_RPC_GUEST_ROOT}"
        )))
    }
}

pub(crate) fn service_javascript_fs_sync_rpc(
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    kernel_pid: u32,
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    match request.method.as_str() {
        "fs.open" | "fs.openSync" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem open path")?;
            let flags = javascript_sync_rpc_arg_u32(&request.args, 1, "filesystem open flags")?;
            let mode =
                javascript_sync_rpc_arg_u32_optional(&request.args, 2, "filesystem open mode")?;
            match mapped_runtime_host_path(process, path, mapped_host_open_is_writable(flags)) {
                Some(MappedRuntimeHostAccess::Writable(mapped_host)) => {
                    materialize_mapped_host_path_from_kernel(
                        kernel,
                        kernel_pid,
                        path,
                        &mapped_host.host_path,
                    )?;
                    let opened = open_mapped_runtime_beneath(
                        &mapped_host,
                        "fs.open",
                        OFlag::from_bits_truncate(flags as i32),
                        Mode::from_bits_truncate(mode.unwrap_or(0o666)),
                    )?;
                    let host_path = opened.host_path.clone();
                    return open_mapped_host_fd(
                        process,
                        path,
                        host_path,
                        opened.handle.proc_path(),
                        flags,
                        mode,
                    );
                }
                Some(MappedRuntimeHostAccess::ReadOnly(_)) => {
                    return Err(read_only_mapped_runtime_host_path_error(path));
                }
                None => {}
            }
            kernel
                .fd_open(EXECUTION_DRIVER_NAME, kernel_pid, path, flags, mode)
                .map(|fd| json!(fd))
                .map_err(|error| kernel_path_error("fs.open", path, error))
        }
        "fs.read" | "fs.readSync" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "filesystem read fd")?;
            let length = usize::try_from(javascript_sync_rpc_arg_u64(
                &request.args,
                1,
                "filesystem read length",
            )?)
            .map_err(|_| {
                SidecarError::InvalidState(
                    "filesystem read length must fit within usize".to_string(),
                )
            })?;
            let position =
                javascript_sync_rpc_arg_u64_optional(&request.args, 2, "filesystem read position")?;
            if let Some(mapped) = process.mapped_host_fd_mut(fd) {
                return read_mapped_host_fd(mapped, fd, length, position);
            }
            let bytes = match position {
                Some(offset) => {
                    kernel.fd_pread(EXECUTION_DRIVER_NAME, kernel_pid, fd, length, offset)
                }
                None => kernel.fd_read(EXECUTION_DRIVER_NAME, kernel_pid, fd, length),
            }
            .map_err(kernel_error)?;
            Ok(javascript_sync_rpc_bytes_value(&bytes))
        }
        "fs.write" | "fs.writeSync" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "filesystem write fd")?;
            let contents =
                javascript_sync_rpc_bytes_arg(&request.args, 1, "filesystem write contents")?;
            let position = javascript_sync_rpc_arg_u64_optional(
                &request.args,
                2,
                "filesystem write position",
            )?;
            if let Some(mapped) = process.mapped_host_fd_mut(fd) {
                return write_mapped_host_fd(mapped, fd, &contents, position);
            }
            let written = match position {
                Some(offset) => kernel
                    .fd_pwrite(EXECUTION_DRIVER_NAME, kernel_pid, fd, &contents, offset)
                    .map_err(kernel_error)?,
                None => kernel
                    .fd_write(EXECUTION_DRIVER_NAME, kernel_pid, fd, &contents)
                    .map_err(kernel_error)?,
            };
            if position.is_none() && kernel_fd_surfaces_stdio_event(kernel, kernel_pid, fd)? {
                let event = if fd == 1 {
                    ActiveExecutionEvent::Stdout(contents.clone())
                } else {
                    ActiveExecutionEvent::Stderr(contents.clone())
                };
                process.pending_execution_events.push_back(event);
            }
            Ok(json!(written))
        }
        "fs.close" | "fs.closeSync" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "filesystem close fd")?;
            if process.close_mapped_host_fd(fd) {
                return Ok(Value::Null);
            }
            kernel
                .fd_close(EXECUTION_DRIVER_NAME, kernel_pid, fd)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.fstat" | "fs.fstatSync" => {
            let fd = javascript_sync_rpc_arg_u32(&request.args, 0, "filesystem fstat fd")?;
            if let Some(mapped) = process.mapped_host_fd(fd) {
                let metadata = mapped.file.metadata().map_err(|error| {
                    SidecarError::Io(format!(
                        "failed to stat mapped guest fd {fd} -> {}: {error}",
                        mapped.path.display()
                    ))
                })?;
                return Ok(javascript_sync_rpc_host_stat_value(&metadata));
            }
            kernel
                .fd_stat(EXECUTION_DRIVER_NAME, kernel_pid, fd)
                .map_err(kernel_error)?;
            kernel
                .dev_fd_stat(EXECUTION_DRIVER_NAME, kernel_pid, fd)
                .map(javascript_sync_rpc_stat_value)
                .map_err(kernel_error)
        }
        "fs.readFileSync" | "fs.promises.readFile" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem readFile path")?;
            let encoding = javascript_sync_rpc_encoding(&request.args);
            if let Some(mapped_host) = mapped_runtime_host_path_for_read(process, path) {
                materialize_mapped_host_path_from_kernel(
                    kernel,
                    kernel_pid,
                    path,
                    &mapped_host.host_path,
                )?;
                let opened = open_mapped_runtime_beneath(
                    &mapped_host,
                    "fs.readFile",
                    OFlag::O_RDONLY,
                    Mode::empty(),
                )?;
                let content = fs::read(opened.handle.proc_path()).map_err(|error| {
                    SidecarError::Io(format!(
                        "failed to read mapped guest file {} -> {}: {error}",
                        path,
                        opened.host_path.display()
                    ))
                })?;
                return Ok(match encoding.as_deref() {
                    Some("utf8") | Some("utf-8") => {
                        Value::String(String::from_utf8_lossy(&content).into_owned())
                    }
                    _ => javascript_sync_rpc_bytes_value(&content),
                });
            }
            kernel
                .read_file_for_process(EXECUTION_DRIVER_NAME, kernel_pid, path)
                .map(|content| match encoding.as_deref() {
                    Some("utf8") | Some("utf-8") => {
                        Value::String(String::from_utf8_lossy(&content).into_owned())
                    }
                    _ => javascript_sync_rpc_bytes_value(&content),
                })
                .map_err(kernel_error)
        }
        "fs.writeFileSync" | "fs.promises.writeFile" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem writeFile path")?;
            let contents =
                javascript_sync_rpc_bytes_arg(&request.args, 1, "filesystem writeFile contents")?;
            match mapped_runtime_host_path(process, path, true) {
                Some(MappedRuntimeHostAccess::Writable(mapped_host)) => {
                    let opened = open_mapped_runtime_beneath(
                        &mapped_host,
                        "fs.writeFile",
                        OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_TRUNC,
                        Mode::from_bits_truncate(
                            javascript_sync_rpc_option_u32(&request.args, 2, "mode")?
                                .unwrap_or(0o666),
                        ),
                    )?;
                    fs::write(opened.handle.proc_path(), contents).map_err(|error| {
                        SidecarError::Io(format!(
                            "failed to write mapped guest file {} -> {}: {error}",
                            path,
                            opened.host_path.display()
                        ))
                    })?;
                    return Ok(Value::Null);
                }
                Some(MappedRuntimeHostAccess::ReadOnly(_)) => {
                    return Err(read_only_mapped_runtime_host_path_error(path));
                }
                None => {}
            }
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
        "fs.statSync" | "fs.promises.stat" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem stat path")?;
            if let Some(mapped_host) = mapped_runtime_host_path_for_read(process, path) {
                materialize_mapped_host_path_from_kernel(
                    kernel,
                    kernel_pid,
                    path,
                    &mapped_host.host_path,
                )?;
                let opened = open_mapped_runtime_beneath(
                    &mapped_host,
                    "fs.stat",
                    OFlag::O_PATH,
                    Mode::empty(),
                )?;
                let metadata = fs::metadata(opened.handle.proc_path()).map_err(|error| {
                    SidecarError::Io(format!(
                        "failed to stat mapped guest path {} -> {}: {error}",
                        path,
                        opened.host_path.display()
                    ))
                })?;
                return Ok(javascript_sync_rpc_host_stat_value(&metadata));
            }
            kernel
                .stat_for_process(EXECUTION_DRIVER_NAME, kernel_pid, path)
                .map(javascript_sync_rpc_stat_value)
                .map_err(kernel_error)
        }
        "fs.lstatSync" | "fs.promises.lstat" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem lstat path")?;
            if let Some(mapped_host) = mapped_runtime_host_path_for_read(process, path) {
                materialize_mapped_host_path_from_kernel(
                    kernel,
                    kernel_pid,
                    path,
                    &mapped_host.host_path,
                )?;
                let parent = open_mapped_runtime_parent_beneath(&mapped_host, "fs.lstat")?;
                let host_path = parent.host_path.join(&parent.child_name);
                let metadata = fs::symlink_metadata(mapped_runtime_parent_child_path(&parent))
                    .map_err(|error| {
                        SidecarError::Io(format!(
                            "failed to lstat mapped guest path {} -> {}: {error}",
                            path,
                            host_path.display()
                        ))
                    })?;
                return Ok(javascript_sync_rpc_host_stat_value(&metadata));
            }
            kernel
                .lstat_for_process(EXECUTION_DRIVER_NAME, kernel_pid, path)
                .map(javascript_sync_rpc_stat_value)
                .map_err(kernel_error)
        }
        "fs.readdirSync" | "fs.promises.readdir" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem readdir path")?;
            if let Some(MappedRuntimeHostAccess::Writable(mapped_host)) =
                mapped_runtime_host_path(process, path, false)
            {
                let directory = open_mapped_runtime_beneath(
                    &mapped_host,
                    "fs.readdir",
                    OFlag::O_DIRECTORY | OFlag::O_RDONLY,
                    Mode::empty(),
                )?;
                let mut entries = fs::read_dir(directory.handle.proc_path())
                    .map_err(|error| {
                        SidecarError::Io(format!(
                            "failed to read mapped guest directory {} -> {}: {error}",
                            path,
                            directory.host_path.display()
                        ))
                    })?
                    .filter_map(|entry| entry.ok())
                    .filter(|entry| {
                        let child = MappedRuntimeHostPath {
                            guest_path: normalize_path(&format!(
                                "{}/{}",
                                path.trim_end_matches('/'),
                                entry.file_name().to_string_lossy()
                            )),
                            host_root: mapped_host.host_root.clone(),
                            host_path: directory.host_path.join(entry.file_name()),
                        };
                        open_mapped_runtime_beneath(
                            &child,
                            "fs.readdir entry",
                            OFlag::O_PATH,
                            Mode::empty(),
                        )
                        .is_ok()
                    })
                    .filter_map(|entry| entry.file_name().into_string().ok())
                    .collect::<BTreeSet<_>>();
                entries.extend(mapped_runtime_child_mount_basenames(process, path));
                return Ok(javascript_sync_rpc_readdir_value(
                    entries.into_iter().collect(),
                ));
            }
            kernel
                .read_dir_for_process(EXECUTION_DRIVER_NAME, kernel_pid, path)
                .map(javascript_sync_rpc_readdir_value)
                .map_err(kernel_error)
        }
        "fs.mkdirSync" | "fs.promises.mkdir" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem mkdir path")?;
            let recursive =
                javascript_sync_rpc_option_bool(&request.args, 1, "recursive").unwrap_or(false);
            match mapped_runtime_host_path(process, path, true) {
                Some(MappedRuntimeHostAccess::Writable(mapped_host)) => {
                    if recursive {
                        ensure_mapped_runtime_parent_dirs(&mapped_host, "fs.mkdir")?;
                        let parent = open_mapped_runtime_parent_beneath(&mapped_host, "fs.mkdir")?;
                        fs::create_dir(mapped_runtime_parent_child_path(&parent)).map_err(
                            |error| {
                                SidecarError::Io(format!(
                                    "failed to create mapped guest directory {} -> {}: {error}",
                                    path,
                                    parent.host_path.join(&parent.child_name).display()
                                ))
                            },
                        )?;
                    } else {
                        let parent = open_mapped_runtime_parent_beneath(&mapped_host, "fs.mkdir")?;
                        fs::create_dir(mapped_runtime_parent_child_path(&parent)).map_err(
                            |error| {
                                SidecarError::Io(format!(
                                    "failed to create mapped guest directory {} -> {}: {error}",
                                    path,
                                    parent.host_path.join(&parent.child_name).display()
                                ))
                            },
                        )?;
                    }
                    return Ok(Value::Null);
                }
                Some(MappedRuntimeHostAccess::ReadOnly(_)) => {
                    return Err(read_only_mapped_runtime_host_path_error(path));
                }
                None => {}
            }
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
        "fs.accessSync" | "fs.promises.access" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem access path")?;
            if let Some(mapped_host) = mapped_runtime_host_path_for_read(process, path) {
                materialize_mapped_host_path_from_kernel(
                    kernel,
                    kernel_pid,
                    path,
                    &mapped_host.host_path,
                )?;
                let opened = open_mapped_runtime_beneath(
                    &mapped_host,
                    "fs.access",
                    OFlag::O_PATH,
                    Mode::empty(),
                )?;
                fs::metadata(opened.handle.proc_path()).map_err(|error| {
                    SidecarError::Io(format!(
                        "failed to access mapped guest path {} -> {}: {error}",
                        path,
                        opened.host_path.display()
                    ))
                })?;
                return Ok(Value::Null);
            }
            kernel
                .stat_for_process(EXECUTION_DRIVER_NAME, kernel_pid, path)
                .map(|_| Value::Null)
                .map_err(kernel_error)
        }
        "fs.copyFileSync" | "fs.promises.copyFile" => {
            let source =
                javascript_sync_rpc_arg_str(&request.args, 0, "filesystem copyFile source")?;
            let destination =
                javascript_sync_rpc_arg_str(&request.args, 1, "filesystem copyFile destination")?;
            let source_host = mapped_runtime_host_path(process, source, false);
            let destination_host = mapped_runtime_host_path(process, destination, true);
            if matches!(destination_host, Some(MappedRuntimeHostAccess::ReadOnly(_))) {
                return Err(read_only_mapped_runtime_host_path_error(destination));
            }
            if source_host.is_some() || destination_host.is_some() {
                let contents = match source_host {
                    Some(MappedRuntimeHostAccess::Writable(ref mapped_host)) => {
                        let opened = open_mapped_runtime_beneath(
                            mapped_host,
                            "fs.copyFile source",
                            OFlag::O_RDONLY,
                            Mode::empty(),
                        )?;
                        fs::read(opened.handle.proc_path()).map_err(|error| {
                            SidecarError::Io(format!(
                                "failed to read mapped guest file {} -> {}: {error}",
                                source,
                                opened.host_path.display()
                            ))
                        })?
                    }
                    Some(MappedRuntimeHostAccess::ReadOnly(ref mapped_host)) => {
                        let opened = open_mapped_runtime_beneath(
                            mapped_host,
                            "fs.copyFile source",
                            OFlag::O_RDONLY,
                            Mode::empty(),
                        )?;
                        fs::read(opened.handle.proc_path()).map_err(|error| {
                            SidecarError::Io(format!(
                                "failed to read mapped guest file {} -> {}: {error}",
                                source,
                                opened.host_path.display()
                            ))
                        })?
                    }
                    None => kernel
                        .read_file_for_process(EXECUTION_DRIVER_NAME, kernel_pid, source)
                        .map_err(kernel_error)?,
                };
                return match destination_host {
                    Some(MappedRuntimeHostAccess::Writable(mapped_host)) => {
                        let opened = open_mapped_runtime_beneath(
                            &mapped_host,
                            "fs.copyFile destination",
                            OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_TRUNC,
                            Mode::from_bits_truncate(0o666),
                        )?;
                        fs::write(opened.handle.proc_path(), contents)
                            .map(|()| Value::Null)
                            .map_err(|error| {
                                SidecarError::Io(format!(
                                    "failed to write mapped guest file {} -> {}: {error}",
                                    destination,
                                    opened.host_path.display()
                                ))
                            })
                    }
                    Some(MappedRuntimeHostAccess::ReadOnly(_)) => {
                        Err(read_only_mapped_runtime_host_path_error(destination))
                    }
                    None => kernel
                        .write_file_for_process(
                            EXECUTION_DRIVER_NAME,
                            kernel_pid,
                            destination,
                            contents,
                            None,
                        )
                        .map(|()| Value::Null)
                        .map_err(kernel_error),
                };
            }
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
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem exists path")?;
            if let Some(mapped_host) = mapped_runtime_host_path_for_read(process, path) {
                let exists = match open_mapped_runtime_beneath(
                    &mapped_host,
                    "fs.exists",
                    OFlag::O_PATH,
                    Mode::empty(),
                ) {
                    Ok(opened) => fs::metadata(opened.handle.proc_path()).is_ok(),
                    Err(_) => false,
                };
                return Ok(Value::Bool(exists));
            }
            kernel
                .exists_for_process(EXECUTION_DRIVER_NAME, kernel_pid, path)
                .map(Value::Bool)
                .map_err(kernel_error)
        }
        "fs.readlinkSync" | "fs.promises.readlink" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem readlink path")?;
            if let Some(mapped_host) = mapped_runtime_host_path_for_read(process, path) {
                let parent = open_mapped_runtime_parent_beneath(&mapped_host, "fs.readlink")?;
                let host_path = parent.host_path.join(&parent.child_name);
                let target =
                    fs::read_link(mapped_runtime_parent_child_path(&parent)).map_err(|error| {
                        SidecarError::Io(format!(
                            "failed to read mapped guest symlink {} -> {}: {error}",
                            path,
                            host_path.display()
                        ))
                    })?;
                return Ok(Value::String(target.to_string_lossy().into_owned()));
            }
            kernel
                .read_link_for_process(EXECUTION_DRIVER_NAME, kernel_pid, path)
                .map(Value::String)
                .map_err(kernel_error)
        }
        "fs.symlinkSync" | "fs.promises.symlink" => {
            let target =
                javascript_sync_rpc_arg_str(&request.args, 0, "filesystem symlink target")?;
            let link_path =
                javascript_sync_rpc_arg_str(&request.args, 1, "filesystem symlink path")?;
            match mapped_runtime_host_path(process, link_path, true) {
                Some(MappedRuntimeHostAccess::Writable(mapped_host)) => {
                    ensure_mapped_runtime_parent_dirs(&mapped_host, "fs.symlink")?;
                    let parent = open_mapped_runtime_parent_beneath(&mapped_host, "fs.symlink")?;
                    let host_path = parent.host_path.join(&parent.child_name);
                    remove_shadow_path_if_exists(&host_path, link_path)?;
                    symlink(&target, mapped_runtime_parent_child_path(&parent)).map_err(
                        |error| {
                            SidecarError::Io(format!(
                            "failed to create mapped guest symlink {} -> {} ({target}): {error}",
                            link_path,
                            host_path.display()
                        ))
                        },
                    )?;
                    return Ok(Value::Null);
                }
                Some(MappedRuntimeHostAccess::ReadOnly(_)) => {
                    return Err(read_only_mapped_runtime_host_path_error(link_path));
                }
                None => {}
            }
            kernel
                .symlink(target, link_path)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.linkSync" | "fs.promises.link" => {
            let source = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem link source")?;
            let destination =
                javascript_sync_rpc_arg_str(&request.args, 1, "filesystem link path")?;
            kernel
                .link(source, destination)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.renameSync" | "fs.promises.rename" => {
            let source = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem rename source")?;
            let destination =
                javascript_sync_rpc_arg_str(&request.args, 1, "filesystem rename destination")?;
            let source_host = mapped_runtime_host_path(process, source, true);
            let destination_host = mapped_runtime_host_path(process, destination, true);
            if matches!(source_host, Some(MappedRuntimeHostAccess::ReadOnly(_))) {
                return Err(read_only_mapped_runtime_host_path_error(source));
            }
            if matches!(destination_host, Some(MappedRuntimeHostAccess::ReadOnly(_))) {
                return Err(read_only_mapped_runtime_host_path_error(destination));
            }
            if source_host.is_some() || destination_host.is_some() {
                return rename_mapped_host_path(source, source_host, destination, destination_host);
            }
            kernel
                .rename(source, destination)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.rmdirSync" | "fs.promises.rmdir" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem rmdir path")?;
            match mapped_runtime_host_path(process, path, true) {
                Some(MappedRuntimeHostAccess::Writable(mapped_host)) => {
                    let parent = open_mapped_runtime_parent_beneath(&mapped_host, "fs.rmdir")?;
                    let host_path = parent.host_path.join(&parent.child_name);
                    return fs::remove_dir(mapped_runtime_parent_child_path(&parent))
                        .map(|()| Value::Null)
                        .map_err(|error| {
                            SidecarError::Io(format!(
                                "failed to remove mapped guest directory {} -> {}: {error}",
                                path,
                                host_path.display()
                            ))
                        });
                }
                Some(MappedRuntimeHostAccess::ReadOnly(_)) => {
                    return Err(read_only_mapped_runtime_host_path_error(path));
                }
                None => {}
            }
            kernel
                .remove_dir(path)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.unlinkSync" | "fs.promises.unlink" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem unlink path")?;
            match mapped_runtime_host_path(process, path, true) {
                Some(MappedRuntimeHostAccess::Writable(mapped_host)) => {
                    let parent = open_mapped_runtime_parent_beneath(&mapped_host, "fs.unlink")?;
                    let host_path = parent.host_path.join(&parent.child_name);
                    return fs::remove_file(mapped_runtime_parent_child_path(&parent))
                        .map(|()| Value::Null)
                        .map_err(|error| {
                            SidecarError::Io(format!(
                                "failed to remove mapped guest file {} -> {}: {error}",
                                path,
                                host_path.display()
                            ))
                        });
                }
                Some(MappedRuntimeHostAccess::ReadOnly(_)) => {
                    return Err(read_only_mapped_runtime_host_path_error(path));
                }
                None => {}
            }
            kernel
                .remove_file(path)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.chmodSync" | "fs.promises.chmod" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem chmod path")?;
            let mode = javascript_sync_rpc_arg_u32(&request.args, 1, "filesystem chmod mode")?;
            match mapped_runtime_host_path(process, path, true) {
                Some(MappedRuntimeHostAccess::Writable(mapped_host)) => {
                    materialize_mapped_host_path_from_kernel(
                        kernel,
                        kernel_pid,
                        path,
                        &mapped_host.host_path,
                    )?;
                    let opened = open_mapped_runtime_beneath(
                        &mapped_host,
                        "fs.chmod",
                        OFlag::O_PATH,
                        Mode::empty(),
                    )?;
                    if kernel
                        .exists_for_process(EXECUTION_DRIVER_NAME, kernel_pid, path)
                        .map_err(kernel_error)?
                    {
                        kernel.chmod(path, mode).map_err(kernel_error)?;
                    }
                    fs::set_permissions(
                        opened.handle.proc_path(),
                        fs::Permissions::from_mode(mode & 0o7777),
                    )
                    .map_err(|error| {
                        SidecarError::Io(format!(
                            "failed to chmod mapped guest path {} -> {}: {error}",
                            path,
                            opened.host_path.display()
                        ))
                    })?;
                    return Ok(Value::Null);
                }
                Some(MappedRuntimeHostAccess::ReadOnly(_)) => {
                    return Err(read_only_mapped_runtime_host_path_error(path));
                }
                None => {}
            }
            kernel
                .chmod(path, mode)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.chownSync" | "fs.promises.chown" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem chown path")?;
            let uid = javascript_sync_rpc_arg_u32(&request.args, 1, "filesystem chown uid")?;
            let gid = javascript_sync_rpc_arg_u32(&request.args, 2, "filesystem chown gid")?;
            kernel
                .chown(path, uid, gid)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.utimesSync" | "fs.promises.utimes" | "fs.lutimesSync" | "fs.promises.lutimes" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem utimes path")?;
            let atime = parse_utime_arg(&request.args, 1, "filesystem utimes atime")?;
            let mtime = parse_utime_arg(&request.args, 2, "filesystem utimes mtime")?;
            let follow_symlinks = !matches!(
                request.method.as_str(),
                "fs.lutimesSync" | "fs.promises.lutimes"
            );
            match mapped_runtime_host_path(process, path, true) {
                Some(MappedRuntimeHostAccess::Writable(mapped_host)) => {
                    materialize_mapped_host_path_from_kernel(
                        kernel,
                        kernel_pid,
                        path,
                        &mapped_host.host_path,
                    )?;
                    let proc_path;
                    if follow_symlinks {
                        let opened = open_mapped_runtime_beneath(
                            &mapped_host,
                            "fs.utimes",
                            OFlag::O_PATH,
                            Mode::empty(),
                        )?;
                        proc_path = opened.handle.proc_path();
                    } else {
                        let parent =
                            open_mapped_runtime_parent_beneath(&mapped_host, "fs.lutimes")?;
                        proc_path = mapped_runtime_parent_child_path(&parent);
                    }
                    if kernel
                        .exists_for_process(EXECUTION_DRIVER_NAME, kernel_pid, path)
                        .map_err(kernel_error)?
                    {
                        if follow_symlinks {
                            kernel
                                .utimes_spec(path, atime, mtime)
                                .map_err(kernel_error)?;
                        } else {
                            kernel.lutimes(path, atime, mtime).map_err(kernel_error)?;
                        }
                    }
                    apply_host_path_utimens(
                        &proc_path,
                        atime,
                        mtime,
                        follow_symlinks,
                        &format!("failed to update mapped guest path times {path}"),
                    )?;
                    return Ok(Value::Null);
                }
                Some(MappedRuntimeHostAccess::ReadOnly(_)) => {
                    return Err(read_only_mapped_runtime_host_path_error(path));
                }
                None => {}
            }
            if follow_symlinks {
                kernel
                    .utimes_spec(path, atime, mtime)
                    .map(|()| Value::Null)
                    .map_err(kernel_error)
            } else {
                kernel
                    .lutimes(path, atime, mtime)
                    .map(|()| Value::Null)
                    .map_err(kernel_error)
            }
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

fn guest_filesystem_stat(stat: VirtualStat) -> GuestFilesystemStat {
    GuestFilesystemStat {
        mode: stat.mode,
        size: stat.size,
        blocks: stat.blocks,
        dev: stat.dev,
        rdev: stat.rdev,
        is_directory: stat.is_directory,
        is_symbolic_link: stat.is_symbolic_link,
        atime_ms: stat.atime_ms,
        mtime_ms: stat.mtime_ms,
        ctime_ms: stat.ctime_ms,
        birthtime_ms: stat.birthtime_ms,
        ino: stat.ino,
        nlink: stat.nlink,
        uid: stat.uid,
        gid: stat.gid,
    }
}

fn decode_guest_filesystem_content(
    path: &str,
    content: Option<&str>,
    encoding: Option<RootFilesystemEntryEncoding>,
) -> Result<Vec<u8>, SidecarError> {
    let content = content.ok_or_else(|| {
        SidecarError::InvalidState(format!(
            "guest filesystem write_file for {path} requires content",
        ))
    })?;

    match encoding.unwrap_or(RootFilesystemEntryEncoding::Utf8) {
        RootFilesystemEntryEncoding::Utf8 => Ok(content.as_bytes().to_vec()),
        RootFilesystemEntryEncoding::Base64 => base64::engine::general_purpose::STANDARD
            .decode(content)
            .map_err(|error| {
                SidecarError::InvalidState(format!(
                    "invalid base64 guest filesystem content for {path}: {error}",
                ))
            }),
    }
}

fn javascript_sync_rpc_stat_value(stat: VirtualStat) -> Value {
    json!({
        "mode": stat.mode,
        "size": stat.size,
        "blocks": stat.blocks,
        "dev": stat.dev,
        "rdev": stat.rdev,
        "isDirectory": stat.is_directory,
        "isSymbolicLink": stat.is_symbolic_link,
        "atimeMs": stat.atime_ms,
        "atimeNsec": stat.atime_nsec,
        "mtimeMs": stat.mtime_ms,
        "mtimeNsec": stat.mtime_nsec,
        "ctimeMs": stat.ctime_ms,
        "ctimeNsec": stat.ctime_nsec,
        "birthtimeMs": stat.birthtime_ms,
        "ino": stat.ino,
        "nlink": stat.nlink,
        "uid": stat.uid,
        "gid": stat.gid,
    })
}

fn javascript_sync_rpc_host_stat_value(metadata: &fs::Metadata) -> Value {
    json!({
        "mode": metadata.mode(),
        "size": metadata.size(),
        "blocks": metadata.blocks(),
        "dev": metadata.dev(),
        "rdev": metadata.rdev(),
        "isDirectory": metadata.is_dir(),
        "isSymbolicLink": metadata.file_type().is_symlink(),
        "atimeMs": metadata.atime() * 1000 + (metadata.atime_nsec() / 1_000_000),
        "mtimeMs": metadata.mtime() * 1000 + (metadata.mtime_nsec() / 1_000_000),
        "ctimeMs": metadata.ctime() * 1000 + (metadata.ctime_nsec() / 1_000_000),
        "birthtimeMs": metadata.ctime() * 1000 + (metadata.ctime_nsec() / 1_000_000),
        "ino": metadata.ino(),
        "nlink": metadata.nlink(),
        "uid": metadata.uid(),
        "gid": metadata.gid(),
    })
}

fn mapped_runtime_host_path(
    process: &ActiveProcess,
    guest_path: &str,
    writable: bool,
) -> Option<MappedRuntimeHostAccess> {
    let normalized = if guest_path.starts_with('/') {
        normalize_path(guest_path)
    } else {
        normalize_path(&format!(
            "{}/{}",
            process.guest_cwd.trim_end_matches('/'),
            guest_path
        ))
    };
    let mappings = process
        .env
        .get("AGENT_OS_GUEST_PATH_MAPPINGS")
        .and_then(|value| serde_json::from_str::<Vec<RuntimeGuestPathMappingWire>>(value).ok())?;
    let mut sorted_mappings = mappings
        .into_iter()
        .filter_map(|mapping| {
            (!mapping.guest_path.is_empty() && !mapping.host_path.is_empty()).then_some((
                normalize_path(&mapping.guest_path),
                PathBuf::from(mapping.host_path),
            ))
        })
        .collect::<Vec<_>>();
    sorted_mappings.sort_by(|left, right| right.0.len().cmp(&left.0.len()));
    let readable_roots = runtime_host_access_roots(process, "AGENT_OS_EXTRA_FS_READ_PATHS")?;
    let writable_roots = writable
        .then(|| runtime_host_access_roots(process, "AGENT_OS_EXTRA_FS_WRITE_PATHS"))
        .flatten()
        .unwrap_or_default();

    for (guest_root, host_root) in sorted_mappings {
        if guest_root != "/"
            && normalized != guest_root
            && !normalized.starts_with(&format!("{guest_root}/"))
        {
            continue;
        }
        if guest_root == "/" && !normalized.starts_with('/') {
            continue;
        }

        let normalized_host_root = if host_root.is_absolute() {
            normalize_host_path(&host_root)
        } else {
            normalize_host_path(&std::env::current_dir().ok()?.join(host_root))
        };
        let suffix = if guest_root == "/" {
            normalized.trim_start_matches('/')
        } else {
            normalized
                .strip_prefix(&guest_root)
                .unwrap_or_default()
                .trim_start_matches('/')
        };
        let host_path = if suffix.is_empty() {
            normalized_host_root.clone()
        } else {
            normalized_host_root.join(suffix)
        };

        let is_asset_path = guest_root == PYTHON_PYODIDE_GUEST_ROOT
            || normalized == PYTHON_PYODIDE_GUEST_ROOT
            || normalized.starts_with(&format!("{PYTHON_PYODIDE_GUEST_ROOT}/"));
        let is_cache_path = guest_root == PYTHON_PYODIDE_CACHE_GUEST_ROOT
            || normalized == PYTHON_PYODIDE_CACHE_GUEST_ROOT
            || normalized.starts_with(&format!("{PYTHON_PYODIDE_CACHE_GUEST_ROOT}/"));
        if is_asset_path && !writable {
            return Some(MappedRuntimeHostAccess::Writable(MappedRuntimeHostPath {
                guest_path: normalized.clone(),
                host_root: normalized_host_root.clone(),
                host_path,
            }));
        }
        if is_cache_path {
            return Some(MappedRuntimeHostAccess::Writable(MappedRuntimeHostPath {
                guest_path: normalized.clone(),
                host_root: normalized_host_root.clone(),
                host_path,
            }));
        }

        let Some(read_root) = readable_roots
            .iter()
            .find(|root| path_is_within_root(&host_path, root))
            .cloned()
        else {
            continue;
        };
        if !writable {
            return Some(MappedRuntimeHostAccess::Writable(MappedRuntimeHostPath {
                guest_path: normalized.clone(),
                host_root: read_root.clone(),
                host_path,
            }));
        }
        if let Some(write_root) = writable_roots
            .iter()
            .find(|root| path_is_within_root(&host_path, root))
            .cloned()
        {
            return Some(MappedRuntimeHostAccess::Writable(MappedRuntimeHostPath {
                guest_path: normalized.clone(),
                host_root: write_root.clone(),
                host_path,
            }));
        }
        if guest_root != "/" {
            return Some(MappedRuntimeHostAccess::ReadOnly(MappedRuntimeHostPath {
                guest_path: normalized.clone(),
                host_root: read_root.clone(),
                host_path,
            }));
        }
    }

    None
}

fn mapped_runtime_host_path_for_read(
    process: &ActiveProcess,
    guest_path: &str,
) -> Option<MappedRuntimeHostPath> {
    match mapped_runtime_host_path(process, guest_path, false) {
        Some(MappedRuntimeHostAccess::Writable(mapped_host))
        | Some(MappedRuntimeHostAccess::ReadOnly(mapped_host)) => Some(mapped_host),
        None => None,
    }
}

fn runtime_host_access_roots(process: &ActiveProcess, key: &str) -> Option<Vec<PathBuf>> {
    process
        .env
        .get(key)
        .and_then(|value| serde_json::from_str::<Vec<String>>(value).ok())
        .map(|roots| {
            roots
                .into_iter()
                .map(PathBuf::from)
                .map(|root| normalize_host_path(&root))
                .collect()
        })
}

fn mapped_runtime_child_mount_basenames(process: &ActiveProcess, guest_path: &str) -> Vec<String> {
    let normalized = normalize_path(guest_path);
    let mappings = process
        .env
        .get("AGENT_OS_GUEST_PATH_MAPPINGS")
        .and_then(|value| serde_json::from_str::<Vec<RuntimeGuestPathMappingWire>>(value).ok())
        .unwrap_or_default();
    let mut basenames = BTreeSet::new();
    for mapping in mappings {
        let guest_root = normalize_path(&mapping.guest_path);
        if guest_root == "/" || guest_root == normalized {
            continue;
        }
        if mapped_runtime_parent_path(&guest_root) == normalized {
            basenames.insert(mapped_runtime_basename(&guest_root));
        }
    }
    basenames.into_iter().collect()
}

fn mapped_runtime_parent_path(path: &str) -> String {
    let normalized = normalize_path(path);
    let parent = Path::new(&normalized)
        .parent()
        .unwrap_or_else(|| Path::new("/"));
    let value = parent.to_string_lossy();
    if value.is_empty() {
        String::from("/")
    } else {
        value.into_owned()
    }
}

fn mapped_runtime_basename(path: &str) -> String {
    let normalized = normalize_path(path);
    Path::new(&normalized)
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from("/"))
}

fn read_only_mapped_runtime_host_path_error(guest_path: &str) -> SidecarError {
    SidecarError::Kernel(format!("EROFS: read-only filesystem: {guest_path}"))
}

fn mapped_runtime_resolve_flags() -> ResolveFlag {
    ResolveFlag::RESOLVE_BENEATH | ResolveFlag::RESOLVE_NO_MAGICLINKS
}

fn mapped_runtime_relative_path(mapped: &MappedRuntimeHostPath) -> Result<PathBuf, SidecarError> {
    let normalized_root = normalize_host_path(&mapped.host_root);
    let normalized_path = normalize_host_path(&mapped.host_path);
    if !path_is_within_root(&normalized_path, &normalized_root) {
        return Err(mapped_runtime_host_path_escape_error(
            mapped,
            &normalized_path,
        ));
    }
    let relative = normalized_path
        .strip_prefix(&normalized_root)
        .map_err(|error| {
            SidecarError::InvalidState(format!(
                "failed to relativize mapped guest path {} ({} against {}): {error}",
                mapped.guest_path,
                normalized_path.display(),
                normalized_root.display()
            ))
        })?;
    Ok(if relative.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        relative.to_path_buf()
    })
}

fn open_mapped_runtime_root_dir(
    mapped: &MappedRuntimeHostPath,
    operation: &str,
) -> Result<AnchoredFd, SidecarError> {
    let fd = open(
        &mapped.host_root,
        OFlag::O_CLOEXEC | OFlag::O_DIRECTORY | OFlag::O_RDONLY,
        Mode::empty(),
    )
    .map_err(|error| {
        SidecarError::Io(format!(
            "{operation}: failed to open mapped host root {} for {}: {}",
            mapped.host_root.display(),
            mapped.guest_path,
            std::io::Error::from_raw_os_error(error as i32)
        ))
    })?;
    Ok(AnchoredFd { fd })
}

fn open_mapped_runtime_beneath(
    mapped: &MappedRuntimeHostPath,
    operation: &str,
    flags: OFlag,
    mode: Mode,
) -> Result<MappedRuntimeOpenedPath, SidecarError> {
    let root_dir = open_mapped_runtime_root_dir(mapped, operation)?;
    let relative = mapped_runtime_relative_path(mapped)?;
    let open_mode = if flags.intersects(OFlag::O_CREAT | OFlag::O_TMPFILE) {
        mode
    } else {
        Mode::empty()
    };
    let fd = openat2(
        root_dir.as_raw_fd(),
        &relative,
        OpenHow::new()
            .flags(flags | OFlag::O_CLOEXEC)
            .mode(open_mode)
            .resolve(mapped_runtime_resolve_flags()),
    )
    .map_err(|error| mapped_runtime_open_error(operation, mapped, error))?;
    let handle = AnchoredFd { fd };
    let host_path = mapped_runtime_host_path_from_fd(mapped, operation, &handle)?;
    Ok(MappedRuntimeOpenedPath { handle, host_path })
}

fn open_mapped_runtime_directory_beneath(
    mapped: &MappedRuntimeHostPath,
    operation: &str,
    relative: &Path,
) -> Result<MappedRuntimeOpenedPath, SidecarError> {
    let root_dir = open_mapped_runtime_root_dir(mapped, operation)?;
    let fd = openat2(
        root_dir.as_raw_fd(),
        relative,
        OpenHow::new()
            .flags(OFlag::O_CLOEXEC | OFlag::O_DIRECTORY | OFlag::O_RDONLY)
            .mode(Mode::empty())
            .resolve(mapped_runtime_resolve_flags()),
    )
    .map_err(|error| mapped_runtime_open_error(operation, mapped, error))?;
    let handle = AnchoredFd { fd };
    let host_path = mapped_runtime_host_path_from_fd(mapped, operation, &handle)?;
    Ok(MappedRuntimeOpenedPath { handle, host_path })
}

fn open_mapped_runtime_parent_beneath(
    mapped: &MappedRuntimeHostPath,
    operation: &str,
) -> Result<MappedRuntimeParentPath, SidecarError> {
    let relative = mapped_runtime_relative_path(mapped)?;
    let child_name = relative.file_name().ok_or_else(|| {
        SidecarError::InvalidState(format!(
            "{operation}: mapped guest path {} has no parent-relative basename",
            mapped.guest_path
        ))
    })?;
    let parent_relative = relative
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let directory = open_mapped_runtime_directory_beneath(mapped, operation, parent_relative)?;
    Ok(MappedRuntimeParentPath {
        directory: directory.handle,
        host_path: directory.host_path,
        child_name: child_name.to_os_string(),
    })
}

fn mapped_runtime_host_path_from_fd(
    mapped: &MappedRuntimeHostPath,
    operation: &str,
    fd: &AnchoredFd,
) -> Result<PathBuf, SidecarError> {
    fs::read_link(fd.proc_path()).map_err(|error| {
        SidecarError::Io(format!(
            "{operation}: failed to resolve anchored mapped guest path {}: {error}",
            mapped.guest_path
        ))
    })
}

fn mapped_runtime_parent_child_path(parent: &MappedRuntimeParentPath) -> PathBuf {
    parent.directory.proc_path().join(&parent.child_name)
}

fn ensure_mapped_runtime_parent_dirs(
    mapped: &MappedRuntimeHostPath,
    operation: &str,
) -> Result<(), SidecarError> {
    let relative = mapped_runtime_relative_path(mapped)?;
    let Some(parent_relative) = relative
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Ok(());
    };
    if parent_relative == Path::new(".") {
        return Ok(());
    }

    for index in 0..parent_relative.components().count() {
        let prefix = parent_relative
            .components()
            .take(index + 1)
            .collect::<PathBuf>();
        if open_mapped_runtime_directory_beneath(mapped, operation, &prefix).is_ok() {
            continue;
        }

        let prefix_parent = prefix
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let prefix_name = prefix.file_name().ok_or_else(|| {
            SidecarError::InvalidState(format!(
                "{operation}: invalid mapped guest directory prefix for {}",
                mapped.guest_path
            ))
        })?;
        let parent_dir = open_mapped_runtime_directory_beneath(mapped, operation, prefix_parent)?;
        fs::create_dir(parent_dir.handle.proc_path().join(prefix_name)).map_err(|error| {
            SidecarError::Io(format!(
                "{operation}: failed to create mapped guest parent {} under {}: {error}",
                mapped.guest_path,
                parent_dir.host_path.display()
            ))
        })?;
    }

    Ok(())
}

fn mapped_runtime_open_error(
    operation: &str,
    mapped: &MappedRuntimeHostPath,
    error: Errno,
) -> SidecarError {
    match error {
        Errno::EXDEV => mapped_runtime_host_path_escape_error(mapped, &mapped.host_path),
        other => SidecarError::Io(format!(
            "{operation}: failed to open mapped guest path {} beneath {}: {}",
            mapped.guest_path,
            mapped.host_root.display(),
            std::io::Error::from_raw_os_error(other as i32)
        )),
    }
}

fn mapped_runtime_host_path_escape_error(
    mapped: &MappedRuntimeHostPath,
    resolved: &Path,
) -> SidecarError {
    SidecarError::Io(format!(
        "mapped guest path {} escapes mapped host root {} via {}",
        mapped.guest_path,
        mapped.host_root.display(),
        resolved.display()
    ))
}

fn mapped_host_open_is_writable(flags: u32) -> bool {
    let access_mode = flags & libc::O_ACCMODE as u32;
    access_mode == libc::O_WRONLY as u32
        || access_mode == libc::O_RDWR as u32
        || flags & libc::O_APPEND as u32 != 0
        || flags & libc::O_CREAT as u32 != 0
        || flags & libc::O_TRUNC as u32 != 0
}

fn materialize_mapped_host_path_from_kernel(
    kernel: &mut SidecarKernel,
    kernel_pid: u32,
    guest_path: &str,
    host_path: &Path,
) -> Result<(), SidecarError> {
    match fs::symlink_metadata(host_path) {
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(SidecarError::Io(format!(
                "failed to inspect mapped host path for {} -> {}: {error}",
                guest_path,
                host_path.display()
            )));
        }
    }

    if !kernel
        .exists_for_process(EXECUTION_DRIVER_NAME, kernel_pid, guest_path)
        .map_err(kernel_error)?
    {
        return Ok(());
    }

    let stat = kernel
        .lstat_for_process(EXECUTION_DRIVER_NAME, kernel_pid, guest_path)
        .map_err(kernel_error)?;

    if let Some(parent) = host_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            SidecarError::Io(format!(
                "failed to create mapped host parent for {} -> {}: {error}",
                guest_path,
                host_path.display()
            ))
        })?;
    }

    if stat.is_symbolic_link {
        let target = kernel
            .read_link_for_process(EXECUTION_DRIVER_NAME, kernel_pid, guest_path)
            .map_err(kernel_error)?;
        symlink(&target, host_path).map_err(|error| {
            SidecarError::Io(format!(
                "failed to materialize mapped guest symlink {} -> {} ({target}): {error}",
                guest_path,
                host_path.display()
            ))
        })?;
        return Ok(());
    } else if stat.is_directory {
        fs::create_dir_all(host_path).map_err(|error| {
            SidecarError::Io(format!(
                "failed to materialize mapped guest directory {} -> {}: {error}",
                guest_path,
                host_path.display()
            ))
        })?;
    } else {
        let bytes = kernel
            .read_file_for_process(EXECUTION_DRIVER_NAME, kernel_pid, guest_path)
            .map_err(kernel_error)?;
        fs::write(host_path, bytes).map_err(|error| {
            SidecarError::Io(format!(
                "failed to materialize mapped guest file {} -> {}: {error}",
                guest_path,
                host_path.display()
            ))
        })?;
    }

    fs::set_permissions(host_path, fs::Permissions::from_mode(stat.mode & 0o7777)).map_err(
        |error| {
            SidecarError::Io(format!(
                "failed to set permissions for materialized mapped guest path {} -> {}: {error}",
                guest_path,
                host_path.display()
            ))
        },
    )?;

    Ok(())
}

fn open_mapped_host_fd(
    process: &mut ActiveProcess,
    guest_path: &str,
    host_path: PathBuf,
    proc_path: PathBuf,
    flags: u32,
    mode: Option<u32>,
) -> Result<Value, SidecarError> {
    let access_mode = flags & libc::O_ACCMODE as u32;
    let mut options = OpenOptions::new();
    match access_mode {
        x if x == libc::O_WRONLY as u32 => {
            options.write(true);
        }
        x if x == libc::O_RDWR as u32 => {
            options.read(true).write(true);
        }
        _ => {
            options.read(true);
        }
    }
    if flags & libc::O_APPEND as u32 != 0 {
        options.append(true);
    }
    if flags & libc::O_CREAT as u32 != 0 {
        options.create(true);
    }
    if flags & libc::O_EXCL as u32 != 0 {
        options.create_new(true);
    }
    if flags & libc::O_TRUNC as u32 != 0 {
        options.truncate(true);
    }

    let masked_flags = flags
        & !(libc::O_ACCMODE as u32
            | libc::O_APPEND as u32
            | libc::O_CREAT as u32
            | libc::O_EXCL as u32
            | libc::O_TRUNC as u32);
    options.mode(mode.unwrap_or(0o666));
    options.custom_flags(masked_flags as i32);

    let file = options.open(&proc_path).map_err(|error| {
        SidecarError::Io(format!(
            "failed to open mapped guest file {} -> {}: {error}",
            guest_path,
            host_path.display()
        ))
    })?;
    let fd = process.allocate_mapped_host_fd(crate::state::ActiveMappedHostFd {
        file,
        path: host_path,
    });
    Ok(json!(fd))
}

fn read_mapped_host_fd(
    mapped: &mut crate::state::ActiveMappedHostFd,
    fd: u32,
    length: usize,
    position: Option<u64>,
) -> Result<Value, SidecarError> {
    let mut bytes = vec![0_u8; length];
    let read = match position {
        Some(offset) => mapped.file.read_at(&mut bytes, offset),
        None => mapped.file.read(&mut bytes),
    }
    .map_err(|error| {
        SidecarError::Io(format!(
            "failed to read mapped guest fd {fd} -> {}: {error}",
            mapped.path.display()
        ))
    })?;
    bytes.truncate(read);
    Ok(javascript_sync_rpc_bytes_value(&bytes))
}

fn write_mapped_host_fd(
    mapped: &mut crate::state::ActiveMappedHostFd,
    fd: u32,
    contents: &[u8],
    position: Option<u64>,
) -> Result<Value, SidecarError> {
    let written = match position {
        Some(offset) => mapped.file.write_at(contents, offset),
        None => mapped.file.write(contents),
    }
    .map_err(|error| {
        SidecarError::Io(format!(
            "failed to write mapped guest fd {fd} -> {}: {error}",
            mapped.path.display()
        ))
    })?;
    Ok(json!(written))
}

fn rename_mapped_host_path(
    source: &str,
    source_host: Option<MappedRuntimeHostAccess>,
    destination: &str,
    destination_host: Option<MappedRuntimeHostAccess>,
) -> Result<Value, SidecarError> {
    match (source_host, destination_host) {
        (
            Some(MappedRuntimeHostAccess::Writable(source_host)),
            Some(MappedRuntimeHostAccess::Writable(destination_host)),
        ) => {
            if normalize_host_path(&source_host.host_root)
                != normalize_host_path(&destination_host.host_root)
            {
                return Err(SidecarError::Kernel(format!(
                    "EXDEV: invalid cross-device link: {source} -> {destination}"
                )));
            }
            let source_parent = open_mapped_runtime_parent_beneath(&source_host, "fs.rename")?;
            let destination_parent =
                open_mapped_runtime_parent_beneath(&destination_host, "fs.rename")?;
            let source_host_path = source_parent.host_path.join(&source_parent.child_name);
            let destination_host_path = destination_parent
                .host_path
                .join(&destination_parent.child_name);
            rename_mapped_host_path_with_fallback(
                &mapped_runtime_parent_child_path(&source_parent),
                &mapped_runtime_parent_child_path(&destination_parent),
            )
            .map(|()| Value::Null)
            .map_err(|error| {
                SidecarError::Io(format!(
                    "failed to rename mapped guest path {} -> {} ({} -> {}): {error}",
                    source,
                    destination,
                    source_host_path.display(),
                    destination_host_path.display()
                ))
            })
        }
        (Some(MappedRuntimeHostAccess::ReadOnly(_)), _) => {
            Err(read_only_mapped_runtime_host_path_error(source))
        }
        (_, Some(MappedRuntimeHostAccess::ReadOnly(_))) => {
            Err(read_only_mapped_runtime_host_path_error(destination))
        }
        _ => Err(SidecarError::Kernel(format!(
            "EXDEV: invalid cross-device link: {source} -> {destination}"
        ))),
    }
}

fn rename_mapped_host_path_with_fallback(source: &Path, destination: &Path) -> std::io::Result<()> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::rename(source, destination) {
        Ok(()) => Ok(()),
        Err(error) if error.raw_os_error() == Some(libc::EXDEV) => {
            move_mapped_host_path_across_devices(source, destination)
        }
        Err(error) => Err(error),
    }
}

fn move_mapped_host_path_across_devices(source: &Path, destination: &Path) -> std::io::Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    remove_existing_mapped_host_destination(destination)?;
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }

    if metadata.file_type().is_symlink() {
        let target = fs::read_link(source)?;
        symlink(&target, destination)?;
        fs::remove_file(source)?;
        return Ok(());
    }

    if metadata.is_dir() {
        fs::create_dir_all(destination)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            let source_child = entry.path();
            let destination_child = destination.join(entry.file_name());
            move_mapped_host_path_across_devices(&source_child, &destination_child)?;
        }
        fs::set_permissions(destination, metadata.permissions())?;
        fs::remove_dir(source)?;
        return Ok(());
    }

    fs::copy(source, destination)?;
    fs::set_permissions(destination, metadata.permissions())?;
    fs::remove_file(source)?;
    Ok(())
}

fn remove_existing_mapped_host_destination(path: &Path) -> std::io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || metadata.is_file() => {
            fs::remove_file(path)
        }
        Ok(metadata) if metadata.is_dir() => fs::remove_dir(path),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn javascript_sync_rpc_readdir_value(entries: Vec<String>) -> Value {
    json!(entries
        .into_iter()
        .filter(|entry| entry != "." && entry != "..")
        .collect::<Vec<_>>())
}

fn mirror_guest_file_write_to_shadow(
    vm: &mut VmState,
    guest_path: &str,
    bytes: &[u8],
) -> Result<(), SidecarError> {
    let guest_path = normalize_path(guest_path);
    let shadow_path = if guest_path == "/" {
        vm.cwd.clone()
    } else {
        vm.cwd.join(guest_path.trim_start_matches('/'))
    };

    if let Some(parent) = shadow_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            SidecarError::Io(format!(
                "failed to create shadow parent for {}: {error}",
                guest_path
            ))
        })?;
    }

    match fs::symlink_metadata(&shadow_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            fs::remove_file(&shadow_path).map_err(|error| {
                SidecarError::Io(format!(
                    "failed to replace shadow symlink for {}: {error}",
                    guest_path
                ))
            })?;
        }
        Ok(metadata) if metadata.is_dir() => {
            fs::remove_dir_all(&shadow_path).map_err(|error| {
                SidecarError::Io(format!(
                    "failed to replace shadow directory for {}: {error}",
                    guest_path
                ))
            })?;
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(SidecarError::Io(format!(
                "failed to inspect shadow path for {}: {error}",
                guest_path
            )));
        }
    }
    fs::write(&shadow_path, bytes).map_err(|error| {
        SidecarError::Io(format!(
            "failed to mirror guest file {} into shadow root: {error}",
            guest_path
        ))
    })?;

    let stat = vm.kernel.lstat(&guest_path).map_err(kernel_error)?;
    fs::set_permissions(&shadow_path, fs::Permissions::from_mode(stat.mode & 0o7777)).map_err(
        |error| {
            SidecarError::Io(format!(
                "failed to set shadow mode for {}: {error}",
                guest_path
            ))
        },
    )?;

    Ok(())
}

fn mirror_guest_directory_write_to_shadow(
    vm: &mut VmState,
    guest_path: &str,
) -> Result<(), SidecarError> {
    let guest_path = normalize_path(guest_path);
    let shadow_path = shadow_host_path_for_guest(&vm.cwd, &guest_path);

    fs::create_dir_all(&shadow_path).map_err(|error| {
        SidecarError::Io(format!(
            "failed to mirror guest directory {} into shadow root: {error}",
            guest_path
        ))
    })?;

    let stat = vm.kernel.lstat(&guest_path).map_err(kernel_error)?;
    fs::set_permissions(&shadow_path, fs::Permissions::from_mode(stat.mode & 0o7777)).map_err(
        |error| {
            SidecarError::Io(format!(
                "failed to set shadow mode for directory {}: {error}",
                guest_path
            ))
        },
    )?;

    Ok(())
}

fn ensure_guest_path_materialized_in_shadow(
    vm: &mut VmState,
    guest_path: &str,
) -> Result<PathBuf, SidecarError> {
    let guest_path = normalize_path(guest_path);
    let shadow_path = shadow_host_path_for_guest(&vm.cwd, &guest_path);
    if fs::symlink_metadata(&shadow_path).is_ok() {
        return Ok(shadow_path);
    }

    let stat = vm.kernel.lstat(&guest_path).map_err(kernel_error)?;
    if stat.is_symbolic_link {
        let target = vm.kernel.read_link(&guest_path).map_err(kernel_error)?;
        mirror_guest_symlink_to_shadow(vm, &guest_path, &target)?;
    } else if stat.is_directory {
        mirror_guest_directory_write_to_shadow(vm, &guest_path)?;
    } else {
        let bytes = vm.kernel.read_file(&guest_path).map_err(kernel_error)?;
        mirror_guest_file_write_to_shadow(vm, &guest_path, &bytes)?;
    }

    Ok(shadow_path)
}

fn mirror_guest_symlink_to_shadow(
    vm: &mut VmState,
    guest_path: &str,
    target: &str,
) -> Result<(), SidecarError> {
    let guest_path = normalize_path(guest_path);
    let shadow_path = shadow_host_path_for_guest(&vm.cwd, &guest_path);
    let shadow_target = shadow_symlink_target_for_guest(&vm.cwd, &guest_path, target);

    if let Some(parent) = shadow_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            SidecarError::Io(format!(
                "failed to create shadow parent for symlink {}: {error}",
                guest_path
            ))
        })?;
    }

    remove_shadow_path_if_exists(&shadow_path, &guest_path)?;
    symlink(&shadow_target, &shadow_path).map_err(|error| {
        SidecarError::Io(format!(
            "failed to mirror guest symlink {} into shadow root: {error}",
            guest_path
        ))
    })
}

fn mirror_guest_link_to_shadow(
    vm: &mut VmState,
    source_path: &str,
    destination_path: &str,
) -> Result<(), SidecarError> {
    let source_path = normalize_path(source_path);
    let destination_path = normalize_path(destination_path);
    let source_shadow_path = ensure_guest_path_materialized_in_shadow(vm, &source_path)?;
    let destination_shadow_path = shadow_host_path_for_guest(&vm.cwd, &destination_path);

    if let Some(parent) = destination_shadow_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            SidecarError::Io(format!(
                "failed to create shadow parent for link {}: {error}",
                destination_path
            ))
        })?;
    }

    remove_shadow_path_if_exists(&destination_shadow_path, &destination_path)?;
    fs::hard_link(&source_shadow_path, &destination_shadow_path).map_err(|error| {
        SidecarError::Io(format!(
            "failed to mirror guest link {} -> {} into shadow root: {error}",
            source_path, destination_path
        ))
    })
}

fn mirror_guest_chmod_to_shadow(
    vm: &mut VmState,
    guest_path: &str,
    mode: u32,
) -> Result<(), SidecarError> {
    let shadow_path = ensure_guest_path_materialized_in_shadow(vm, guest_path)?;
    fs::set_permissions(&shadow_path, fs::Permissions::from_mode(mode & 0o7777)).map_err(|error| {
        SidecarError::Io(format!(
            "failed to set shadow mode for {}: {error}",
            normalize_path(guest_path)
        ))
    })
}

fn mirror_guest_utimes_to_shadow(
    vm: &mut VmState,
    guest_path: &str,
    atime: VirtualUtimeSpec,
    mtime: VirtualUtimeSpec,
    follow_symlinks: bool,
) -> Result<(), SidecarError> {
    let shadow_path = ensure_guest_path_materialized_in_shadow(vm, guest_path)?;
    apply_host_path_utimens(
        &shadow_path,
        atime,
        mtime,
        follow_symlinks,
        &format!(
            "failed to mirror guest utimes for {} into shadow root",
            normalize_path(guest_path)
        ),
    )
}

fn mirror_guest_truncate_to_shadow(
    vm: &mut VmState,
    guest_path: &str,
    len: u64,
) -> Result<(), SidecarError> {
    let shadow_path = ensure_guest_path_materialized_in_shadow(vm, guest_path)?;
    OpenOptions::new()
        .write(true)
        .open(&shadow_path)
        .and_then(|file| file.set_len(len))
        .map_err(|error| {
            SidecarError::Io(format!(
                "failed to mirror guest truncate for {} into shadow root: {error}",
                normalize_path(guest_path)
            ))
        })
}

fn remove_guest_shadow_path(vm: &mut VmState, guest_path: &str) -> Result<(), SidecarError> {
    let guest_path = normalize_path(guest_path);
    let shadow_path = shadow_host_path_for_guest(&vm.cwd, &guest_path);
    remove_shadow_path_if_exists(&shadow_path, &guest_path)
}

fn rename_guest_shadow_path(
    vm: &mut VmState,
    from_path: &str,
    to_path: &str,
) -> Result<(), SidecarError> {
    let from_path = normalize_path(from_path);
    let to_path = normalize_path(to_path);
    let from_shadow_path = shadow_host_path_for_guest(&vm.cwd, &from_path);
    let to_shadow_path = shadow_host_path_for_guest(&vm.cwd, &to_path);

    if !from_shadow_path.exists() {
        remove_shadow_path_if_exists(&to_shadow_path, &to_path)?;
        return Ok(());
    }

    if let Some(parent) = to_shadow_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            SidecarError::Io(format!(
                "failed to create shadow parent for rename {} -> {}: {error}",
                from_path, to_path
            ))
        })?;
    }

    remove_shadow_path_if_exists(&to_shadow_path, &to_path)?;
    fs::rename(&from_shadow_path, &to_shadow_path).map_err(|error| {
        SidecarError::Io(format!(
            "failed to mirror guest rename {} -> {} into shadow root: {error}",
            from_path, to_path
        ))
    })?;

    Ok(())
}

fn remove_shadow_path_if_exists(shadow_path: &Path, guest_path: &str) -> Result<(), SidecarError> {
    match fs::symlink_metadata(shadow_path) {
        Ok(metadata) => {
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                fs::remove_dir_all(shadow_path).map_err(|error| {
                    SidecarError::Io(format!(
                        "failed to remove shadow directory for {}: {error}",
                        guest_path
                    ))
                })?;
            } else {
                fs::remove_file(shadow_path).map_err(|error| {
                    SidecarError::Io(format!(
                        "failed to remove shadow path for {}: {error}",
                        guest_path
                    ))
                })?;
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(SidecarError::Io(format!(
            "failed to inspect shadow path for {}: {error}",
            guest_path
        ))),
    }
}

fn sync_active_shadow_path_to_kernel(
    vm: &mut VmState,
    guest_path: &str,
) -> Result<(), SidecarError> {
    sync_active_process_host_writes_to_kernel(vm)?;
    let guest_path = normalize_path(guest_path);
    let mut host_paths = active_process_shadow_host_paths_for_guest(vm, &guest_path);
    if host_paths.is_empty() && !vm.kernel.exists(&guest_path).unwrap_or(false) {
        host_paths.push(shadow_host_path_for_guest(&vm.cwd, &guest_path));
    }

    for host_path in host_paths {
        let metadata = match fs::symlink_metadata(&host_path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(SidecarError::Io(format!(
                    "failed to stat host shadow path {}: {error}",
                    host_path.display()
                )));
            }
        };

        if metadata.file_type().is_symlink() {
            sync_host_symlink_to_kernel(vm, &guest_path, &host_path)?;
            return Ok(());
        }

        if metadata.is_dir() {
            sync_host_directory_to_kernel(vm, &guest_path, &metadata)?;
            return Ok(());
        }

        if metadata.is_file() {
            sync_host_file_to_kernel(vm, &guest_path, &host_path, &metadata)?;
            return Ok(());
        }
    }

    Ok(())
}

fn active_process_shadow_host_paths_for_guest(vm: &VmState, guest_path: &str) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let mut seen = BTreeSet::new();

    for process in vm.active_processes.values() {
        if let Some(host_path) = resolve_process_guest_path_to_host(process, guest_path) {
            push_unique_host_path(&mut candidates, &mut seen, host_path);
        }
    }

    candidates
}

fn push_unique_host_path(
    candidates: &mut Vec<PathBuf>,
    seen: &mut BTreeSet<PathBuf>,
    host_path: PathBuf,
) {
    if seen.insert(host_path.clone()) {
        candidates.push(host_path);
    }
}

fn shadow_host_path_for_guest(shadow_root: &Path, guest_path: &str) -> PathBuf {
    if guest_path == "/" {
        shadow_root.to_path_buf()
    } else {
        shadow_root.join(guest_path.trim_start_matches('/'))
    }
}

fn shadow_symlink_target_for_guest(shadow_root: &Path, guest_path: &str, target: &str) -> PathBuf {
    if !target.starts_with('/') {
        return PathBuf::from(target);
    }

    let link_shadow_path = shadow_host_path_for_guest(shadow_root, guest_path);
    let link_parent = link_shadow_path.parent().unwrap_or(shadow_root);
    let target_shadow_path = shadow_host_path_for_guest(shadow_root, target);
    relative_path_from(link_parent, &target_shadow_path)
}

fn relative_path_from(base_dir: &Path, target: &Path) -> PathBuf {
    let base_components: Vec<_> = base_dir.components().collect();
    let target_components: Vec<_> = target.components().collect();

    let mut shared_prefix = 0;
    while shared_prefix < base_components.len()
        && shared_prefix < target_components.len()
        && base_components[shared_prefix] == target_components[shared_prefix]
    {
        shared_prefix += 1;
    }

    let mut relative = PathBuf::new();
    for _ in shared_prefix..base_components.len() {
        relative.push("..");
    }
    for component in target_components.iter().skip(shared_prefix) {
        relative.push(component.as_os_str());
    }

    if relative.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        relative
    }
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
    let mut host_root = process.host_cwd.clone();
    for _ in normalized_guest_cwd
        .trim_start_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
    {
        host_root = host_root.parent()?.to_path_buf();
    }
    Some(shadow_host_path_for_guest(
        &host_root,
        &normalized_guest_path,
    ))
}

fn sync_host_directory_to_kernel(
    vm: &mut VmState,
    guest_path: &str,
    metadata: &fs::Metadata,
) -> Result<(), SidecarError> {
    vm.kernel.mkdir(guest_path, true).map_err(kernel_error)?;
    vm.kernel
        .chmod(guest_path, metadata.permissions().mode() & 0o7777)
        .map_err(kernel_error)?;
    Ok(())
}

fn sync_host_file_to_kernel(
    vm: &mut VmState,
    guest_path: &str,
    host_path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), SidecarError> {
    ensure_guest_parent_dir(vm, guest_path)?;
    let bytes = fs::read(host_path).map_err(|error| {
        SidecarError::Io(format!(
            "failed to read host shadow file {}: {error}",
            host_path.display()
        ))
    })?;
    vm.kernel
        .write_file(guest_path, bytes)
        .map_err(kernel_error)?;
    vm.kernel
        .chmod(guest_path, metadata.permissions().mode() & 0o7777)
        .map_err(kernel_error)?;
    Ok(())
}

fn sync_host_symlink_to_kernel(
    vm: &mut VmState,
    guest_path: &str,
    host_path: &Path,
) -> Result<(), SidecarError> {
    ensure_guest_parent_dir(vm, guest_path)?;
    let target = fs::read_link(host_path).map_err(|error| {
        SidecarError::Io(format!(
            "failed to read host shadow symlink {}: {error}",
            host_path.display()
        ))
    })?;

    let target = restore_guest_symlink_target_from_shadow(vm, guest_path, host_path, &target)
        .unwrap_or_else(|| target.to_string_lossy().into_owned());

    replace_guest_symlink(vm, guest_path, &target)
}

fn restore_guest_symlink_target_from_shadow(
    vm: &VmState,
    guest_path: &str,
    host_path: &Path,
    shadow_target: &Path,
) -> Option<String> {
    if shadow_target.is_absolute() {
        return None;
    }

    let existing_target = vm.kernel.read_link(guest_path).ok()?;
    if !existing_target.starts_with('/') {
        return None;
    }

    let host_parent = host_path.parent().unwrap_or(&vm.cwd);
    let resolved_host_target = normalize_host_path(&host_parent.join(shadow_target));
    let normalized_shadow_root = normalize_host_path(&vm.cwd);
    if resolved_host_target == normalized_shadow_root {
        return Some(String::from("/"));
    }

    resolved_host_target
        .strip_prefix(&normalized_shadow_root)
        .ok()
        .map(|suffix| format!("/{}", suffix.to_string_lossy().trim_start_matches('/')))
}

fn replace_guest_symlink(
    vm: &mut VmState,
    guest_path: &str,
    target: &str,
) -> Result<(), SidecarError> {
    if vm.kernel.symlink(target, guest_path).is_ok() {
        return Ok(());
    }

    if let Ok(existing_target) = vm.kernel.read_link(guest_path) {
        if existing_target == target {
            return Ok(());
        }
    }

    let _ = vm.kernel.remove_file(guest_path);
    let _ = vm.kernel.remove_dir(guest_path);
    vm.kernel
        .symlink(target, guest_path)
        .map_err(kernel_error)?;
    Ok(())
}

fn ensure_guest_parent_dir(vm: &mut VmState, guest_path: &str) -> Result<(), SidecarError> {
    let Some(parent) = Path::new(guest_path).parent() else {
        return Ok(());
    };
    let parent = parent.to_string_lossy();
    if parent.is_empty() || parent == "/" {
        return Ok(());
    }
    vm.kernel
        .mkdir(&normalize_path(&parent), true)
        .map_err(kernel_error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        mapped_runtime_relative_path, open_mapped_runtime_parent_beneath, rename_mapped_host_path,
        MappedRuntimeHostAccess, MappedRuntimeHostPath, SidecarError,
    };
    use crate::execution::javascript_sync_rpc_error_code;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn writable_mapping(guest_path: &str, host_root: &str) -> MappedRuntimeHostAccess {
        let host_root = PathBuf::from(host_root);
        MappedRuntimeHostAccess::Writable(MappedRuntimeHostPath {
            guest_path: guest_path.to_owned(),
            host_path: host_root.join("file.txt"),
            host_root: host_root.clone(),
        })
    }

    #[test]
    fn rename_mapped_host_path_reports_exdev_for_cross_mount_guest_errno() {
        for (source_host, destination_host) in [
            (
                Some(writable_mapping(
                    "/mapped/file.txt",
                    "/tmp/agent-os-mapped-source",
                )),
                None,
            ),
            (
                None,
                Some(writable_mapping(
                    "/mapped-dst/file.txt",
                    "/tmp/agent-os-mapped-destination",
                )),
            ),
        ] {
            let error = rename_mapped_host_path(
                "/mapped/file.txt",
                source_host,
                "/kernel/file.txt",
                destination_host,
            )
            .expect_err("cross-mount rename should fail with EXDEV");
            assert!(
                matches!(error, SidecarError::Kernel(ref message) if message.starts_with("EXDEV:")),
                "expected EXDEV kernel error, got {error:?}"
            );
            assert_eq!(javascript_sync_rpc_error_code(&error), "EXDEV");
        }
    }

    #[test]
    fn mapped_runtime_parent_treats_single_segment_relative_paths_as_root_children() {
        let host_root = std::env::temp_dir().join(format!(
            "agent-os-sidecar-fs-parent-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time before unix epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&host_root).expect("create mapped host root");
        let mapped = MappedRuntimeHostPath {
            guest_path: String::from("/workspace"),
            host_root: host_root.clone(),
            host_path: host_root.join("workspace"),
        };

        assert_eq!(
            mapped_runtime_relative_path(&mapped).expect("relative path"),
            PathBuf::from("workspace")
        );

        let parent = open_mapped_runtime_parent_beneath(&mapped, "test")
            .expect("open mapped parent for root child");
        assert_eq!(parent.host_path, host_root);
        assert_eq!(parent.child_name.to_string_lossy(), "workspace");
    }
}
