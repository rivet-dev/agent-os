use crate::vfs::{
    validate_path, VfsError, VfsResult, VirtualDirEntry, VirtualFileSystem, VirtualStat,
    VirtualUtimeSpec,
};
use std::collections::{BTreeMap, HashMap};
use std::error::Error;
use std::fmt;
use std::path::Path;
use std::sync::Arc;

pub type FsPermissionCheck = Arc<dyn Fn(&FsAccessRequest) -> PermissionDecision + Send + Sync>;
pub type NetworkPermissionCheck =
    Arc<dyn Fn(&NetworkAccessRequest) -> PermissionDecision + Send + Sync>;
pub type CommandPermissionCheck =
    Arc<dyn Fn(&CommandAccessRequest) -> PermissionDecision + Send + Sync>;
pub type EnvironmentPermissionCheck =
    Arc<dyn Fn(&EnvAccessRequest) -> PermissionDecision + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionDecision {
    pub allow: bool,
    pub reason: Option<String>,
}

impl PermissionDecision {
    pub fn allow() -> Self {
        Self {
            allow: true,
            reason: None,
        }
    }

    pub fn deny(reason: impl Into<String>) -> Self {
        Self {
            allow: false,
            reason: Some(reason.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionError {
    code: &'static str,
    message: String,
}

impl PermissionError {
    pub fn code(&self) -> &'static str {
        self.code
    }

    fn access_denied(subject: impl Into<String>, reason: Option<&str>) -> Self {
        let subject = subject.into();
        let message = match reason {
            Some(reason) => format!("permission denied, {subject}: {reason}"),
            None => format!("permission denied, {subject}"),
        };

        Self {
            code: "EACCES",
            message,
        }
    }
}

impl fmt::Display for PermissionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl Error for PermissionError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsOperation {
    Read,
    Write,
    Mkdir,
    CreateDir,
    ReadDir,
    Stat,
    Remove,
    Rename,
    Exists,
    Symlink,
    ReadLink,
    Link,
    Chmod,
    Chown,
    Utimes,
    Truncate,
    MountSensitive,
}

impl FsOperation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Mkdir => "mkdir",
            Self::CreateDir => "createDir",
            Self::ReadDir => "readdir",
            Self::Stat => "stat",
            Self::Remove => "rm",
            Self::Rename => "rename",
            Self::Exists => "exists",
            Self::Symlink => "symlink",
            Self::ReadLink => "readlink",
            Self::Link => "link",
            Self::Chmod => "chmod",
            Self::Chown => "chown",
            Self::Utimes => "utimes",
            Self::Truncate => "truncate",
            Self::MountSensitive => "mount",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsAccessRequest {
    pub vm_id: String,
    pub op: FsOperation,
    pub path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkOperation {
    Fetch,
    Http,
    Dns,
    Listen,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkAccessRequest {
    pub vm_id: String,
    pub op: NetworkOperation,
    pub resource: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandAccessRequest {
    pub vm_id: String,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvironmentOperation {
    Read,
    Write,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvAccessRequest {
    pub vm_id: String,
    pub op: EnvironmentOperation,
    pub key: String,
    pub value: Option<String>,
}

#[derive(Clone, Default)]
pub struct Permissions {
    pub filesystem: Option<FsPermissionCheck>,
    pub network: Option<NetworkPermissionCheck>,
    pub child_process: Option<CommandPermissionCheck>,
    pub environment: Option<EnvironmentPermissionCheck>,
}

impl Permissions {
    pub fn allow_all() -> Self {
        Self {
            filesystem: Some(Arc::new(|_: &FsAccessRequest| PermissionDecision::allow())),
            network: Some(Arc::new(|_: &NetworkAccessRequest| {
                PermissionDecision::allow()
            })),
            child_process: Some(Arc::new(|_: &CommandAccessRequest| {
                PermissionDecision::allow()
            })),
            environment: Some(Arc::new(|_: &EnvAccessRequest| PermissionDecision::allow())),
        }
    }
}

pub fn permission_glob_matches(pattern: &str, value: &str) -> bool {
    fn matches(
        pattern: &[u8],
        value: &[u8],
        pattern_index: usize,
        value_index: usize,
        memo: &mut HashMap<(usize, usize), bool>,
    ) -> bool {
        if let Some(result) = memo.get(&(pattern_index, value_index)) {
            return *result;
        }

        let result = if pattern_index == pattern.len() {
            value_index == value.len()
        } else {
            match pattern[pattern_index] {
                b'?' => {
                    value_index < value.len()
                        && value[value_index] != b'/'
                        && matches(pattern, value, pattern_index + 1, value_index + 1, memo)
                }
                b'*' => {
                    let mut next_pattern_index = pattern_index;
                    while next_pattern_index < pattern.len() && pattern[next_pattern_index] == b'*'
                    {
                        next_pattern_index += 1;
                    }

                    if matches(pattern, value, next_pattern_index, value_index, memo) {
                        true
                    } else {
                        let crosses_separators = next_pattern_index - pattern_index > 1;
                        let mut next_value_index = value_index;
                        while next_value_index < value.len()
                            && (crosses_separators || value[next_value_index] != b'/')
                        {
                            next_value_index += 1;
                            if matches(pattern, value, next_pattern_index, next_value_index, memo) {
                                return true;
                            }
                        }
                        false
                    }
                }
                expected => {
                    value_index < value.len()
                        && expected == value[value_index]
                        && matches(pattern, value, pattern_index + 1, value_index + 1, memo)
                }
            }
        };

        memo.insert((pattern_index, value_index), result);
        result
    }

    matches(
        pattern.as_bytes(),
        value.as_bytes(),
        0,
        0,
        &mut HashMap::new(),
    )
}

pub fn filter_env(
    vm_id: &str,
    env: &BTreeMap<String, String>,
    permissions: &Permissions,
) -> BTreeMap<String, String> {
    let Some(check) = permissions.environment.as_ref() else {
        return BTreeMap::new();
    };

    env.iter()
        .filter_map(|(key, value)| {
            let request = EnvAccessRequest {
                vm_id: vm_id.to_owned(),
                op: EnvironmentOperation::Read,
                key: key.clone(),
                value: Some(value.clone()),
            };
            let decision = check(&request);
            decision.allow.then(|| (key.clone(), value.clone()))
        })
        .collect()
}

pub fn check_command_execution(
    vm_id: &str,
    permissions: &Permissions,
    command: &str,
    args: &[String],
    cwd: Option<&str>,
    env: &BTreeMap<String, String>,
) -> Result<(), PermissionError> {
    let Some(check) = permissions.child_process.as_ref() else {
        return Ok(());
    };

    let request = CommandAccessRequest {
        vm_id: vm_id.to_owned(),
        command: command.to_owned(),
        args: args.to_vec(),
        cwd: cwd.map(ToOwned::to_owned),
        env: env.clone(),
    };
    let decision = check(&request);
    if decision.allow {
        Ok(())
    } else {
        Err(PermissionError::access_denied(
            format!("spawn '{command}'"),
            decision.reason.as_deref(),
        ))
    }
}

pub fn check_network_access(
    vm_id: &str,
    permissions: &Permissions,
    op: NetworkOperation,
    resource: &str,
) -> Result<(), PermissionError> {
    let Some(check) = permissions.network.as_ref() else {
        return Ok(());
    };

    let request = NetworkAccessRequest {
        vm_id: vm_id.to_owned(),
        op,
        resource: resource.to_owned(),
    };
    let decision = check(&request);
    if decision.allow {
        Ok(())
    } else {
        Err(PermissionError::access_denied(
            resource,
            decision.reason.as_deref(),
        ))
    }
}

#[derive(Clone)]
pub struct PermissionedFileSystem<F> {
    inner: F,
    vm_id: String,
    permissions: Permissions,
}

impl<F> PermissionedFileSystem<F> {
    pub fn new(inner: F, vm_id: impl Into<String>, permissions: Permissions) -> Self {
        Self {
            inner,
            vm_id: vm_id.into(),
            permissions,
        }
    }

    pub fn into_inner(self) -> F {
        self.inner
    }

    pub fn inner(&self) -> &F {
        &self.inner
    }

    pub fn inner_mut(&mut self) -> &mut F {
        &mut self.inner
    }

    fn check(&self, op: FsOperation, path: &str) -> VfsResult<()> {
        validate_path(path)?;
        let Some(check) = self.permissions.filesystem.as_ref() else {
            return Err(VfsError::access_denied(op.as_str(), path, None));
        };

        let request = FsAccessRequest {
            vm_id: self.vm_id.clone(),
            op,
            path: path.to_owned(),
        };
        let decision = check(&request);
        if decision.allow {
            Ok(())
        } else {
            Err(VfsError::access_denied(
                op.as_str(),
                path,
                decision.reason.as_deref(),
            ))
        }
    }
}

impl<F: VirtualFileSystem> PermissionedFileSystem<F> {
    fn resolved_existing_path(&self, path: &str) -> VfsResult<String> {
        self.inner.realpath(path)
    }

    fn resolved_destination_path(&self, path: &str) -> VfsResult<String> {
        let normalized = crate::vfs::normalize_path(path);
        if normalized == "/" {
            return Ok(normalized);
        }

        let parent = Path::new(&normalized)
            .parent()
            .unwrap_or_else(|| Path::new("/"))
            .to_string_lossy()
            .into_owned();
        let basename = Path::new(&normalized)
            .file_name()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_default();

        let mut candidate = parent;
        let mut unresolved_segments = Vec::new();

        let resolved_parent = loop {
            match self.inner.realpath(&candidate) {
                Ok(resolved) => break resolved,
                Err(error) if matches!(error.code(), "ENOENT" | "ENOTDIR") => {
                    if candidate == "/" {
                        break String::from("/");
                    }
                    let candidate_path = Path::new(&candidate);
                    if let Some(segment) = candidate_path.file_name() {
                        unresolved_segments.push(segment.to_string_lossy().into_owned());
                    }
                    candidate = candidate_path
                        .parent()
                        .unwrap_or_else(|| Path::new("/"))
                        .to_string_lossy()
                        .into_owned();
                }
                Err(error) => return Err(error),
            }
        };

        let mut resolved = resolved_parent;
        for segment in unresolved_segments.iter().rev() {
            if resolved == "/" {
                resolved = format!("/{segment}");
            } else {
                resolved = format!("{resolved}/{segment}");
            }
        }

        if resolved == "/" {
            Ok(format!("/{basename}"))
        } else {
            Ok(format!("{resolved}/{basename}"))
        }
    }

    fn permission_subject(&self, op: FsOperation, path: &str) -> VfsResult<String> {
        validate_path(path)?;
        match op {
            FsOperation::Read
            | FsOperation::ReadDir
            | FsOperation::Stat
            | FsOperation::ReadLink
            | FsOperation::Chmod
            | FsOperation::Chown
            | FsOperation::Utimes
            | FsOperation::Truncate => self.resolved_existing_path(path),
            FsOperation::Exists | FsOperation::Write => self
                .resolved_existing_path(path)
                .or_else(|_| self.resolved_destination_path(path)),
            FsOperation::Mkdir
            | FsOperation::CreateDir
            | FsOperation::Rename
            | FsOperation::Symlink
            | FsOperation::Link
            | FsOperation::MountSensitive
            | FsOperation::Remove => self.resolved_destination_path(path),
        }
    }

    fn check_subject(&self, op: FsOperation, path: &str) -> VfsResult<()> {
        let subject = self.permission_subject(op, path)?;
        self.check(op, &subject)
    }

    fn check_existing_subject(&self, op: FsOperation, path: &str) -> VfsResult<()> {
        validate_path(path)?;
        let subject = self.resolved_existing_path(path)?;
        self.check(op, &subject)
    }

    fn check_destination_subject(&self, op: FsOperation, path: &str) -> VfsResult<()> {
        validate_path(path)?;
        let subject = self.resolved_destination_path(path)?;
        self.check(op, &subject)
    }

    pub fn check_path(&self, op: FsOperation, path: &str) -> VfsResult<()> {
        self.check_subject(op, path)
    }

    pub fn check_virtual_path(&self, op: FsOperation, path: &str) -> VfsResult<()> {
        self.check(op, path)
    }

    pub fn exists(&self, path: &str) -> VfsResult<bool> {
        if let Err(error) = self.check_subject(FsOperation::Exists, path) {
            if matches!(error.code(), "EACCES" | "ENOENT" | "ENOTDIR" | "ELOOP") {
                return Ok(false);
            }
            return Err(error);
        }
        Ok(self.inner.exists(path))
    }
}

impl<F: VirtualFileSystem> VirtualFileSystem for PermissionedFileSystem<F> {
    fn read_file(&mut self, path: &str) -> VfsResult<Vec<u8>> {
        self.check_subject(FsOperation::Read, path)?;
        self.inner.read_file(path)
    }

    fn read_dir(&mut self, path: &str) -> VfsResult<Vec<String>> {
        self.check_subject(FsOperation::ReadDir, path)?;
        self.inner.read_dir(path)
    }

    fn read_dir_limited(&mut self, path: &str, max_entries: usize) -> VfsResult<Vec<String>> {
        self.check_subject(FsOperation::ReadDir, path)?;
        self.inner.read_dir_limited(path, max_entries)
    }

    fn read_dir_with_types(&mut self, path: &str) -> VfsResult<Vec<VirtualDirEntry>> {
        self.check_subject(FsOperation::ReadDir, path)?;
        self.inner.read_dir_with_types(path)
    }

    fn write_file(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<()> {
        self.check_subject(FsOperation::Write, path)?;
        self.inner.write_file(path, content)
    }

    fn create_file_exclusive(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<()> {
        self.check_subject(FsOperation::Write, path)?;
        self.inner.create_file_exclusive(path, content)
    }

    fn append_file(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<u64> {
        self.check_subject(FsOperation::Write, path)?;
        self.inner.append_file(path, content)
    }

    fn create_dir(&mut self, path: &str) -> VfsResult<()> {
        self.check_subject(FsOperation::CreateDir, path)?;
        self.inner.create_dir(path)
    }

    fn mkdir(&mut self, path: &str, recursive: bool) -> VfsResult<()> {
        self.check_subject(FsOperation::Mkdir, path)?;
        self.inner.mkdir(path, recursive)
    }

    fn exists(&self, path: &str) -> bool {
        PermissionedFileSystem::exists(self, path).unwrap_or(false)
    }

    fn stat(&mut self, path: &str) -> VfsResult<VirtualStat> {
        self.check_subject(FsOperation::Stat, path)?;
        self.inner.stat(path)
    }

    fn remove_file(&mut self, path: &str) -> VfsResult<()> {
        self.check_subject(FsOperation::Remove, path)?;
        self.inner.remove_file(path)
    }

    fn remove_dir(&mut self, path: &str) -> VfsResult<()> {
        self.check_subject(FsOperation::Remove, path)?;
        self.inner.remove_dir(path)
    }

    fn rename(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        self.check_subject(FsOperation::Rename, old_path)?;
        self.check_subject(FsOperation::Rename, new_path)?;
        self.inner.rename(old_path, new_path)
    }

    fn realpath(&self, path: &str) -> VfsResult<String> {
        self.check_subject(FsOperation::Read, path)?;
        self.inner.realpath(path)
    }

    fn symlink(&mut self, target: &str, link_path: &str) -> VfsResult<()> {
        self.check_subject(FsOperation::Symlink, link_path)?;
        self.inner.symlink(target, link_path)
    }

    fn read_link(&self, path: &str) -> VfsResult<String> {
        self.check(FsOperation::ReadLink, &crate::vfs::normalize_path(path))?;
        self.inner.read_link(path)
    }

    fn lstat(&self, path: &str) -> VfsResult<VirtualStat> {
        self.check(FsOperation::Stat, &crate::vfs::normalize_path(path))?;
        self.inner.lstat(path)
    }

    fn link(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        self.check_existing_subject(FsOperation::Link, old_path)?;
        self.check_destination_subject(FsOperation::Link, new_path)?;
        self.inner.link(old_path, new_path)
    }

    fn chmod(&mut self, path: &str, mode: u32) -> VfsResult<()> {
        self.check_subject(FsOperation::Chmod, path)?;
        self.inner.chmod(path, mode)
    }

    fn chown(&mut self, path: &str, uid: u32, gid: u32) -> VfsResult<()> {
        self.check_subject(FsOperation::Chown, path)?;
        self.inner.chown(path, uid, gid)
    }

    fn utimes(&mut self, path: &str, atime_ms: u64, mtime_ms: u64) -> VfsResult<()> {
        self.check_subject(FsOperation::Utimes, path)?;
        self.inner.utimes(path, atime_ms, mtime_ms)
    }

    fn utimes_spec(
        &mut self,
        path: &str,
        atime: VirtualUtimeSpec,
        mtime: VirtualUtimeSpec,
        follow_symlinks: bool,
    ) -> VfsResult<()> {
        self.check_subject(FsOperation::Utimes, path)?;
        self.inner.utimes_spec(path, atime, mtime, follow_symlinks)
    }

    fn truncate(&mut self, path: &str, length: u64) -> VfsResult<()> {
        self.check_subject(FsOperation::Truncate, path)?;
        self.inner.truncate(path, length)
    }

    fn pread(&mut self, path: &str, offset: u64, length: usize) -> VfsResult<Vec<u8>> {
        self.check_subject(FsOperation::Read, path)?;
        self.inner.pread(path, offset, length)
    }
}
