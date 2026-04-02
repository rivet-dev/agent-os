use agent_os_kernel::mount_plugin::{
    FileSystemPluginFactory, OpenFileSystemPluginRequest, PluginError,
};
use agent_os_kernel::mount_table::{
    MountedFileSystem, MountedVirtualFileSystem, ReadOnlyFileSystem,
};
use agent_os_kernel::vfs::{
    normalize_path, VfsError, VfsResult, VirtualDirEntry, VirtualFileSystem, VirtualStat,
};
use filetime::{set_file_times, FileTime};
use nix::unistd::{chown, Gid, Uid};
use serde::Deserialize;
use std::fs::{self, File};
use std::io;
use std::os::unix::fs::{symlink as create_symlink, FileExt, MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HostDirMountConfig {
    host_path: String,
    read_only: Option<bool>,
}

#[derive(Debug)]
pub(crate) struct HostDirMountPlugin;

impl<Context> FileSystemPluginFactory<Context> for HostDirMountPlugin {
    fn plugin_id(&self) -> &'static str {
        "host_dir"
    }

    fn open(
        &self,
        request: OpenFileSystemPluginRequest<'_, Context>,
    ) -> Result<Box<dyn MountedFileSystem>, PluginError> {
        let config: HostDirMountConfig = serde_json::from_value(request.config.clone())
            .map_err(|error| PluginError::invalid_input(error.to_string()))?;
        let filesystem = HostDirFilesystem::new(&config.host_path)?;
        let mounted = MountedVirtualFileSystem::new(filesystem);

        if config.read_only.unwrap_or(false) {
            Ok(Box::new(ReadOnlyFileSystem::new(mounted)))
        } else {
            Ok(Box::new(mounted))
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct HostDirFilesystem {
    host_root: PathBuf,
}

impl HostDirFilesystem {
    pub(crate) fn new(host_path: impl AsRef<Path>) -> VfsResult<Self> {
        let canonical_root = fs::canonicalize(host_path.as_ref())
            .map_err(|error| io_error_to_vfs("open", "/", error))?;
        let metadata =
            fs::metadata(&canonical_root).map_err(|error| io_error_to_vfs("stat", "/", error))?;
        if !metadata.is_dir() {
            return Err(VfsError::new(
                "ENOTDIR",
                format!(
                    "host_dir root is not a directory: {}",
                    canonical_root.display()
                ),
            ));
        }

        Ok(Self {
            host_root: canonical_root,
        })
    }

    fn ensure_within_root(&self, resolved: &Path, virtual_path: &str) -> VfsResult<()> {
        if resolved == self.host_root {
            return Ok(());
        }

        if resolved.starts_with(&self.host_root) {
            return Ok(());
        }

        Err(VfsError::access_denied(
            "open",
            virtual_path,
            Some("path escapes host directory"),
        ))
    }

    fn lexical_host_path(&self, path: &str) -> VfsResult<PathBuf> {
        let normalized = normalize_path(path);
        let relative = normalized.trim_start_matches('/');
        let joined = lexical_normalize_path(&self.host_root.join(relative));
        self.ensure_within_root(&joined, &normalized)?;
        Ok(joined)
    }

    fn resolve(&self, path: &str) -> VfsResult<PathBuf> {
        let joined = self.lexical_host_path(path)?;
        match fs::canonicalize(&joined) {
            Ok(real) => {
                self.ensure_within_root(&real, path)?;
                Ok(real)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let parent = joined
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| self.host_root.clone());
                match fs::canonicalize(&parent) {
                    Ok(real_parent) => {
                        self.ensure_within_root(&real_parent, path)?;
                    }
                    Err(parent_error) if parent_error.kind() == io::ErrorKind::NotFound => {
                        self.ensure_within_root(&joined, path)?;
                    }
                    Err(parent_error) => {
                        return Err(io_error_to_vfs("open", path, parent_error));
                    }
                }
                Ok(joined)
            }
            Err(error) => Err(io_error_to_vfs("open", path, error)),
        }
    }

    fn resolve_no_follow(&self, path: &str) -> VfsResult<PathBuf> {
        let joined = self.lexical_host_path(path)?;
        let parent = joined
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.host_root.clone());
        match fs::canonicalize(&parent) {
            Ok(real_parent) => {
                self.ensure_within_root(&real_parent, path)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.ensure_within_root(&joined, path)?;
            }
            Err(error) => return Err(io_error_to_vfs("open", path, error)),
        }
        Ok(joined)
    }

    fn host_to_virtual_path(&self, host_path: &Path, virtual_path: &str) -> VfsResult<String> {
        let normalized = lexical_normalize_path(host_path);
        self.ensure_within_root(&normalized, virtual_path)?;
        let relative = normalized.strip_prefix(&self.host_root).map_err(|_| {
            VfsError::access_denied("open", virtual_path, Some("path escapes host directory"))
        })?;

        if relative.as_os_str().is_empty() {
            return Ok(String::from("/"));
        }

        let segments = relative
            .components()
            .filter_map(|component| match component {
                Component::Normal(segment) => Some(segment.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect::<Vec<_>>();
        Ok(format!("/{}", segments.join("/")))
    }

    fn stat_from_metadata(metadata: fs::Metadata) -> VirtualStat {
        let atime_ms = metadata.atime().max(0) as u64 * 1_000
            + (metadata.atime_nsec().max(0) as u64 / 1_000_000);
        let mtime_ms = metadata.mtime().max(0) as u64 * 1_000
            + (metadata.mtime_nsec().max(0) as u64 / 1_000_000);
        let ctime_ms = metadata.ctime().max(0) as u64 * 1_000
            + (metadata.ctime_nsec().max(0) as u64 / 1_000_000);
        VirtualStat {
            mode: metadata.mode(),
            size: metadata.size(),
            is_directory: metadata.is_dir(),
            is_symbolic_link: metadata.file_type().is_symlink(),
            atime_ms,
            mtime_ms,
            ctime_ms,
            birthtime_ms: ctime_ms,
            ino: metadata.ino(),
            nlink: metadata.nlink(),
            uid: metadata.uid(),
            gid: metadata.gid(),
        }
    }
}

impl VirtualFileSystem for HostDirFilesystem {
    fn read_file(&mut self, path: &str) -> VfsResult<Vec<u8>> {
        fs::read(self.resolve(path)?).map_err(|error| io_error_to_vfs("open", path, error))
    }

    fn read_dir(&mut self, path: &str) -> VfsResult<Vec<String>> {
        let mut entries = fs::read_dir(self.resolve(path)?)
            .map_err(|error| io_error_to_vfs("readdir", path, error))?
            .map(|entry| {
                entry
                    .map_err(|error| io_error_to_vfs("readdir", path, error))
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
            })
            .collect::<VfsResult<Vec<_>>>()?;
        entries.sort();
        Ok(entries)
    }

    fn read_dir_with_types(&mut self, path: &str) -> VfsResult<Vec<VirtualDirEntry>> {
        let mut entries = fs::read_dir(self.resolve(path)?)
            .map_err(|error| io_error_to_vfs("readdir", path, error))?
            .map(|entry| {
                let entry = entry.map_err(|error| io_error_to_vfs("readdir", path, error))?;
                let file_type = entry
                    .file_type()
                    .map_err(|error| io_error_to_vfs("readdir", path, error))?;
                Ok(VirtualDirEntry {
                    name: entry.file_name().to_string_lossy().into_owned(),
                    is_directory: file_type.is_dir(),
                    is_symbolic_link: file_type.is_symlink(),
                })
            })
            .collect::<VfsResult<Vec<_>>>()?;
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(entries)
    }

    fn write_file(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<()> {
        let host_path = self.resolve(path)?;
        if let Some(parent) = host_path.parent() {
            fs::create_dir_all(parent).map_err(|error| io_error_to_vfs("mkdir", path, error))?;
        }
        fs::write(host_path, content.into()).map_err(|error| io_error_to_vfs("write", path, error))
    }

    fn create_dir(&mut self, path: &str) -> VfsResult<()> {
        fs::create_dir(self.resolve(path)?).map_err(|error| io_error_to_vfs("mkdir", path, error))
    }

    fn mkdir(&mut self, path: &str, recursive: bool) -> VfsResult<()> {
        let host_path = self.resolve(path)?;
        if recursive {
            fs::create_dir_all(host_path)
        } else {
            fs::create_dir(host_path)
        }
        .map_err(|error| io_error_to_vfs("mkdir", path, error))
    }

    fn exists(&self, path: &str) -> bool {
        self.resolve(path)
            .map(|resolved| resolved.exists())
            .unwrap_or(false)
    }

    fn stat(&mut self, path: &str) -> VfsResult<VirtualStat> {
        fs::metadata(self.resolve(path)?)
            .map(Self::stat_from_metadata)
            .map_err(|error| io_error_to_vfs("stat", path, error))
    }

    fn remove_file(&mut self, path: &str) -> VfsResult<()> {
        fs::remove_file(self.resolve_no_follow(path)?)
            .map_err(|error| io_error_to_vfs("unlink", path, error))
    }

    fn remove_dir(&mut self, path: &str) -> VfsResult<()> {
        fs::remove_dir(self.resolve(path)?).map_err(|error| io_error_to_vfs("rmdir", path, error))
    }

    fn rename(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        let old_host_path = self.resolve_no_follow(old_path)?;
        let new_host_path = self.resolve_no_follow(new_path)?;
        if let Some(parent) = new_host_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| io_error_to_vfs("mkdir", new_path, error))?;
        }
        fs::rename(old_host_path, new_host_path)
            .map_err(|error| io_error_to_vfs("rename", old_path, error))
    }

    fn realpath(&self, path: &str) -> VfsResult<String> {
        let resolved = fs::canonicalize(self.resolve_no_follow(path)?)
            .map_err(|error| io_error_to_vfs("realpath", path, error))?;
        self.host_to_virtual_path(&resolved, path)
    }

    fn symlink(&mut self, target: &str, link_path: &str) -> VfsResult<()> {
        let host_link_path = self.resolve_no_follow(link_path)?;
        if let Some(parent) = host_link_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| io_error_to_vfs("mkdir", link_path, error))?;
        }

        let link_virtual_path = normalize_path(link_path);
        let target_virtual_path = if target.starts_with('/') {
            normalize_path(target)
        } else {
            normalize_path(&format!(
                "{}/{}",
                virtual_dirname(&link_virtual_path),
                target
            ))
        };
        let host_target_path = self.lexical_host_path(&target_virtual_path)?;
        let relative_target = relative_path(
            host_link_path.parent().unwrap_or(self.host_root.as_path()),
            &host_target_path,
        );
        create_symlink(&relative_target, host_link_path)
            .map_err(|error| io_error_to_vfs("symlink", link_path, error))
    }

    fn read_link(&self, path: &str) -> VfsResult<String> {
        let host_link_path = self.resolve_no_follow(path)?;
        let link_target = fs::read_link(&host_link_path)
            .map_err(|error| io_error_to_vfs("readlink", path, error))?;
        let resolved_target = if link_target.is_absolute() {
            lexical_normalize_path(&link_target)
        } else {
            lexical_normalize_path(
                &host_link_path
                    .parent()
                    .unwrap_or(self.host_root.as_path())
                    .join(link_target),
            )
        };
        self.host_to_virtual_path(&resolved_target, path)
    }

    fn lstat(&self, path: &str) -> VfsResult<VirtualStat> {
        fs::symlink_metadata(self.resolve_no_follow(path)?)
            .map(Self::stat_from_metadata)
            .map_err(|error| io_error_to_vfs("lstat", path, error))
    }

    fn link(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        let host_old_path = self.resolve_no_follow(old_path)?;
        let host_new_path = self.resolve_no_follow(new_path)?;
        if let Some(parent) = host_new_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| io_error_to_vfs("mkdir", new_path, error))?;
        }
        fs::hard_link(host_old_path, host_new_path)
            .map_err(|error| io_error_to_vfs("link", new_path, error))
    }

    fn chmod(&mut self, path: &str, mode: u32) -> VfsResult<()> {
        fs::set_permissions(self.resolve(path)?, fs::Permissions::from_mode(mode))
            .map_err(|error| io_error_to_vfs("chmod", path, error))
    }

    fn chown(&mut self, path: &str, uid: u32, gid: u32) -> VfsResult<()> {
        chown(
            &self.resolve(path)?,
            Some(Uid::from_raw(uid)),
            Some(Gid::from_raw(gid)),
        )
        .map_err(|error| VfsError::new(error_code(&error), error.to_string()))
    }

    fn utimes(&mut self, path: &str, atime_ms: u64, mtime_ms: u64) -> VfsResult<()> {
        set_file_times(
            self.resolve(path)?,
            FileTime::from_unix_time(
                (atime_ms / 1_000) as i64,
                ((atime_ms % 1_000) * 1_000_000) as u32,
            ),
            FileTime::from_unix_time(
                (mtime_ms / 1_000) as i64,
                ((mtime_ms % 1_000) * 1_000_000) as u32,
            ),
        )
        .map_err(|error| io_error_to_vfs("utimes", path, error))
    }

    fn truncate(&mut self, path: &str, length: u64) -> VfsResult<()> {
        File::options()
            .write(true)
            .open(self.resolve(path)?)
            .and_then(|file| file.set_len(length))
            .map_err(|error| io_error_to_vfs("truncate", path, error))
    }

    fn pread(&mut self, path: &str, offset: u64, length: usize) -> VfsResult<Vec<u8>> {
        let file = File::open(self.resolve(path)?)
            .map_err(|error| io_error_to_vfs("open", path, error))?;
        let mut buffer = vec![0; length];
        let bytes_read = file
            .read_at(&mut buffer, offset)
            .map_err(|error| io_error_to_vfs("open", path, error))?;
        buffer.truncate(bytes_read);
        Ok(buffer)
    }
}

fn io_error_to_vfs(op: &'static str, path: &str, error: io::Error) -> VfsError {
    let code = match error.raw_os_error() {
        Some(1) => "EPERM",
        Some(2) => "ENOENT",
        Some(13) => "EACCES",
        Some(17) => "EEXIST",
        Some(18) => "EXDEV",
        Some(20) => "ENOTDIR",
        Some(21) => "EISDIR",
        Some(22) => "EINVAL",
        Some(30) => "EROFS",
        Some(39) => "ENOTEMPTY",
        Some(40) => "ELOOP",
        _ => match error.kind() {
            io::ErrorKind::NotFound => "ENOENT",
            io::ErrorKind::PermissionDenied => "EACCES",
            io::ErrorKind::AlreadyExists => "EEXIST",
            io::ErrorKind::InvalidInput => "EINVAL",
            _ => "EIO",
        },
    };
    VfsError::new(code, format!("{op} '{path}': {error}"))
}

fn error_code(error: &nix::Error) -> &'static str {
    match error {
        nix::Error::EACCES => "EACCES",
        nix::Error::EEXIST => "EEXIST",
        nix::Error::EINVAL => "EINVAL",
        nix::Error::EISDIR => "EISDIR",
        nix::Error::ELOOP => "ELOOP",
        nix::Error::ENOENT => "ENOENT",
        nix::Error::ENOTDIR => "ENOTDIR",
        nix::Error::ENOTEMPTY => "ENOTEMPTY",
        nix::Error::EPERM => "EPERM",
        nix::Error::EROFS => "EROFS",
        _ => "EIO",
    }
}

fn lexical_normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir => normalized.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(segment) => normalized.push(segment),
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
        }
    }

    if normalized.as_os_str().is_empty() {
        PathBuf::from("/")
    } else {
        normalized
    }
}

fn relative_path(from_dir: &Path, to: &Path) -> PathBuf {
    let from_components = from_dir.components().collect::<Vec<_>>();
    let to_components = to.components().collect::<Vec<_>>();
    let shared = from_components
        .iter()
        .zip(to_components.iter())
        .take_while(|(left, right)| left == right)
        .count();

    let mut relative = PathBuf::new();
    for _ in shared..from_components.len() {
        relative.push("..");
    }
    for component in &to_components[shared..] {
        if let Component::Normal(segment) = component {
            relative.push(segment);
        }
    }

    if relative.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        relative
    }
}

fn virtual_dirname(path: &str) -> String {
    let normalized = normalize_path(path);
    match normalized.rsplit_once('/') {
        Some((head, _)) if !head.is_empty() => head.to_owned(),
        _ => String::from("/"),
    }
}

#[cfg(test)]
mod tests {
    use super::{HostDirFilesystem, HostDirMountPlugin};
    use agent_os_kernel::mount_plugin::{FileSystemPluginFactory, OpenFileSystemPluginRequest};
    use agent_os_kernel::mount_table::MountedFileSystem;
    use agent_os_kernel::vfs::VirtualFileSystem;
    use serde_json::json;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(prefix: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be monotonic enough for temp paths")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{prefix}-{suffix}"));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    #[test]
    fn filesystem_rejects_symlink_escapes_and_round_trips_writes() {
        let host_dir = temp_dir("agent-os-host-dir-plugin");
        fs::write(host_dir.join("hello.txt"), "hello from host").expect("seed host file");
        std::os::unix::fs::symlink("/etc", host_dir.join("escape")).expect("seed escape symlink");

        let mut filesystem = HostDirFilesystem::new(&host_dir).expect("create host dir fs");
        assert_eq!(
            filesystem
                .read_text_file("/hello.txt")
                .expect("read host file"),
            "hello from host"
        );

        filesystem
            .write_file("/nested/out.txt", b"written from vm".to_vec())
            .expect("write through host dir fs");
        assert_eq!(
            fs::read_to_string(host_dir.join("nested/out.txt")).expect("read written host file"),
            "written from vm"
        );

        let error = filesystem
            .read_file("/escape/hostname")
            .expect_err("escape symlink should fail closed");
        assert_eq!(error.code(), "EACCES");

        fs::remove_dir_all(host_dir).expect("remove temp dir");
    }

    #[test]
    fn plugin_config_can_enforce_read_only_mounts() {
        let host_dir = temp_dir("agent-os-host-dir-plugin-readonly");
        fs::write(host_dir.join("hello.txt"), "hello from host").expect("seed host file");

        let plugin = HostDirMountPlugin;
        let mut mounted = plugin
            .open(OpenFileSystemPluginRequest {
                vm_id: "vm-1",
                guest_path: "/workspace",
                read_only: false,
                config: &json!({
                    "hostPath": host_dir,
                    "readOnly": true,
                }),
                context: &(),
            })
            .expect("open host_dir plugin");

        assert_eq!(
            mounted.read_file("/hello.txt").expect("read host file"),
            b"hello from host".to_vec()
        );
        let error = mounted
            .write_file("/blocked.txt", b"blocked".to_vec())
            .expect_err("readonly plugin config should reject writes");
        assert_eq!(error.code(), "EROFS");

        fs::remove_dir_all(host_dir).expect("remove temp dir");
    }
}
