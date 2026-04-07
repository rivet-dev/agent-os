//! Guest filesystem and VFS dispatch extracted from service.rs.

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
use crate::state::{BridgeError, SidecarKernel, EXECUTION_DRIVER_NAME, PYTHON_VFS_RPC_GUEST_ROOT};
use crate::{DispatchResult, NativeSidecar, NativeSidecarBridge, SidecarError};

use agent_os_execution::{
    JavascriptSyncRpcRequest, PythonVfsRpcMethod, PythonVfsRpcRequest, PythonVfsRpcResponsePayload,
    PythonVfsRpcStat,
};
use agent_os_kernel::vfs::VirtualStat;
use base64::Engine;
use serde_json::{json, Value};
use std::fmt;

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
                .write_file(&payload.path, bytes)
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
        GuestFilesystemOperation::CreateDir => {
            vm.kernel.create_dir(&payload.path).map_err(kernel_error)?;
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
        GuestFilesystemOperation::Exists => GuestFilesystemResultResponse {
            operation: payload.operation,
            path: payload.path.clone(),
            content: None,
            encoding: None,
            entries: None,
            stat: None,
            exists: Some(vm.kernel.exists(&payload.path).map_err(kernel_error)?),
            target: None,
        },
        GuestFilesystemOperation::Stat => GuestFilesystemResultResponse {
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
        },
        GuestFilesystemOperation::Lstat => GuestFilesystemResultResponse {
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
        },
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
    kernel_pid: u32,
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    match request.method.as_str() {
        "fs.open" | "fs.openSync" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem open path")?;
            let flags = javascript_sync_rpc_arg_u32(&request.args, 1, "filesystem open flags")?;
            let mode =
                javascript_sync_rpc_arg_u32_optional(&request.args, 2, "filesystem open mode")?;
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
            kernel
                .fd_close(EXECUTION_DRIVER_NAME, kernel_pid, fd)
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
        "fs.readFileSync" | "fs.promises.readFile" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem readFile path")?;
            let encoding = javascript_sync_rpc_encoding(&request.args);
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
            kernel
                .read_dir_for_process(EXECUTION_DRIVER_NAME, kernel_pid, path)
                .map(javascript_sync_rpc_readdir_value)
                .map_err(kernel_error)
        }
        "fs.mkdirSync" | "fs.promises.mkdir" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem mkdir path")?;
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
        "fs.accessSync" | "fs.promises.access" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem access path")?;
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
            kernel
                .rename(source, destination)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.rmdirSync" | "fs.promises.rmdir" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem rmdir path")?;
            kernel
                .remove_dir(path)
                .map(|()| Value::Null)
                .map_err(kernel_error)
        }
        "fs.unlinkSync" | "fs.promises.unlink" => {
            let path = javascript_sync_rpc_arg_str(&request.args, 0, "filesystem unlink path")?;
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

fn javascript_sync_rpc_readdir_value(entries: Vec<String>) -> Value {
    json!(entries
        .into_iter()
        .filter(|entry| entry != "." && entry != "..")
        .collect::<Vec<_>>())
}
