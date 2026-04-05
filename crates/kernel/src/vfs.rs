use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

pub const S_IFREG: u32 = 0o100000;
pub const S_IFDIR: u32 = 0o040000;
pub const S_IFLNK: u32 = 0o120000;
const MEMORY_FILESYSTEM_DEVICE_ID: u64 = 1;

const DEFAULT_UID: u32 = 1000;
const DEFAULT_GID: u32 = 1000;
const DIRECTORY_SIZE: u64 = 4096;
pub const MAX_PATH_LENGTH: usize = 4096;
const MAX_SYMLINK_DEPTH: usize = 40;

pub type VfsResult<T> = Result<T, VfsError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VfsError {
    code: &'static str,
    message: String,
}

impl VfsError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn io(message: impl Into<String>) -> Self {
        Self::new("EIO", message)
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new("ENOSYS", message)
    }

    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    fn not_found(op: &'static str, path: &str) -> Self {
        Self::new(
            "ENOENT",
            format!("no such file or directory, {op} '{path}'"),
        )
    }

    fn already_exists(op: &'static str, path: &str) -> Self {
        Self::new("EEXIST", format!("file already exists, {op} '{path}'"))
    }

    fn is_directory(op: &'static str, path: &str) -> Self {
        Self::new(
            "EISDIR",
            format!("illegal operation on a directory, {op} '{path}'"),
        )
    }

    fn not_directory(op: &'static str, path: &str) -> Self {
        Self::new("ENOTDIR", format!("not a directory, {op} '{path}'"))
    }

    fn path_too_long(path: &str) -> Self {
        Self::new("ENAMETOOLONG", format!("file name too long: {path}"))
    }

    fn not_empty(path: &str) -> Self {
        Self::new("ENOTEMPTY", format!("directory not empty, rmdir '{path}'"))
    }

    pub(crate) fn permission_denied(op: &'static str, path: &str) -> Self {
        Self::new("EPERM", format!("operation not permitted, {op} '{path}'"))
    }

    pub fn access_denied(op: &'static str, path: &str, reason: Option<&str>) -> Self {
        let message = match reason {
            Some(reason) => format!("permission denied, {op} '{path}': {reason}"),
            None => format!("permission denied, {op} '{path}'"),
        };

        Self::new("EACCES", message)
    }

    fn symlink_loop(path: &str) -> Self {
        Self::new(
            "ELOOP",
            format!("too many levels of symbolic links, '{path}'"),
        )
    }

    fn invalid_input(message: impl Into<String>) -> Self {
        Self::new("EINVAL", message)
    }

    fn invalid_utf8(path: &str) -> Self {
        Self::new("EINVAL", format!("file contains invalid UTF-8, '{path}'"))
    }
}

impl fmt::Display for VfsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl Error for VfsError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    File,
    Directory,
    SymbolicLink,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualDirEntry {
    pub name: String,
    pub is_directory: bool,
    pub is_symbolic_link: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualStat {
    pub mode: u32,
    pub size: u64,
    pub blocks: u64,
    pub dev: u64,
    pub rdev: u64,
    pub is_directory: bool,
    pub is_symbolic_link: bool,
    pub atime_ms: u64,
    pub mtime_ms: u64,
    pub ctime_ms: u64,
    pub birthtime_ms: u64,
    pub ino: u64,
    pub nlink: u64,
    pub uid: u32,
    pub gid: u32,
}

pub trait VirtualFileSystem {
    fn read_file(&mut self, path: &str) -> VfsResult<Vec<u8>>;
    fn read_text_file(&mut self, path: &str) -> VfsResult<String> {
        String::from_utf8(self.read_file(path)?).map_err(|_| VfsError::invalid_utf8(path))
    }
    fn read_dir(&mut self, path: &str) -> VfsResult<Vec<String>>;
    fn read_dir_limited(&mut self, path: &str, max_entries: usize) -> VfsResult<Vec<String>> {
        let entries = self.read_dir(path)?;
        if entries.len() > max_entries {
            return Err(VfsError::new(
                "ENOMEM",
                format!(
                    "directory listing for '{path}' exceeds configured limit of {max_entries} entries"
                ),
            ));
        }
        Ok(entries)
    }
    fn read_dir_with_types(&mut self, path: &str) -> VfsResult<Vec<VirtualDirEntry>>;
    fn write_file(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<()>;
    fn create_file_exclusive(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<()> {
        let content = content.into();
        if self.exists(path) {
            return Err(VfsError::already_exists("open", path));
        }
        self.write_file(path, content)
    }
    fn append_file(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<u64> {
        let content = content.into();
        let mut existing = self.read_file(path)?;
        existing.extend_from_slice(&content);
        let new_len = existing.len() as u64;
        self.write_file(path, existing)?;
        Ok(new_len)
    }
    fn create_dir(&mut self, path: &str) -> VfsResult<()>;
    fn mkdir(&mut self, path: &str, recursive: bool) -> VfsResult<()>;
    fn exists(&self, path: &str) -> bool;
    fn stat(&mut self, path: &str) -> VfsResult<VirtualStat>;
    fn remove_file(&mut self, path: &str) -> VfsResult<()>;
    fn remove_dir(&mut self, path: &str) -> VfsResult<()>;
    fn rename(&mut self, old_path: &str, new_path: &str) -> VfsResult<()>;
    fn realpath(&self, path: &str) -> VfsResult<String>;
    fn symlink(&mut self, target: &str, link_path: &str) -> VfsResult<()>;
    fn read_link(&self, path: &str) -> VfsResult<String>;
    fn lstat(&self, path: &str) -> VfsResult<VirtualStat>;
    fn link(&mut self, old_path: &str, new_path: &str) -> VfsResult<()>;
    fn chmod(&mut self, path: &str, mode: u32) -> VfsResult<()>;
    fn chown(&mut self, path: &str, uid: u32, gid: u32) -> VfsResult<()>;
    fn utimes(&mut self, path: &str, atime_ms: u64, mtime_ms: u64) -> VfsResult<()>;
    fn truncate(&mut self, path: &str, length: u64) -> VfsResult<()>;
    fn pread(&mut self, path: &str, offset: u64, length: usize) -> VfsResult<Vec<u8>>;
    fn pwrite(&mut self, path: &str, content: impl Into<Vec<u8>>, offset: u64) -> VfsResult<()> {
        let content = content.into();
        let mut existing = self.read_file(path)?;
        let start = offset as usize;
        if start > existing.len() {
            existing.resize(start, 0);
        }
        let end = start.saturating_add(content.len());
        if end > existing.len() {
            existing.resize(end, 0);
        }
        existing[start..end].copy_from_slice(&content);
        self.write_file(path, existing)
    }
}

#[derive(Debug, Clone)]
struct Metadata {
    mode: u32,
    uid: u32,
    gid: u32,
    nlink: u64,
    ino: u64,
    atime_ms: u64,
    mtime_ms: u64,
    ctime_ms: u64,
    birthtime_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryFileSystemSnapshotMetadata {
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub nlink: u64,
    pub ino: u64,
    pub atime_ms: u64,
    pub mtime_ms: u64,
    pub ctime_ms: u64,
    pub birthtime_ms: u64,
}

#[derive(Debug, Clone)]
enum InodeKind {
    File { data: Vec<u8> },
    Directory,
    SymbolicLink { target: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryFileSystemSnapshotInodeKind {
    File { data: Vec<u8> },
    Directory,
    SymbolicLink { target: String },
}

#[derive(Debug, Clone)]
struct Inode {
    metadata: Metadata,
    kind: InodeKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryFileSystemSnapshotInode {
    pub metadata: MemoryFileSystemSnapshotMetadata,
    pub kind: MemoryFileSystemSnapshotInodeKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryFileSystemSnapshot {
    pub path_index: BTreeMap<String, u64>,
    pub inodes: BTreeMap<u64, MemoryFileSystemSnapshotInode>,
    pub next_ino: u64,
}

#[derive(Debug)]
pub struct MemoryFileSystem {
    path_index: BTreeMap<String, u64>,
    inodes: BTreeMap<u64, Inode>,
    next_ino: u64,
}

impl MemoryFileSystem {
    pub fn new() -> Self {
        let mut filesystem = Self {
            path_index: BTreeMap::new(),
            inodes: BTreeMap::new(),
            next_ino: 1,
        };

        let root_ino = filesystem.allocate_inode(InodeKind::Directory, S_IFDIR | 0o755);
        filesystem.path_index.insert(String::from("/"), root_ino);
        filesystem
    }

    fn allocate_inode(&mut self, kind: InodeKind, mode: u32) -> u64 {
        let ino = self.next_ino;
        self.next_ino += 1;
        let now = now_ms();
        let nlink = if matches!(kind, InodeKind::Directory) {
            2
        } else {
            1
        };
        self.inodes.insert(
            ino,
            Inode {
                metadata: Metadata {
                    mode,
                    uid: DEFAULT_UID,
                    gid: DEFAULT_GID,
                    nlink,
                    ino,
                    atime_ms: now,
                    mtime_ms: now,
                    ctime_ms: now,
                    birthtime_ms: now,
                },
                kind,
            },
        );
        ino
    }

    pub fn symlink_with_metadata(
        &mut self,
        target: &str,
        link_path: &str,
        mode: u32,
        uid: u32,
        gid: u32,
    ) -> VfsResult<()> {
        let normalized = self.resolve_exact_path(link_path)?;
        if self.path_index.contains_key(&normalized) {
            return Err(VfsError::already_exists("symlink", link_path));
        }

        self.assert_directory_path(&dirname(&normalized), "symlink")?;
        let ino = self.allocate_inode(
            InodeKind::SymbolicLink {
                target: String::from(target),
            },
            if mode & 0o170000 == 0 {
                S_IFLNK | (mode & 0o7777)
            } else {
                mode
            },
        );
        let inode = self
            .inodes
            .get_mut(&ino)
            .expect("allocated inode should exist");
        inode.metadata.uid = uid;
        inode.metadata.gid = gid;
        self.path_index.insert(normalized, ino);
        Ok(())
    }

    fn resolve_path_with_options(
        &self,
        path: &str,
        follow_final_symlink: bool,
        depth: usize,
    ) -> VfsResult<String> {
        validate_path(path)?;
        if depth > MAX_SYMLINK_DEPTH {
            return Err(VfsError::symlink_loop(path));
        }

        let normalized = normalize_path(path);
        if normalized == "/" {
            return Ok(normalized);
        }

        let components: Vec<&str> = normalized
            .split('/')
            .filter(|part| !part.is_empty())
            .collect();
        let mut current = String::from("/");

        for (index, component) in components.iter().enumerate() {
            let candidate = if current == "/" {
                format!("/{}", component)
            } else {
                format!("{current}/{}", component)
            };
            let is_final = index + 1 == components.len();
            let should_follow = !is_final || follow_final_symlink;

            if let Some(ino) = self.path_index.get(&candidate) {
                let inode = self
                    .inodes
                    .get(ino)
                    .expect("path index should always point at a valid inode");

                if should_follow {
                    if let InodeKind::SymbolicLink { target } = &inode.kind {
                        let target_path = if target.starts_with('/') {
                            target.clone()
                        } else {
                            normalize_path(&format!("{}/{}", dirname(&candidate), target))
                        };
                        let remainder = components[index + 1..].join("/");
                        let next_path = if remainder.is_empty() {
                            target_path
                        } else {
                            normalize_path(&format!("{target_path}/{remainder}"))
                        };
                        return self.resolve_path_with_options(
                            &next_path,
                            follow_final_symlink,
                            depth + 1,
                        );
                    }
                }

                if !is_final && !matches!(inode.kind, InodeKind::Directory) {
                    return Err(VfsError::not_directory("stat", &candidate));
                }
            }

            current = candidate;
        }

        Ok(current)
    }

    fn resolve_path(&self, path: &str, depth: usize) -> VfsResult<String> {
        self.resolve_path_with_options(path, true, depth)
    }

    fn resolve_exact_path(&self, path: &str) -> VfsResult<String> {
        self.resolve_path_with_options(path, false, 0)
    }

    fn inode_id_for_existing_path(
        &self,
        path: &str,
        op: &'static str,
        follow_symlinks: bool,
    ) -> VfsResult<u64> {
        let normalized = normalize_path(path);
        let resolved = if follow_symlinks {
            self.resolve_path(&normalized, 0)?
        } else {
            self.resolve_exact_path(&normalized)?
        };
        self.path_index
            .get(&resolved)
            .copied()
            .ok_or_else(|| VfsError::not_found(op, path))
    }

    fn inode_for_existing_path(
        &self,
        path: &str,
        op: &'static str,
        follow_symlinks: bool,
    ) -> VfsResult<&Inode> {
        let ino = self.inode_id_for_existing_path(path, op, follow_symlinks)?;
        Ok(self
            .inodes
            .get(&ino)
            .expect("existing path should resolve to a live inode"))
    }

    fn inode_mut_for_existing_path(
        &mut self,
        path: &str,
        op: &'static str,
        follow_symlinks: bool,
    ) -> VfsResult<&mut Inode> {
        let ino = self.inode_id_for_existing_path(path, op, follow_symlinks)?;
        Ok(self
            .inodes
            .get_mut(&ino)
            .expect("existing path should resolve to a live inode"))
    }

    fn assert_directory_path(&self, path: &str, op: &'static str) -> VfsResult<()> {
        let inode = self.inode_for_existing_path(path, op, true)?;
        if matches!(inode.kind, InodeKind::Directory) {
            Ok(())
        } else {
            Err(VfsError::not_directory(op, path))
        }
    }

    fn remove_exact_path(&mut self, path: &str) -> VfsResult<()> {
        let normalized = self.resolve_exact_path(path)?;
        let ino = self
            .path_index
            .get(&normalized)
            .copied()
            .ok_or_else(|| VfsError::not_found("unlink", path))?;
        let inode = self
            .inodes
            .get(&ino)
            .expect("existing path should resolve to a live inode");

        if matches!(inode.kind, InodeKind::Directory) {
            return Err(VfsError::is_directory("unlink", path));
        }

        self.inodes
            .get_mut(&ino)
            .expect("inode should exist when unlinking")
            .metadata
            .ctime_ms = now_ms();
        self.path_index.remove(&normalized);
        self.decrement_link_count(ino);
        Ok(())
    }

    fn remove_existing_destination(&mut self, path: &str) -> VfsResult<()> {
        let normalized = self.resolve_exact_path(path)?;
        let Some(ino) = self.path_index.get(&normalized).copied() else {
            return Ok(());
        };

        let inode = self
            .inodes
            .get(&ino)
            .expect("existing path should resolve to a live inode");

        if matches!(inode.kind, InodeKind::Directory) {
            let prefix = format!("{normalized}/");
            if self
                .path_index
                .keys()
                .any(|candidate| candidate.starts_with(&prefix))
            {
                return Err(VfsError::not_empty(path));
            }
        }

        self.inodes
            .get_mut(&ino)
            .expect("inode should exist when removing destination")
            .metadata
            .ctime_ms = now_ms();
        self.path_index.remove(&normalized);
        self.decrement_link_count(ino);
        Ok(())
    }

    fn decrement_link_count(&mut self, ino: u64) {
        let should_remove = {
            let inode = self
                .inodes
                .get_mut(&ino)
                .expect("inode should exist when decrementing link count");
            inode.metadata.nlink = inode.metadata.nlink.saturating_sub(1);
            inode.metadata.nlink == 0
        };

        if should_remove {
            self.inodes.remove(&ino);
        }
    }

    fn build_stat(&self, inode: &Inode) -> VirtualStat {
        let size = match &inode.kind {
            InodeKind::File { data } => data.len() as u64,
            InodeKind::Directory => DIRECTORY_SIZE,
            InodeKind::SymbolicLink { target } => target.len() as u64,
        };

        VirtualStat {
            mode: inode.metadata.mode,
            size,
            blocks: block_count_for_size(size),
            dev: MEMORY_FILESYSTEM_DEVICE_ID,
            rdev: 0,
            is_directory: matches!(inode.kind, InodeKind::Directory),
            is_symbolic_link: matches!(inode.kind, InodeKind::SymbolicLink { .. }),
            atime_ms: inode.metadata.atime_ms,
            mtime_ms: inode.metadata.mtime_ms,
            ctime_ms: inode.metadata.ctime_ms,
            birthtime_ms: inode.metadata.birthtime_ms,
            ino: inode.metadata.ino,
            nlink: inode.metadata.nlink,
            uid: inode.metadata.uid,
            gid: inode.metadata.gid,
        }
    }

    pub fn snapshot(&self) -> MemoryFileSystemSnapshot {
        MemoryFileSystemSnapshot {
            path_index: self.path_index.clone(),
            inodes: self
                .inodes
                .iter()
                .map(|(ino, inode)| {
                    (
                        *ino,
                        MemoryFileSystemSnapshotInode {
                            metadata: MemoryFileSystemSnapshotMetadata {
                                mode: inode.metadata.mode,
                                uid: inode.metadata.uid,
                                gid: inode.metadata.gid,
                                nlink: inode.metadata.nlink,
                                ino: inode.metadata.ino,
                                atime_ms: inode.metadata.atime_ms,
                                mtime_ms: inode.metadata.mtime_ms,
                                ctime_ms: inode.metadata.ctime_ms,
                                birthtime_ms: inode.metadata.birthtime_ms,
                            },
                            kind: match &inode.kind {
                                InodeKind::File { data } => {
                                    MemoryFileSystemSnapshotInodeKind::File { data: data.clone() }
                                }
                                InodeKind::Directory => {
                                    MemoryFileSystemSnapshotInodeKind::Directory
                                }
                                InodeKind::SymbolicLink { target } => {
                                    MemoryFileSystemSnapshotInodeKind::SymbolicLink {
                                        target: target.clone(),
                                    }
                                }
                            },
                        },
                    )
                })
                .collect(),
            next_ino: self.next_ino,
        }
    }

    pub fn from_snapshot(snapshot: MemoryFileSystemSnapshot) -> Self {
        Self {
            path_index: snapshot.path_index,
            inodes: snapshot
                .inodes
                .into_iter()
                .map(|(ino, inode)| {
                    (
                        ino,
                        Inode {
                            metadata: Metadata {
                                mode: inode.metadata.mode,
                                uid: inode.metadata.uid,
                                gid: inode.metadata.gid,
                                nlink: inode.metadata.nlink,
                                ino: inode.metadata.ino,
                                atime_ms: inode.metadata.atime_ms,
                                mtime_ms: inode.metadata.mtime_ms,
                                ctime_ms: inode.metadata.ctime_ms,
                                birthtime_ms: inode.metadata.birthtime_ms,
                            },
                            kind: match inode.kind {
                                MemoryFileSystemSnapshotInodeKind::File { data } => {
                                    InodeKind::File { data }
                                }
                                MemoryFileSystemSnapshotInodeKind::Directory => {
                                    InodeKind::Directory
                                }
                                MemoryFileSystemSnapshotInodeKind::SymbolicLink { target } => {
                                    InodeKind::SymbolicLink { target }
                                }
                            },
                        },
                    )
                })
                .collect(),
            next_ino: snapshot.next_ino,
        }
    }
}

impl VirtualFileSystem for MemoryFileSystem {
    fn read_file(&mut self, path: &str) -> VfsResult<Vec<u8>> {
        let inode = self.inode_mut_for_existing_path(path, "open", true)?;
        match &inode.kind {
            InodeKind::File { data } => {
                inode.metadata.atime_ms = now_ms();
                Ok(data.clone())
            }
            InodeKind::Directory => Err(VfsError::is_directory("open", path)),
            InodeKind::SymbolicLink { .. } => Err(VfsError::not_found("open", path)),
        }
    }

    fn read_dir(&mut self, path: &str) -> VfsResult<Vec<String>> {
        Ok(self
            .read_dir_with_types(path)?
            .into_iter()
            .map(|entry| entry.name)
            .collect())
    }

    fn read_dir_limited(&mut self, path: &str, max_entries: usize) -> VfsResult<Vec<String>> {
        self.assert_directory_path(path, "scandir")?;
        let resolved = self.resolve_path(path, 0)?;
        self.inode_mut_for_existing_path(&resolved, "scandir", false)?
            .metadata
            .atime_ms = now_ms();
        let prefix = if resolved == "/" {
            String::from("/")
        } else {
            format!("{resolved}/")
        };

        let mut entries = BTreeMap::<String, String>::new();
        for (candidate_path, _) in self.path_index.range(prefix.clone()..) {
            if !candidate_path.starts_with(&prefix) {
                break;
            }

            let rest = &candidate_path[prefix.len()..];
            if rest.is_empty() || rest.contains('/') {
                continue;
            }

            entries.insert(String::from(rest), String::from(rest));
            if entries.len() > max_entries {
                return Err(VfsError::new(
                    "ENOMEM",
                    format!(
                        "directory listing for '{path}' exceeds configured limit of {max_entries} entries"
                    ),
                ));
            }
        }

        Ok(entries.into_values().collect())
    }

    fn read_dir_with_types(&mut self, path: &str) -> VfsResult<Vec<VirtualDirEntry>> {
        self.assert_directory_path(path, "scandir")?;
        let resolved = self.resolve_path(path, 0)?;
        self.inode_mut_for_existing_path(&resolved, "scandir", false)?
            .metadata
            .atime_ms = now_ms();
        let prefix = if resolved == "/" {
            String::from("/")
        } else {
            format!("{resolved}/")
        };

        let mut entries = BTreeMap::<String, VirtualDirEntry>::new();
        for (candidate_path, ino) in self.path_index.range(prefix.clone()..) {
            if !candidate_path.starts_with(&prefix) {
                break;
            }

            let rest = &candidate_path[prefix.len()..];
            if rest.is_empty() || rest.contains('/') {
                continue;
            }

            let inode = self
                .inodes
                .get(ino)
                .expect("path index should always point at a valid inode");
            entries.insert(
                String::from(rest),
                VirtualDirEntry {
                    name: String::from(rest),
                    is_directory: matches!(inode.kind, InodeKind::Directory),
                    is_symbolic_link: matches!(inode.kind, InodeKind::SymbolicLink { .. }),
                },
            );
        }

        Ok(entries.into_values().collect())
    }

    fn write_file(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<()> {
        let normalized = self.resolve_path(path, 0)?;
        self.mkdir(&dirname(&normalized), true)?;
        let data = content.into();

        if self.path_index.contains_key(&normalized) {
            let inode = self.inode_mut_for_existing_path(&normalized, "open", false)?;
            let now = now_ms();
            match &mut inode.kind {
                InodeKind::File { data: existing } => {
                    *existing = data;
                    inode.metadata.mtime_ms = now;
                    inode.metadata.ctime_ms = now;
                    return Ok(());
                }
                InodeKind::Directory => return Err(VfsError::is_directory("open", path)),
                InodeKind::SymbolicLink { .. } => return Err(VfsError::not_found("open", path)),
            }
        }

        let ino = self.allocate_inode(InodeKind::File { data }, S_IFREG | 0o644);
        self.path_index.insert(normalized, ino);
        Ok(())
    }

    fn create_file_exclusive(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<()> {
        let normalized = self.resolve_path(path, 0)?;
        self.mkdir(&dirname(&normalized), true)?;
        if self.path_index.contains_key(&normalized) {
            return Err(VfsError::already_exists("open", path));
        }

        let ino = self.allocate_inode(
            InodeKind::File {
                data: content.into(),
            },
            S_IFREG | 0o644,
        );
        self.path_index.insert(normalized, ino);
        Ok(())
    }

    fn append_file(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<u64> {
        let normalized = self.resolve_path(path, 0)?;
        let data = content.into();
        let inode = self.inode_mut_for_existing_path(&normalized, "open", false)?;
        let now = now_ms();
        match &mut inode.kind {
            InodeKind::File { data: existing } => {
                existing.extend_from_slice(&data);
                inode.metadata.mtime_ms = now;
                inode.metadata.ctime_ms = now;
                Ok(existing.len() as u64)
            }
            InodeKind::Directory => Err(VfsError::is_directory("open", path)),
            InodeKind::SymbolicLink { .. } => Err(VfsError::not_found("open", path)),
        }
    }

    fn create_dir(&mut self, path: &str) -> VfsResult<()> {
        let normalized = self.resolve_exact_path(path)?;
        if normalized == "/" {
            return Ok(());
        }

        self.assert_directory_path(&dirname(&normalized), "mkdir")?;
        if let Some(existing) = self.path_index.get(&normalized) {
            let inode = self
                .inodes
                .get(existing)
                .expect("path index should always point at a valid inode");
            if matches!(inode.kind, InodeKind::Directory) {
                return Ok(());
            }
            return Err(VfsError::already_exists("mkdir", path));
        }

        let ino = self.allocate_inode(InodeKind::Directory, S_IFDIR | 0o755);
        self.path_index.insert(normalized, ino);
        Ok(())
    }

    fn mkdir(&mut self, path: &str, recursive: bool) -> VfsResult<()> {
        let normalized = normalize_path(path);
        if normalized == "/" {
            return Ok(());
        }

        if !recursive {
            return self.create_dir(path);
        }

        let parts: Vec<&str> = normalized
            .split('/')
            .filter(|part| !part.is_empty())
            .collect();
        let mut current = String::from("/");

        for (index, part) in parts.iter().enumerate() {
            let raw_path = if current == "/" {
                format!("/{}", part)
            } else {
                format!("{current}/{}", part)
            };
            let resolved =
                self.resolve_path_with_options(&raw_path, index + 1 != parts.len(), 0)?;

            match self.path_index.get(&resolved).copied() {
                Some(ino) => {
                    let inode = self
                        .inodes
                        .get(&ino)
                        .expect("path index should always point at a valid inode");
                    if !matches!(inode.kind, InodeKind::Directory) {
                        return Err(VfsError::not_directory("mkdir", &raw_path));
                    }
                }
                None => {
                    let ino = self.allocate_inode(InodeKind::Directory, S_IFDIR | 0o755);
                    self.path_index.insert(resolved.clone(), ino);
                }
            }

            current = resolved;
        }

        Ok(())
    }

    fn exists(&self, path: &str) -> bool {
        self.resolve_path(path, 0)
            .ok()
            .is_some_and(|resolved| self.path_index.contains_key(&resolved))
    }

    fn stat(&mut self, path: &str) -> VfsResult<VirtualStat> {
        let inode = self.inode_for_existing_path(path, "stat", true)?;
        Ok(self.build_stat(inode))
    }

    fn remove_file(&mut self, path: &str) -> VfsResult<()> {
        self.remove_exact_path(path)
    }

    fn remove_dir(&mut self, path: &str) -> VfsResult<()> {
        let normalized = self.resolve_exact_path(path)?;
        if normalized == "/" {
            return Err(VfsError::permission_denied("rmdir", path));
        }

        let ino = self
            .path_index
            .get(&normalized)
            .copied()
            .ok_or_else(|| VfsError::not_found("rmdir", path))?;
        let inode = self
            .inodes
            .get(&ino)
            .expect("path index should always point at a valid inode");
        if !matches!(inode.kind, InodeKind::Directory) {
            return Err(VfsError::not_directory("rmdir", path));
        }

        let prefix = format!("{normalized}/");
        if self
            .path_index
            .keys()
            .any(|candidate| candidate.starts_with(&prefix))
        {
            return Err(VfsError::not_empty(path));
        }

        self.path_index.remove(&normalized);
        self.decrement_link_count(ino);
        Ok(())
    }

    fn rename(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        let old_normalized = self.resolve_exact_path(old_path)?;
        let new_normalized = self.resolve_exact_path(new_path)?;

        if old_normalized == "/" {
            return Err(VfsError::permission_denied("rename", old_path));
        }

        if old_normalized == new_normalized {
            return Ok(());
        }

        self.assert_directory_path(&dirname(&new_normalized), "rename")?;

        if new_normalized.starts_with(&(old_normalized.clone() + "/")) {
            return Err(VfsError::invalid_input(format!(
                "cannot move '{}' into its own descendant '{}'",
                old_path, new_path
            )));
        }

        let ino = self
            .path_index
            .get(&old_normalized)
            .copied()
            .ok_or_else(|| VfsError::not_found("rename", old_path))?;
        let is_directory = matches!(
            self.inodes
                .get(&ino)
                .expect("path index should always point at a valid inode")
                .kind,
            InodeKind::Directory
        );

        self.remove_existing_destination(new_path)?;

        if !is_directory {
            self.path_index.remove(&old_normalized);
            self.path_index.insert(new_normalized, ino);
            self.inodes
                .get_mut(&ino)
                .expect("renamed inode should exist")
                .metadata
                .ctime_ms = now_ms();
            return Ok(());
        }

        let prefix = format!("{old_normalized}/");
        let to_move: Vec<(String, u64)> = self
            .path_index
            .iter()
            .filter(|(path, _)| **path == old_normalized || path.starts_with(&prefix))
            .map(|(path, inode_id)| (path.clone(), *inode_id))
            .collect();

        for (path, _) in &to_move {
            self.path_index.remove(path);
        }

        for (path, inode_id) in to_move {
            let relocated_path = if path == old_normalized {
                new_normalized.clone()
            } else {
                format!("{new_normalized}{}", &path[old_normalized.len()..])
            };
            self.path_index.insert(relocated_path, inode_id);
        }

        self.inodes
            .get_mut(&ino)
            .expect("renamed directory inode should exist")
            .metadata
            .ctime_ms = now_ms();

        Ok(())
    }

    fn realpath(&self, path: &str) -> VfsResult<String> {
        let resolved = self.resolve_path(path, 0)?;
        if !self.path_index.contains_key(&resolved) {
            return Err(VfsError::not_found("realpath", path));
        }
        Ok(resolved)
    }

    fn symlink(&mut self, target: &str, link_path: &str) -> VfsResult<()> {
        self.symlink_with_metadata(target, link_path, S_IFLNK | 0o777, DEFAULT_UID, DEFAULT_GID)
    }

    fn read_link(&self, path: &str) -> VfsResult<String> {
        let inode = self.inode_for_existing_path(path, "readlink", false)?;
        match &inode.kind {
            InodeKind::SymbolicLink { target } => Ok(target.clone()),
            _ => Err(VfsError::invalid_input(format!(
                "invalid argument, readlink '{path}'"
            ))),
        }
    }

    fn lstat(&self, path: &str) -> VfsResult<VirtualStat> {
        let inode = self.inode_for_existing_path(path, "lstat", false)?;
        Ok(self.build_stat(inode))
    }

    fn link(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        let ino = self.inode_id_for_existing_path(old_path, "link", true)?;
        let inode = self
            .inodes
            .get(&ino)
            .expect("path index should always point at a valid inode");
        if !matches!(inode.kind, InodeKind::File { .. }) {
            return Err(VfsError::permission_denied("link", old_path));
        }

        let normalized = self.resolve_exact_path(new_path)?;
        if self.path_index.contains_key(&normalized) {
            return Err(VfsError::already_exists("link", new_path));
        }

        self.assert_directory_path(&dirname(&normalized), "link")?;
        self.path_index.insert(normalized, ino);
        let inode = self
            .inodes
            .get_mut(&ino)
            .expect("path index should always point at a valid inode");
        inode.metadata.nlink += 1;
        inode.metadata.ctime_ms = now_ms();
        Ok(())
    }

    fn chmod(&mut self, path: &str, mode: u32) -> VfsResult<()> {
        let inode = self.inode_mut_for_existing_path(path, "chmod", true)?;
        let type_bits = if mode & 0o170000 == 0 {
            inode.metadata.mode & 0o170000
        } else {
            mode & 0o170000
        };
        inode.metadata.mode = type_bits | (mode & 0o7777);
        inode.metadata.ctime_ms = now_ms();
        Ok(())
    }

    fn chown(&mut self, path: &str, uid: u32, gid: u32) -> VfsResult<()> {
        let inode = self.inode_mut_for_existing_path(path, "chown", true)?;
        inode.metadata.uid = uid;
        inode.metadata.gid = gid;
        inode.metadata.ctime_ms = now_ms();
        Ok(())
    }

    fn utimes(&mut self, path: &str, atime_ms: u64, mtime_ms: u64) -> VfsResult<()> {
        let inode = self.inode_mut_for_existing_path(path, "utimes", true)?;
        inode.metadata.atime_ms = atime_ms;
        inode.metadata.mtime_ms = mtime_ms;
        inode.metadata.ctime_ms = now_ms();
        Ok(())
    }

    fn truncate(&mut self, path: &str, length: u64) -> VfsResult<()> {
        let inode = self.inode_mut_for_existing_path(path, "truncate", true)?;
        let now = now_ms();
        match &mut inode.kind {
            InodeKind::File { data } => {
                data.resize(length as usize, 0);
                inode.metadata.mtime_ms = now;
                inode.metadata.ctime_ms = now;
                Ok(())
            }
            InodeKind::Directory => Err(VfsError::is_directory("truncate", path)),
            InodeKind::SymbolicLink { .. } => Err(VfsError::not_found("truncate", path)),
        }
    }

    fn pread(&mut self, path: &str, offset: u64, length: usize) -> VfsResult<Vec<u8>> {
        let inode = self.inode_mut_for_existing_path(path, "open", true)?;
        match &mut inode.kind {
            InodeKind::File { data } => {
                inode.metadata.atime_ms = now_ms();
                let start = offset as usize;
                if start >= data.len() {
                    return Ok(Vec::new());
                }
                let end = start.saturating_add(length).min(data.len());
                Ok(data[start..end].to_vec())
            }
            InodeKind::Directory => Err(VfsError::is_directory("open", path)),
            InodeKind::SymbolicLink { .. } => Err(VfsError::not_found("open", path)),
        }
    }
}

impl Default for MemoryFileSystem {
    fn default() -> Self {
        Self::new()
    }
}

pub fn validate_path(path: &str) -> VfsResult<()> {
    let normalized = normalize_path(path);
    if normalized.len() > MAX_PATH_LENGTH {
        return Err(VfsError::path_too_long(path));
    }
    Ok(())
}

pub fn normalize_path(path: &str) -> String {
    if path.is_empty() {
        return String::from("/");
    }

    let candidate = if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("/{path}")
    };

    let mut resolved = Vec::new();
    for part in candidate.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                resolved.pop();
            }
            component => resolved.push(component),
        }
    }

    if resolved.is_empty() {
        String::from("/")
    } else {
        format!("/{}", resolved.join("/"))
    }
}

fn block_count_for_size(size: u64) -> u64 {
    if size == 0 {
        0
    } else {
        size.div_ceil(512)
    }
}

fn dirname(path: &str) -> String {
    let normalized = normalize_path(path);
    let Some((head, _)) = normalized.rsplit_once('/') else {
        return String::from("/");
    };

    if head.is_empty() {
        String::from("/")
    } else {
        String::from(head)
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
