#![allow(unsafe_code)]

use super::vfs::{
    normalize_path, VfsError, VfsResult, VirtualDirEntry, VirtualFileSystem, VirtualStat,
    VirtualUtimeSpec,
};
use crate::package_format::{
    generated::v1::{self, TarEntryKind},
    parse_aospkg_header, validate_mount_range, AospkgHeader,
};
use memmap2::Mmap;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

const MAX_TAR_INDEX_ENTRIES: usize = 200_000;
const MAX_TAR_CACHE_ARCHIVES: usize = 64;
const MAX_TAR_SYMLINKS: usize = 40;
const MAX_TAR_REALPATH_CACHE_ENTRIES: usize = 32_768;

/// Read-only filesystem backed by the mount chunk of a `.aospkg` package.
///
/// The package container is `header + manifest + mount index + mount.tar`.
/// `TarFileSystem::open` decodes only the precomputed mount index and serves
/// reads from the uncompressed `mount.tar` chunk at `mountBase + offset`.
/// Extraction would create a duplicate host tree, thousands of physical inodes,
/// and a cleanup problem before reading the same bytes again.
///
/// File reads are an O(log n) index lookup plus a page-cache-backed memory
/// slice; metadata and directory listings come from the in-memory index. The
/// index must be pre-sorted by canonical path, and load performs a release-safe
/// adjacent-order check before any binary-search-dependent path can run. The
/// mmap is keyed by file identity and shared across VMs, so RSS follows the
/// pages actually touched rather than the full archive size. This open path is
/// on VM startup (`configure_vm`), so it must stay O(index decode): never parse
/// tar headers, read/hash the whole archive, or recover metadata from legacy
/// in-archive JSON here.
///
/// This filesystem is mounted only as a granular package-version leaf such as
/// `/opt/agentos/pkgs/<pkg>/<version>`. Managed commands and `current` aliases
/// are separate symlink leaf mounts, while parent directories stay writable
/// overlay directories so user-installed files can coexist with managed ones.
#[derive(Clone)]
pub struct TarFileSystem {
    archive: Arc<CachedTarArchive>,
    root: String,
}

impl TarFileSystem {
    pub fn open(path: impl AsRef<Path>) -> VfsResult<Self> {
        Self::open_at(path, "/")
    }

    pub fn open_at(path: impl AsRef<Path>, root: &str) -> VfsResult<Self> {
        let path = path.as_ref().to_path_buf();
        let archive = cached_archive(path)?;
        let root = normalize_path(root);
        let node = archive.node(&root)?;
        if !matches!(node.kind, TarNodeKind::Directory) {
            return Err(VfsError::new(
                "ENOTDIR",
                format!("tar mount root is not a directory: {root}"),
            ));
        }
        Ok(Self { archive, root })
    }

    #[doc(hidden)]
    pub fn archive_ptr(&self) -> usize {
        Arc::as_ptr(&self.archive) as usize
    }

    pub fn source_path(&self) -> &Path {
        &self.archive.path
    }

    pub fn archive_root(&self) -> &str {
        &self.root
    }

    fn to_archive_path(&self, path: &str) -> String {
        let normalized = normalize_path(path);
        if self.root == "/" {
            normalized
        } else if normalized == "/" {
            self.root.clone()
        } else {
            normalize_path(&format!(
                "{}/{}",
                self.root,
                normalized.trim_start_matches('/')
            ))
        }
    }

    fn to_guest_path(&self, archive_path: &str) -> VfsResult<String> {
        if self.root == "/" {
            return Ok(archive_path.to_owned());
        }
        if archive_path == self.root {
            return Ok(String::from("/"));
        }
        let prefix = format!("{}/", self.root.trim_end_matches('/'));
        let suffix = archive_path.strip_prefix(&prefix).ok_or_else(|| {
            VfsError::new(
                "EXDEV",
                format!("tar symlink resolved outside mounted subtree: {archive_path}"),
            )
        })?;
        Ok(format!("/{suffix}"))
    }

    fn ensure_within_root(&self, archive_path: &str) -> VfsResult<()> {
        if self.root == "/" || archive_path == self.root {
            return Ok(());
        }
        let prefix = format!("{}/", self.root.trim_end_matches('/'));
        if archive_path.starts_with(&prefix) {
            Ok(())
        } else {
            Err(VfsError::new(
                "EXDEV",
                format!("tar path resolved outside mounted subtree: {archive_path}"),
            ))
        }
    }

    fn resolve_path(&self, path: &str, follow_final_symlink: bool) -> VfsResult<String> {
        let normalized = self.to_archive_path(path);
        if normalized == "/" {
            return Ok(normalized);
        }
        if let Some(result) = self
            .archive
            .realpath_cache
            .lock()
            .expect("tar realpath cache poisoned")
            .get(&(normalized.clone(), follow_final_symlink))
            .cloned()
        {
            let resolved = result?;
            self.ensure_within_root(&resolved)?;
            return Ok(resolved);
        }

        let result = self.resolve_archive_path_uncached(&normalized, path, follow_final_symlink);
        let mut cache = self
            .archive
            .realpath_cache
            .lock()
            .expect("tar realpath cache poisoned");
        if cache.len() >= MAX_TAR_REALPATH_CACHE_ENTRIES {
            cache.clear();
        }
        cache.insert((normalized, follow_final_symlink), result.clone());
        drop(cache);

        let resolved = result?;
        self.ensure_within_root(&resolved)?;
        Ok(resolved)
    }

    fn resolve_archive_path_uncached(
        &self,
        normalized: &str,
        path: &str,
        follow_final_symlink: bool,
    ) -> VfsResult<String> {
        let mut pending = path_components(normalized);
        let mut current = String::from("/");
        let mut followed = 0usize;

        while let Some(component) = pending.pop_front() {
            let candidate = join_path(&current, &component);
            let node = self.archive.node(&candidate)?;
            let should_follow = follow_final_symlink || !pending.is_empty();

            if should_follow {
                if let TarNodeKind::Symlink { target } = &node.kind {
                    followed += 1;
                    if followed > MAX_TAR_SYMLINKS {
                        return Err(VfsError::new(
                            "ELOOP",
                            format!("too many levels of symbolic links, '{path}'"),
                        ));
                    }
                    let target_path = if target.starts_with('/') {
                        normalize_path(target)
                    } else {
                        normalize_path(&format!("{}/{}", parent_path(&candidate), target))
                    };
                    ensure_archive_path(&target_path)?;
                    let mut target_components = path_components(&target_path);
                    target_components.extend(pending);
                    pending = target_components;
                    current = String::from("/");
                    continue;
                }
            }

            if !pending.is_empty() && !matches!(node.kind, TarNodeKind::Directory) {
                return Err(VfsError::new(
                    "ENOTDIR",
                    format!("not a directory, realpath '{candidate}'"),
                ));
            }

            current = candidate;
        }

        Ok(current)
    }

    fn readonly_error(op: &str, path: &str) -> VfsError {
        VfsError::new("EROFS", format!("read-only tar filesystem, {op} '{path}'"))
    }
}

impl VirtualFileSystem for TarFileSystem {
    fn read_file(&mut self, path: &str) -> VfsResult<Vec<u8>> {
        let resolved = self.resolve_path(path, true)?;
        let node = self.archive.node(&resolved)?;
        let TarNodeKind::File { offset, size } = node.kind else {
            return Err(if matches!(node.kind, TarNodeKind::Directory) {
                VfsError::new(
                    "EISDIR",
                    format!("illegal operation on a directory, read '{path}'"),
                )
            } else {
                VfsError::new("EINVAL", format!("not a regular file, read '{path}'"))
            });
        };
        self.archive.validate_backing_file()?;
        let range = validate_mount_range(&self.archive.container, offset, size)?;
        Ok(self.archive.mmap[range].to_vec())
    }

    fn read_dir(&mut self, path: &str) -> VfsResult<Vec<String>> {
        Ok(self
            .read_dir_with_types(path)?
            .into_iter()
            .map(|entry| entry.name)
            .collect())
    }

    fn read_dir_with_types(&mut self, path: &str) -> VfsResult<Vec<VirtualDirEntry>> {
        let resolved = self.resolve_path(path, true)?;
        let node = self.archive.node(&resolved)?;
        if !matches!(node.kind, TarNodeKind::Directory) {
            return Err(VfsError::new(
                "ENOTDIR",
                format!("not a directory, readdir '{path}'"),
            ));
        }

        let children = self
            .archive
            .children
            .get(&resolved)
            .cloned()
            .unwrap_or_default();
        Ok(children
            .into_iter()
            .filter_map(|name| {
                let child_path = join_path(&resolved, &name);
                self.archive
                    .nodes
                    .get(&child_path)
                    .map(|child| VirtualDirEntry {
                        name,
                        is_directory: matches!(child.kind, TarNodeKind::Directory),
                        is_symbolic_link: matches!(child.kind, TarNodeKind::Symlink { .. }),
                    })
            })
            .collect())
    }

    fn write_file(&mut self, path: &str, _content: impl Into<Vec<u8>>) -> VfsResult<()> {
        Err(Self::readonly_error("write", path))
    }

    fn create_dir(&mut self, path: &str) -> VfsResult<()> {
        Err(Self::readonly_error("mkdir", path))
    }

    fn mkdir(&mut self, path: &str, _recursive: bool) -> VfsResult<()> {
        Err(Self::readonly_error("mkdir", path))
    }

    fn exists(&self, path: &str) -> bool {
        self.resolve_path(path, true)
            .map(|resolved| self.archive.nodes.contains_key(&resolved))
            .unwrap_or(false)
    }

    fn stat(&mut self, path: &str) -> VfsResult<VirtualStat> {
        let resolved = self.resolve_path(path, true)?;
        Ok(self.archive.node(&resolved)?.stat())
    }

    fn remove_file(&mut self, path: &str) -> VfsResult<()> {
        Err(Self::readonly_error("unlink", path))
    }

    fn remove_dir(&mut self, path: &str) -> VfsResult<()> {
        Err(Self::readonly_error("rmdir", path))
    }

    fn rename(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        Err(VfsError::new(
            "EROFS",
            format!("read-only tar filesystem, rename '{old_path}' to '{new_path}'"),
        ))
    }

    fn realpath(&self, path: &str) -> VfsResult<String> {
        let resolved = self.resolve_path(path, true)?;
        self.to_guest_path(&resolved)
    }

    fn symlink(&mut self, target: &str, link_path: &str) -> VfsResult<()> {
        Err(VfsError::new(
            "EROFS",
            format!("read-only tar filesystem, symlink '{link_path}' -> '{target}'"),
        ))
    }

    fn read_link(&self, path: &str) -> VfsResult<String> {
        let normalized = self.resolve_path(path, false)?;
        match &self.archive.node(&normalized)?.kind {
            TarNodeKind::Symlink { target } => Ok(target.clone()),
            _ => Err(VfsError::new(
                "EINVAL",
                format!("not a symlink, readlink '{path}'"),
            )),
        }
    }

    fn lstat(&self, path: &str) -> VfsResult<VirtualStat> {
        let normalized = self.resolve_path(path, false)?;
        Ok(self.archive.node(&normalized)?.stat())
    }

    fn link(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        Err(VfsError::new(
            "EROFS",
            format!("read-only tar filesystem, link '{old_path}' to '{new_path}'"),
        ))
    }

    fn chmod(&mut self, path: &str, _mode: u32) -> VfsResult<()> {
        Err(Self::readonly_error("chmod", path))
    }

    fn chown(&mut self, path: &str, _uid: u32, _gid: u32) -> VfsResult<()> {
        Err(Self::readonly_error("chown", path))
    }

    fn utimes(&mut self, path: &str, _atime_ms: u64, _mtime_ms: u64) -> VfsResult<()> {
        Err(Self::readonly_error("utimes", path))
    }

    fn utimes_spec(
        &mut self,
        path: &str,
        _atime: VirtualUtimeSpec,
        _mtime: VirtualUtimeSpec,
        _follow_symlinks: bool,
    ) -> VfsResult<()> {
        Err(Self::readonly_error("utimes", path))
    }

    fn truncate(&mut self, path: &str, _length: u64) -> VfsResult<()> {
        Err(Self::readonly_error("truncate", path))
    }

    fn pread(&mut self, path: &str, offset: u64, length: usize) -> VfsResult<Vec<u8>> {
        let resolved = self.resolve_path(path, true)?;
        let node = self.archive.node(&resolved)?;
        let TarNodeKind::File {
            offset: file_offset,
            size,
        } = node.kind
        else {
            return Err(if matches!(node.kind, TarNodeKind::Directory) {
                VfsError::new(
                    "EISDIR",
                    format!("illegal operation on a directory, pread '{path}'"),
                )
            } else {
                VfsError::new("EINVAL", format!("not a regular file, pread '{path}'"))
            });
        };
        if offset >= size {
            return Ok(Vec::new());
        }
        let readable = (size - offset).min(length as u64);
        self.archive.validate_backing_file()?;
        let range = validate_mount_range(
            &self.archive.container,
            file_offset
                .checked_add(offset)
                .ok_or_else(|| VfsError::new("EOVERFLOW", "pread offset overflows u64"))?,
            readable,
        )?;
        Ok(self.archive.mmap[range].to_vec())
    }
}

struct CachedTarArchive {
    path: PathBuf,
    file: File,
    mmap: Mmap,
    container: AospkgHeader,
    identity: FileIdentity,
    nodes: HashMap<String, TarNode>,
    children: BTreeMap<String, BTreeSet<String>>,
    realpath_cache: Mutex<HashMap<(String, bool), VfsResult<String>>>,
}

impl CachedTarArchive {
    fn node(&self, path: &str) -> VfsResult<&TarNode> {
        self.nodes
            .get(path)
            .ok_or_else(|| VfsError::new("ENOENT", format!("no such file or directory, '{path}'")))
    }

    fn validate_backing_file(&self) -> VfsResult<()> {
        let current = FileIdentity::from_file(&self.file)?;
        if current != self.identity {
            return Err(VfsError::new(
                "ESTALE",
                format!(
                    "tar archive backing file changed while mounted: {}",
                    self.path.display()
                ),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct TarNode {
    kind: TarNodeKind,
    mode: u32,
    uid: u32,
    gid: u32,
    mtime_ms: u64,
    ino: u64,
    dev: u64,
}

impl TarNode {
    fn stat(&self) -> VirtualStat {
        let size = match &self.kind {
            TarNodeKind::File { size, .. } => *size,
            TarNodeKind::Directory => 4096,
            TarNodeKind::Symlink { target } => target.len() as u64,
        };
        VirtualStat {
            mode: self.mode,
            size,
            blocks: size.div_ceil(512),
            dev: self.dev,
            rdev: 0,
            is_directory: matches!(self.kind, TarNodeKind::Directory),
            is_symbolic_link: matches!(self.kind, TarNodeKind::Symlink { .. }),
            atime_ms: self.mtime_ms,
            atime_nsec: 0,
            mtime_ms: self.mtime_ms,
            mtime_nsec: 0,
            ctime_ms: self.mtime_ms,
            ctime_nsec: 0,
            birthtime_ms: self.mtime_ms,
            ino: self.ino,
            nlink: 1,
            uid: self.uid,
            gid: self.gid,
        }
    }
}

#[derive(Debug, Clone)]
enum TarNodeKind {
    File { offset: u64, size: u64 },
    Directory,
    Symlink { target: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct FileIdentity {
    len: u64,
    dev: u64,
    ino: u64,
    mtime_nsec: i128,
    ctime_nsec: i128,
}

impl FileIdentity {
    fn from_file(file: &File) -> VfsResult<Self> {
        Self::from_metadata(file.metadata().map_err(io_to_vfs)?)
    }

    fn from_metadata(metadata: std::fs::Metadata) -> VfsResult<Self> {
        #[cfg(unix)]
        let (dev, ino, mtime_nsec, ctime_nsec) = {
            use std::os::unix::fs::MetadataExt;
            (
                metadata.dev(),
                metadata.ino(),
                unix_time_nsec(metadata.mtime(), metadata.mtime_nsec()),
                unix_time_nsec(metadata.ctime(), metadata.ctime_nsec()),
            )
        };
        #[cfg(not(unix))]
        let (dev, ino, mtime_nsec, ctime_nsec) = {
            let modified = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_nanos() as i128)
                .unwrap_or_default();
            (0, 0, modified, 0)
        };
        Ok(Self {
            len: metadata.len(),
            dev,
            ino,
            mtime_nsec,
            ctime_nsec,
        })
    }
}

#[cfg(unix)]
fn unix_time_nsec(sec: i64, nsec: i64) -> i128 {
    i128::from(sec) * 1_000_000_000 + i128::from(nsec)
}

fn cached_archive(path: PathBuf) -> VfsResult<Arc<CachedTarArchive>> {
    let file = File::open(&path).map_err(io_to_vfs)?;
    let identity = FileIdentity::from_file(&file)?;
    let cache = archive_cache();
    let mut guard = cache
        .lock()
        .map_err(|_| VfsError::new("EIO", "tar archive cache mutex poisoned"))?;

    // A cache hit means the Weak upgraded to a live archive that still holds
    // the source fd open. On Unix that fd pins the inode, so `(dev, ino)` cannot
    // be reused for a different file while the cached entry is alive. Including
    // `ctime_nsec` catches in-place rewrites even when build tools normalize or
    // preserve mtime; including nanosecond mtime avoids the old millisecond
    // collision window. The mutex covers lookup, load, and insert.
    if let Some(existing) = guard.archives.get(&identity).and_then(Weak::upgrade) {
        if existing.path == path {
            return Ok(existing);
        }
        return Err(VfsError::new(
            "EINVAL",
            format!(
                "tar identity collision or moved source: identity {identity:?} already maps to {} not {}",
                existing.path.display(),
                path.display()
            ),
        ));
    }

    guard.archives.retain(|key, weak| {
        let live = weak.strong_count() > 0;
        if !live {
            tracing::warn!(
                identity = ?key,
                "evicting unused tar archive cache entry"
            );
        }
        live
    });

    if guard.archives.len() >= MAX_TAR_CACHE_ARCHIVES {
        return Err(VfsError::new(
            "ENOMEM",
            format!(
                "tar archive cache entries exceeded: {} entries > {} entries (raise via invariant.tarArchiveCacheEntries)",
                guard.archives.len() + 1,
                MAX_TAR_CACHE_ARCHIVES
            ),
        ));
    }

    let archive = Arc::new(load_archive(path, file, identity)?);
    guard.archives.insert(identity, Arc::downgrade(&archive));
    Ok(archive)
}

fn archive_cache() -> &'static Mutex<TarArchiveCache> {
    static CACHE: OnceLock<Mutex<TarArchiveCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(TarArchiveCache::default()))
}

#[derive(Default)]
struct TarArchiveCache {
    archives: BTreeMap<FileIdentity, Weak<CachedTarArchive>>,
}

fn load_archive(path: PathBuf, file: File, identity: FileIdentity) -> VfsResult<CachedTarArchive> {
    let mmap = unsafe {
        // SAFETY: TarFileSystem is only constructed for immutable package tar
        // artifacts. We hold the opened file for the lifetime of the mmap and
        // validate size/identity before reading from mapped member ranges. A
        // caller that truncates the same inode while a VM is live violates the
        // package-store lifecycle documented on TarFileSystem.
        Mmap::map(&file)
    }
    .map_err(io_to_vfs)?;

    let container = parse_aospkg_header(&mmap)?;
    let index =
        crate::package_format::versioned::decode_mount_index(&mmap[container.index.clone()])
            .map_err(|error| {
                VfsError::new("EINVAL", format!("decode .aospkg mount index: {error}"))
            })?;
    validate_sorted_entries(&index.tar_entries)?;

    let mut nodes = HashMap::new();
    let mut children = BTreeMap::<String, BTreeSet<String>>::new();
    let dev = identity_device(&identity);
    for (next_ino, entry) in (1u64..).zip(index.tar_entries) {
        let path = entry.path;
        ensure_archive_path(&path)?;
        ensure_index_capacity(nodes.len() + 1)?;
        if matches!(entry.kind, TarEntryKind::File) {
            validate_mount_range(&container, entry.offset, entry.size)?;
        }
        let kind = match entry.kind {
            TarEntryKind::File => TarNodeKind::File {
                offset: entry.offset,
                size: entry.size,
            },
            TarEntryKind::Directory => TarNodeKind::Directory,
            TarEntryKind::Symlink => TarNodeKind::Symlink {
                target: entry.link_target.ok_or_else(|| {
                    VfsError::new("EINVAL", format!("missing linkTarget for symlink {path}"))
                })?,
            },
        };
        let mtime_ms = u64::try_from(entry.mtime)
            .map_err(|_| VfsError::new("EINVAL", format!("negative mtime for {path}")))?
            .checked_mul(1_000)
            .ok_or_else(|| VfsError::new("EOVERFLOW", format!("mtime overflows ms for {path}")))?;
        nodes.insert(
            path.clone(),
            TarNode {
                kind,
                mode: entry.mode,
                uid: entry.uid,
                gid: entry.gid,
                mtime_ms,
                ino: next_ino,
                dev,
            },
        );
        add_child(&path, &mut children);
        if matches!(
            nodes.get(&path).map(|node| &node.kind),
            Some(TarNodeKind::Directory)
        ) {
            children.entry(path).or_default();
        }
    }

    Ok(CachedTarArchive {
        path,
        file,
        mmap,
        container,
        identity,
        nodes,
        children,
        realpath_cache: Mutex::new(HashMap::new()),
    })
}

fn add_child(path: &str, children: &mut BTreeMap<String, BTreeSet<String>>) {
    if path == "/" {
        children.entry(String::from("/")).or_default();
        return;
    }
    let parent = parent_path(path);
    let name = basename(path);
    children.entry(parent).or_default().insert(name);
}

fn ensure_index_capacity(observed: usize) -> VfsResult<()> {
    if observed > MAX_TAR_INDEX_ENTRIES {
        return Err(VfsError::new(
            "ENOMEM",
            format!(
                "tar filesystem index entries exceeded: {observed} entries > {MAX_TAR_INDEX_ENTRIES} entries (raise via invariant.tarFilesystemIndexEntries)"
            ),
        ));
    }
    Ok(())
}

fn validate_sorted_entries(entries: &[v1::TarEntry]) -> VfsResult<()> {
    for pair in entries.windows(2) {
        let [previous, current] = pair else {
            continue;
        };
        if previous.path >= current.path {
            return Err(VfsError::new(
                "EINVAL",
                format!(
                    ".aospkg mount index is not sorted by canonical path: {:?} before {:?}",
                    previous.path, current.path
                ),
            ));
        }
    }
    Ok(())
}

fn ensure_archive_path(path: &str) -> VfsResult<()> {
    let normalized = normalize_path(path);
    if normalized != path {
        return Err(VfsError::new(
            "EINVAL",
            format!("path normalization mismatch in tar filesystem: {path}"),
        ));
    }
    Ok(())
}

fn path_components(path: &str) -> std::collections::VecDeque<String> {
    normalize_path(path)
        .split('/')
        .filter(|part| !part.is_empty())
        .map(String::from)
        .collect()
}

fn join_path(parent: &str, child: &str) -> String {
    if parent == "/" {
        format!("/{child}")
    } else {
        format!("{parent}/{child}")
    }
}

fn parent_path(path: &str) -> String {
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

fn basename(path: &str) -> String {
    let normalized = normalize_path(path);
    Path::new(&normalized)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from("/"))
}

fn identity_device(identity: &FileIdentity) -> u64 {
    // Guest-visible `st_dev` must be stable for repeated opens of the same
    // package and practically distinct across package files. Derive it from
    // the host file identity rather than archive bytes so VM startup never
    // reintroduces a whole-tar read/hash.
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    identity.dev.hash(&mut hasher);
    identity.ino.hash(&mut hasher);
    hasher.finish().max(1)
}

fn io_to_vfs(error: io::Error) -> VfsError {
    let code = match error.kind() {
        io::ErrorKind::NotFound => "ENOENT",
        io::ErrorKind::PermissionDenied => "EACCES",
        io::ErrorKind::AlreadyExists => "EEXIST",
        io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData => "EINVAL",
        io::ErrorKind::UnexpectedEof => "EIO",
        _ => "EIO",
    };
    VfsError::new(code, error.to_string())
}
