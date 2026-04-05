use agent_os_kernel::mount_plugin::{
    FileSystemPluginFactory, OpenFileSystemPluginRequest, PluginError,
};
use agent_os_kernel::mount_table::{
    MountedFileSystem, MountedVirtualFileSystem, ReadOnlyFileSystem,
};
use agent_os_kernel::vfs::{
    normalize_path, VfsError, VfsResult, VirtualDirEntry, VirtualFileSystem, VirtualStat,
};
use nix::errno::Errno;
use nix::fcntl::{openat2, readlinkat, renameat, AtFlags, OFlag, OpenHow, ResolveFlag};
use nix::sys::stat::{
    fchmodat, fstatat, mkdirat, utimensat, FchmodatFlags, Mode, SFlag, UtimensatFlags,
};
use nix::sys::time::{TimeSpec, TimeValLike};
use nix::unistd::{fchownat, linkat, symlinkat, unlinkat, Gid, Uid, UnlinkatFlags};
use serde::Deserialize;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

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
    host_root_dir: Arc<File>,
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
            host_root: canonical_root.clone(),
            host_root_dir: Arc::new(
                File::open(&canonical_root).map_err(|error| io_error_to_vfs("open", "/", error))?,
            ),
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

    fn relative_virtual_path(&self, path: &str) -> (String, PathBuf) {
        let normalized = normalize_path(path);
        let relative = normalized.trim_start_matches('/');
        let relative = if relative.is_empty() {
            PathBuf::from(".")
        } else {
            PathBuf::from(relative)
        };
        (normalized, relative)
    }

    fn resolve_flags() -> ResolveFlag {
        ResolveFlag::RESOLVE_BENEATH | ResolveFlag::RESOLVE_NO_MAGICLINKS
    }

    fn open_beneath(&self, relative: &Path, flags: OFlag, mode: Mode) -> VfsResult<AnchoredFd> {
        let relative_display = relative.display().to_string();
        let fd = openat2(
            self.host_root_dir.as_raw_fd(),
            relative,
            OpenHow::new()
                .flags(flags | OFlag::O_CLOEXEC)
                .mode(mode)
                .resolve(Self::resolve_flags()),
        )
        .map_err(|error| match error {
            Errno::EXDEV => VfsError::access_denied(
                "open",
                &relative_display,
                Some("path escapes host directory"),
            ),
            other => io_error_to_vfs("open", &relative_display, nix_to_io(other)),
        })?;
        Ok(AnchoredFd { fd })
    }

    fn open_directory_beneath(&self, relative: &Path) -> VfsResult<AnchoredFd> {
        self.open_beneath(
            relative,
            OFlag::O_DIRECTORY | OFlag::O_RDONLY,
            Mode::empty(),
        )
    }

    fn host_path_for_fd(&self, fd: &AnchoredFd, virtual_path: &str) -> VfsResult<PathBuf> {
        let host_path = fs::read_link(fd.proc_path())
            .map_err(|error| io_error_to_vfs("open", virtual_path, error))?;
        self.ensure_within_root(&host_path, virtual_path)?;
        Ok(host_path)
    }

    fn ensure_directory_tree(&self, relative_dir: &Path, virtual_path: &str) -> VfsResult<()> {
        if relative_dir == Path::new(".") {
            return Ok(());
        }

        let mut prefix = PathBuf::new();
        for component in relative_dir.components() {
            match component {
                Component::Normal(segment) => prefix.push(segment),
                Component::CurDir => continue,
                _ => {
                    return Err(VfsError::new(
                        "EINVAL",
                        format!("invalid host_dir component in {virtual_path}"),
                    ));
                }
            }

            if self.open_directory_beneath(&prefix).is_ok() {
                continue;
            }

            let parent = match prefix.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => parent,
                _ => Path::new("."),
            };
            let parent_dir = self.open_directory_beneath(parent)?;
            let name = prefix.file_name().ok_or_else(|| {
                VfsError::new("EINVAL", format!("invalid directory path: {virtual_path}"))
            })?;
            match mkdirat(
                Some(parent_dir.as_raw_fd()),
                name,
                Mode::from_bits_truncate(0o755),
            ) {
                Ok(()) => {}
                Err(Errno::EEXIST) => {}
                Err(error) => {
                    return Err(io_error_to_vfs("mkdir", virtual_path, nix_to_io(error)));
                }
            }
        }

        Ok(())
    }

    fn split_parent(
        &self,
        path: &str,
        create_parent_dirs: bool,
    ) -> VfsResult<(AnchoredFd, PathBuf, std::ffi::OsString, String)> {
        let (normalized, relative) = self.relative_virtual_path(path);
        let name = relative.file_name().ok_or_else(|| {
            VfsError::new(
                "EINVAL",
                format!("path does not reference an entry: {normalized}"),
            )
        })?;
        let parent = match relative.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
            _ => PathBuf::from("."),
        };
        if create_parent_dirs {
            self.ensure_directory_tree(&parent, &normalized)?;
        }
        let parent_dir = self.open_directory_beneath(&parent)?;
        Ok((parent_dir, parent, name.to_os_string(), normalized))
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
            blocks: metadata.blocks(),
            dev: metadata.dev(),
            rdev: metadata.rdev(),
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

    fn stat_from_file_stat(stat: nix::sys::stat::FileStat) -> VirtualStat {
        let file_type = SFlag::from_bits_truncate(stat.st_mode);
        let atime_ms =
            stat.st_atime.max(0) as u64 * 1_000 + (stat.st_atime_nsec.max(0) as u64 / 1_000_000);
        let mtime_ms =
            stat.st_mtime.max(0) as u64 * 1_000 + (stat.st_mtime_nsec.max(0) as u64 / 1_000_000);
        let ctime_ms =
            stat.st_ctime.max(0) as u64 * 1_000 + (stat.st_ctime_nsec.max(0) as u64 / 1_000_000);

        VirtualStat {
            mode: stat.st_mode,
            size: stat.st_size as u64,
            blocks: stat.st_blocks as u64,
            dev: stat.st_dev,
            rdev: stat.st_rdev,
            is_directory: file_type == SFlag::S_IFDIR,
            is_symbolic_link: file_type == SFlag::S_IFLNK,
            atime_ms,
            mtime_ms,
            ctime_ms,
            birthtime_ms: ctime_ms,
            ino: stat.st_ino,
            nlink: stat.st_nlink,
            uid: stat.st_uid,
            gid: stat.st_gid,
        }
    }
}

impl VirtualFileSystem for HostDirFilesystem {
    fn read_file(&mut self, path: &str) -> VfsResult<Vec<u8>> {
        let (_, relative) = self.relative_virtual_path(path);
        let handle = self.open_beneath(&relative, OFlag::O_RDONLY, Mode::empty())?;
        let mut file =
            File::open(handle.proc_path()).map_err(|error| io_error_to_vfs("open", path, error))?;
        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer)
            .map_err(|error| io_error_to_vfs("open", path, error))?;
        Ok(buffer)
    }

    fn read_dir(&mut self, path: &str) -> VfsResult<Vec<String>> {
        let (_, relative) = self.relative_virtual_path(path);
        let directory = self.open_directory_beneath(&relative)?;
        let mut entries = fs::read_dir(directory.proc_path())
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
        let (_, relative) = self.relative_virtual_path(path);
        let directory = self.open_directory_beneath(&relative)?;
        let mut entries = fs::read_dir(directory.proc_path())
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
        let (_, relative) = self.relative_virtual_path(path);
        if let Some(parent) = relative.parent() {
            self.ensure_directory_tree(parent, path)?;
        }
        let handle = self.open_beneath(
            &relative,
            OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_TRUNC,
            Mode::from_bits_truncate(0o644),
        )?;
        let mut file = File::options()
            .write(true)
            .open(handle.proc_path())
            .map_err(|error| io_error_to_vfs("write", path, error))?;
        file.write_all(&content.into())
            .map_err(|error| io_error_to_vfs("write", path, error))
    }

    fn create_dir(&mut self, path: &str) -> VfsResult<()> {
        let (parent_dir, _, name, normalized) = self.split_parent(path, false)?;
        mkdirat(
            Some(parent_dir.as_raw_fd()),
            name.as_os_str(),
            Mode::from_bits_truncate(0o755),
        )
        .map_err(|error| io_error_to_vfs("mkdir", &normalized, nix_to_io(error)))
    }

    fn mkdir(&mut self, path: &str, recursive: bool) -> VfsResult<()> {
        if recursive {
            let (normalized, relative) = self.relative_virtual_path(path);
            self.ensure_directory_tree(&relative, &normalized)
        } else {
            self.create_dir(path)
        }
    }

    fn exists(&self, path: &str) -> bool {
        let (_, relative) = self.relative_virtual_path(path);
        self.open_beneath(&relative, OFlag::O_PATH, Mode::empty())
            .is_ok()
    }

    fn stat(&mut self, path: &str) -> VfsResult<VirtualStat> {
        let (_, relative) = self.relative_virtual_path(path);
        let handle = self.open_beneath(&relative, OFlag::O_PATH, Mode::empty())?;
        fs::metadata(handle.proc_path())
            .map(Self::stat_from_metadata)
            .map_err(|error| io_error_to_vfs("stat", path, error))
    }

    fn remove_file(&mut self, path: &str) -> VfsResult<()> {
        let (parent_dir, _, name, normalized) = self.split_parent(path, false)?;
        unlinkat(
            Some(parent_dir.as_raw_fd()),
            name.as_os_str(),
            UnlinkatFlags::NoRemoveDir,
        )
        .map_err(|error| io_error_to_vfs("unlink", &normalized, nix_to_io(error)))
    }

    fn remove_dir(&mut self, path: &str) -> VfsResult<()> {
        let (parent_dir, _, name, normalized) = self.split_parent(path, false)?;
        unlinkat(
            Some(parent_dir.as_raw_fd()),
            name.as_os_str(),
            UnlinkatFlags::RemoveDir,
        )
        .map_err(|error| io_error_to_vfs("rmdir", &normalized, nix_to_io(error)))
    }

    fn rename(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        let (old_parent_dir, _, old_name, old_normalized) = self.split_parent(old_path, false)?;
        let (new_parent_dir, _, new_name, _) = self.split_parent(new_path, true)?;
        renameat(
            Some(old_parent_dir.as_raw_fd()),
            old_name.as_os_str(),
            Some(new_parent_dir.as_raw_fd()),
            new_name.as_os_str(),
        )
        .map_err(|error| io_error_to_vfs("rename", &old_normalized, nix_to_io(error)))
    }

    fn realpath(&self, path: &str) -> VfsResult<String> {
        let (_, relative) = self.relative_virtual_path(path);
        let file = self.open_beneath(&relative, OFlag::O_PATH, Mode::empty())?;
        let resolved = self.host_path_for_fd(&file, path)?;
        self.host_to_virtual_path(&resolved, path)
    }

    fn symlink(&mut self, target: &str, link_path: &str) -> VfsResult<()> {
        let (parent_dir, _, name, normalized) = self.split_parent(link_path, true)?;
        let parent_host_path = self.host_path_for_fd(&parent_dir, &normalized)?;
        let host_link_path = parent_host_path.join(&name);

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
        symlinkat(
            &relative_target,
            Some(parent_dir.as_raw_fd()),
            name.as_os_str(),
        )
        .map_err(|error| io_error_to_vfs("symlink", link_path, nix_to_io(error)))
    }

    fn read_link(&self, path: &str) -> VfsResult<String> {
        let (parent_dir, _, name, normalized) = self.split_parent(path, false)?;
        let parent_host_path = self.host_path_for_fd(&parent_dir, &normalized)?;
        let host_link_path = parent_host_path.join(&name);
        let link_target = readlinkat(Some(parent_dir.as_raw_fd()), name.as_os_str())
            .map_err(|error| io_error_to_vfs("readlink", path, nix_to_io(error)))?;
        let link_target_path = PathBuf::from(&link_target);
        let resolved_target = if link_target_path.is_absolute() {
            lexical_normalize_path(&link_target_path)
        } else {
            lexical_normalize_path(
                &host_link_path
                    .parent()
                    .unwrap_or(self.host_root.as_path())
                    .join(link_target_path),
            )
        };
        self.host_to_virtual_path(&resolved_target, path)
    }

    fn lstat(&self, path: &str) -> VfsResult<VirtualStat> {
        let (parent_dir, _, name, normalized) = self.split_parent(path, false)?;
        fstatat(
            Some(parent_dir.as_raw_fd()),
            name.as_os_str(),
            AtFlags::AT_SYMLINK_NOFOLLOW,
        )
        .map(Self::stat_from_file_stat)
        .map_err(|error| io_error_to_vfs("lstat", &normalized, nix_to_io(error)))
    }

    fn link(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        let (old_parent_dir, _, old_name, _) = self.split_parent(old_path, false)?;
        let (new_parent_dir, _, new_name, new_normalized) = self.split_parent(new_path, true)?;
        linkat(
            Some(old_parent_dir.as_raw_fd()),
            old_name.as_os_str(),
            Some(new_parent_dir.as_raw_fd()),
            new_name.as_os_str(),
            AtFlags::empty(),
        )
        .map_err(|error| io_error_to_vfs("link", &new_normalized, nix_to_io(error)))
    }

    fn chmod(&mut self, path: &str, mode: u32) -> VfsResult<()> {
        let (_, relative) = self.relative_virtual_path(path);
        fchmodat(
            Some(self.host_root_dir.as_raw_fd()),
            &relative,
            Mode::from_bits_truncate(mode),
            FchmodatFlags::FollowSymlink,
        )
        .map_err(|error| io_error_to_vfs("chmod", path, nix_to_io(error)))
    }

    fn chown(&mut self, path: &str, uid: u32, gid: u32) -> VfsResult<()> {
        let (_, relative) = self.relative_virtual_path(path);
        fchownat(
            Some(self.host_root_dir.as_raw_fd()),
            &relative,
            Some(Uid::from_raw(uid)),
            Some(Gid::from_raw(gid)),
            AtFlags::empty(),
        )
        .map_err(|error| VfsError::new(error_code(&error), error.to_string()))
    }

    fn utimes(&mut self, path: &str, atime_ms: u64, mtime_ms: u64) -> VfsResult<()> {
        let (_, relative) = self.relative_virtual_path(path);
        utimensat(
            Some(self.host_root_dir.as_raw_fd()),
            &relative,
            &TimeSpec::nanoseconds((atime_ms as i64) * 1_000_000),
            &TimeSpec::nanoseconds((mtime_ms as i64) * 1_000_000),
            UtimensatFlags::FollowSymlink,
        )
        .map_err(|error| io_error_to_vfs("utimes", path, nix_to_io(error)))
    }

    fn truncate(&mut self, path: &str, length: u64) -> VfsResult<()> {
        let (_, relative) = self.relative_virtual_path(path);
        let handle = self.open_beneath(&relative, OFlag::O_WRONLY, Mode::empty())?;
        let file = File::options()
            .write(true)
            .open(handle.proc_path())
            .map_err(|error| io_error_to_vfs("truncate", path, error))?;
        file.set_len(length)
            .map_err(|error| io_error_to_vfs("truncate", path, error))
    }

    fn pread(&mut self, path: &str, offset: u64, length: usize) -> VfsResult<Vec<u8>> {
        let (_, relative) = self.relative_virtual_path(path);
        let handle = self.open_beneath(&relative, OFlag::O_RDONLY, Mode::empty())?;
        let file =
            File::open(handle.proc_path()).map_err(|error| io_error_to_vfs("open", path, error))?;
        let mut buffer = vec![0; length];
        let bytes_read = file
            .read_at(&mut buffer, offset)
            .map_err(|error| io_error_to_vfs("open", path, error))?;
        buffer.truncate(bytes_read);
        Ok(buffer)
    }
}

fn nix_to_io(error: Errno) -> io::Error {
    io::Error::from_raw_os_error(error as i32)
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
        let outside_dir = temp_dir("agent-os-host-dir-plugin-outside");
        fs::write(host_dir.join("hello.txt"), "hello from host").expect("seed host file");
        std::os::unix::fs::symlink(&outside_dir, host_dir.join("escape"))
            .expect("seed escape symlink");

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
        assert!(
            !outside_dir.join("hostname").exists(),
            "read should not materialize files outside the host mount"
        );

        let error = filesystem
            .write_file("/escape/owned.txt", b"owned".to_vec())
            .expect_err("escape symlink write should fail closed");
        assert_eq!(error.code(), "EACCES");
        assert!(
            !outside_dir.join("owned.txt").exists(),
            "write should not escape the mounted host directory"
        );

        fs::remove_dir_all(host_dir).expect("remove temp dir");
        fs::remove_dir_all(outside_dir).expect("remove outside temp dir");
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
