use crate::bridge::MountPluginContext;
use crate::protocol::{
    JsBridgeCallRequest, JsBridgeResultResponse, OwnershipScope, SidecarRequestPayload,
    SidecarResponsePayload,
};
use crate::SidecarError;

use agent_os_kernel::mount_plugin::{
    FileSystemPluginFactory, OpenFileSystemPluginRequest, PluginError,
};
use agent_os_kernel::mount_table::{MountedFileSystem, MountedVirtualFileSystem};
use agent_os_kernel::vfs::{VfsError, VfsResult, VirtualDirEntry, VirtualFileSystem, VirtualStat};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

const JS_BRIDGE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsBridgeMountConfig {
    #[serde(default)]
    mount_id: Option<String>,
}

#[derive(Debug)]
pub(crate) struct JsBridgeMountPlugin;

impl<B> FileSystemPluginFactory<MountPluginContext<B>> for JsBridgeMountPlugin {
    fn plugin_id(&self) -> &'static str {
        "js_bridge"
    }

    fn open(
        &self,
        request: OpenFileSystemPluginRequest<'_, MountPluginContext<B>>,
    ) -> Result<Box<dyn MountedFileSystem>, PluginError> {
        let config: JsBridgeMountConfig = match &request.config {
            Value::Null => JsBridgeMountConfig::default(),
            Value::Object(_) => serde_json::from_value(request.config.clone())
                .map_err(|error| PluginError::invalid_input(error.to_string()))?,
            _ => {
                return Err(PluginError::invalid_input(
                    "js_bridge mount config must be an object or null",
                ));
            }
        };
        let mount_id = config
            .mount_id
            .unwrap_or_else(|| request.guest_path.to_owned());
        let ownership = OwnershipScope::vm(
            request.context.connection_id.clone(),
            request.context.session_id.clone(),
            request.context.vm_id.clone(),
        );

        Ok(Box::new(MountedVirtualFileSystem::new(
            JsBridgeFilesystem::new(
                request.context.sidecar_requests.clone(),
                ownership,
                mount_id,
            ),
        )))
    }
}

#[derive(Clone)]
struct JsBridgeFilesystem {
    requests: crate::state::SharedSidecarRequestClient,
    ownership: OwnershipScope,
    mount_id: String,
    next_call_id: Arc<AtomicU64>,
}

impl JsBridgeFilesystem {
    fn new(
        requests: crate::state::SharedSidecarRequestClient,
        ownership: OwnershipScope,
        mount_id: String,
    ) -> Self {
        Self {
            requests,
            ownership,
            mount_id,
            next_call_id: Arc::new(AtomicU64::new(1)),
        }
    }

    fn next_call_id(&self) -> String {
        format!(
            "js-bridge-call-{}",
            self.next_call_id.fetch_add(1, Ordering::Relaxed)
        )
    }

    fn request(&self, operation: &str, args: Value) -> VfsResult<Option<Value>> {
        let payload = SidecarRequestPayload::JsBridgeCall(JsBridgeCallRequest {
            call_id: self.next_call_id(),
            mount_id: self.mount_id.clone(),
            operation: operation.to_owned(),
            args,
        });
        match self
            .requests
            .invoke(self.ownership.clone(), payload, JS_BRIDGE_TIMEOUT)
            .map_err(|error| Self::sidecar_error_to_vfs(operation, "/", error))?
        {
            SidecarResponsePayload::JsBridgeResult(JsBridgeResultResponse { result, error, .. }) => {
                if let Some(error) = error {
                    return Err(Self::js_error_to_vfs(operation, "/", &error));
                }
                Ok(result)
            }
            other => Err(VfsError::io(format!(
                "unexpected js_bridge response payload: {other:?}"
            ))),
        }
    }

    fn request_path(
        &self,
        operation: &str,
        path: &str,
        args: Value,
    ) -> VfsResult<Option<Value>> {
        let payload = SidecarRequestPayload::JsBridgeCall(JsBridgeCallRequest {
            call_id: self.next_call_id(),
            mount_id: self.mount_id.clone(),
            operation: operation.to_owned(),
            args,
        });
        match self
            .requests
            .invoke(self.ownership.clone(), payload, JS_BRIDGE_TIMEOUT)
            .map_err(|error| Self::sidecar_error_to_vfs(operation, path, error))?
        {
            SidecarResponsePayload::JsBridgeResult(JsBridgeResultResponse { result, error, .. }) => {
                if let Some(error) = error {
                    return Err(Self::js_error_to_vfs(operation, path, &error));
                }
                Ok(result)
            }
            other => Err(VfsError::io(format!(
                "unexpected js_bridge response payload: {other:?}"
            ))),
        }
    }

    fn sidecar_error_to_vfs(operation: &str, path: &str, error: SidecarError) -> VfsError {
        match error {
            SidecarError::Io(message) if message.contains("timed out") => {
                VfsError::io(format!("{operation} {path}: {message}"))
            }
            other => VfsError::io(format!("{operation} {path}: {other}")),
        }
    }

    fn js_error_to_vfs(operation: &str, path: &str, error: &str) -> VfsError {
        let lower = error.to_ascii_lowercase();
        let code = if lower.contains("enoent")
            || lower.contains("not found")
            || lower.contains("no such file")
        {
            "ENOENT"
        } else if lower.contains("eacces")
            || lower.contains("eperm")
            || lower.contains("permission denied")
        {
            "EACCES"
        } else if lower.contains("eexist") || lower.contains("already exists") {
            "EEXIST"
        } else {
            "EIO"
        };
        VfsError::new(code, format!("{error}, {operation} '{path}'"))
    }

    fn parse_required<T>(&self, operation: &str, path: &str, result: Option<Value>) -> VfsResult<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let value = result.ok_or_else(|| {
            VfsError::io(format!(
                "js_bridge returned no payload for {operation} '{path}'"
            ))
        })?;
        serde_json::from_value(value).map_err(|error| {
            VfsError::io(format!(
                "invalid js_bridge payload for {operation} '{path}': {error}"
            ))
        })
    }

    fn parse_bytes(&self, operation: &str, path: &str, result: Option<Value>) -> VfsResult<Vec<u8>> {
        match result.ok_or_else(|| {
            VfsError::io(format!(
                "js_bridge returned no payload for {operation} '{path}'"
            ))
        })? {
            Value::String(encoded) => BASE64_STANDARD.decode(encoded).map_err(|error| {
                VfsError::io(format!(
                    "invalid js_bridge base64 payload for {operation} '{path}': {error}"
                ))
            }),
            Value::Array(values) => values
                .into_iter()
                .map(|value| match value {
                    Value::Number(number) => number
                        .as_u64()
                        .and_then(|value| u8::try_from(value).ok())
                        .ok_or_else(|| {
                            VfsError::io(format!(
                                "invalid js_bridge byte payload for {operation} '{path}'"
                            ))
                        }),
                    _ => Err(VfsError::io(format!(
                        "invalid js_bridge byte payload for {operation} '{path}'"
                    ))),
                })
                .collect(),
            other => Err(VfsError::io(format!(
                "unsupported js_bridge payload for {operation} '{path}': {other:?}"
            ))),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsBridgeVirtualStat {
    mode: u32,
    size: u64,
    blocks: u64,
    dev: u64,
    rdev: u64,
    #[serde(alias = "is_directory")]
    is_directory: bool,
    #[serde(alias = "is_symbolic_link")]
    is_symbolic_link: bool,
    #[serde(alias = "atime_ms")]
    atime_ms: u64,
    #[serde(alias = "mtime_ms")]
    mtime_ms: u64,
    #[serde(alias = "ctime_ms")]
    ctime_ms: u64,
    #[serde(alias = "birthtime_ms")]
    birthtime_ms: u64,
    ino: u64,
    nlink: u64,
    uid: u32,
    gid: u32,
}

impl From<JsBridgeVirtualStat> for VirtualStat {
    fn from(stat: JsBridgeVirtualStat) -> Self {
        Self {
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
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsBridgeDirEntry {
    name: String,
    #[serde(alias = "is_directory")]
    is_directory: bool,
    #[serde(alias = "is_symbolic_link")]
    is_symbolic_link: bool,
}

impl From<JsBridgeDirEntry> for VirtualDirEntry {
    fn from(entry: JsBridgeDirEntry) -> Self {
        Self {
            name: entry.name,
            is_directory: entry.is_directory,
            is_symbolic_link: entry.is_symbolic_link,
        }
    }
}

impl VirtualFileSystem for JsBridgeFilesystem {
    fn read_file(&mut self, path: &str) -> VfsResult<Vec<u8>> {
        let result = self.request_path("readFile", path, json!({ "path": path }))?;
        self.parse_bytes("readFile", path, result)
    }

    fn read_dir(&mut self, path: &str) -> VfsResult<Vec<String>> {
        self.parse_required("readDir", path, self.request_path("readDir", path, json!({ "path": path }))?)
    }

    fn read_dir_with_types(&mut self, path: &str) -> VfsResult<Vec<VirtualDirEntry>> {
        let entries: Vec<JsBridgeDirEntry> = self.parse_required(
            "readDirWithTypes",
            path,
            self.request_path("readDirWithTypes", path, json!({ "path": path }))?,
        )?;
        Ok(entries.into_iter().map(Into::into).collect())
    }

    fn write_file(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<()> {
        let content = BASE64_STANDARD.encode(content.into());
        self.request_path(
            "writeFile",
            path,
            json!({
                "path": path,
                "content": content,
            }),
        )?;
        Ok(())
    }

    fn create_dir(&mut self, path: &str) -> VfsResult<()> {
        self.request_path("createDir", path, json!({ "path": path }))?;
        Ok(())
    }

    fn mkdir(&mut self, path: &str, recursive: bool) -> VfsResult<()> {
        self.request_path(
            "mkdir",
            path,
            json!({
                "path": path,
                "recursive": recursive,
            }),
        )?;
        Ok(())
    }

    fn exists(&self, path: &str) -> bool {
        self.requests
            .invoke(
                self.ownership.clone(),
                SidecarRequestPayload::JsBridgeCall(JsBridgeCallRequest {
                    call_id: self.next_call_id(),
                    mount_id: self.mount_id.clone(),
                    operation: String::from("exists"),
                    args: json!({ "path": path }),
                }),
                JS_BRIDGE_TIMEOUT,
            )
            .ok()
            .and_then(|payload| match payload {
                SidecarResponsePayload::JsBridgeResult(JsBridgeResultResponse { result, error, .. })
                    if error.is_none() =>
                {
                    result
                }
                _ => None,
            })
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
    }

    fn stat(&mut self, path: &str) -> VfsResult<VirtualStat> {
        let stat: JsBridgeVirtualStat =
            self.parse_required("stat", path, self.request_path("stat", path, json!({ "path": path }))?)?;
        Ok(stat.into())
    }

    fn remove_file(&mut self, path: &str) -> VfsResult<()> {
        self.request_path("removeFile", path, json!({ "path": path }))?;
        Ok(())
    }

    fn remove_dir(&mut self, path: &str) -> VfsResult<()> {
        self.request_path("removeDir", path, json!({ "path": path }))?;
        Ok(())
    }

    fn rename(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        self.request_path(
            "rename",
            old_path,
            json!({
                "oldPath": old_path,
                "newPath": new_path,
            }),
        )?;
        Ok(())
    }

    fn realpath(&self, path: &str) -> VfsResult<String> {
        self.parse_required(
            "realpath",
            path,
            self.request_path("realpath", path, json!({ "path": path }))?,
        )
    }

    fn symlink(&mut self, target: &str, link_path: &str) -> VfsResult<()> {
        self.request_path(
            "symlink",
            link_path,
            json!({
                "target": target,
                "linkPath": link_path,
            }),
        )?;
        Ok(())
    }

    fn read_link(&self, path: &str) -> VfsResult<String> {
        self.parse_required(
            "readlink",
            path,
            self.request_path("readlink", path, json!({ "path": path }))?,
        )
    }

    fn lstat(&self, path: &str) -> VfsResult<VirtualStat> {
        let stat: JsBridgeVirtualStat = self.parse_required(
            "lstat",
            path,
            self.request_path("lstat", path, json!({ "path": path }))?,
        )?;
        Ok(stat.into())
    }

    fn link(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        self.request_path(
            "link",
            old_path,
            json!({
                "oldPath": old_path,
                "newPath": new_path,
            }),
        )?;
        Ok(())
    }

    fn chmod(&mut self, path: &str, mode: u32) -> VfsResult<()> {
        self.request_path(
            "chmod",
            path,
            json!({
                "path": path,
                "mode": mode,
            }),
        )?;
        Ok(())
    }

    fn chown(&mut self, path: &str, uid: u32, gid: u32) -> VfsResult<()> {
        self.request_path(
            "chown",
            path,
            json!({
                "path": path,
                "uid": uid,
                "gid": gid,
            }),
        )?;
        Ok(())
    }

    fn utimes(&mut self, path: &str, atime_ms: u64, mtime_ms: u64) -> VfsResult<()> {
        self.request_path(
            "utimes",
            path,
            json!({
                "path": path,
                "atimeMs": atime_ms,
                "mtimeMs": mtime_ms,
            }),
        )?;
        Ok(())
    }

    fn truncate(&mut self, path: &str, length: u64) -> VfsResult<()> {
        self.request_path(
            "truncate",
            path,
            json!({
                "path": path,
                "length": length,
            }),
        )?;
        Ok(())
    }

    fn pread(&mut self, path: &str, offset: u64, length: usize) -> VfsResult<Vec<u8>> {
        let result = self.request_path(
            "pread",
            path,
            json!({
                "path": path,
                "offset": offset,
                "length": length,
            }),
        )?;
        self.parse_bytes("pread", path, result)
    }

    fn pwrite(&mut self, path: &str, content: impl Into<Vec<u8>>, offset: u64) -> VfsResult<()> {
        let content = BASE64_STANDARD.encode(content.into());
        self.request_path(
            "pwrite",
            path,
            json!({
                "path": path,
                "offset": offset,
                "content": content,
            }),
        )?;
        Ok(())
    }
}
