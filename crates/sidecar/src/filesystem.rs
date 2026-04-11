//! Guest filesystem and VFS dispatch extracted from service.rs.

use crate::execution::host_path_from_runtime_guest_mappings;
use crate::protocol::{
    GuestFilesystemCallRequest, GuestFilesystemOperation, GuestFilesystemResultResponse,
    GuestFilesystemStat, RequestFrame, ResponsePayload, RootFilesystemEntryEncoding,
};
use crate::service::{
    javascript_sync_rpc_arg_str, javascript_sync_rpc_arg_u32, javascript_sync_rpc_arg_u32_optional,
    javascript_sync_rpc_arg_u64, javascript_sync_rpc_arg_u64_optional,
    javascript_sync_rpc_bytes_arg, javascript_sync_rpc_bytes_value, javascript_sync_rpc_encoding,
    javascript_sync_rpc_option_bool, javascript_sync_rpc_option_u32, kernel_error, normalize_path,
};
use crate::state::{
    ActiveProcess, BridgeError, SidecarKernel, VmState, EXECUTION_DRIVER_NAME,
    PYTHON_VFS_RPC_GUEST_ROOT,
};
use crate::{DispatchResult, NativeSidecar, NativeSidecarBridge, SidecarError};

use agent_os_execution::{
    JavascriptSyncRpcRequest, PythonVfsRpcMethod, PythonVfsRpcRequest, PythonVfsRpcResponsePayload,
    PythonVfsRpcStat,
};
use agent_os_kernel::vfs::VirtualStat;
use base64::Engine;
use nix::libc;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

const PYTHON_PYODIDE_GUEST_ROOT: &str = "/__agent_os_pyodide";
const PYTHON_PYODIDE_CACHE_GUEST_ROOT: &str = "/__agent_os_pyodide_cache";

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

    let vm = sidecar.vms.get_mut(&vm_id).expect("owned VM should exist");
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
    let response = match normalize_python_vfs_rpc_path(&request.path) {
        Ok(path) => {
            let vm = sidecar.vms.get_mut(vm_id).expect("VM should exist");
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

    let vm = sidecar.vms.get_mut(vm_id).expect("VM should exist");
    let process = vm
        .active_processes
        .get_mut(process_id)
        .expect("process should still exist");

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
            if let Some(host_path) = mapped_python_runtime_host_path(
                process,
                path,
                mapped_host_open_is_writable(flags),
            ) {
                return open_mapped_host_fd(process, path, host_path, flags, mode);
            }
            kernel
                .fd_open(EXECUTION_DRIVER_NAME, kernel_pid, path, flags, mode)
                .map(|fd| json!(fd))
                .map_err(kernel_error)
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
            match position {
                Some(offset) => kernel
                    .fd_pwrite(EXECUTION_DRIVER_NAME, kernel_pid, fd, &contents, offset)
                    .map(|written| json!(written))
                    .map_err(kernel_error),
                None => kernel
                    .fd_write(EXECUTION_DRIVER_NAME, kernel_pid, fd, &contents)
                    .map(|written| json!(written))
                    .map_err(kernel_error),
            }
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
            if let Some(host_path) = mapped_python_runtime_host_path(process, path, false) {
                let content = fs::read(&host_path).map_err(|error| {
                    SidecarError::Io(format!(
                        "failed to read mapped guest file {} -> {}: {error}",
                        path,
                        host_path.display()
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
            if let Some(host_path) = mapped_python_runtime_host_path(process, path, true) {
                fs::write(&host_path, contents).map_err(|error| {
                    SidecarError::Io(format!(
                        "failed to write mapped guest file {} -> {}: {error}",
                        path,
                        host_path.display()
                    ))
                })?;
                return Ok(Value::Null);
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
                .map_err(kernel_error)
        }
        "fs.statSync" | "fs.promises.stat" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem stat path")?;
            if let Some(host_path) = mapped_python_runtime_host_path(process, path, false) {
                let metadata = fs::metadata(&host_path).map_err(|error| {
                    SidecarError::Io(format!(
                        "failed to stat mapped guest path {} -> {}: {error}",
                        path,
                        host_path.display()
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
            kernel
                .lstat_for_process(EXECUTION_DRIVER_NAME, kernel_pid, path)
                .map(javascript_sync_rpc_stat_value)
                .map_err(kernel_error)
        }
        "fs.readdirSync" | "fs.promises.readdir" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem readdir path")?;
            if let Some(host_path) = mapped_python_runtime_host_path(process, path, false) {
                let entries = fs::read_dir(&host_path)
                    .map_err(|error| {
                        SidecarError::Io(format!(
                            "failed to read mapped guest directory {} -> {}: {error}",
                            path,
                            host_path.display()
                        ))
                    })?
                    .filter_map(|entry| entry.ok())
                    .filter_map(|entry| entry.file_name().into_string().ok())
                    .collect::<Vec<_>>();
                return Ok(javascript_sync_rpc_readdir_value(entries));
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
            if let Some(host_path) = mapped_python_runtime_host_path(process, path, true) {
                if recursive {
                    fs::create_dir_all(&host_path).map_err(|error| {
                        SidecarError::Io(format!(
                            "failed to create mapped guest directory {} -> {}: {error}",
                            path,
                            host_path.display()
                        ))
                    })?;
                } else {
                    fs::create_dir(&host_path).map_err(|error| {
                        SidecarError::Io(format!(
                            "failed to create mapped guest directory {} -> {}: {error}",
                            path,
                            host_path.display()
                        ))
                    })?;
                }
                return Ok(Value::Null);
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
            if let Some(host_path) = mapped_python_runtime_host_path(process, path, false) {
                fs::metadata(&host_path).map_err(|error| {
                    SidecarError::Io(format!(
                        "failed to access mapped guest path {} -> {}: {error}",
                        path,
                        host_path.display()
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
            let source_host = mapped_python_runtime_host_path(process, source, false);
            let destination_host = mapped_python_runtime_host_path(process, destination, true);
            if source_host.is_some() || destination_host.is_some() {
                let contents = match source_host {
                    Some(ref host_path) => fs::read(host_path).map_err(|error| {
                        SidecarError::Io(format!(
                            "failed to read mapped guest file {} -> {}: {error}",
                            source,
                            host_path.display()
                        ))
                    })?,
                    None => kernel
                        .read_file_for_process(EXECUTION_DRIVER_NAME, kernel_pid, source)
                        .map_err(kernel_error)?,
                };
                return match destination_host {
                    Some(host_path) => fs::write(&host_path, contents)
                        .map(|()| Value::Null)
                        .map_err(|error| {
                            SidecarError::Io(format!(
                                "failed to write mapped guest file {} -> {}: {error}",
                                destination,
                                host_path.display()
                            ))
                        }),
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
            if let Some(host_path) = mapped_python_runtime_host_path(process, path, false) {
                return Ok(Value::Bool(host_path.exists()));
            }
            kernel
                .exists_for_process(EXECUTION_DRIVER_NAME, kernel_pid, path)
                .map(Value::Bool)
                .map_err(kernel_error)
        }
        "fs.readlinkSync" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem readlink path")?;
            kernel
                .read_link_for_process(EXECUTION_DRIVER_NAME, kernel_pid, path)
                .map(Value::String)
                .map_err(kernel_error)
        }
        "fs.symlinkSync" => {
            let target =
                javascript_sync_rpc_arg_str(&request.args, 0, "filesystem symlink target")?;
            let link_path =
                javascript_sync_rpc_arg_str(&request.args, 1, "filesystem symlink path")?;
            kernel
                .symlink(target, link_path)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.linkSync" => {
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
            let source_host = mapped_python_runtime_host_path(process, source, false);
            let destination_host = mapped_python_runtime_host_path(process, destination, true);
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
            if let Some(host_path) = mapped_python_runtime_host_path(process, path, true) {
                return fs::remove_dir(&host_path).map(|()| Value::Null).map_err(|error| {
                    SidecarError::Io(format!(
                        "failed to remove mapped guest directory {} -> {}: {error}",
                        path,
                        host_path.display()
                    ))
                });
            }
            kernel
                .remove_dir(path)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.unlinkSync" | "fs.promises.unlink" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem unlink path")?;
            if let Some(host_path) = mapped_python_runtime_host_path(process, path, true) {
                return fs::remove_file(&host_path).map(|()| Value::Null).map_err(|error| {
                    SidecarError::Io(format!(
                        "failed to remove mapped guest file {} -> {}: {error}",
                        path,
                        host_path.display()
                    ))
                });
            }
            kernel
                .remove_file(path)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.chmodSync" | "fs.promises.chmod" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem chmod path")?;
            let mode = javascript_sync_rpc_arg_u32(&request.args, 1, "filesystem chmod mode")?;
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
        "fs.utimesSync" | "fs.promises.utimes" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem utimes path")?;
            let atime_ms =
                javascript_sync_rpc_arg_u64(&request.args, 1, "filesystem utimes atime")?;
            let mtime_ms =
                javascript_sync_rpc_arg_u64(&request.args, 2, "filesystem utimes mtime")?;
            kernel
                .utimes(path, atime_ms, mtime_ms)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        _ => Err(SidecarError::InvalidState(format!(
            "unsupported JavaScript sync RPC method {}",
            request.method
        ))),
    }
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
        "mtimeMs": stat.mtime_ms,
        "ctimeMs": stat.ctime_ms,
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

fn mapped_python_runtime_host_path(
    process: &ActiveProcess,
    guest_path: &str,
    writable: bool,
) -> Option<PathBuf> {
    let normalized = normalize_path(guest_path);
    let mapped = host_path_from_runtime_guest_mappings(&process.env, &normalized)?;
    let is_asset_path = normalized == PYTHON_PYODIDE_GUEST_ROOT
        || normalized.starts_with(&format!("{PYTHON_PYODIDE_GUEST_ROOT}/"));
    let is_cache_path = normalized == PYTHON_PYODIDE_CACHE_GUEST_ROOT
        || normalized.starts_with(&format!("{PYTHON_PYODIDE_CACHE_GUEST_ROOT}/"));
    if is_asset_path && !writable {
        return Some(mapped);
    }
    if is_cache_path {
        return Some(mapped);
    }
    None
}

fn mapped_host_open_is_writable(flags: u32) -> bool {
    let access_mode = flags & libc::O_ACCMODE as u32;
    access_mode == libc::O_WRONLY as u32
        || access_mode == libc::O_RDWR as u32
        || flags & libc::O_APPEND as u32 != 0
        || flags & libc::O_CREAT as u32 != 0
        || flags & libc::O_TRUNC as u32 != 0
}

fn open_mapped_host_fd(
    process: &mut ActiveProcess,
    guest_path: &str,
    host_path: PathBuf,
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

    let file = options.open(&host_path).map_err(|error| {
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
    source_host: Option<PathBuf>,
    destination: &str,
    destination_host: Option<PathBuf>,
) -> Result<Value, SidecarError> {
    match (source_host, destination_host) {
        (Some(source_host), Some(destination_host)) => {
            fs::rename(&source_host, &destination_host)
                .map(|()| Value::Null)
                .map_err(|error| {
                    SidecarError::Io(format!(
                        "failed to rename mapped guest path {} -> {} ({} -> {}): {error}",
                        source,
                        destination,
                        source_host.display(),
                        destination_host.display()
                    ))
                })
        }
        _ => Err(SidecarError::InvalidState(format!(
            "cannot rename across mapped and kernel-backed paths: {source} -> {destination}"
        ))),
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

    let _ = fs::remove_file(&shadow_path);
    let _ = fs::remove_dir_all(&shadow_path);
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

fn sync_active_shadow_path_to_kernel(
    vm: &mut VmState,
    guest_path: &str,
) -> Result<(), SidecarError> {
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
                )))
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

    match vm.kernel.lstat(guest_path) {
        Ok(stat) if stat.is_directory => {
            let _ = vm.kernel.remove_dir(guest_path);
        }
        Ok(_) => {
            let _ = vm.kernel.remove_file(guest_path);
        }
        Err(_) => {}
    }

    vm.kernel
        .symlink(&target.to_string_lossy(), guest_path)
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
