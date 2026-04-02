use crate::vfs::{VfsError, VfsResult, VirtualDirEntry, VirtualFileSystem, VirtualStat};
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
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
    pub fn exists(&self, path: &str) -> VfsResult<bool> {
        self.check(FsOperation::Exists, path)?;
        Ok(self.inner.exists(path))
    }
}

impl<F: VirtualFileSystem> VirtualFileSystem for PermissionedFileSystem<F> {
    fn read_file(&mut self, path: &str) -> VfsResult<Vec<u8>> {
        self.check(FsOperation::Read, path)?;
        self.inner.read_file(path)
    }

    fn read_dir(&mut self, path: &str) -> VfsResult<Vec<String>> {
        self.check(FsOperation::ReadDir, path)?;
        self.inner.read_dir(path)
    }

    fn read_dir_with_types(&mut self, path: &str) -> VfsResult<Vec<VirtualDirEntry>> {
        self.check(FsOperation::ReadDir, path)?;
        self.inner.read_dir_with_types(path)
    }

    fn write_file(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<()> {
        self.check(FsOperation::Write, path)?;
        self.inner.write_file(path, content)
    }

    fn create_dir(&mut self, path: &str) -> VfsResult<()> {
        self.check(FsOperation::CreateDir, path)?;
        self.inner.create_dir(path)
    }

    fn mkdir(&mut self, path: &str, recursive: bool) -> VfsResult<()> {
        self.check(FsOperation::Mkdir, path)?;
        self.inner.mkdir(path, recursive)
    }

    fn exists(&self, path: &str) -> bool {
        match PermissionedFileSystem::exists(self, path) {
            Ok(exists) => exists,
            Err(error) if error.code() == "EACCES" => self.inner.exists(path),
            Err(_) => false,
        }
    }

    fn stat(&mut self, path: &str) -> VfsResult<VirtualStat> {
        self.check(FsOperation::Stat, path)?;
        self.inner.stat(path)
    }

    fn remove_file(&mut self, path: &str) -> VfsResult<()> {
        self.check(FsOperation::Remove, path)?;
        self.inner.remove_file(path)
    }

    fn remove_dir(&mut self, path: &str) -> VfsResult<()> {
        self.check(FsOperation::Remove, path)?;
        self.inner.remove_dir(path)
    }

    fn rename(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        self.check(FsOperation::Rename, old_path)?;
        self.check(FsOperation::Rename, new_path)?;
        self.inner.rename(old_path, new_path)
    }

    fn realpath(&self, path: &str) -> VfsResult<String> {
        self.check(FsOperation::Read, path)?;
        self.inner.realpath(path)
    }

    fn symlink(&mut self, target: &str, link_path: &str) -> VfsResult<()> {
        self.check(FsOperation::Symlink, link_path)?;
        self.inner.symlink(target, link_path)
    }

    fn read_link(&self, path: &str) -> VfsResult<String> {
        self.check(FsOperation::ReadLink, path)?;
        self.inner.read_link(path)
    }

    fn lstat(&self, path: &str) -> VfsResult<VirtualStat> {
        self.check(FsOperation::Stat, path)?;
        self.inner.lstat(path)
    }

    fn link(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        self.check(FsOperation::Link, new_path)?;
        self.inner.link(old_path, new_path)
    }

    fn chmod(&mut self, path: &str, mode: u32) -> VfsResult<()> {
        self.check(FsOperation::Chmod, path)?;
        self.inner.chmod(path, mode)
    }

    fn chown(&mut self, path: &str, uid: u32, gid: u32) -> VfsResult<()> {
        self.check(FsOperation::Chown, path)?;
        self.inner.chown(path, uid, gid)
    }

    fn utimes(&mut self, path: &str, atime_ms: u64, mtime_ms: u64) -> VfsResult<()> {
        self.check(FsOperation::Utimes, path)?;
        self.inner.utimes(path, atime_ms, mtime_ms)
    }

    fn truncate(&mut self, path: &str, length: u64) -> VfsResult<()> {
        self.check(FsOperation::Truncate, path)?;
        self.inner.truncate(path, length)
    }

    fn pread(&mut self, path: &str, offset: u64, length: usize) -> VfsResult<Vec<u8>> {
        self.check(FsOperation::Read, path)?;
        self.inner.pread(path, offset, length)
    }
}
