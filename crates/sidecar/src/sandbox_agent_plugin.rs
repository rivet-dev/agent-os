use agent_os_kernel::mount_plugin::{
    FileSystemPluginFactory, OpenFileSystemPluginRequest, PluginError,
};
use agent_os_kernel::mount_table::{MountedFileSystem, MountedVirtualFileSystem};
use agent_os_kernel::vfs::{
    normalize_path, VfsError, VfsResult, VirtualDirEntry, VirtualFileSystem, VirtualStat, S_IFDIR,
    S_IFREG,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Read;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_MAX_FULL_READ_BYTES: u64 = 256 * 1024;
const DEFAULT_PROCESS_TIMEOUT_MS: u64 = 10_000;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SandboxAgentMountConfig {
    base_url: String,
    token: Option<String>,
    headers: Option<BTreeMap<String, String>>,
    base_path: Option<String>,
    timeout_ms: Option<u64>,
    max_full_read_bytes: Option<u64>,
}

#[derive(Debug)]
pub(crate) struct SandboxAgentMountPlugin;

impl<Context> FileSystemPluginFactory<Context> for SandboxAgentMountPlugin {
    fn plugin_id(&self) -> &'static str {
        "sandbox_agent"
    }

    fn open(
        &self,
        request: OpenFileSystemPluginRequest<'_, Context>,
    ) -> Result<Box<dyn MountedFileSystem>, PluginError> {
        let config: SandboxAgentMountConfig = serde_json::from_value(request.config.clone())
            .map_err(|error| PluginError::invalid_input(error.to_string()))?;
        let filesystem = SandboxAgentFilesystem::from_config(config)?;
        Ok(Box::new(MountedVirtualFileSystem::new(filesystem)))
    }
}

struct SandboxAgentFilesystem {
    client: SandboxAgentFilesystemClient,
    base_path: String,
    max_full_read_bytes: u64,
    process_runtime: Mutex<Option<RemoteProcessRuntime>>,
}

impl SandboxAgentFilesystem {
    fn from_config(config: SandboxAgentMountConfig) -> Result<Self, PluginError> {
        let base_url = config.base_url.trim().trim_end_matches('/').to_owned();
        if base_url.is_empty() {
            return Err(PluginError::invalid_input(
                "sandbox_agent mount requires a non-empty baseUrl",
            ));
        }

        let timeout_ms = config.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
        let timeout = Duration::from_millis(timeout_ms);
        let base_path = match config.base_path.as_deref() {
            None | Some("") | Some("/") => String::from("/"),
            Some(path) if path.starts_with('/') => normalize_path(path),
            Some(path) => path.trim_end_matches('/').to_owned(),
        };

        Ok(Self {
            client: SandboxAgentFilesystemClient::new(
                base_url,
                config.token,
                config.headers.unwrap_or_default(),
                timeout,
            ),
            base_path,
            max_full_read_bytes: config
                .max_full_read_bytes
                .unwrap_or(DEFAULT_MAX_FULL_READ_BYTES),
            process_runtime: Mutex::new(None),
        })
    }

    fn scoped_path(&self, path: &str) -> String {
        let normalized = normalize_path(path);
        if self.base_path == "/" {
            return normalized;
        }

        let suffix = normalized.trim_start_matches('/');
        if self.base_path.starts_with('/') {
            return normalize_path(&format!(
                "{}/{}",
                self.base_path.trim_end_matches('/'),
                suffix
            ));
        }

        if suffix.is_empty() {
            self.base_path.clone()
        } else {
            format!("{}/{}", self.base_path.trim_end_matches('/'), suffix)
        }
    }

    fn stat_from_remote(stat: &SandboxAgentFsStat) -> VirtualStat {
        let modified_ms = now_ms();
        let is_directory = stat.entry_type == "directory";

        VirtualStat {
            mode: if is_directory {
                S_IFDIR | 0o755
            } else {
                S_IFREG | 0o644
            },
            size: stat.size,
            blocks: if stat.size == 0 {
                0
            } else {
                stat.size.div_ceil(512)
            },
            dev: 1,
            rdev: 0,
            is_directory,
            is_symbolic_link: false,
            atime_ms: modified_ms,
            mtime_ms: modified_ms,
            ctime_ms: modified_ms,
            birthtime_ms: modified_ms,
            ino: 0,
            nlink: 1,
            uid: 0,
            gid: 0,
        }
    }

    fn ensure_buffered_full_read_allowed(
        &self,
        path: &str,
        stat: &SandboxAgentFsStat,
        target_size: u64,
        op: &'static str,
    ) -> VfsResult<()> {
        if stat.entry_type == "directory" {
            return Err(VfsError::new(
                "EISDIR",
                format!("illegal operation on a directory, {op} '{path}'"),
            ));
        }

        let required_size = stat.size.max(target_size);
        if required_size <= self.max_full_read_bytes {
            return Ok(());
        }

        Err(VfsError::unsupported(format!(
            "sandbox_agent {op} '{path}' requires ranged reads for files larger than {} bytes; current sandbox-agent servers only expose full-file reads",
            self.max_full_read_bytes
        )))
    }

    fn scoped_target(&self, target: &str) -> String {
        if target.starts_with('/') {
            self.scoped_path(target)
        } else {
            target.to_owned()
        }
    }

    fn strip_base_path_prefix<'a>(&self, target: &'a str) -> Option<&'a str> {
        if self.base_path == "/" || !target.starts_with('/') {
            return None;
        }
        if target == self.base_path {
            Some("")
        } else {
            target
                .strip_prefix(self.base_path.as_str())
                .filter(|stripped| stripped.starts_with('/'))
        }
    }

    fn unscoped_target(&self, target: String) -> String {
        if !target.starts_with('/') {
            return target;
        }
        match self.strip_base_path_prefix(&target) {
            Some(stripped) => format!("/{}", stripped.trim_start_matches('/')),
            None => target,
        }
    }

    fn process_runtimes(&self) -> Vec<RemoteProcessRuntime> {
        let cached = *self
            .process_runtime
            .lock()
            .expect("lock sandbox_agent process runtime cache");
        let mut runtimes = Vec::with_capacity(3);
        if let Some(runtime) = cached {
            runtimes.push(runtime);
        }
        for runtime in [
            RemoteProcessRuntime::Python3,
            RemoteProcessRuntime::Python,
            RemoteProcessRuntime::Node,
        ] {
            if Some(runtime) != cached {
                runtimes.push(runtime);
            }
        }
        runtimes
    }

    fn remember_process_runtime(&self, runtime: RemoteProcessRuntime) {
        *self
            .process_runtime
            .lock()
            .expect("lock sandbox_agent process runtime cache") = Some(runtime);
    }

    fn run_fs_script(
        &self,
        op: &'static str,
        path: &str,
        python_script: &'static str,
        node_script: &'static str,
        args: &[String],
    ) -> VfsResult<Option<String>> {
        let mut saw_runtime_candidate = false;

        for runtime in self.process_runtimes() {
            saw_runtime_candidate = true;
            match self.run_fs_script_with_runtime(
                runtime,
                op,
                path,
                python_script,
                node_script,
                args,
            ) {
                Ok(result) => {
                    self.remember_process_runtime(runtime);
                    return Ok(result);
                }
                Err(ProcessFallbackError::RuntimeUnavailable) => continue,
                Err(ProcessFallbackError::Unsupported(message)) => {
                    return Err(VfsError::unsupported(format!(
                        "sandbox_agent {op} '{path}' requires remote process execution but the sandbox-agent server does not support the process API: {message}"
                    )));
                }
                Err(ProcessFallbackError::Operation(error)) => return Err(error),
            }
        }

        debug_assert!(saw_runtime_candidate);
        Err(VfsError::unsupported(format!(
            "sandbox_agent {op} '{path}' requires a remote `python3`, `python`, or `node` runtime via the sandbox-agent process API, but none were available"
        )))
    }

    fn run_fs_script_with_runtime(
        &self,
        runtime: RemoteProcessRuntime,
        op: &'static str,
        path: &str,
        python_script: &'static str,
        node_script: &'static str,
        args: &[String],
    ) -> Result<Option<String>, ProcessFallbackError> {
        let request = runtime.process_request(args, python_script, node_script);
        let response = self
            .client
            .run_process(&request)
            .map_err(|error| match error {
                SandboxAgentClientError::Status { status, problem }
                    if matches!(status, 404 | 405 | 501) =>
                {
                    ProcessFallbackError::Unsupported(
                        problem
                            .detail
                            .or(problem.title)
                            .unwrap_or_else(|| String::from("process API unavailable")),
                    )
                }
                other => {
                    ProcessFallbackError::Operation(sandbox_client_error_to_vfs(op, path, other))
                }
            })?;

        if response.timed_out {
            return Err(ProcessFallbackError::Operation(VfsError::io(format!(
                "{op} '{path}': remote process helper timed out after {} ms",
                DEFAULT_PROCESS_TIMEOUT_MS
            ))));
        }

        if response.exit_code.unwrap_or_default() == 0 {
            if response.stdout.is_empty() {
                return Ok(None);
            }
            return parse_process_json_output(&response.stdout, op, path)
                .map(Some)
                .map_err(ProcessFallbackError::Operation);
        }

        if runtime.command_missing(&response) {
            return Err(ProcessFallbackError::RuntimeUnavailable);
        }

        Err(ProcessFallbackError::Operation(process_response_to_vfs(
            op, path, response,
        )))
    }
}

impl VirtualFileSystem for SandboxAgentFilesystem {
    fn read_file(&mut self, path: &str) -> VfsResult<Vec<u8>> {
        let remote_path = self.scoped_path(path);
        self.client
            .read_fs_file(&remote_path)
            .map_err(|error| sandbox_client_error_to_vfs("open", path, error))
    }

    fn read_dir(&mut self, path: &str) -> VfsResult<Vec<String>> {
        let remote_path = self.scoped_path(path);
        let mut entries = self
            .client
            .list_fs_entries(&remote_path)
            .map_err(|error| sandbox_client_error_to_vfs("readdir", path, error))?
            .into_iter()
            .map(|entry| entry.name)
            .filter(|name| name != "." && name != "..")
            .collect::<Vec<_>>();
        entries.sort();
        Ok(entries)
    }

    fn read_dir_with_types(&mut self, path: &str) -> VfsResult<Vec<VirtualDirEntry>> {
        let remote_path = self.scoped_path(path);
        let mut entries = self
            .client
            .list_fs_entries(&remote_path)
            .map_err(|error| sandbox_client_error_to_vfs("readdir", path, error))?
            .into_iter()
            .filter(|entry| entry.name != "." && entry.name != "..")
            .map(|entry| VirtualDirEntry {
                name: entry.name,
                is_directory: entry.entry_type == "directory",
                is_symbolic_link: false,
            })
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(entries)
    }

    fn write_file(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<()> {
        let remote_path = self.scoped_path(path);
        self.client
            .write_fs_file(&remote_path, &content.into())
            .map_err(|error| sandbox_client_error_to_vfs("write", path, error))
    }

    fn create_dir(&mut self, path: &str) -> VfsResult<()> {
        self.mkdir(path, false)
    }

    fn mkdir(&mut self, path: &str, recursive: bool) -> VfsResult<()> {
        if !recursive {
            let parent_path = dirname(path);
            if parent_path != "/" {
                let parent_remote = self.scoped_path(&parent_path);
                let parent = self
                    .client
                    .stat_fs(&parent_remote)
                    .map_err(|error| sandbox_client_error_to_vfs("mkdir", &parent_path, error))?;
                if parent.entry_type != "directory" {
                    return Err(VfsError::new(
                        "ENOTDIR",
                        format!("not a directory, mkdir '{parent_path}'"),
                    ));
                }
            }
        }

        let remote_path = self.scoped_path(path);
        self.client
            .mkdir_fs(&remote_path)
            .map_err(|error| sandbox_client_error_to_vfs("mkdir", path, error))
    }

    fn exists(&self, path: &str) -> bool {
        let remote_path = self.scoped_path(path);
        self.client.stat_fs(&remote_path).is_ok()
    }

    fn stat(&mut self, path: &str) -> VfsResult<VirtualStat> {
        let remote_path = self.scoped_path(path);
        let stat = self
            .client
            .stat_fs(&remote_path)
            .map_err(|error| sandbox_client_error_to_vfs("stat", path, error))?;
        Ok(Self::stat_from_remote(&stat))
    }

    fn remove_file(&mut self, path: &str) -> VfsResult<()> {
        let remote_path = self.scoped_path(path);
        self.client
            .delete_fs_entry(&remote_path, false)
            .map_err(|error| sandbox_client_error_to_vfs("unlink", path, error))
    }

    fn remove_dir(&mut self, path: &str) -> VfsResult<()> {
        let remote_path = self.scoped_path(path);
        let entries = self
            .client
            .list_fs_entries(&remote_path)
            .map_err(|error| sandbox_client_error_to_vfs("rmdir", path, error))?;
        let children = entries
            .into_iter()
            .filter(|entry| entry.name != "." && entry.name != "..")
            .count();
        if children > 0 {
            return Err(VfsError::new(
                "ENOTEMPTY",
                format!("directory not empty, rmdir '{path}'"),
            ));
        }

        self.client
            .delete_fs_entry(&remote_path, false)
            .map_err(|error| sandbox_client_error_to_vfs("rmdir", path, error))
    }

    fn rename(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        let old_remote = self.scoped_path(old_path);
        let new_remote = self.scoped_path(new_path);
        self.client
            .move_fs(&old_remote, &new_remote, true)
            .map_err(|error| sandbox_client_error_to_vfs("rename", old_path, error))
    }

    fn realpath(&self, path: &str) -> VfsResult<String> {
        let remote_path = self.scoped_path(path);
        let resolved = self.run_fs_script(
            "realpath",
            path,
            PYTHON_REALPATH_SCRIPT,
            NODE_REALPATH_SCRIPT,
            &[remote_path],
        )?;
        Ok(self.unscoped_target(resolved.unwrap_or_else(|| normalize_path(path))))
    }

    fn symlink(&mut self, target: &str, link_path: &str) -> VfsResult<()> {
        let remote_target = self.scoped_target(target);
        let remote_link = self.scoped_path(link_path);
        self.run_fs_script(
            "symlink",
            link_path,
            PYTHON_SYMLINK_SCRIPT,
            NODE_SYMLINK_SCRIPT,
            &[remote_target, remote_link],
        )?;
        Ok(())
    }

    fn read_link(&self, path: &str) -> VfsResult<String> {
        let remote_path = self.scoped_path(path);
        let target = self.run_fs_script(
            "readlink",
            path,
            PYTHON_READLINK_SCRIPT,
            NODE_READLINK_SCRIPT,
            &[remote_path],
        )?;
        Ok(match target {
            Some(target) if target.starts_with('/') => self.unscoped_target(target),
            Some(target) => target,
            None => String::new(),
        })
    }

    fn lstat(&self, path: &str) -> VfsResult<VirtualStat> {
        let remote_path = self.scoped_path(path);
        let stat = self
            .client
            .stat_fs(&remote_path)
            .map_err(|error| sandbox_client_error_to_vfs("lstat", path, error))?;
        Ok(Self::stat_from_remote(&stat))
    }

    fn link(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        let old_remote = self.scoped_path(old_path);
        let new_remote = self.scoped_path(new_path);
        self.run_fs_script(
            "link",
            new_path,
            PYTHON_LINK_SCRIPT,
            NODE_LINK_SCRIPT,
            &[old_remote, new_remote],
        )?;
        Ok(())
    }

    fn chmod(&mut self, path: &str, mode: u32) -> VfsResult<()> {
        let remote_path = self.scoped_path(path);
        self.run_fs_script(
            "chmod",
            path,
            PYTHON_CHMOD_SCRIPT,
            NODE_CHMOD_SCRIPT,
            &[remote_path, mode.to_string()],
        )?;
        Ok(())
    }

    fn chown(&mut self, path: &str, uid: u32, gid: u32) -> VfsResult<()> {
        let remote_path = self.scoped_path(path);
        self.run_fs_script(
            "chown",
            path,
            PYTHON_CHOWN_SCRIPT,
            NODE_CHOWN_SCRIPT,
            &[remote_path, uid.to_string(), gid.to_string()],
        )?;
        Ok(())
    }

    fn utimes(&mut self, path: &str, atime_ms: u64, mtime_ms: u64) -> VfsResult<()> {
        let remote_path = self.scoped_path(path);
        self.run_fs_script(
            "utimes",
            path,
            PYTHON_UTIMES_SCRIPT,
            NODE_UTIMES_SCRIPT,
            &[remote_path, atime_ms.to_string(), mtime_ms.to_string()],
        )?;
        Ok(())
    }

    fn truncate(&mut self, path: &str, length: u64) -> VfsResult<()> {
        if length == 0 {
            return self.write_file(path, Vec::<u8>::new());
        }

        let remote_path = self.scoped_path(path);
        self.run_fs_script(
            "truncate",
            path,
            PYTHON_TRUNCATE_SCRIPT,
            NODE_TRUNCATE_SCRIPT,
            &[remote_path, length.to_string()],
        )?;
        Ok(())
    }

    fn pread(&mut self, path: &str, offset: u64, length: usize) -> VfsResult<Vec<u8>> {
        if length == 0 {
            return Ok(Vec::new());
        }

        let remote_path = self.scoped_path(path);
        let stat = self
            .client
            .stat_fs(&remote_path)
            .map_err(|error| sandbox_client_error_to_vfs("open", path, error))?;
        self.ensure_buffered_full_read_allowed(path, &stat, stat.size, "pread")?;

        let content = self
            .client
            .read_fs_file(&remote_path)
            .map_err(|error| sandbox_client_error_to_vfs("open", path, error))?;
        let start = usize::try_from(offset).unwrap_or(usize::MAX);
        if start >= content.len() {
            return Ok(Vec::new());
        }
        let end = start.saturating_add(length).min(content.len());
        Ok(content[start..end].to_vec())
    }
}

struct SandboxAgentFilesystemClient {
    base_url: String,
    token: Option<String>,
    headers: BTreeMap<String, String>,
    agent: ureq::Agent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RemoteProcessRuntime {
    Python3,
    Python,
    Node,
}

impl RemoteProcessRuntime {
    fn command(self) -> &'static str {
        match self {
            Self::Python3 => "python3",
            Self::Python => "python",
            Self::Node => "node",
        }
    }

    fn process_request(
        self,
        args: &[String],
        python_script: &'static str,
        node_script: &'static str,
    ) -> SandboxAgentProcessRunRequest {
        match self {
            Self::Python3 | Self::Python => {
                let mut process_args = vec![String::from("-c"), python_script.to_owned()];
                process_args.extend(args.iter().cloned());
                SandboxAgentProcessRunRequest {
                    command: self.command().to_owned(),
                    args: process_args,
                    cwd: None,
                    env: None,
                    max_output_bytes: None,
                    timeout_ms: Some(DEFAULT_PROCESS_TIMEOUT_MS),
                }
            }
            Self::Node => {
                let mut process_args = vec![String::from("-e"), node_script.to_owned()];
                if !args.is_empty() {
                    process_args.push(String::from("--"));
                    process_args.extend(args.iter().cloned());
                }
                SandboxAgentProcessRunRequest {
                    command: self.command().to_owned(),
                    args: process_args,
                    cwd: None,
                    env: None,
                    max_output_bytes: None,
                    timeout_ms: Some(DEFAULT_PROCESS_TIMEOUT_MS),
                }
            }
        }
    }

    fn command_missing(self, response: &SandboxAgentProcessRunResponse) -> bool {
        if serde_json::from_str::<FsScriptJsonError>(response.stderr.trim()).is_ok() {
            return false;
        }
        let stderr = response.stderr.to_ascii_lowercase();
        response.exit_code == Some(127)
            || stderr.contains("command not found")
            || stderr.contains("executable file not found")
            || stderr.contains("enoent")
    }
}

enum ProcessFallbackError {
    RuntimeUnavailable,
    Unsupported(String),
    Operation(VfsError),
}

impl SandboxAgentFilesystemClient {
    fn new(
        base_url: String,
        token: Option<String>,
        headers: BTreeMap<String, String>,
        timeout: Duration,
    ) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(timeout)
            .timeout_read(timeout)
            .timeout_write(timeout)
            .build();

        Self {
            base_url,
            token,
            headers,
            agent,
        }
    }

    fn list_fs_entries(
        &self,
        path: &str,
    ) -> Result<Vec<SandboxAgentFsEntry>, SandboxAgentClientError> {
        self.request_json(
            "GET",
            "/v1/fs/entries",
            vec![(String::from("path"), path.to_owned())],
            RequestBody::None,
            Some("application/json"),
        )
    }

    fn read_fs_file(&self, path: &str) -> Result<Vec<u8>, SandboxAgentClientError> {
        self.request_bytes(
            "GET",
            "/v1/fs/file",
            vec![(String::from("path"), path.to_owned())],
            Some("application/octet-stream"),
        )
    }

    fn write_fs_file(&self, path: &str, content: &[u8]) -> Result<(), SandboxAgentClientError> {
        self.request_empty(
            "PUT",
            "/v1/fs/file",
            vec![(String::from("path"), path.to_owned())],
            RequestBody::Bytes(content.to_vec()),
            Some("application/json"),
        )
    }

    fn delete_fs_entry(&self, path: &str, recursive: bool) -> Result<(), SandboxAgentClientError> {
        let mut query = vec![(String::from("path"), path.to_owned())];
        if recursive {
            query.push((String::from("recursive"), String::from("true")));
        }

        self.request_empty(
            "DELETE",
            "/v1/fs/entry",
            query,
            RequestBody::None,
            Some("application/json"),
        )
    }

    fn mkdir_fs(&self, path: &str) -> Result<(), SandboxAgentClientError> {
        self.request_empty(
            "POST",
            "/v1/fs/mkdir",
            vec![(String::from("path"), path.to_owned())],
            RequestBody::None,
            Some("application/json"),
        )
    }

    fn move_fs(
        &self,
        from: &str,
        to: &str,
        overwrite: bool,
    ) -> Result<(), SandboxAgentClientError> {
        self.request_empty(
            "POST",
            "/v1/fs/move",
            Vec::new(),
            RequestBody::Json(serde_json::json!({
                "from": from,
                "to": to,
                "overwrite": overwrite,
            })),
            Some("application/json"),
        )
    }

    fn stat_fs(&self, path: &str) -> Result<SandboxAgentFsStat, SandboxAgentClientError> {
        self.request_json(
            "GET",
            "/v1/fs/stat",
            vec![(String::from("path"), path.to_owned())],
            RequestBody::None,
            Some("application/json"),
        )
    }

    fn run_process(
        &self,
        request: &SandboxAgentProcessRunRequest,
    ) -> Result<SandboxAgentProcessRunResponse, SandboxAgentClientError> {
        self.request_json(
            "POST",
            "/v1/processes/run",
            Vec::new(),
            RequestBody::Json(
                serde_json::to_value(request).expect("serialize process run request"),
            ),
            Some("application/json"),
        )
    }

    fn request_json<T: DeserializeOwned>(
        &self,
        method: &str,
        path: &str,
        query: Vec<(String, String)>,
        body: RequestBody,
        accept: Option<&str>,
    ) -> Result<T, SandboxAgentClientError> {
        let response = self.request_raw(method, path, query, body, accept)?;
        response
            .into_json::<T>()
            .map_err(|error| SandboxAgentClientError::Decode(error.to_string()))
    }

    fn request_bytes(
        &self,
        method: &str,
        path: &str,
        query: Vec<(String, String)>,
        accept: Option<&str>,
    ) -> Result<Vec<u8>, SandboxAgentClientError> {
        let response = self.request_raw(method, path, query, RequestBody::None, accept)?;
        let mut reader = response.into_reader();
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .map_err(|error| SandboxAgentClientError::Decode(error.to_string()))?;
        Ok(bytes)
    }

    fn request_empty(
        &self,
        method: &str,
        path: &str,
        query: Vec<(String, String)>,
        body: RequestBody,
        accept: Option<&str>,
    ) -> Result<(), SandboxAgentClientError> {
        self.request_raw(method, path, query, body, accept)?;
        Ok(())
    }

    fn request_raw(
        &self,
        method: &str,
        path: &str,
        query: Vec<(String, String)>,
        body: RequestBody,
        accept: Option<&str>,
    ) -> Result<ureq::Response, SandboxAgentClientError> {
        let mut request = self
            .agent
            .request(method, &format!("{}{}", self.base_url, path));

        if let Some(token) = &self.token {
            request = request.set("Authorization", &format!("Bearer {token}"));
        }

        for (name, value) in &self.headers {
            request = request.set(name, value);
        }

        if let Some(accept) = accept {
            request = request.set("Accept", accept);
        }

        for (name, value) in query {
            request = request.query(&name, &value);
        }

        let response = match body {
            RequestBody::None => request.call(),
            RequestBody::Json(value) => request.send_json(value),
            RequestBody::Bytes(content) => request
                .set("Content-Type", "application/octet-stream")
                .send_bytes(&content),
        };

        match response {
            Ok(response) => Ok(response),
            Err(ureq::Error::Status(status, response)) => Err(SandboxAgentClientError::Status {
                status,
                problem: read_problem_details(response),
            }),
            Err(ureq::Error::Transport(error)) => {
                Err(SandboxAgentClientError::Transport(error.to_string()))
            }
        }
    }
}

enum RequestBody {
    None,
    Json(serde_json::Value),
    Bytes(Vec<u8>),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SandboxAgentFsEntry {
    name: String,
    #[serde(rename = "path")]
    _path: String,
    entry_type: String,
    #[serde(rename = "size")]
    _size: u64,
    #[serde(rename = "modified")]
    _modified: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SandboxAgentFsStat {
    #[serde(rename = "path")]
    _path: String,
    entry_type: String,
    size: u64,
    #[serde(rename = "modified")]
    _modified: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SandboxAgentProcessRunRequest {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    env: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SandboxAgentProcessRunResponse {
    #[serde(rename = "durationMs")]
    _duration_ms: u64,
    exit_code: Option<i32>,
    stderr: String,
    #[serde(rename = "stderrTruncated")]
    _stderr_truncated: bool,
    stdout: String,
    #[serde(rename = "stdoutTruncated")]
    _stdout_truncated: bool,
    timed_out: bool,
}

#[derive(Debug, Deserialize)]
struct FsScriptJsonOutput {
    result: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FsScriptJsonError {
    errno: Option<i32>,
    message: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct SandboxAgentProblemDetails {
    title: Option<String>,
    detail: Option<String>,
    status: Option<u16>,
}

#[derive(Debug)]
enum SandboxAgentClientError {
    Status {
        status: u16,
        problem: SandboxAgentProblemDetails,
    },
    Transport(String),
    Decode(String),
}

fn read_problem_details(response: ureq::Response) -> SandboxAgentProblemDetails {
    match response.into_string() {
        Ok(body) if !body.is_empty() => {
            serde_json::from_str(&body).unwrap_or_else(|_| SandboxAgentProblemDetails {
                detail: Some(body),
                ..SandboxAgentProblemDetails::default()
            })
        }
        _ => SandboxAgentProblemDetails::default(),
    }
}

fn sandbox_client_error_to_vfs(
    op: &'static str,
    path: &str,
    error: SandboxAgentClientError,
) -> VfsError {
    match error {
        SandboxAgentClientError::Status { status, problem } => {
            let status = problem.status.unwrap_or(status);
            let detail = problem
                .detail
                .or(problem.title)
                .unwrap_or_else(|| format!("sandbox-agent request failed with status {status}"));

            let code = if status == 401 || status == 403 {
                "EACCES"
            } else if status == 404 || detail.contains("path not found") {
                "ENOENT"
            } else if detail.contains("path is not a file") {
                "EISDIR"
            } else if detail.contains("destination already exists") {
                "EEXIST"
            } else if status == 409 {
                "EEXIST"
            } else if status == 400 {
                "EINVAL"
            } else {
                "EIO"
            };

            VfsError::new(code, format!("{op} '{path}': {detail}"))
        }
        SandboxAgentClientError::Transport(message) | SandboxAgentClientError::Decode(message) => {
            VfsError::io(format!("{op} '{path}': {message}"))
        }
    }
}

fn parse_process_json_output(stdout: &str, op: &'static str, path: &str) -> VfsResult<String> {
    let trimmed = stdout.trim();
    let output: FsScriptJsonOutput = serde_json::from_str(trimmed).map_err(|error| {
        VfsError::io(format!(
            "{op} '{path}': failed to decode process helper output: {error}"
        ))
    })?;
    Ok(output.result.unwrap_or_default())
}

fn process_response_to_vfs(
    op: &'static str,
    path: &str,
    response: SandboxAgentProcessRunResponse,
) -> VfsError {
    let trimmed_stderr = response.stderr.trim();
    if let Ok(error) = serde_json::from_str::<FsScriptJsonError>(trimmed_stderr) {
        let message = error
            .message
            .unwrap_or_else(|| String::from("remote filesystem helper failed"));
        if let Some(errno) = error.errno {
            return VfsError::new(
                errno_to_vfs_code(errno),
                format!("{op} '{path}': {message}"),
            );
        }
        return VfsError::io(format!("{op} '{path}': {message}"));
    }

    let detail = if trimmed_stderr.is_empty() {
        format!(
            "remote process exited with code {}",
            response
                .exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| String::from("unknown"))
        )
    } else {
        trimmed_stderr.to_owned()
    };
    VfsError::io(format!("{op} '{path}': {detail}"))
}

fn errno_to_vfs_code(errno: i32) -> &'static str {
    match errno {
        nix::libc::EACCES => "EACCES",
        nix::libc::EEXIST => "EEXIST",
        nix::libc::EINVAL => "EINVAL",
        nix::libc::EISDIR => "EISDIR",
        nix::libc::ELOOP => "ELOOP",
        nix::libc::ENOENT => "ENOENT",
        nix::libc::ENOSYS => "ENOSYS",
        nix::libc::ENOTDIR => "ENOTDIR",
        nix::libc::ENOTEMPTY => "ENOTEMPTY",
        nix::libc::EPERM => "EPERM",
        nix::libc::EXDEV => "EXDEV",
        _ => "EIO",
    }
}

fn dirname(path: &str) -> String {
    let normalized = normalize_path(path);
    match normalized.rsplit_once('/') {
        Some((head, _)) if !head.is_empty() => head.to_owned(),
        _ => String::from("/"),
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

const PYTHON_REALPATH_SCRIPT: &str = r#"import json, os, sys
path = sys.argv[1]
try:
    resolved = os.path.realpath(path)
    os.stat(resolved)
    print(json.dumps({"result": resolved}))
except Exception as exc:
    payload = {"message": str(exc)}
    if isinstance(exc, OSError):
        payload["errno"] = exc.errno
    print(json.dumps(payload), file=sys.stderr)
    sys.exit(1)
"#;

const NODE_REALPATH_SCRIPT: &str = r#"const fs = require("node:fs/promises");
(async () => {
  try {
    const resolved = await fs.realpath(process.argv[1]);
    console.log(JSON.stringify({ result: resolved }));
  } catch (error) {
    console.error(JSON.stringify({ errno: typeof error?.errno === "number" ? Math.abs(error.errno) : undefined, message: error?.message ?? String(error) }));
    process.exit(1);
  }
})();"#;

const PYTHON_SYMLINK_SCRIPT: &str = r#"import json, os, sys
target, link_path = sys.argv[1], sys.argv[2]
try:
    os.symlink(target, link_path)
    print(json.dumps({"result": None}))
except Exception as exc:
    payload = {"message": str(exc)}
    if isinstance(exc, OSError):
        payload["errno"] = exc.errno
    print(json.dumps(payload), file=sys.stderr)
    sys.exit(1)
"#;

const NODE_SYMLINK_SCRIPT: &str = r#"const fs = require("node:fs/promises");
(async () => {
  try {
    await fs.symlink(process.argv[1], process.argv[2]);
    console.log(JSON.stringify({ result: null }));
  } catch (error) {
    console.error(JSON.stringify({ errno: typeof error?.errno === "number" ? Math.abs(error.errno) : undefined, message: error?.message ?? String(error) }));
    process.exit(1);
  }
})();"#;

const PYTHON_READLINK_SCRIPT: &str = r#"import json, os, sys
path = sys.argv[1]
try:
    print(json.dumps({"result": os.readlink(path)}))
except Exception as exc:
    payload = {"message": str(exc)}
    if isinstance(exc, OSError):
        payload["errno"] = exc.errno
    print(json.dumps(payload), file=sys.stderr)
    sys.exit(1)
"#;

const NODE_READLINK_SCRIPT: &str = r#"const fs = require("node:fs/promises");
(async () => {
  try {
    const target = await fs.readlink(process.argv[1]);
    console.log(JSON.stringify({ result: target }));
  } catch (error) {
    console.error(JSON.stringify({ errno: typeof error?.errno === "number" ? Math.abs(error.errno) : undefined, message: error?.message ?? String(error) }));
    process.exit(1);
  }
})();"#;

const PYTHON_LINK_SCRIPT: &str = r#"import json, os, sys
source, destination = sys.argv[1], sys.argv[2]
try:
    os.link(source, destination)
    print(json.dumps({"result": None}))
except Exception as exc:
    payload = {"message": str(exc)}
    if isinstance(exc, OSError):
        payload["errno"] = exc.errno
    print(json.dumps(payload), file=sys.stderr)
    sys.exit(1)
"#;

const NODE_LINK_SCRIPT: &str = r#"const fs = require("node:fs/promises");
(async () => {
  try {
    await fs.link(process.argv[1], process.argv[2]);
    console.log(JSON.stringify({ result: null }));
  } catch (error) {
    console.error(JSON.stringify({ errno: typeof error?.errno === "number" ? Math.abs(error.errno) : undefined, message: error?.message ?? String(error) }));
    process.exit(1);
  }
})();"#;

const PYTHON_CHMOD_SCRIPT: &str = r#"import json, os, sys
path, mode = sys.argv[1], int(sys.argv[2])
try:
    os.chmod(path, mode)
    print(json.dumps({"result": None}))
except Exception as exc:
    payload = {"message": str(exc)}
    if isinstance(exc, OSError):
        payload["errno"] = exc.errno
    print(json.dumps(payload), file=sys.stderr)
    sys.exit(1)
"#;

const NODE_CHMOD_SCRIPT: &str = r#"const fs = require("node:fs/promises");
(async () => {
  try {
    await fs.chmod(process.argv[1], Number(process.argv[2]));
    console.log(JSON.stringify({ result: null }));
  } catch (error) {
    console.error(JSON.stringify({ errno: typeof error?.errno === "number" ? Math.abs(error.errno) : undefined, message: error?.message ?? String(error) }));
    process.exit(1);
  }
})();"#;

const PYTHON_CHOWN_SCRIPT: &str = r#"import json, os, sys
path, uid, gid = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
try:
    os.chown(path, uid, gid)
    print(json.dumps({"result": None}))
except Exception as exc:
    payload = {"message": str(exc)}
    if isinstance(exc, OSError):
        payload["errno"] = exc.errno
    print(json.dumps(payload), file=sys.stderr)
    sys.exit(1)
"#;

const NODE_CHOWN_SCRIPT: &str = r#"const fs = require("node:fs/promises");
(async () => {
  try {
    await fs.chown(process.argv[1], Number(process.argv[2]), Number(process.argv[3]));
    console.log(JSON.stringify({ result: null }));
  } catch (error) {
    console.error(JSON.stringify({ errno: typeof error?.errno === "number" ? Math.abs(error.errno) : undefined, message: error?.message ?? String(error) }));
    process.exit(1);
  }
})();"#;

const PYTHON_UTIMES_SCRIPT: &str = r#"import json, os, sys
path, atime_ms, mtime_ms = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
try:
    os.utime(path, ns=(atime_ms * 1_000_000, mtime_ms * 1_000_000))
    print(json.dumps({"result": None}))
except Exception as exc:
    payload = {"message": str(exc)}
    if isinstance(exc, OSError):
        payload["errno"] = exc.errno
    print(json.dumps(payload), file=sys.stderr)
    sys.exit(1)
"#;

const NODE_UTIMES_SCRIPT: &str = r#"const fs = require("node:fs/promises");
(async () => {
  try {
    await fs.utimes(process.argv[1], Number(process.argv[2]) / 1000, Number(process.argv[3]) / 1000);
    console.log(JSON.stringify({ result: null }));
  } catch (error) {
    console.error(JSON.stringify({ errno: typeof error?.errno === "number" ? Math.abs(error.errno) : undefined, message: error?.message ?? String(error) }));
    process.exit(1);
  }
})();"#;

const PYTHON_TRUNCATE_SCRIPT: &str = r#"import json, os, sys
path, length = sys.argv[1], int(sys.argv[2])
try:
    os.truncate(path, length)
    print(json.dumps({"result": None}))
except Exception as exc:
    payload = {"message": str(exc)}
    if isinstance(exc, OSError):
        payload["errno"] = exc.errno
    print(json.dumps(payload), file=sys.stderr)
    sys.exit(1)
"#;

const NODE_TRUNCATE_SCRIPT: &str = r#"const fs = require("node:fs/promises");
(async () => {
  try {
    await fs.truncate(process.argv[1], Number(process.argv[2]));
    console.log(JSON.stringify({ result: null }));
  } catch (error) {
    console.error(JSON.stringify({ errno: typeof error?.errno === "number" ? Math.abs(error.errno) : undefined, message: error?.message ?? String(error) }));
    process.exit(1);
  }
})();"#;

#[cfg(test)]
pub(crate) mod test_support {
    use serde::{Deserialize, Serialize};
    use std::collections::BTreeMap;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    #[derive(Debug, Clone)]
    pub(crate) struct LoggedRequest {
        pub method: String,
        pub path: String,
        pub query: BTreeMap<String, String>,
        pub headers: BTreeMap<String, String>,
    }

    pub(crate) struct MockSandboxAgentServer {
        base_url: String,
        root: PathBuf,
        shutdown: Arc<AtomicBool>,
        requests: Arc<Mutex<Vec<LoggedRequest>>>,
        handle: Option<JoinHandle<()>>,
    }

    impl MockSandboxAgentServer {
        pub(crate) fn start(prefix: &str, token: Option<&str>) -> Self {
            Self::start_with_process_api(prefix, token, true)
        }

        pub(crate) fn start_without_process_api(prefix: &str, token: Option<&str>) -> Self {
            Self::start_with_process_api(prefix, token, false)
        }

        fn start_with_process_api(
            prefix: &str,
            token: Option<&str>,
            process_api_supported: bool,
        ) -> Self {
            let root = temp_dir(prefix);
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock sandbox-agent");
            listener
                .set_nonblocking(true)
                .expect("configure mock sandbox-agent listener");
            let address = listener
                .local_addr()
                .expect("resolve mock sandbox-agent address");
            let shutdown = Arc::new(AtomicBool::new(false));
            let requests = Arc::new(Mutex::new(Vec::new()));
            let token = token.map(str::to_owned);
            let root_for_thread = root.clone();
            let shutdown_for_thread = Arc::clone(&shutdown);
            let requests_for_thread = Arc::clone(&requests);

            let handle = thread::spawn(move || {
                while !shutdown_for_thread.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            handle_stream(
                                stream,
                                &root_for_thread,
                                token.as_deref(),
                                process_api_supported,
                                &requests_for_thread,
                            );
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(10));
                        }
                        Err(_) => break,
                    }
                }
            });

            Self {
                base_url: format!("http://{}", address),
                root,
                shutdown,
                requests,
                handle: Some(handle),
            }
        }

        pub(crate) fn base_url(&self) -> &str {
            &self.base_url
        }

        pub(crate) fn root(&self) -> &Path {
            &self.root
        }

        pub(crate) fn requests(&self) -> Vec<LoggedRequest> {
            self.requests
                .lock()
                .expect("lock mock sandbox-agent request log")
                .clone()
        }
    }

    impl Drop for MockSandboxAgentServer {
        fn drop(&mut self) {
            self.shutdown.store(true, Ordering::SeqCst);
            if let Some(handle) = self.handle.take() {
                handle.join().expect("join mock sandbox-agent thread");
            }
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[derive(Debug, Deserialize)]
    struct MoveRequest {
        from: String,
        to: String,
        overwrite: Option<bool>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct ProcessRunRequestBody {
        command: String,
        args: Option<Vec<String>>,
        cwd: Option<String>,
        env: Option<BTreeMap<String, String>>,
        #[serde(rename = "maxOutputBytes")]
        _max_output_bytes: Option<u64>,
        #[serde(rename = "timeoutMs")]
        _timeout_ms: Option<u64>,
    }

    #[derive(Debug, Serialize)]
    #[serde(rename_all = "camelCase")]
    struct ProcessRunResponseBody {
        duration_ms: u64,
        exit_code: Option<i32>,
        stderr: String,
        stderr_truncated: bool,
        stdout: String,
        stdout_truncated: bool,
        timed_out: bool,
    }

    #[derive(Debug, Serialize)]
    #[serde(rename_all = "camelCase")]
    struct FsEntryBody {
        name: String,
        path: String,
        entry_type: &'static str,
        size: u64,
        modified: Option<String>,
    }

    #[derive(Debug, Serialize)]
    #[serde(rename_all = "camelCase")]
    struct FsStatBody {
        path: String,
        entry_type: &'static str,
        size: u64,
        modified: Option<String>,
    }

    fn handle_stream(
        mut stream: TcpStream,
        root: &Path,
        token: Option<&str>,
        process_api_supported: bool,
        requests: &Arc<Mutex<Vec<LoggedRequest>>>,
    ) {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("set mock sandbox-agent read timeout");

        let mut buffer = Vec::new();
        let mut header_end = None;
        while header_end.is_none() {
            let mut chunk = [0; 1024];
            match stream.read(&mut chunk) {
                Ok(0) => return,
                Ok(read) => {
                    buffer.extend_from_slice(&chunk[..read]);
                    header_end = find_header_end(&buffer);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
                Err(_) => return,
            }
        }

        let header_end = header_end.expect("parse mock sandbox-agent headers");
        let header_text = String::from_utf8_lossy(&buffer[..header_end]);
        let mut lines = header_text.split("\r\n");
        let request_line = match lines.next() {
            Some(line) if !line.is_empty() => line,
            _ => return,
        };
        let mut request_line_parts = request_line.split_whitespace();
        let method = request_line_parts.next().unwrap_or_default().to_owned();
        let target = request_line_parts.next().unwrap_or_default().to_owned();
        let (path, query) = split_target(&target);

        let mut headers = BTreeMap::new();
        for line in lines {
            if line.is_empty() {
                continue;
            }
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }

        let content_length = headers
            .get("content-length")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        while buffer.len() < header_end + 4 + content_length {
            let mut chunk = [0; 1024];
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => buffer.extend_from_slice(&chunk[..read]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
                Err(_) => break,
            }
        }
        let body = &buffer[header_end + 4..header_end + 4 + content_length];

        requests
            .lock()
            .expect("record mock sandbox-agent request")
            .push(LoggedRequest {
                method: method.clone(),
                path: path.clone(),
                query: query.clone(),
                headers: headers.clone(),
            });

        if let Some(expected_token) = token {
            let authorization = headers
                .get("authorization")
                .map(String::as_str)
                .unwrap_or_default();
            if authorization != format!("Bearer {expected_token}") {
                send_problem(&mut stream, 401, "Unauthorized", "authentication required");
                return;
            }
        }

        match (method.as_str(), path.as_str()) {
            ("GET", "/v1/fs/entries") => {
                let path = query
                    .get("path")
                    .cloned()
                    .unwrap_or_else(|| String::from("."));
                let target = resolve_fs_path(root, &path);
                match fs::read_dir(&target) {
                    Ok(entries) => {
                        let mut payload = entries
                            .filter_map(Result::ok)
                            .map(|entry| {
                                let metadata = entry.metadata().expect("read mock entry metadata");
                                FsEntryBody {
                                    name: entry.file_name().to_string_lossy().into_owned(),
                                    path: entry.path().to_string_lossy().into_owned(),
                                    entry_type: if metadata.is_dir() {
                                        "directory"
                                    } else {
                                        "file"
                                    },
                                    size: metadata.len(),
                                    modified: None,
                                }
                            })
                            .collect::<Vec<_>>();
                        payload.sort_by(|left, right| left.name.cmp(&right.name));
                        send_json(&mut stream, 200, &payload);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        send_problem(
                            &mut stream,
                            400,
                            "Bad Request",
                            &format!("path not found: {}", target.display()),
                        );
                    }
                    Err(error) => send_problem(
                        &mut stream,
                        500,
                        "Internal Server Error",
                        &error.to_string(),
                    ),
                }
            }
            ("GET", "/v1/fs/file") => {
                let path = query.get("path").cloned().unwrap_or_default();
                let target = resolve_fs_path(root, &path);
                match fs::metadata(&target) {
                    Ok(metadata) if metadata.is_file() => match fs::read(&target) {
                        Ok(bytes) => {
                            send_bytes(&mut stream, 200, "application/octet-stream", &bytes)
                        }
                        Err(error) => send_problem(
                            &mut stream,
                            500,
                            "Internal Server Error",
                            &error.to_string(),
                        ),
                    },
                    Ok(_) => send_problem(
                        &mut stream,
                        400,
                        "Bad Request",
                        &format!("path is not a file: {}", target.display()),
                    ),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        send_problem(
                            &mut stream,
                            400,
                            "Bad Request",
                            &format!("path not found: {}", target.display()),
                        );
                    }
                    Err(error) => send_problem(
                        &mut stream,
                        500,
                        "Internal Server Error",
                        &error.to_string(),
                    ),
                }
            }
            ("PUT", "/v1/fs/file") => {
                let path = query.get("path").cloned().unwrap_or_default();
                let target = resolve_fs_path(root, &path);
                if let Some(parent) = target.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                match fs::write(&target, body) {
                    Ok(()) => send_json(
                        &mut stream,
                        200,
                        &serde_json::json!({
                            "path": target.to_string_lossy(),
                            "bytesWritten": body.len(),
                        }),
                    ),
                    Err(error) => send_problem(
                        &mut stream,
                        500,
                        "Internal Server Error",
                        &error.to_string(),
                    ),
                }
            }
            ("DELETE", "/v1/fs/entry") => {
                let path = query.get("path").cloned().unwrap_or_default();
                let recursive = query
                    .get("recursive")
                    .map(|value| value == "true")
                    .unwrap_or(false);
                let target = resolve_fs_path(root, &path);
                match fs::metadata(&target) {
                    Ok(metadata) if metadata.is_dir() => {
                        let result = if recursive {
                            fs::remove_dir_all(&target)
                        } else {
                            fs::remove_dir(&target)
                        };
                        match result {
                            Ok(()) => send_json(
                                &mut stream,
                                200,
                                &serde_json::json!({ "path": target.to_string_lossy() }),
                            ),
                            Err(error) => send_problem(
                                &mut stream,
                                500,
                                "Internal Server Error",
                                &error.to_string(),
                            ),
                        }
                    }
                    Ok(_) => match fs::remove_file(&target) {
                        Ok(()) => send_json(
                            &mut stream,
                            200,
                            &serde_json::json!({ "path": target.to_string_lossy() }),
                        ),
                        Err(error) => send_problem(
                            &mut stream,
                            500,
                            "Internal Server Error",
                            &error.to_string(),
                        ),
                    },
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        send_problem(
                            &mut stream,
                            400,
                            "Bad Request",
                            &format!("path not found: {}", target.display()),
                        );
                    }
                    Err(error) => send_problem(
                        &mut stream,
                        500,
                        "Internal Server Error",
                        &error.to_string(),
                    ),
                }
            }
            ("POST", "/v1/fs/mkdir") => {
                let path = query.get("path").cloned().unwrap_or_default();
                let target = resolve_fs_path(root, &path);
                match fs::create_dir_all(&target) {
                    Ok(()) => send_json(
                        &mut stream,
                        200,
                        &serde_json::json!({ "path": target.to_string_lossy() }),
                    ),
                    Err(error) => send_problem(
                        &mut stream,
                        500,
                        "Internal Server Error",
                        &error.to_string(),
                    ),
                }
            }
            ("POST", "/v1/fs/move") => {
                let request: MoveRequest =
                    serde_json::from_slice(body).expect("parse mock move request");
                let source = resolve_fs_path(root, &request.from);
                let destination = resolve_fs_path(root, &request.to);

                if destination.exists() {
                    if request.overwrite.unwrap_or(false) {
                        let metadata =
                            fs::metadata(&destination).expect("inspect mock destination metadata");
                        let remove_result = if metadata.is_dir() {
                            fs::remove_dir_all(&destination)
                        } else {
                            fs::remove_file(&destination)
                        };
                        if let Err(error) = remove_result {
                            send_problem(
                                &mut stream,
                                500,
                                "Internal Server Error",
                                &error.to_string(),
                            );
                            return;
                        }
                    } else {
                        send_problem(
                            &mut stream,
                            400,
                            "Bad Request",
                            &format!("destination already exists: {}", destination.display()),
                        );
                        return;
                    }
                }

                if let Some(parent) = destination.parent() {
                    let _ = fs::create_dir_all(parent);
                }

                match fs::rename(&source, &destination) {
                    Ok(()) => send_json(
                        &mut stream,
                        200,
                        &serde_json::json!({
                            "from": source.to_string_lossy(),
                            "to": destination.to_string_lossy(),
                        }),
                    ),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        send_problem(
                            &mut stream,
                            400,
                            "Bad Request",
                            &format!("path not found: {}", source.display()),
                        );
                    }
                    Err(error) => send_problem(
                        &mut stream,
                        500,
                        "Internal Server Error",
                        &error.to_string(),
                    ),
                }
            }
            ("GET", "/v1/fs/stat") => {
                let path = query.get("path").cloned().unwrap_or_default();
                let target = resolve_fs_path(root, &path);
                match fs::metadata(&target) {
                    Ok(metadata) => send_json(
                        &mut stream,
                        200,
                        &FsStatBody {
                            path: target.to_string_lossy().into_owned(),
                            entry_type: if metadata.is_dir() {
                                "directory"
                            } else {
                                "file"
                            },
                            size: metadata.len(),
                            modified: None,
                        },
                    ),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        send_problem(
                            &mut stream,
                            400,
                            "Bad Request",
                            &format!("path not found: {}", target.display()),
                        );
                    }
                    Err(error) => send_problem(
                        &mut stream,
                        500,
                        "Internal Server Error",
                        &error.to_string(),
                    ),
                }
            }
            ("POST", "/v1/processes/run") => {
                if !process_api_supported {
                    send_problem(
                        &mut stream,
                        501,
                        "Not Implemented",
                        "process API unsupported by mock sandbox-agent",
                    );
                    return;
                }

                let request: ProcessRunRequestBody =
                    serde_json::from_slice(body).expect("parse mock process run request");
                let started = Instant::now();
                let mut command = Command::new(&request.command);
                command.args(rewrite_process_args(root, request.args.unwrap_or_default()));
                if let Some(cwd) = request.cwd {
                    if cwd.starts_with('/') {
                        command.current_dir(resolve_fs_path(root, &cwd));
                    } else {
                        command.current_dir(cwd);
                    }
                }
                if let Some(env) = request.env {
                    command.envs(env);
                }

                match command.output() {
                    Ok(output) => send_json(
                        &mut stream,
                        200,
                        &ProcessRunResponseBody {
                            duration_ms: started.elapsed().as_millis() as u64,
                            exit_code: output.status.code(),
                            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                            stderr_truncated: false,
                            stdout: sanitize_process_stdout(
                                root,
                                String::from_utf8_lossy(&output.stdout).into_owned(),
                            ),
                            stdout_truncated: false,
                            timed_out: false,
                        },
                    ),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => send_json(
                        &mut stream,
                        200,
                        &ProcessRunResponseBody {
                            duration_ms: started.elapsed().as_millis() as u64,
                            exit_code: Some(127),
                            stderr: error.to_string(),
                            stderr_truncated: false,
                            stdout: String::new(),
                            stdout_truncated: false,
                            timed_out: false,
                        },
                    ),
                    Err(error) => send_problem(
                        &mut stream,
                        500,
                        "Internal Server Error",
                        &error.to_string(),
                    ),
                }
            }
            _ => send_problem(&mut stream, 404, "Not Found", "unknown mock route"),
        }
    }

    fn find_header_end(buffer: &[u8]) -> Option<usize> {
        buffer.windows(4).position(|window| window == b"\r\n\r\n")
    }

    fn split_target(target: &str) -> (String, BTreeMap<String, String>) {
        let Some((path, query)) = target.split_once('?') else {
            return (target.to_owned(), BTreeMap::new());
        };

        let query = query
            .split('&')
            .filter(|pair| !pair.is_empty())
            .map(|pair| match pair.split_once('=') {
                Some((name, value)) => (percent_decode(name), percent_decode(value)),
                None => (percent_decode(pair), String::new()),
            })
            .collect::<BTreeMap<_, _>>();
        (path.to_owned(), query)
    }

    fn percent_decode(raw: &str) -> String {
        let bytes = raw.as_bytes();
        let mut index = 0;
        let mut decoded = Vec::with_capacity(bytes.len());
        while index < bytes.len() {
            match bytes[index] {
                b'+' => {
                    decoded.push(b' ');
                    index += 1;
                }
                b'%' if index + 2 < bytes.len() => {
                    if let Ok(value) = u8::from_str_radix(&raw[index + 1..index + 3], 16) {
                        decoded.push(value);
                        index += 3;
                    } else {
                        decoded.push(bytes[index]);
                        index += 1;
                    }
                }
                byte => {
                    decoded.push(byte);
                    index += 1;
                }
            }
        }
        String::from_utf8(decoded).expect("decode mock sandbox-agent query")
    }

    fn resolve_fs_path(root: &Path, path: &str) -> PathBuf {
        let normalized = agent_os_kernel::vfs::normalize_path(path);
        root.join(normalized.trim_start_matches('/'))
    }

    fn rewrite_process_args(root: &Path, args: Vec<String>) -> Vec<String> {
        args.into_iter()
            .map(|arg| {
                if arg.starts_with('/') {
                    resolve_fs_path(root, &arg).to_string_lossy().into_owned()
                } else {
                    arg
                }
            })
            .collect()
    }

    fn sanitize_process_stdout(root: &Path, stdout: String) -> String {
        let trimmed = stdout.trim();
        let Ok(mut value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            return stdout;
        };

        if let Some(result) = value
            .get("result")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
        {
            let root_string = root.to_string_lossy();
            if result == root_string {
                value["result"] = serde_json::Value::String(String::from("/"));
            } else if let Some(stripped) = result.strip_prefix(root_string.as_ref()) {
                value["result"] =
                    serde_json::Value::String(format!("/{}", stripped.trim_start_matches('/')));
            }
        }

        serde_json::to_string(&value).expect("serialize sanitized process stdout")
    }

    fn send_json(stream: &mut TcpStream, status: u16, value: &impl Serialize) {
        let body = serde_json::to_vec(value).expect("serialize mock sandbox-agent response");
        send_bytes(stream, status, "application/json", &body);
    }

    fn send_problem(stream: &mut TcpStream, status: u16, title: &str, detail: &str) {
        send_json(
            stream,
            status,
            &serde_json::json!({
                "type": "about:blank",
                "title": title,
                "status": status,
                "detail": detail,
            }),
        );
    }

    fn send_bytes(stream: &mut TcpStream, status: u16, content_type: &str, body: &[u8]) {
        let status_text = match status {
            200 => "OK",
            400 => "Bad Request",
            401 => "Unauthorized",
            404 => "Not Found",
            501 => "Not Implemented",
            _ => "Internal Server Error",
        };
        let headers = format!(
            "HTTP/1.1 {status} {status_text}\r\nContent-Length: {}\r\nContent-Type: {content_type}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(headers.as_bytes());
        let _ = stream.write_all(body);
        let _ = stream.flush();
    }

    fn temp_dir(prefix: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic enough for temp paths")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{prefix}-{suffix}"));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::MockSandboxAgentServer;
    use super::{SandboxAgentFilesystem, SandboxAgentMountConfig, SandboxAgentMountPlugin};
    use agent_os_kernel::mount_plugin::{FileSystemPluginFactory, OpenFileSystemPluginRequest};
    use agent_os_kernel::vfs::VirtualFileSystem;
    use nix::unistd::{Gid, Uid};
    use serde_json::json;
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    #[test]
    fn filesystem_round_trips_small_files_and_gates_large_pread_without_range_support() {
        let server = MockSandboxAgentServer::start("agent-os-sandbox-plugin", None);
        fs::write(server.root().join("hello.txt"), "hello from sandbox").expect("seed file");
        fs::write(server.root().join("large.bin"), vec![b'x'; 512]).expect("seed large file");

        let mut filesystem = SandboxAgentFilesystem::from_config(SandboxAgentMountConfig {
            base_url: server.base_url().to_owned(),
            token: None,
            headers: None,
            base_path: None,
            timeout_ms: Some(5_000),
            max_full_read_bytes: Some(128),
        })
        .expect("create sandbox_agent filesystem");

        assert_eq!(
            filesystem
                .read_text_file("/hello.txt")
                .expect("read remote file"),
            "hello from sandbox"
        );

        filesystem
            .write_file("/nested/from-vm.txt", b"native sandbox mount".to_vec())
            .expect("write remote file");
        assert_eq!(
            fs::read_to_string(server.root().join("nested/from-vm.txt"))
                .expect("read written file"),
            "native sandbox mount"
        );

        let error = filesystem
            .pread("/large.bin", 4, 8)
            .expect_err("large pread should fail closed without ranged reads");
        assert_eq!(error.code(), "ENOSYS");

        let logged_requests = server.requests();
        assert!(
            !logged_requests.iter().any(|request| {
                request.method == "GET"
                    && request.path == "/v1/fs/file"
                    && request.query.get("path") == Some(&String::from("/large.bin"))
            }),
            "pread gate should reject before issuing a full-file GET"
        );
    }

    #[test]
    fn filesystem_truncate_uses_process_api_without_full_file_buffering() {
        let server = MockSandboxAgentServer::start("agent-os-sandbox-plugin-truncate", None);
        fs::write(server.root().join("large.bin"), vec![b'x'; 512]).expect("seed large file");

        let mut filesystem = SandboxAgentFilesystem::from_config(SandboxAgentMountConfig {
            base_url: server.base_url().to_owned(),
            token: None,
            headers: None,
            base_path: None,
            timeout_ms: Some(5_000),
            max_full_read_bytes: Some(128),
        })
        .expect("create sandbox_agent filesystem");

        filesystem
            .truncate("/large.bin", 3)
            .expect("truncate large file through process helper");
        assert_eq!(
            fs::read(server.root().join("large.bin")).expect("read truncated file"),
            b"xxx".to_vec()
        );

        filesystem
            .truncate("/large.bin", 6)
            .expect("extend file through process helper");
        assert_eq!(
            fs::read(server.root().join("large.bin")).expect("read extended file"),
            vec![b'x', b'x', b'x', 0, 0, 0]
        );

        filesystem
            .truncate("/large.bin", 0)
            .expect("truncate to zero through write_file path");
        assert_eq!(
            fs::metadata(server.root().join("large.bin"))
                .expect("stat zero-length file")
                .len(),
            0
        );

        let logged_requests = server.requests();
        assert!(
            logged_requests
                .iter()
                .any(|request| { request.method == "POST" && request.path == "/v1/processes/run" }),
            "non-zero truncate should use process helper"
        );
        assert!(
            !logged_requests.iter().any(|request| {
                request.method == "GET"
                    && request.path == "/v1/fs/file"
                    && request.query.get("path") == Some(&String::from("/large.bin"))
            }),
            "truncate should not issue a full-file GET"
        );
        assert!(
            logged_requests.iter().any(|request| {
                request.method == "PUT"
                    && request.path == "/v1/fs/file"
                    && request.query.get("path") == Some(&String::from("/large.bin"))
            }),
            "truncate(path, 0) should still use the write_file path"
        );
    }

    #[test]
    fn plugin_scopes_base_path_and_preserves_auth_headers() {
        let server =
            MockSandboxAgentServer::start("agent-os-sandbox-plugin-auth", Some("secret-token"));
        fs::create_dir_all(server.root().join("scoped")).expect("create scoped root");
        fs::write(server.root().join("scoped/hello.txt"), "scoped hello")
            .expect("seed scoped file");

        let plugin = SandboxAgentMountPlugin;
        let mut mounted = plugin
            .open(OpenFileSystemPluginRequest {
                vm_id: "vm-1",
                guest_path: "/sandbox",
                read_only: false,
                config: &json!({
                    "baseUrl": server.base_url(),
                    "token": "secret-token",
                    "headers": {
                        "x-sandbox-test": "enabled"
                    },
                    "basePath": "/scoped"
                }),
                context: &(),
            })
            .expect("open sandbox_agent mount");

        assert_eq!(
            mounted.read_file("/hello.txt").expect("read scoped file"),
            b"scoped hello".to_vec()
        );
        mounted
            .write_file("/from-plugin.txt", b"written through plugin".to_vec())
            .expect("write scoped file");
        assert_eq!(
            fs::read_to_string(server.root().join("scoped/from-plugin.txt"))
                .expect("read plugin output"),
            "written through plugin"
        );

        let logged_requests = server.requests();
        assert!(logged_requests.iter().any(|request| {
            request.headers.get("x-sandbox-test") == Some(&String::from("enabled"))
        }));
    }

    #[test]
    fn filesystem_uses_process_api_for_symlink_and_metadata_operations() {
        let server = MockSandboxAgentServer::start("agent-os-sandbox-plugin-process", None);
        fs::write(server.root().join("original.txt"), "hello from sandbox")
            .expect("seed original file");

        let mut filesystem = SandboxAgentFilesystem::from_config(SandboxAgentMountConfig {
            base_url: server.base_url().to_owned(),
            token: None,
            headers: None,
            base_path: None,
            timeout_ms: Some(5_000),
            max_full_read_bytes: Some(128),
        })
        .expect("create sandbox_agent filesystem");

        filesystem
            .symlink("/original.txt", "/alias.txt")
            .expect("create remote symlink");
        assert_eq!(
            filesystem
                .read_link("/alias.txt")
                .expect("read remote symlink"),
            "/original.txt"
        );
        assert_eq!(
            filesystem
                .realpath("/alias.txt")
                .expect("resolve remote symlink"),
            "/original.txt"
        );

        filesystem
            .link("/original.txt", "/linked.txt")
            .expect("create remote hard link");
        let original_metadata =
            fs::metadata(server.root().join("original.txt")).expect("stat original hard link");
        let linked_metadata =
            fs::metadata(server.root().join("linked.txt")).expect("stat linked hard link");
        assert_eq!(original_metadata.ino(), linked_metadata.ino());

        filesystem
            .write_file("/linked.txt", b"updated through hard link".to_vec())
            .expect("write through hard link");
        assert_eq!(
            fs::read_to_string(server.root().join("original.txt"))
                .expect("read original after linked write"),
            "updated through hard link"
        );

        filesystem
            .chmod("/original.txt", 0o600)
            .expect("chmod remote file");
        assert_eq!(
            fs::metadata(server.root().join("original.txt"))
                .expect("stat chmod result")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        let uid = Uid::current().as_raw();
        let gid = Gid::current().as_raw();
        filesystem
            .chown("/original.txt", uid, gid)
            .expect("chown remote file to current owner");
        let chown_metadata =
            fs::metadata(server.root().join("original.txt")).expect("stat chown result");
        assert_eq!(chown_metadata.uid(), uid);
        assert_eq!(chown_metadata.gid(), gid);

        let atime_ms = 1_700_000_000_000_u64;
        let mtime_ms = 1_710_000_000_000_u64;
        filesystem
            .utimes("/original.txt", atime_ms, mtime_ms)
            .expect("update remote timestamps");
        let utimes_metadata =
            fs::metadata(server.root().join("original.txt")).expect("stat utimes result");
        let observed_atime_ms =
            utimes_metadata.atime() * 1000 + utimes_metadata.atime_nsec() / 1_000_000;
        let observed_mtime_ms =
            utimes_metadata.mtime() * 1000 + utimes_metadata.mtime_nsec() / 1_000_000;
        assert_eq!(observed_atime_ms, atime_ms as i64);
        assert_eq!(observed_mtime_ms, mtime_ms as i64);

        let logged_requests = server.requests();
        assert!(logged_requests
            .iter()
            .any(|request| { request.method == "POST" && request.path == "/v1/processes/run" }));
    }

    #[test]
    fn filesystem_reports_clear_error_when_process_api_is_unavailable() {
        let server = MockSandboxAgentServer::start_without_process_api(
            "agent-os-sandbox-plugin-no-proc",
            None,
        );
        fs::write(server.root().join("original.txt"), "hello from sandbox")
            .expect("seed original file");

        let mut filesystem = SandboxAgentFilesystem::from_config(SandboxAgentMountConfig {
            base_url: server.base_url().to_owned(),
            token: None,
            headers: None,
            base_path: None,
            timeout_ms: Some(5_000),
            max_full_read_bytes: Some(128),
        })
        .expect("create sandbox_agent filesystem");

        let error = filesystem
            .symlink("/original.txt", "/alias.txt")
            .expect_err("symlink should fail clearly without process API");
        assert_eq!(error.code(), "ENOSYS");
        assert!(
            error.to_string().contains("process API"),
            "error should mention process API availability: {error}"
        );
    }
}
