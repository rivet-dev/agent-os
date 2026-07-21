use std::collections::{btree_map::Values, BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use crate::vfs::VirtualStat;

pub const MAX_FDS_PER_PROCESS: usize = 1024;

// The owned libc uses this private descriptor namespace for immutable WASI
// capability roots. A Linux program may close or replace the visible preopen
// fd while absolute-path libc operations must retain their kernel-owned root.
pub const WASI_HIDDEN_PREOPEN_FD_TAG: u32 = 0x4000_0000;
pub const WASI_HIDDEN_PREOPEN_FD_MASK: u32 = 0x3fff_ffff;

pub const O_RDONLY: u32 = 0;
pub const O_WRONLY: u32 = 1;
pub const O_RDWR: u32 = 2;
pub const O_CREAT: u32 = 0o100;
pub const O_EXCL: u32 = 0o200;
pub const O_TRUNC: u32 = 0o1000;
pub const O_APPEND: u32 = 0o2000;
pub const O_NONBLOCK: u32 = 0o4000;
pub const O_DIRECT: u32 = 0o40000;
pub const O_DIRECTORY: u32 = 0o200000;
pub const O_NOFOLLOW: u32 = 0o400000;
pub const F_DUPFD: u32 = 0;
pub const F_GETFD: u32 = 1;
pub const F_SETFD: u32 = 2;
pub const F_GETFL: u32 = 3;
pub const F_SETFL: u32 = 4;
pub const FD_CLOEXEC: u32 = 1;
pub const LOCK_SH: u32 = 1;
pub const LOCK_EX: u32 = 2;
pub const LOCK_NB: u32 = 4;
pub const LOCK_UN: u32 = 8;
const DEFAULT_MAX_RECORD_LOCKS: usize = 4096;

pub const FILETYPE_UNKNOWN: u8 = 0;
pub const FILETYPE_BLOCK_DEVICE: u8 = 1;
pub const FILETYPE_CHARACTER_DEVICE: u8 = 2;
pub const FILETYPE_DIRECTORY: u8 = 3;
pub const FILETYPE_REGULAR_FILE: u8 = 4;
pub const FILETYPE_SOCKET_DGRAM: u8 = 5;
pub const FILETYPE_SOCKET_STREAM: u8 = 6;
// Preview1 has no pipe filetype. Expose UNKNOWN to guests so libc does not
// mistake pipes for sockets; PipeManager's description registry remains the
// authoritative internal pipe classification.
pub const FILETYPE_PIPE: u8 = FILETYPE_UNKNOWN;
pub const FILETYPE_SYMBOLIC_LINK: u8 = 7;

pub const WASI_RIGHT_FD_DATASYNC: u64 = 1 << 0;
pub const WASI_RIGHT_FD_READ: u64 = 1 << 1;
pub const WASI_RIGHT_FD_SEEK: u64 = 1 << 2;
pub const WASI_RIGHT_FD_FDSTAT_SET_FLAGS: u64 = 1 << 3;
pub const WASI_RIGHT_FD_SYNC: u64 = 1 << 4;
pub const WASI_RIGHT_FD_TELL: u64 = 1 << 5;
pub const WASI_RIGHT_FD_WRITE: u64 = 1 << 6;
pub const WASI_RIGHT_FD_ADVISE: u64 = 1 << 7;
pub const WASI_RIGHT_FD_ALLOCATE: u64 = 1 << 8;
pub const WASI_RIGHT_PATH_CREATE_DIRECTORY: u64 = 1 << 9;
pub const WASI_RIGHT_PATH_LINK_SOURCE: u64 = 1 << 10;
pub const WASI_RIGHT_PATH_LINK_TARGET: u64 = 1 << 11;
pub const WASI_RIGHT_PATH_OPEN: u64 = 1 << 13;
pub const WASI_RIGHT_FD_READDIR: u64 = 1 << 14;
pub const WASI_RIGHT_PATH_READLINK: u64 = 1 << 15;
pub const WASI_RIGHT_PATH_RENAME_SOURCE: u64 = 1 << 16;
pub const WASI_RIGHT_PATH_RENAME_TARGET: u64 = 1 << 17;
pub const WASI_RIGHT_PATH_FILESTAT_GET: u64 = 1 << 18;
pub const WASI_RIGHT_PATH_FILESTAT_SET_SIZE: u64 = 1 << 19;
pub const WASI_RIGHT_PATH_FILESTAT_SET_TIMES: u64 = 1 << 20;
pub const WASI_RIGHT_FD_FILESTAT_GET: u64 = 1 << 21;
pub const WASI_RIGHT_FD_FILESTAT_SET_SIZE: u64 = 1 << 22;
pub const WASI_RIGHT_FD_FILESTAT_SET_TIMES: u64 = 1 << 23;
pub const WASI_RIGHT_PATH_SYMLINK: u64 = 1 << 24;
pub const WASI_RIGHT_PATH_REMOVE_DIRECTORY: u64 = 1 << 25;
pub const WASI_RIGHT_PATH_UNLINK_FILE: u64 = 1 << 26;
pub const WASI_RIGHT_POLL_FD_READWRITE: u64 = 1 << 27;

pub const WASI_PREOPEN_READ_RIGHTS_BASE: u64 = WASI_RIGHT_FD_READ
    | WASI_RIGHT_FD_SEEK
    | WASI_RIGHT_FD_FDSTAT_SET_FLAGS
    | WASI_RIGHT_FD_TELL
    | WASI_RIGHT_PATH_OPEN
    | WASI_RIGHT_FD_READDIR
    | WASI_RIGHT_PATH_READLINK
    | WASI_RIGHT_PATH_FILESTAT_GET
    | WASI_RIGHT_FD_FILESTAT_GET
    | WASI_RIGHT_POLL_FD_READWRITE;
// Preview1 libc derives a newly opened descriptor's base rights from the
// parent directory's inheriting mask. Path rights must therefore propagate
// through an opened subdirectory or ordinary POSIX `openat(dirfd, ...)` loses
// PATH_OPEN/metadata authority after one level. The operation still requires
// a directory filetype, and `path_open` intersects every explicit request
// with this mask before installing the child descriptor.
pub const WASI_PREOPEN_READ_RIGHTS_INHERITING: u64 = WASI_PREOPEN_READ_RIGHTS_BASE;
/// Read-write tier rights intentionally exclude namespace destruction. These
/// match the owned runner's historical read-write capability set.
pub const WASI_PREOPEN_READ_WRITE_RIGHTS_BASE: u64 = WASI_PREOPEN_READ_RIGHTS_BASE
    | WASI_RIGHT_FD_DATASYNC
    | WASI_RIGHT_FD_SYNC
    | WASI_RIGHT_FD_WRITE
    | WASI_RIGHT_FD_ADVISE
    | WASI_RIGHT_FD_ALLOCATE
    | WASI_RIGHT_PATH_CREATE_DIRECTORY
    | WASI_RIGHT_PATH_FILESTAT_SET_SIZE
    | WASI_RIGHT_PATH_FILESTAT_SET_TIMES
    | WASI_RIGHT_FD_FILESTAT_SET_SIZE
    | WASI_RIGHT_FD_FILESTAT_SET_TIMES;
pub const WASI_PREOPEN_READ_WRITE_RIGHTS_INHERITING: u64 = WASI_PREOPEN_READ_WRITE_RIGHTS_BASE;
pub const WASI_PREOPEN_WRITE_RIGHTS_BASE: u64 = WASI_PREOPEN_READ_WRITE_RIGHTS_BASE
    | WASI_RIGHT_PATH_LINK_SOURCE
    | WASI_RIGHT_PATH_LINK_TARGET
    | WASI_RIGHT_PATH_RENAME_SOURCE
    | WASI_RIGHT_PATH_RENAME_TARGET
    | WASI_RIGHT_PATH_SYMLINK
    | WASI_RIGHT_PATH_REMOVE_DIRECTORY
    | WASI_RIGHT_PATH_UNLINK_FILE;
pub const WASI_PREOPEN_WRITE_RIGHTS_INHERITING: u64 = WASI_PREOPEN_WRITE_RIGHTS_BASE;

pub const WASI_STDIO_READ_RIGHTS: u64 = WASI_RIGHT_FD_READ
    | WASI_RIGHT_FD_FDSTAT_SET_FLAGS
    | WASI_RIGHT_FD_FILESTAT_GET
    | WASI_RIGHT_POLL_FD_READWRITE;
pub const WASI_STDIO_WRITE_RIGHTS: u64 = WASI_RIGHT_FD_WRITE
    | WASI_RIGHT_FD_FDSTAT_SET_FLAGS
    | WASI_RIGHT_FD_FILESTAT_GET
    | WASI_RIGHT_POLL_FD_READWRITE;

/// Default rights for descriptors created by Linux-style kernel operations.
/// Preview1 `path_open` replaces these with the guest's explicit request after
/// validating it against the parent capability and permission tier.
pub fn wasi_rights_for_open(flags: u32, filetype: u8) -> (u64, u64) {
    let access = flags & (O_WRONLY | O_RDWR);
    let readable = access != O_WRONLY;
    let writable = access != O_RDONLY;
    let seekable = matches!(filetype, FILETYPE_REGULAR_FILE | FILETYPE_DIRECTORY);

    let mut base =
        WASI_RIGHT_FD_FDSTAT_SET_FLAGS | WASI_RIGHT_FD_FILESTAT_GET | WASI_RIGHT_POLL_FD_READWRITE;
    if readable {
        base |= WASI_RIGHT_FD_READ;
    }
    if writable {
        base |= WASI_RIGHT_FD_WRITE | WASI_RIGHT_FD_DATASYNC | WASI_RIGHT_FD_SYNC;
    }
    if seekable {
        base |= WASI_RIGHT_FD_SEEK | WASI_RIGHT_FD_TELL | WASI_RIGHT_FD_ADVISE;
    }
    if filetype == FILETYPE_REGULAR_FILE && writable {
        base |= WASI_RIGHT_FD_ALLOCATE
            | WASI_RIGHT_FD_FILESTAT_SET_SIZE
            | WASI_RIGHT_FD_FILESTAT_SET_TIMES;
    }
    // AgentOS' Linux extensions use Preview1's closest metadata-mutation
    // capability for fchmod/fchown. Anonymous pipes and sockets own mutable
    // inode metadata even though neither resource is seekable and a pipe's
    // read end is O_RDONLY. Grant the synthesized capability at creation;
    // Preview1 path_open still replaces synthesized rights with the guest's
    // exact explicit request.
    if matches!(
        filetype,
        FILETYPE_PIPE | FILETYPE_SOCKET_DGRAM | FILETYPE_SOCKET_STREAM
    ) {
        base |= WASI_RIGHT_FD_FILESTAT_SET_TIMES;
    }
    if filetype == FILETYPE_DIRECTORY {
        base |= WASI_RIGHT_FD_READDIR
            | WASI_RIGHT_PATH_OPEN
            | WASI_RIGHT_PATH_READLINK
            | WASI_RIGHT_PATH_FILESTAT_GET;
    }
    (base, 0)
}

pub type FdResult<T> = Result<T, FdTableError>;
pub type SharedFileDescription = Arc<FileDescription>;

// Every kernel subsystem keys pipes, PTYs, sockets, locks, and poll state by
// open-file-description identity. Allocate that identity from one global
// monotonic domain: per-subsystem counters can eventually overlap and make an
// unrelated regular file look like a stale socket or pipe.
static NEXT_FILE_DESCRIPTION_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) fn allocate_file_description_id() -> u64 {
    NEXT_FILE_DESCRIPTION_ID
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |id| id.checked_add(1))
        .expect("open-file-description id space exhausted")
}

#[derive(Debug, Default)]
pub struct AnonymousFileUsage {
    bytes: AtomicU64,
    inodes: AtomicUsize,
}

impl AnonymousFileUsage {
    pub fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::SeqCst)
    }

    pub fn inodes(&self) -> usize {
        self.inodes.load(Ordering::SeqCst)
    }

    fn add_file(&self, size: u64) {
        self.bytes.fetch_add(size, Ordering::SeqCst);
        self.inodes.fetch_add(1, Ordering::SeqCst);
    }

    fn remove_file(&self, size: u64) {
        self.bytes.fetch_sub(size, Ordering::SeqCst);
        self.inodes.fetch_sub(1, Ordering::SeqCst);
    }

    fn resize_file(&self, old_size: u64, new_size: u64) {
        if new_size >= old_size {
            self.bytes
                .fetch_add(new_size.saturating_sub(old_size), Ordering::SeqCst);
        } else {
            self.bytes
                .fetch_sub(old_size.saturating_sub(new_size), Ordering::SeqCst);
        }
    }
}

#[derive(Debug)]
pub struct AnonymousFile {
    pub data: Vec<u8>,
    pub stat: VirtualStat,
    pub xattrs: BTreeMap<String, Vec<u8>>,
    usage: Arc<AnonymousFileUsage>,
}

impl AnonymousFile {
    pub fn new(
        data: Vec<u8>,
        stat: VirtualStat,
        xattrs: BTreeMap<String, Vec<u8>>,
        usage: Arc<AnonymousFileUsage>,
    ) -> Self {
        usage.add_file(stat.size);
        Self {
            data,
            stat,
            xattrs,
            usage,
        }
    }
}

impl Drop for AnonymousFile {
    fn drop(&mut self) {
        self.usage.remove_file(self.stat.size);
    }
}

pub type SharedAnonymousFile = Arc<Mutex<AnonymousFile>>;

#[derive(Debug)]
enum FileBacking {
    Path(String),
    LinkedAlias {
        former_path: String,
        live_path: String,
    },
    Anonymous {
        former_path: String,
        file: SharedAnonymousFile,
    },
    DetachedDirectory {
        former_path: String,
        stat: VirtualStat,
        xattrs: BTreeMap<String, Vec<u8>>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FdTableError {
    code: &'static str,
    message: String,
}

impl FdTableError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn code(&self) -> &'static str {
        self.code
    }

    fn bad_file_descriptor(fd: u32) -> Self {
        Self {
            code: "EBADF",
            message: format!("bad file descriptor {fd}"),
        }
    }

    fn too_many_open_files() -> Self {
        Self {
            code: "EMFILE",
            message: String::from("too many open files"),
        }
    }

    fn no_memory(message: impl Into<String>) -> Self {
        Self {
            code: "ENOMEM",
            message: message.into(),
        }
    }

    fn already_exists(message: impl Into<String>) -> Self {
        Self {
            code: "EEXIST",
            message: message.into(),
        }
    }

    fn no_data(message: impl Into<String>) -> Self {
        Self {
            code: "ENODATA",
            message: message.into(),
        }
    }

    fn invalid_argument(message: impl Into<String>) -> Self {
        Self {
            code: "EINVAL",
            message: message.into(),
        }
    }

    fn would_block(message: impl Into<String>) -> Self {
        Self {
            code: "EWOULDBLOCK",
            message: message.into(),
        }
    }

    fn deadlock(message: impl Into<String>) -> Self {
        Self {
            code: "EDEADLK",
            message: message.into(),
        }
    }
}

impl fmt::Display for FdTableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl Error for FdTableError {}

fn set_detached_xattr(
    xattrs: &mut BTreeMap<String, Vec<u8>>,
    name: &str,
    value: Vec<u8>,
    flags: u32,
) -> FdResult<()> {
    validate_detached_xattr_name(name)?;
    if value.len() > 64 * 1024 {
        return Err(FdTableError::new(
            "E2BIG",
            format!(
                "extended attribute value is {} bytes; limit is 65536",
                value.len()
            ),
        ));
    }
    if flags & !3 != 0 || flags == 3 {
        return Err(FdTableError::invalid_argument(format!(
            "invalid xattr flags {flags}"
        )));
    }
    let exists = xattrs.contains_key(name);
    if flags == 1 && exists {
        return Err(FdTableError::already_exists(format!(
            "xattr {name} already exists"
        )));
    }
    if flags == 2 && !exists {
        return Err(FdTableError::no_data(format!(
            "xattr {name} does not exist"
        )));
    }
    xattrs.insert(name.to_owned(), value);
    Ok(())
}

fn remove_detached_xattr(xattrs: &mut BTreeMap<String, Vec<u8>>, name: &str) -> FdResult<()> {
    validate_detached_xattr_name(name)?;
    xattrs
        .remove(name)
        .map(|_| ())
        .ok_or_else(|| FdTableError::no_data(format!("xattr {name} does not exist")))
}

fn validate_detached_xattr_name(name: &str) -> FdResult<()> {
    if name.is_empty() || name.len() > 255 || !name.contains('.') || name.contains('\0') {
        return Err(FdTableError::new(
            "ERANGE",
            format!("invalid extended attribute name: {name:?}"),
        ));
    }
    Ok(())
}

#[derive(Debug)]
pub struct FileDescription {
    id: u64,
    backing: Mutex<FileBacking>,
    lock_target: Option<FileLockTarget>,
    cursor: AtomicU64,
    directory_snapshot: Mutex<Option<Vec<DirectorySnapshotEntry>>>,
    flags: AtomicU32,
    ref_count: AtomicUsize,
}

/// One stable record in an open directory description's current getdents
/// stream. This is cursor state, not a second filesystem namespace: rewinding
/// to cookie zero replaces it from the authoritative VFS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectorySnapshotEntry {
    pub name: String,
    pub ino: u64,
    pub is_directory: bool,
    pub is_symbolic_link: bool,
}

impl FileDescription {
    pub fn new(id: u64, path: impl Into<String>, flags: u32) -> Self {
        Self::with_ref_count_and_lock(id, path, flags, 1, None)
    }

    pub fn new_with_lock(
        id: u64,
        path: impl Into<String>,
        flags: u32,
        lock_target: Option<FileLockTarget>,
    ) -> Self {
        Self::with_ref_count_and_lock(id, path, flags, 1, lock_target)
    }

    pub fn with_ref_count(id: u64, path: impl Into<String>, flags: u32, ref_count: usize) -> Self {
        Self::with_ref_count_and_lock(id, path, flags, ref_count, None)
    }

    pub fn with_ref_count_and_lock(
        id: u64,
        path: impl Into<String>,
        flags: u32,
        ref_count: usize,
        lock_target: Option<FileLockTarget>,
    ) -> Self {
        Self {
            id,
            backing: Mutex::new(FileBacking::Path(path.into())),
            lock_target,
            cursor: AtomicU64::new(0),
            directory_snapshot: Mutex::new(None),
            flags: AtomicU32::new(flags),
            ref_count: AtomicUsize::new(ref_count),
        }
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn path(&self) -> String {
        match &*lock_or_recover(&self.backing) {
            FileBacking::Path(path) => path.clone(),
            FileBacking::LinkedAlias { live_path, .. } => live_path.clone(),
            FileBacking::Anonymous { former_path, .. } => former_path.clone(),
            FileBacking::DetachedDirectory { former_path, .. } => former_path.clone(),
        }
    }

    /// Linux renders an unlinked open file description through procfs with a
    /// ` (deleted)` suffix while the description itself remains usable.
    pub fn proc_display_path(&self) -> String {
        match &*lock_or_recover(&self.backing) {
            FileBacking::Path(path) => path.clone(),
            FileBacking::LinkedAlias { former_path, .. }
            | FileBacking::Anonymous { former_path, .. }
            | FileBacking::DetachedDirectory { former_path, .. } => {
                format!("{former_path} (deleted)")
            }
        }
    }

    pub fn is_path_backed_by(&self, expected: &str) -> bool {
        match &*lock_or_recover(&self.backing) {
            FileBacking::Path(path) => path == expected,
            FileBacking::LinkedAlias { live_path, .. } => live_path == expected,
            FileBacking::Anonymous { .. } | FileBacking::DetachedDirectory { .. } => false,
        }
    }

    pub fn rename_path_prefix(&self, old_path: &str, new_path: &str) {
        let mut backing = lock_or_recover(&self.backing);
        let path = match &mut *backing {
            FileBacking::Path(path) => path,
            FileBacking::LinkedAlias { live_path, .. } => live_path,
            FileBacking::Anonymous { .. } | FileBacking::DetachedDirectory { .. } => return,
        };
        if path == old_path {
            *path = new_path.to_string();
        } else if let Some(suffix) = path
            .strip_prefix(old_path)
            .filter(|value| value.starts_with('/'))
        {
            *path = format!("{new_path}{suffix}");
        }
    }

    pub fn detach_path(&self, expected: &str, file: SharedAnonymousFile) -> bool {
        let mut backing = lock_or_recover(&self.backing);
        let former_path = match &*backing {
            FileBacking::Path(path) if path == expected => expected.to_string(),
            FileBacking::LinkedAlias {
                former_path,
                live_path,
            } if live_path == expected => former_path.clone(),
            _ => return false,
        };
        *backing = FileBacking::Anonymous { former_path, file };
        true
    }

    /// Keep an unlinked open description attached to the same inode through a
    /// surviving hard-link name. The former name remains visible through
    /// procfs, while descriptor operations use the live alias.
    pub fn rebind_deleted_path(&self, expected: &str, live_path: &str) -> bool {
        let mut backing = lock_or_recover(&self.backing);
        let former_path = match &*backing {
            FileBacking::Path(path) if path == expected => expected.to_string(),
            FileBacking::LinkedAlias {
                former_path,
                live_path: current,
            } if current == expected => former_path.clone(),
            _ => return false,
        };
        *backing = FileBacking::LinkedAlias {
            former_path,
            live_path: live_path.to_string(),
        };
        true
    }

    pub fn detach_directory(
        &self,
        _expected: &str,
        stat: VirtualStat,
        xattrs: BTreeMap<String, Vec<u8>>,
    ) -> bool {
        let mut backing = lock_or_recover(&self.backing);
        let FileBacking::Path(former_path) = &*backing else {
            return false;
        };
        let former_path = former_path.clone();
        *backing = FileBacking::DetachedDirectory {
            former_path,
            stat,
            xattrs,
        };
        true
    }

    pub fn detached_directory_stat(&self) -> Option<VirtualStat> {
        match &*lock_or_recover(&self.backing) {
            FileBacking::DetachedDirectory { stat, .. } => Some(stat.clone()),
            FileBacking::Path(_)
            | FileBacking::LinkedAlias { .. }
            | FileBacking::Anonymous { .. } => None,
        }
    }

    pub fn anonymous_stat(&self) -> Option<VirtualStat> {
        let file = match &*lock_or_recover(&self.backing) {
            FileBacking::Anonymous { file, .. } => Arc::clone(file),
            FileBacking::Path(_)
            | FileBacking::LinkedAlias { .. }
            | FileBacking::DetachedDirectory { .. } => return None,
        };
        let stat = lock_or_recover(&file).stat.clone();
        Some(stat)
    }

    pub fn anonymous_pread(&self, offset: u64, length: usize) -> Option<Vec<u8>> {
        let file = match &*lock_or_recover(&self.backing) {
            FileBacking::Anonymous { file, .. } => Arc::clone(file),
            FileBacking::Path(_)
            | FileBacking::LinkedAlias { .. }
            | FileBacking::DetachedDirectory { .. } => return None,
        };
        let file = lock_or_recover(&file);
        let start = usize::try_from(offset).unwrap_or(usize::MAX);
        if start >= file.data.len() {
            return Some(Vec::new());
        }
        let end = start.saturating_add(length).min(file.data.len());
        Some(file.data[start..end].to_vec())
    }

    pub fn anonymous_pwrite(&self, offset: u64, data: &[u8]) -> Option<FdResult<u64>> {
        let file = match &*lock_or_recover(&self.backing) {
            FileBacking::Anonymous { file, .. } => Arc::clone(file),
            FileBacking::Path(_)
            | FileBacking::LinkedAlias { .. }
            | FileBacking::DetachedDirectory { .. } => return None,
        };
        let mut file = lock_or_recover(&file);
        let Ok(start) = usize::try_from(offset) else {
            return Some(Err(FdTableError::invalid_argument(
                "anonymous file offset exceeds host address space",
            )));
        };
        let Some(end) = start.checked_add(data.len()) else {
            return Some(Err(FdTableError::invalid_argument(
                "anonymous file write range overflows",
            )));
        };
        if end > file.data.len() {
            let additional = end - file.data.len();
            if file.data.try_reserve(additional).is_err() {
                return Some(Err(FdTableError::no_memory(
                    "anonymous file write allocation failed",
                )));
            }
            file.data.resize(end, 0);
        }
        file.data[start..end].copy_from_slice(data);
        let old_size = file.stat.size;
        file.stat.size = file.data.len() as u64;
        file.stat.blocks = file.stat.size.div_ceil(512);
        file.usage.resize_file(old_size, file.stat.size);
        Some(Ok(file.stat.size))
    }

    pub fn anonymous_truncate(&self, length: u64) -> Option<FdResult<()>> {
        let file = match &*lock_or_recover(&self.backing) {
            FileBacking::Anonymous { file, .. } => Arc::clone(file),
            FileBacking::Path(_)
            | FileBacking::LinkedAlias { .. }
            | FileBacking::DetachedDirectory { .. } => return None,
        };
        let Ok(length) = usize::try_from(length) else {
            return Some(Err(FdTableError::invalid_argument(
                "anonymous file length exceeds host address space",
            )));
        };
        let mut file = lock_or_recover(&file);
        let additional = length.saturating_sub(file.data.len());
        if additional > 0 && file.data.try_reserve(additional).is_err() {
            return Some(Err(FdTableError::no_memory(
                "anonymous file truncate allocation failed",
            )));
        }
        file.data.resize(length, 0);
        let old_size = file.stat.size;
        file.stat.size = length as u64;
        file.stat.blocks = file.stat.size.div_ceil(512);
        file.usage.resize_file(old_size, file.stat.size);
        Some(Ok(()))
    }

    pub fn detached_chmod(&self, mode: u32) -> bool {
        let mut backing = lock_or_recover(&self.backing);
        match &mut *backing {
            FileBacking::Anonymous { file, .. } => {
                let mut file = lock_or_recover(file);
                file.stat.mode = (file.stat.mode & !0o7777) | (mode & 0o7777);
                true
            }
            FileBacking::DetachedDirectory { stat, .. } => {
                stat.mode = (stat.mode & !0o7777) | (mode & 0o7777);
                true
            }
            FileBacking::Path(_) | FileBacking::LinkedAlias { .. } => false,
        }
    }

    pub fn detached_chown(&self, uid: u32, gid: u32, changed_mode: Option<u32>) -> bool {
        let mut backing = lock_or_recover(&self.backing);
        match &mut *backing {
            FileBacking::Anonymous { file, .. } => {
                let mut file = lock_or_recover(file);
                file.stat.uid = uid;
                file.stat.gid = gid;
                if let Some(mode) = changed_mode {
                    file.stat.mode = mode;
                }
                true
            }
            FileBacking::DetachedDirectory { stat, .. } => {
                stat.uid = uid;
                stat.gid = gid;
                true
            }
            FileBacking::Path(_) | FileBacking::LinkedAlias { .. } => false,
        }
    }

    pub fn detached_xattrs(&self) -> Option<BTreeMap<String, Vec<u8>>> {
        match &*lock_or_recover(&self.backing) {
            FileBacking::Anonymous { file, .. } => Some(lock_or_recover(file).xattrs.clone()),
            FileBacking::DetachedDirectory { xattrs, .. } => Some(xattrs.clone()),
            FileBacking::Path(_) | FileBacking::LinkedAlias { .. } => None,
        }
    }

    pub fn detached_get_xattr(&self, name: &str) -> Option<FdResult<Vec<u8>>> {
        let xattrs = self.detached_xattrs()?;
        Some(
            xattrs
                .get(name)
                .cloned()
                .ok_or_else(|| FdTableError::no_data(format!("xattr {name} does not exist"))),
        )
    }

    pub fn detached_set_xattr(
        &self,
        name: &str,
        value: Vec<u8>,
        flags: u32,
    ) -> Option<FdResult<()>> {
        let mut backing = lock_or_recover(&self.backing);
        let xattrs = match &mut *backing {
            FileBacking::Anonymous { file, .. } => {
                let mut file = lock_or_recover(file);
                return Some(set_detached_xattr(&mut file.xattrs, name, value, flags));
            }
            FileBacking::DetachedDirectory { xattrs, .. } => xattrs,
            FileBacking::Path(_) | FileBacking::LinkedAlias { .. } => return None,
        };
        Some(set_detached_xattr(xattrs, name, value, flags))
    }

    pub fn detached_remove_xattr(&self, name: &str) -> Option<FdResult<()>> {
        let mut backing = lock_or_recover(&self.backing);
        let xattrs = match &mut *backing {
            FileBacking::Anonymous { file, .. } => {
                let mut file = lock_or_recover(file);
                return Some(remove_detached_xattr(&mut file.xattrs, name));
            }
            FileBacking::DetachedDirectory { xattrs, .. } => xattrs,
            FileBacking::Path(_) | FileBacking::LinkedAlias { .. } => return None,
        };
        Some(remove_detached_xattr(xattrs, name))
    }

    pub fn lock_target(&self) -> Option<FileLockTarget> {
        self.lock_target
    }

    pub fn cursor(&self) -> u64 {
        self.cursor.load(Ordering::SeqCst)
    }

    pub fn set_cursor(&self, cursor: u64) {
        self.cursor.store(cursor, Ordering::SeqCst);
    }

    /// Replace the entries backing this open directory description's current
    /// getdents stream. Linux directory offsets remain usable when entries
    /// returned by an earlier page are removed, so a mutable vector index into
    /// the live directory is not a valid cookie implementation.
    pub fn reset_directory_snapshot(&self, entries: Vec<DirectorySnapshotEntry>) {
        *lock_or_recover(&self.directory_snapshot) = Some(entries);
    }

    /// Return the requested child-entry slice from the current directory
    /// stream, together with the total number of snapshotted children.
    pub fn directory_snapshot_page(
        &self,
        first_child: usize,
        max_children: usize,
    ) -> Option<(usize, Vec<DirectorySnapshotEntry>)> {
        let snapshot = lock_or_recover(&self.directory_snapshot);
        let entries = snapshot.as_ref()?;
        let page = entries
            .iter()
            .skip(first_child)
            .take(max_children)
            .cloned()
            .collect();
        Some((entries.len(), page))
    }

    pub fn flags(&self) -> u32 {
        self.flags.load(Ordering::SeqCst)
    }

    pub fn update_flags(&self, mask: u32, flags: u32) -> u32 {
        let mut current = self.flags();
        loop {
            let next = (current & !mask) | (flags & mask);
            match self
                .flags
                .compare_exchange(current, next, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => return next,
                Err(observed) => current = observed,
            }
        }
    }

    pub fn ref_count(&self) -> usize {
        self.ref_count.load(Ordering::SeqCst)
    }

    pub fn increment_ref_count(&self) -> usize {
        self.ref_count.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub fn decrement_ref_count(&self) -> usize {
        let mut current = self.ref_count.load(Ordering::SeqCst);
        loop {
            let next = current.saturating_sub(1);
            match self
                .ref_count
                .compare_exchange(current, next, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => return next,
                Err(observed) => current = observed,
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct FdEntry {
    pub fd: u32,
    pub description: SharedFileDescription,
    pub status_flags: SharedStatusFlags,
    pub fd_flags: u32,
    pub rights: u64,
    pub rights_inheriting: u64,
    pub filetype: u8,
    pub wasi_preopen_path: Option<String>,
}

/// Mutable open-file status shared by every descriptor alias of the same
/// open file description. Linux shares `O_NONBLOCK` across `dup`, `fork`, and
/// SCM_RIGHTS while keeping descriptor flags such as `FD_CLOEXEC` per slot.
#[derive(Debug, Clone)]
pub struct SharedStatusFlags(Arc<AtomicU32>);

impl SharedStatusFlags {
    fn new(flags: u32) -> Self {
        Self(Arc::new(AtomicU32::new(flags & ENTRY_STATUS_FLAG_MASK)))
    }

    pub fn get(&self) -> u32 {
        self.0.load(Ordering::SeqCst)
    }

    fn set(&self, flags: u32) {
        self.0
            .store(flags & ENTRY_STATUS_FLAG_MASK, Ordering::SeqCst);
    }
}

impl PartialEq for SharedStatusFlags {
    fn eq(&self, other: &Self) -> bool {
        self.get() == other.get()
    }
}

impl Eq for SharedStatusFlags {}

#[derive(Debug)]
pub struct TransferredFd {
    description: SharedFileDescription,
    status_flags: SharedStatusFlags,
    rights: u64,
    rights_inheriting: u64,
    filetype: u8,
    // Descriptor-local capability metadata is captured for trusted spawn
    // staging, but ordinary SCM_RIGHTS installation deliberately drops it.
    wasi_preopen_path: Option<String>,
}

impl Clone for TransferredFd {
    fn clone(&self) -> Self {
        self.description.increment_ref_count();
        Self {
            description: Arc::clone(&self.description),
            status_flags: self.status_flags.clone(),
            rights: self.rights,
            rights_inheriting: self.rights_inheriting,
            filetype: self.filetype,
            wasi_preopen_path: self.wasi_preopen_path.clone(),
        }
    }
}

impl PartialEq for TransferredFd {
    fn eq(&self, other: &Self) -> bool {
        self.description.id() == other.description.id()
            && self.status_flags == other.status_flags
            && self.rights == other.rights
            && self.rights_inheriting == other.rights_inheriting
            && self.filetype == other.filetype
            && self.wasi_preopen_path == other.wasi_preopen_path
    }
}

impl Eq for TransferredFd {}

impl Drop for TransferredFd {
    fn drop(&mut self) {
        self.description.decrement_ref_count();
    }
}

impl TransferredFd {
    pub(crate) fn description(&self) -> SharedFileDescription {
        Arc::clone(&self.description)
    }

    pub fn description_id(&self) -> u64 {
        self.description.id()
    }

    /// Number of live kernel fd-table/transfer references to this canonical
    /// open description. A sidecar registry may retain one dedicated transfer
    /// lease and retire external resources when only that lease remains.
    pub fn ref_count(&self) -> usize {
        self.description.ref_count()
    }

    pub fn status_flags(&self) -> u32 {
        self.status_flags.get()
    }

    pub fn rights(&self) -> u64 {
        self.rights
    }

    pub fn rights_inheriting(&self) -> u64 {
        self.rights_inheriting
    }

    pub fn filetype(&self) -> u8 {
        self.filetype
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FdStat {
    pub filetype: u8,
    pub flags: u32,
    pub rights: u64,
    pub rights_inheriting: u64,
    pub wasi_preopen_path: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StdioOverride {
    pub description: SharedFileDescription,
    pub filetype: u8,
}

#[derive(Debug, Clone)]
struct DescriptionFactory {}

impl DescriptionFactory {
    fn new(_starting_id: u64) -> Self {
        Self {}
    }

    fn allocate(&self, path: &str, flags: u32) -> SharedFileDescription {
        self.allocate_with_lock(path, flags, None)
    }

    fn allocate_with_lock(
        &self,
        path: &str,
        flags: u32,
        lock_target: Option<FileLockTarget>,
    ) -> SharedFileDescription {
        let next_id = allocate_file_description_id();
        Arc::new(FileDescription::new_with_lock(
            next_id,
            path,
            flags,
            lock_target,
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FileLockTarget {
    dev: u64,
    ino: u64,
}

impl FileLockTarget {
    pub const fn new(dev: u64, ino: u64) -> Self {
        Self { dev, ino }
    }

    pub const fn dev(self) -> u64 {
        self.dev
    }

    pub const fn ino(self) -> u64 {
        self.ino
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileLockMode {
    Shared,
    Exclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordLockType {
    Read,
    Write,
    Unlock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordLock {
    pub lock_type: RecordLockType,
    pub start: u64,
    /// Exclusive end offset. `None` means through EOF, including future growth.
    pub end: Option<u64>,
    pub pid: u32,
}

impl RecordLock {
    pub fn new(lock_type: RecordLockType, start: u64, length: u64, pid: u32) -> FdResult<Self> {
        let end =
            if length == 0 {
                None
            } else {
                Some(start.checked_add(length).ok_or_else(|| {
                    FdTableError::invalid_argument("record lock range exceeds u64")
                })?)
            };
        Ok(Self {
            lock_type,
            start,
            end,
            pid,
        })
    }

    pub fn length(self) -> u64 {
        self.end.map_or(0, |end| end - self.start)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlockOperation {
    Shared { nonblocking: bool },
    Exclusive { nonblocking: bool },
    Unlock,
}

impl FlockOperation {
    pub fn from_bits(operation: u32) -> FdResult<Self> {
        let nonblocking = operation & LOCK_NB != 0;
        match operation & !LOCK_NB {
            LOCK_SH => Ok(Self::Shared { nonblocking }),
            LOCK_EX => Ok(Self::Exclusive { nonblocking }),
            LOCK_UN => Ok(Self::Unlock),
            _ => Err(FdTableError::invalid_argument(format!(
                "invalid flock operation {operation:#x}"
            ))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProcessFdTable {
    entries: BTreeMap<u32, FdEntry>,
    hidden_wasi_preopens: BTreeMap<u32, FdEntry>,
    next_fd: u32,
    alloc_desc: DescriptionFactory,
    max_fds: usize,
    wasi_preopens_initialized: bool,
    canonical_wasi_layout_initialized: bool,
}

impl ProcessFdTable {
    fn new(alloc_desc: DescriptionFactory, max_fds: usize) -> Self {
        Self {
            entries: BTreeMap::new(),
            hidden_wasi_preopens: BTreeMap::new(),
            next_fd: 3,
            alloc_desc,
            max_fds,
            wasi_preopens_initialized: false,
            canonical_wasi_layout_initialized: false,
        }
    }

    pub fn max_fds(&self) -> usize {
        self.max_fds
    }

    /// Change the per-process allocation ceiling without closing descriptors.
    /// Linux permits lowering RLIMIT_NOFILE below the current open count; new
    /// allocations fail until the table falls back below the limit.
    pub fn set_max_fds(&mut self, max_fds: usize) {
        self.max_fds = max_fds;
        if self.next_fd as usize >= max_fds {
            self.next_fd = 0;
        }
    }

    pub fn available_fd_capacity(&self) -> usize {
        self.max_fds.saturating_sub(self.entries.len())
    }

    pub fn init_stdio(
        &mut self,
        stdin_desc: SharedFileDescription,
        stdout_desc: SharedFileDescription,
        stderr_desc: SharedFileDescription,
    ) {
        self.entries.insert(
            0,
            FdEntry {
                fd: 0,
                description: stdin_desc,
                status_flags: SharedStatusFlags::new(0),
                fd_flags: 0,
                rights: WASI_STDIO_READ_RIGHTS,
                rights_inheriting: 0,
                filetype: FILETYPE_CHARACTER_DEVICE,
                wasi_preopen_path: None,
            },
        );
        self.entries.insert(
            1,
            FdEntry {
                fd: 1,
                description: stdout_desc,
                status_flags: SharedStatusFlags::new(0),
                fd_flags: 0,
                rights: WASI_STDIO_WRITE_RIGHTS,
                rights_inheriting: 0,
                filetype: FILETYPE_CHARACTER_DEVICE,
                wasi_preopen_path: None,
            },
        );
        self.entries.insert(
            2,
            FdEntry {
                fd: 2,
                description: stderr_desc,
                status_flags: SharedStatusFlags::new(0),
                fd_flags: 0,
                rights: WASI_STDIO_WRITE_RIGHTS,
                rights_inheriting: 0,
                filetype: FILETYPE_CHARACTER_DEVICE,
                wasi_preopen_path: None,
            },
        );
    }

    pub fn init_stdio_with_types(
        &mut self,
        stdin_desc: SharedFileDescription,
        stdin_type: u8,
        stdout_desc: SharedFileDescription,
        stdout_type: u8,
        stderr_desc: SharedFileDescription,
        stderr_type: u8,
    ) {
        stdin_desc.increment_ref_count();
        stdout_desc.increment_ref_count();
        stderr_desc.increment_ref_count();
        self.entries.insert(
            0,
            FdEntry {
                fd: 0,
                description: stdin_desc,
                status_flags: SharedStatusFlags::new(0),
                fd_flags: 0,
                rights: WASI_STDIO_READ_RIGHTS,
                rights_inheriting: 0,
                filetype: stdin_type,
                wasi_preopen_path: None,
            },
        );
        self.entries.insert(
            1,
            FdEntry {
                fd: 1,
                description: stdout_desc,
                status_flags: SharedStatusFlags::new(0),
                fd_flags: 0,
                rights: WASI_STDIO_WRITE_RIGHTS,
                rights_inheriting: 0,
                filetype: stdout_type,
                wasi_preopen_path: None,
            },
        );
        self.entries.insert(
            2,
            FdEntry {
                fd: 2,
                description: stderr_desc,
                status_flags: SharedStatusFlags::new(0),
                fd_flags: 0,
                rights: WASI_STDIO_WRITE_RIGHTS,
                rights_inheriting: 0,
                filetype: stderr_type,
                wasi_preopen_path: None,
            },
        );
    }

    pub fn open(&mut self, path: &str, flags: u32) -> FdResult<u32> {
        self.open_with_details(path, flags, FILETYPE_REGULAR_FILE, None)
    }

    pub fn open_with_filetype(&mut self, path: &str, flags: u32, filetype: u8) -> FdResult<u32> {
        self.open_with_details(path, flags, filetype, None)
    }

    pub fn open_with_details(
        &mut self,
        path: &str,
        flags: u32,
        filetype: u8,
        lock_target: Option<FileLockTarget>,
    ) -> FdResult<u32> {
        let fd = self.allocate_fd()?;
        let description =
            self.alloc_desc
                .allocate_with_lock(path, description_flags(flags), lock_target);
        let (rights, rights_inheriting) = wasi_rights_for_open(flags, filetype);
        self.entries.insert(
            fd,
            FdEntry {
                fd,
                description,
                status_flags: SharedStatusFlags::new(status_flags(flags)),
                fd_flags: 0,
                rights,
                rights_inheriting,
                filetype,
                wasi_preopen_path: None,
            },
        );
        Ok(fd)
    }

    pub fn open_with(
        &mut self,
        description: SharedFileDescription,
        filetype: u8,
        target_fd: Option<u32>,
    ) -> FdResult<u32> {
        let entry_status_flags = status_flags(description.flags());
        let fd = match target_fd {
            Some(fd) => {
                self.validate_fd_bounds(fd)?;
                if self.entries.contains_key(&fd) {
                    self.close(fd);
                } else if self.entries.len() >= self.max_fds {
                    return Err(FdTableError::too_many_open_files());
                }
                fd
            }
            None => self.allocate_fd()?,
        };
        description.increment_ref_count();
        let (rights, rights_inheriting) = wasi_rights_for_open(description.flags(), filetype);
        self.entries.insert(
            fd,
            FdEntry {
                fd,
                description,
                status_flags: SharedStatusFlags::new(entry_status_flags),
                fd_flags: 0,
                rights,
                rights_inheriting,
                filetype,
                wasi_preopen_path: None,
            },
        );
        Ok(fd)
    }

    pub fn open_pair_with_details(
        &mut self,
        first_path: &str,
        second_path: &str,
        status_flags: u32,
        fd_flags: u32,
        filetype: u8,
    ) -> FdResult<(u32, u32, SharedFileDescription, SharedFileDescription)> {
        if self.entries.len().saturating_add(2) > self.max_fds {
            return Err(FdTableError::too_many_open_files());
        }
        let first_fd = self.allocate_fd()?;
        let second_fd = match self.allocate_fd() {
            Ok(fd) => fd,
            Err(error) => {
                self.next_fd = first_fd;
                return Err(error);
            }
        };
        let first = self.alloc_desc.allocate(first_path, O_RDWR);
        let second = self.alloc_desc.allocate(second_path, O_RDWR);
        let (rights, rights_inheriting) = wasi_rights_for_open(O_RDWR, filetype);
        for (fd, description) in [
            (first_fd, Arc::clone(&first)),
            (second_fd, Arc::clone(&second)),
        ] {
            self.entries.insert(
                fd,
                FdEntry {
                    fd,
                    description,
                    status_flags: SharedStatusFlags::new(status_flags),
                    fd_flags: fd_flags & FD_CLOEXEC,
                    rights,
                    rights_inheriting,
                    filetype,
                    wasi_preopen_path: None,
                },
            );
        }
        Ok((first_fd, second_fd, first, second))
    }

    pub fn transfer(&self, fd: u32) -> FdResult<TransferredFd> {
        let entry = self
            .get(fd)
            .ok_or_else(|| FdTableError::bad_file_descriptor(fd))?;
        entry.description.increment_ref_count();
        Ok(TransferredFd {
            description: Arc::clone(&entry.description),
            status_flags: entry.status_flags.clone(),
            rights: entry.rights,
            rights_inheriting: entry.rights_inheriting,
            filetype: entry.filetype,
            wasi_preopen_path: entry.wasi_preopen_path.clone(),
        })
    }

    /// Create an open-file-description transfer without consuming an fd slot.
    ///
    /// SCM_RIGHTS queues descriptions, not temporary descriptors in the
    /// sending process. Keeping this separate from `open_with_details` avoids
    /// spuriously returning EMFILE when the sender's descriptor table is full.
    pub fn create_transfer(&self, path: &str, flags: u32, filetype: u8) -> TransferredFd {
        let (rights, rights_inheriting) = wasi_rights_for_open(flags, filetype);
        TransferredFd {
            description: self.alloc_desc.allocate(path, description_flags(flags)),
            status_flags: SharedStatusFlags::new(status_flags(flags)),
            rights,
            rights_inheriting,
            filetype,
            wasi_preopen_path: None,
        }
    }

    pub fn install_transferred(
        &mut self,
        transfers: &[TransferredFd],
        close_on_exec: bool,
    ) -> FdResult<Vec<u32>> {
        if self.entries.len().saturating_add(transfers.len()) > self.max_fds {
            return Err(FdTableError::too_many_open_files());
        }
        let mut candidates = Vec::with_capacity(transfers.len());
        let mut cursor = self.next_fd;
        for _ in transfers {
            let start = usize::try_from(cursor).unwrap_or(0) % self.max_fds;
            let candidate = (0..self.max_fds)
                .map(|offset| ((start + offset) % self.max_fds) as u32)
                .find(|fd| !self.entries.contains_key(fd) && !candidates.contains(fd))
                .ok_or_else(FdTableError::too_many_open_files)?;
            candidates.push(candidate);
            cursor = candidate.saturating_add(1);
        }
        for (fd, transfer) in candidates.iter().copied().zip(transfers) {
            transfer.description.increment_ref_count();
            self.entries.insert(
                fd,
                FdEntry {
                    fd,
                    description: Arc::clone(&transfer.description),
                    status_flags: transfer.status_flags.clone(),
                    fd_flags: if close_on_exec { FD_CLOEXEC } else { 0 },
                    rights: transfer.rights,
                    rights_inheriting: transfer.rights_inheriting,
                    filetype: transfer.filetype,
                    wasi_preopen_path: None,
                },
            );
        }
        self.next_fd = cursor;
        Ok(candidates)
    }

    /// Install a transferred open file description at an exact descriptor.
    /// This is used by spawn staging, where the post-file-action descriptor
    /// numbers are already canonical and must not be allocated a second time.
    pub(crate) fn install_transferred_at(
        &mut self,
        transfer: &TransferredFd,
        fd: u32,
        fd_flags: u32,
    ) -> FdResult<()> {
        self.install_transferred_at_with_preopen(transfer, fd, fd_flags, false)
    }

    /// Restore a descriptor captured during trusted fork/exec staging. Unlike
    /// SCM_RIGHTS receipt, this retains the kernel-owned WASI capability-root
    /// marker attached to the inherited descriptor slot.
    pub(crate) fn install_spawn_transferred_at(
        &mut self,
        transfer: &TransferredFd,
        fd: u32,
        fd_flags: u32,
    ) -> FdResult<()> {
        self.install_transferred_at_with_preopen(transfer, fd, fd_flags, true)
    }

    fn install_transferred_at_with_preopen(
        &mut self,
        transfer: &TransferredFd,
        fd: u32,
        fd_flags: u32,
        preserve_wasi_preopen: bool,
    ) -> FdResult<()> {
        self.validate_fd_bounds(fd)?;
        if self.entries.contains_key(&fd) {
            self.close(fd);
        } else if self.entries.len() >= self.max_fds {
            return Err(FdTableError::too_many_open_files());
        }
        transfer.description.increment_ref_count();
        self.entries.insert(
            fd,
            FdEntry {
                fd,
                description: Arc::clone(&transfer.description),
                status_flags: transfer.status_flags.clone(),
                fd_flags: fd_flags & FD_CLOEXEC,
                rights: transfer.rights,
                rights_inheriting: transfer.rights_inheriting,
                filetype: transfer.filetype,
                wasi_preopen_path: preserve_wasi_preopen
                    .then(|| transfer.wasi_preopen_path.clone())
                    .flatten(),
            },
        );
        Ok(())
    }

    pub fn get(&self, fd: u32) -> Option<&FdEntry> {
        if fd & WASI_HIDDEN_PREOPEN_FD_TAG != 0 {
            return self.hidden_wasi_preopens.get(&fd);
        }
        self.entries.get(&fd)
    }

    pub fn values(&self) -> Values<'_, u32, FdEntry> {
        self.entries.values()
    }

    /// Canonicalize the initial direct-executor descriptor namespace.
    ///
    /// WASI capability roots occupy the stable range beginning at fd 3. All
    /// other inherited descriptors retain their sorted order immediately
    /// after that range. This is the same layout the V8 compatibility adapter
    /// projects, but it is committed once in the kernel so a direct executor
    /// never needs a shadow fd table. This operation is only valid before
    /// guest code starts.
    pub fn canonicalize_initial_wasi_layout(&mut self) -> FdResult<()> {
        if self.canonical_wasi_layout_initialized {
            return Ok(());
        }
        let preopen_fds = self
            .entries
            .values()
            .filter(|entry| entry.wasi_preopen_path.is_some())
            .map(|entry| entry.fd)
            .collect::<Vec<_>>();
        if preopen_fds.is_empty() {
            self.canonical_wasi_layout_initialized = true;
            return Ok(());
        }
        let inherited_fds = self
            .entries
            .values()
            .filter(|entry| entry.fd > 2 && entry.wasi_preopen_path.is_none())
            .map(|entry| entry.fd)
            .collect::<Vec<_>>();
        let required = 3usize
            .checked_add(preopen_fds.len())
            .and_then(|value| value.checked_add(inherited_fds.len()))
            .ok_or_else(FdTableError::too_many_open_files)?;
        if required > self.max_fds {
            return Err(FdTableError::too_many_open_files());
        }

        let mut targets = BTreeMap::new();
        for fd in self.entries.keys().copied().filter(|fd| *fd <= 2) {
            targets.insert(fd, fd);
        }
        for (index, fd) in preopen_fds.into_iter().enumerate() {
            targets.insert(fd, 3 + index as u32);
        }
        let inherited_base = 3 + targets.values().filter(|fd| **fd > 2).count() as u32;
        for (index, fd) in inherited_fds.into_iter().enumerate() {
            targets.insert(fd, inherited_base + index as u32);
        }

        let entries = std::mem::take(&mut self.entries);
        for (source, mut entry) in entries {
            let target = targets.get(&source).copied().ok_or_else(|| {
                FdTableError::invalid_argument(format!(
                    "initial WASI layout omitted descriptor {source}"
                ))
            })?;
            entry.fd = target;
            if self.entries.insert(target, entry).is_some() {
                return Err(FdTableError::invalid_argument(format!(
                    "initial WASI layout assigned descriptor {target} twice"
                )));
            }
        }
        self.next_fd = (3..self.max_fds as u32)
            .find(|fd| !self.entries.contains_key(fd))
            .unwrap_or_default();
        for entry in self
            .entries
            .values()
            .filter(|entry| entry.wasi_preopen_path.is_some())
        {
            let hidden_fd = WASI_HIDDEN_PREOPEN_FD_TAG | entry.fd;
            entry.description.increment_ref_count();
            let mut hidden = entry.clone();
            hidden.fd = hidden_fd;
            self.hidden_wasi_preopens.insert(hidden_fd, hidden);
        }
        self.canonical_wasi_layout_initialized = true;
        Ok(())
    }

    /// Mark the one-time installation of WASI capability roots for this
    /// process image. Forked processes inherit this state so spawn file
    /// actions that deliberately close a preopen cannot cause it to be
    /// silently recreated later by an executor.
    pub fn begin_wasi_preopen_initialization(&mut self) -> bool {
        if self.wasi_preopens_initialized {
            return false;
        }
        self.wasi_preopens_initialized = true;
        true
    }

    pub fn mark_wasi_preopen(
        &mut self,
        fd: u32,
        guest_path: String,
        rights_base: u64,
        rights_inheriting: u64,
    ) -> FdResult<()> {
        let entry = self
            .entries
            .get_mut(&fd)
            .ok_or_else(|| FdTableError::bad_file_descriptor(fd))?;
        if entry.filetype != FILETYPE_DIRECTORY {
            return Err(FdTableError::invalid_argument(format!(
                "WASI preopen fd {fd} is not a directory"
            )));
        }
        entry.rights = rights_base;
        entry.rights_inheriting = rights_inheriting;
        entry.wasi_preopen_path = Some(guest_path);
        Ok(())
    }

    pub fn set_rights(
        &mut self,
        fd: u32,
        rights_base: u64,
        rights_inheriting: u64,
    ) -> FdResult<()> {
        let entry = self
            .entries
            .get_mut(&fd)
            .ok_or_else(|| FdTableError::bad_file_descriptor(fd))?;
        entry.rights = rights_base;
        entry.rights_inheriting = rights_inheriting;
        Ok(())
    }

    /// Restrict inherited capability roots after a child tier drop or exec.
    /// Isolated processes close inherited preopen descriptors entirely so
    /// neither preopen enumeration nor fd snapshots reveal a hidden root.
    pub fn restrict_wasi_preopens(
        &mut self,
        rights_base: Option<u64>,
        rights_inheriting: Option<u64>,
    ) {
        if rights_base.is_none() || rights_inheriting.is_none() {
            let preopen_fds = self
                .entries
                .values()
                .filter_map(|entry| entry.wasi_preopen_path.as_ref().map(|_| entry.fd))
                .collect::<Vec<_>>();
            for fd in preopen_fds {
                let closed = self.close(fd);
                debug_assert!(closed);
            }
            for (_, entry) in std::mem::take(&mut self.hidden_wasi_preopens) {
                entry.description.decrement_ref_count();
            }
            return;
        }
        let base = rights_base.expect("checked above");
        let inheriting = rights_inheriting.expect("checked above");
        for entry in self.entries.values_mut() {
            if entry.wasi_preopen_path.is_none() {
                continue;
            }
            entry.rights &= base;
            entry.rights_inheriting &= inheriting;
        }
        for entry in self.hidden_wasi_preopens.values_mut() {
            entry.rights &= base;
            entry.rights_inheriting &= inheriting;
        }
    }

    pub fn close(&mut self, fd: u32) -> bool {
        let Some(entry) = self.entries.remove(&fd) else {
            return false;
        };
        entry.description.decrement_ref_count();
        true
    }

    /// File descriptors that must be closed when the current process image is
    /// replaced by exec(2). The caller performs the closes through the kernel
    /// so pipe/socket lifecycle accounting receives the same notifications as
    /// an explicit close(2).
    pub fn close_on_exec_fds(&self) -> Vec<u32> {
        self.entries
            .iter()
            .filter_map(|(fd, entry)| (entry.fd_flags & FD_CLOEXEC != 0).then_some(*fd))
            .collect()
    }

    pub fn dup(&mut self, fd: u32) -> FdResult<u32> {
        self.dup_with_status_flags(fd, None)
    }

    pub fn dup_with_status_flags(
        &mut self,
        fd: u32,
        status_flags_override: Option<u32>,
    ) -> FdResult<u32> {
        let entry = self
            .entries
            .get(&fd)
            .cloned()
            .ok_or_else(|| FdTableError::bad_file_descriptor(fd))?;
        let new_fd = self.allocate_fd()?;
        self.duplicate_entry(
            &entry,
            new_fd,
            status_flags_override
                .map(SharedStatusFlags::new)
                .unwrap_or_else(|| entry.status_flags.clone()),
            0,
        )
    }

    pub fn dup2(&mut self, old_fd: u32, new_fd: u32) -> FdResult<()> {
        let entry = self
            .entries
            .get(&old_fd)
            .cloned()
            .ok_or_else(|| FdTableError::bad_file_descriptor(old_fd))?;
        self.validate_fd_bounds(new_fd)?;
        if old_fd == new_fd {
            return Ok(());
        }

        if self.entries.contains_key(&new_fd) {
            self.close(new_fd);
        } else if self.entries.len() >= self.max_fds {
            return Err(FdTableError::too_many_open_files());
        }

        self.duplicate_entry(&entry, new_fd, entry.status_flags.clone(), 0)?;
        Ok(())
    }

    pub fn stat(&self, fd: u32) -> FdResult<FdStat> {
        let entry = self
            .get(fd)
            .ok_or_else(|| FdTableError::bad_file_descriptor(fd))?;
        Ok(FdStat {
            filetype: entry.filetype,
            flags: visible_fd_flags(entry.description.flags(), entry.status_flags.get()),
            rights: entry.rights,
            rights_inheriting: entry.rights_inheriting,
            wasi_preopen_path: entry.wasi_preopen_path.clone(),
        })
    }

    pub fn fcntl(&mut self, fd: u32, command: u32, arg: u32) -> FdResult<u32> {
        match command {
            F_DUPFD => {
                let entry = self
                    .entries
                    .get(&fd)
                    .cloned()
                    .ok_or_else(|| FdTableError::bad_file_descriptor(fd))?;
                let min_fd = self.validate_fcntl_dup_min(arg)?;
                let new_fd = self.allocate_fd_from(min_fd)?;
                self.duplicate_entry(&entry, new_fd, entry.status_flags.clone(), 0)
            }
            F_GETFD => {
                let entry = self
                    .entries
                    .get(&fd)
                    .ok_or_else(|| FdTableError::bad_file_descriptor(fd))?;
                Ok(entry.fd_flags & FD_CLOEXEC)
            }
            F_SETFD => {
                let entry = self
                    .entries
                    .get_mut(&fd)
                    .ok_or_else(|| FdTableError::bad_file_descriptor(fd))?;
                entry.fd_flags = arg & FD_CLOEXEC;
                Ok(0)
            }
            F_GETFL => {
                let entry = self
                    .entries
                    .get(&fd)
                    .ok_or_else(|| FdTableError::bad_file_descriptor(fd))?;
                Ok(visible_fd_flags(
                    entry.description.flags(),
                    entry.status_flags.get(),
                ))
            }
            F_SETFL => {
                let entry = self
                    .entries
                    .get_mut(&fd)
                    .ok_or_else(|| FdTableError::bad_file_descriptor(fd))?;
                entry.status_flags.set(arg);
                entry.description.update_flags(SHARED_STATUS_FLAG_MASK, arg);
                Ok(0)
            }
            _ => Err(FdTableError::invalid_argument(format!(
                "unsupported fcntl command {command}"
            ))),
        }
    }

    pub fn fork(&self) -> Self {
        self.fork_with_cloexec(false)
    }

    /// Clone this descriptor table at the fork half of a deferred exec.
    ///
    /// Linux keeps `FD_CLOEXEC` descriptors across `fork(2)` so spawn file
    /// actions can use them as sources. The exec half closes any descriptors
    /// that remain marked after those actions have run.
    pub fn fork_preserving_cloexec(&self) -> Self {
        self.fork_with_cloexec(true)
    }

    fn fork_with_cloexec(&self, preserve_cloexec: bool) -> Self {
        let mut child = Self::new(self.alloc_desc.clone(), self.max_fds);
        child.next_fd = self.next_fd;
        child.wasi_preopens_initialized = self.wasi_preopens_initialized;
        child.canonical_wasi_layout_initialized = self.canonical_wasi_layout_initialized;

        for (fd, entry) in &self.entries {
            // Kernel process creation is spawn (fork + exec combined), so
            // close-on-exec descriptors must not leak into the child. This
            // matters for pipe write ends: an inherited writer keeps the
            // pipe's writer refcount above zero forever, so a blocked reader
            // (for example a grandchild sharing the parent's stdin pipe)
            // would never observe EOF.
            if !preserve_cloexec && entry.fd_flags & FD_CLOEXEC != 0 {
                continue;
            }
            entry.description.increment_ref_count();
            child.entries.insert(
                *fd,
                FdEntry {
                    fd: *fd,
                    description: Arc::clone(&entry.description),
                    status_flags: entry.status_flags.clone(),
                    fd_flags: entry.fd_flags,
                    rights: entry.rights,
                    rights_inheriting: entry.rights_inheriting,
                    filetype: entry.filetype,
                    wasi_preopen_path: entry.wasi_preopen_path.clone(),
                },
            );
        }
        for (fd, entry) in &self.hidden_wasi_preopens {
            entry.description.increment_ref_count();
            child.hidden_wasi_preopens.insert(*fd, entry.clone());
        }

        child
    }

    pub fn len_for_exec(&self) -> usize {
        self.entries
            .values()
            .filter(|entry| entry.fd_flags & FD_CLOEXEC == 0)
            .count()
    }

    pub fn close_all(&mut self) {
        let fds: Vec<u32> = self.entries.keys().copied().collect();
        for fd in fds {
            self.close(fd);
        }
        for (_, entry) in std::mem::take(&mut self.hidden_wasi_preopens) {
            entry.description.decrement_ref_count();
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> Values<'_, u32, FdEntry> {
        self.entries.values()
    }

    fn allocate_fd(&mut self) -> FdResult<u32> {
        if self.entries.len() >= self.max_fds {
            return Err(FdTableError::too_many_open_files());
        }

        let start = usize::try_from(self.next_fd).unwrap_or(0) % self.max_fds;
        for offset in 0..self.max_fds {
            let candidate = ((start + offset) % self.max_fds) as u32;
            if !self.entries.contains_key(&candidate) {
                self.next_fd = candidate.saturating_add(1);
                return Ok(candidate);
            }
        }

        Err(FdTableError::too_many_open_files())
    }

    fn allocate_fd_from(&mut self, min_fd: u32) -> FdResult<u32> {
        if self.entries.len() >= self.max_fds {
            return Err(FdTableError::too_many_open_files());
        }

        if min_fd as usize >= self.max_fds {
            return Err(FdTableError::invalid_argument(format!(
                "fd {min_fd} exceeds process fd limit"
            )));
        }

        for candidate in min_fd..self.max_fds as u32 {
            if !self.entries.contains_key(&candidate) {
                self.next_fd = candidate.saturating_add(1);
                return Ok(candidate);
            }
        }

        Err(FdTableError::too_many_open_files())
    }

    fn duplicate_entry(
        &mut self,
        entry: &FdEntry,
        new_fd: u32,
        status_flags: SharedStatusFlags,
        fd_flags: u32,
    ) -> FdResult<u32> {
        entry.description.increment_ref_count();
        self.entries.insert(
            new_fd,
            FdEntry {
                fd: new_fd,
                description: Arc::clone(&entry.description),
                status_flags,
                fd_flags,
                rights: entry.rights,
                rights_inheriting: entry.rights_inheriting,
                filetype: entry.filetype,
                wasi_preopen_path: entry.wasi_preopen_path.clone(),
            },
        );
        Ok(new_fd)
    }

    fn validate_fd_bounds(&self, fd: u32) -> FdResult<()> {
        if fd as usize >= self.max_fds {
            return Err(FdTableError::bad_file_descriptor(fd));
        }
        Ok(())
    }

    fn validate_fcntl_dup_min(&self, min_fd: u32) -> FdResult<u32> {
        if min_fd as usize >= self.max_fds {
            return Err(FdTableError::invalid_argument(format!(
                "fd {min_fd} exceeds process fd limit"
            )));
        }
        Ok(min_fd)
    }
}

fn description_flags(flags: u32) -> u32 {
    flags & !status_flags(flags)
}

fn status_flags(flags: u32) -> u32 {
    flags & ENTRY_STATUS_FLAG_MASK
}

fn visible_fd_flags(description_flags: u32, entry_status_flags: u32) -> u32 {
    (description_flags & (0b11 | SHARED_STATUS_FLAG_MASK))
        | (entry_status_flags & ENTRY_STATUS_FLAG_MASK)
}

const SHARED_STATUS_FLAG_MASK: u32 = O_APPEND;
const ENTRY_STATUS_FLAG_MASK: u32 = O_NONBLOCK;

impl<'a> IntoIterator for &'a ProcessFdTable {
    type Item = &'a FdEntry;
    type IntoIter = Values<'a, u32, FdEntry>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.values()
    }
}

#[derive(Debug, Clone)]
pub struct FdTableManager {
    tables: BTreeMap<u32, ProcessFdTable>,
    alloc_desc: DescriptionFactory,
    max_fds: usize,
}

impl Default for FdTableManager {
    fn default() -> Self {
        Self {
            tables: BTreeMap::new(),
            alloc_desc: DescriptionFactory::new(1),
            max_fds: MAX_FDS_PER_PROCESS,
        }
    }
}

impl FdTableManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_max_fds(max_fds: usize) -> Self {
        Self {
            max_fds,
            ..Self::default()
        }
    }

    pub fn create(&mut self, pid: u32) -> &mut ProcessFdTable {
        let mut table = ProcessFdTable::new(self.alloc_desc.clone(), self.max_fds);
        table.init_stdio(
            self.alloc_desc.allocate("/dev/stdin", O_RDONLY),
            self.alloc_desc.allocate("/dev/stdout", O_WRONLY),
            self.alloc_desc.allocate("/dev/stderr", O_WRONLY),
        );
        self.remove(pid);
        self.tables.insert(pid, table);
        self.tables
            .get_mut(&pid)
            .expect("newly created FD table should be stored")
    }

    pub fn create_with_stdio(
        &mut self,
        pid: u32,
        stdin_override: Option<StdioOverride>,
        stdout_override: Option<StdioOverride>,
        stderr_override: Option<StdioOverride>,
    ) -> &mut ProcessFdTable {
        let mut table = ProcessFdTable::new(self.alloc_desc.clone(), self.max_fds);
        let stdin_desc = stdin_override
            .as_ref()
            .map(|entry| Arc::clone(&entry.description))
            .unwrap_or_else(|| self.alloc_desc.allocate("/dev/stdin", O_RDONLY));
        let stdout_desc = stdout_override
            .as_ref()
            .map(|entry| Arc::clone(&entry.description))
            .unwrap_or_else(|| self.alloc_desc.allocate("/dev/stdout", O_WRONLY));
        let stderr_desc = stderr_override
            .as_ref()
            .map(|entry| Arc::clone(&entry.description))
            .unwrap_or_else(|| self.alloc_desc.allocate("/dev/stderr", O_WRONLY));

        table.init_stdio_with_types(
            stdin_desc,
            stdin_override
                .as_ref()
                .map(|entry| entry.filetype)
                .unwrap_or(FILETYPE_CHARACTER_DEVICE),
            stdout_desc,
            stdout_override
                .as_ref()
                .map(|entry| entry.filetype)
                .unwrap_or(FILETYPE_CHARACTER_DEVICE),
            stderr_desc,
            stderr_override
                .as_ref()
                .map(|entry| entry.filetype)
                .unwrap_or(FILETYPE_CHARACTER_DEVICE),
        );
        self.remove(pid);
        self.tables.insert(pid, table);
        self.tables
            .get_mut(&pid)
            .expect("newly created FD table should be stored")
    }

    pub fn fork(&mut self, parent_pid: u32, child_pid: u32) -> &mut ProcessFdTable {
        self.fork_with_cloexec(parent_pid, child_pid, false)
    }

    pub fn fork_preserving_cloexec(
        &mut self,
        parent_pid: u32,
        child_pid: u32,
    ) -> &mut ProcessFdTable {
        self.fork_with_cloexec(parent_pid, child_pid, true)
    }

    fn fork_with_cloexec(
        &mut self,
        parent_pid: u32,
        child_pid: u32,
        preserve_cloexec: bool,
    ) -> &mut ProcessFdTable {
        if !self.tables.contains_key(&parent_pid) {
            return self.create(child_pid);
        }

        let parent = self
            .tables
            .get(&parent_pid)
            .expect("parent table presence was checked");
        let child = if preserve_cloexec {
            parent.fork_preserving_cloexec()
        } else {
            parent.fork()
        };
        self.remove(child_pid);
        self.tables.insert(child_pid, child);
        self.tables
            .get_mut(&child_pid)
            .expect("forked FD table should be stored")
    }

    pub fn get(&self, pid: u32) -> Option<&ProcessFdTable> {
        self.tables.get(&pid)
    }

    pub fn get_mut(&mut self, pid: u32) -> Option<&mut ProcessFdTable> {
        self.tables.get_mut(&pid)
    }

    pub fn has(&self, pid: u32) -> bool {
        self.tables.contains_key(&pid)
    }

    pub fn len(&self) -> usize {
        self.tables.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tables.is_empty()
    }

    pub fn total_open_fds(&self) -> usize {
        self.tables.values().map(ProcessFdTable::len).sum()
    }

    pub fn pids(&self) -> Vec<u32> {
        self.tables.keys().copied().collect()
    }

    pub fn remove(&mut self, pid: u32) {
        if let Some(mut table) = self.tables.remove(&pid) {
            table.close_all();
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct FileLockManager {
    inner: Arc<FileLockManagerInner>,
}

#[derive(Debug)]
struct FileLockManagerInner {
    state: Mutex<FileLockState>,
    wake: Condvar,
    max_record_locks: usize,
}

impl Default for FileLockManagerInner {
    fn default() -> Self {
        Self {
            state: Mutex::new(FileLockState::default()),
            wake: Condvar::new(),
            max_record_locks: DEFAULT_MAX_RECORD_LOCKS,
        }
    }
}

#[derive(Debug, Default)]
struct FileLockState {
    entries: BTreeMap<FileLockTarget, FileLockEntry>,
    record_locks: Vec<(FileLockTarget, RecordLock)>,
    record_lock_waits: BTreeMap<u32, RecordLockWait>,
    warned_near_record_lock_limit: bool,
    warned_near_record_lock_wait_limit: bool,
}

#[derive(Debug, Clone)]
struct RecordLockWait {
    target: FileLockTarget,
    request: RecordLock,
    blockers: BTreeSet<u32>,
}

#[derive(Debug, Default)]
struct FileLockEntry {
    shared: BTreeSet<u64>,
    exclusive: Option<u64>,
}

impl FileLockManager {
    pub fn new() -> Self {
        Self::with_record_lock_limit(DEFAULT_MAX_RECORD_LOCKS)
    }

    pub fn with_record_lock_limit(max_record_locks: usize) -> Self {
        Self {
            inner: Arc::new(FileLockManagerInner {
                state: Mutex::new(FileLockState::default()),
                wake: Condvar::new(),
                max_record_locks: max_record_locks.max(1),
            }),
        }
    }

    pub fn apply(
        &self,
        owner_id: u64,
        target: FileLockTarget,
        operation: FlockOperation,
    ) -> FdResult<()> {
        match operation {
            FlockOperation::Shared { nonblocking } => {
                self.acquire(owner_id, target, FileLockMode::Shared, nonblocking)
            }
            FlockOperation::Exclusive { nonblocking } => {
                self.acquire(owner_id, target, FileLockMode::Exclusive, nonblocking)
            }
            FlockOperation::Unlock => {
                self.release_owner(owner_id);
                Ok(())
            }
        }
    }

    pub fn release_owner(&self, owner_id: u64) -> bool {
        let mut state = lock_or_recover(&self.inner.state);
        let mut released = false;
        state.entries.retain(|_, entry| {
            let entry_changed = entry.shared.remove(&owner_id) || entry.exclusive == Some(owner_id);
            if entry.exclusive == Some(owner_id) {
                entry.exclusive = None;
            }
            released |= entry_changed;
            !entry.is_empty()
        });
        drop(state);
        if released {
            self.inner.wake.notify_all();
        }
        released
    }

    pub fn query_record_lock(
        &self,
        target: FileLockTarget,
        request: RecordLock,
    ) -> Option<RecordLock> {
        let state = lock_or_recover(&self.inner.state);
        state
            .record_locks
            .iter()
            .filter_map(|(candidate_target, candidate)| {
                (*candidate_target == target
                    && candidate.pid != request.pid
                    && record_locks_conflict(*candidate, request))
                .then_some(*candidate)
            })
            .min_by_key(|candidate| (candidate.start, candidate.pid))
    }

    pub fn set_record_lock(&self, target: FileLockTarget, request: RecordLock) -> FdResult<()> {
        self.set_record_lock_inner(target, request, false)
    }

    pub fn set_blocking_record_lock(
        &self,
        target: FileLockTarget,
        request: RecordLock,
    ) -> FdResult<()> {
        self.set_record_lock_inner(target, request, true)
    }

    fn set_record_lock_inner(
        &self,
        target: FileLockTarget,
        request: RecordLock,
        blocking: bool,
    ) -> FdResult<()> {
        let mut state = lock_or_recover(&self.inner.state);
        let blockers = if request.lock_type == RecordLockType::Unlock {
            BTreeSet::new()
        } else {
            conflicting_record_lock_pids(&state.record_locks, target, request)
        };
        if !blockers.is_empty() {
            if blocking {
                self.register_record_lock_wait(&mut state, target, request, blockers)?;
            } else {
                state.record_lock_waits.remove(&request.pid);
            }
            return Err(FdTableError::would_block(
                "POSIX record lock is held by another process",
            ));
        }

        state.record_lock_waits.remove(&request.pid);

        let mut next = Vec::with_capacity(state.record_locks.len().saturating_add(2));
        for (candidate_target, candidate) in state.record_locks.iter().copied() {
            if candidate_target != target
                || candidate.pid != request.pid
                || !record_ranges_overlap(candidate, request)
            {
                next.push((candidate_target, candidate));
                continue;
            }

            if candidate.start < request.start {
                next.push((
                    target,
                    RecordLock {
                        end: Some(request.start),
                        ..candidate
                    },
                ));
            }
            if let Some(request_end) = request.end {
                if candidate
                    .end
                    .is_none_or(|candidate_end| candidate_end > request_end)
                {
                    next.push((
                        target,
                        RecordLock {
                            start: request_end,
                            ..candidate
                        },
                    ));
                }
            }
        }

        if request.lock_type != RecordLockType::Unlock {
            next.push((target, request));
        }
        coalesce_record_locks(&mut next);
        let warning_threshold = (self.inner.max_record_locks.saturating_mul(4) / 5).max(1);
        if !state.warned_near_record_lock_limit && next.len() >= warning_threshold {
            state.warned_near_record_lock_limit = true;
            eprintln!(
                "[agentos] POSIX record lock usage {}/{} is near the limit derived from limits.resources.maxOpenFds; raise limits.resources.maxOpenFds if needed",
                next.len(), self.inner.max_record_locks
            );
        }
        if next.len() > self.inner.max_record_locks {
            return Err(FdTableError {
                code: "ENOLCK",
                message: format!(
                    "POSIX record lock table limit ({}) derived from limits.resources.maxOpenFds reached; raise limits.resources.maxOpenFds if needed",
                    self.inner.max_record_locks
                ),
            });
        }
        state.record_locks = next;
        refresh_record_lock_waits(&mut state);
        drop(state);
        self.inner.wake.notify_all();
        Ok(())
    }

    pub fn cancel_record_lock_wait(&self, pid: u32) -> bool {
        let mut state = lock_or_recover(&self.inner.state);
        state.record_lock_waits.remove(&pid).is_some()
    }

    /// POSIX process-associated locks are all discarded when the process
    /// closes any descriptor referring to the same file.
    pub fn release_process_target(&self, pid: u32, target: FileLockTarget) -> bool {
        let mut state = lock_or_recover(&self.inner.state);
        let previous = state.record_locks.len();
        state
            .record_locks
            .retain(|(candidate_target, lock)| *candidate_target != target || lock.pid != pid);
        let wait_cancelled = state.record_lock_waits.remove(&pid).is_some();
        let released = state.record_locks.len() != previous;
        if released || wait_cancelled {
            refresh_record_lock_waits(&mut state);
        }
        drop(state);
        if released || wait_cancelled {
            self.inner.wake.notify_all();
        }
        released || wait_cancelled
    }

    pub fn release_process(&self, pid: u32) -> bool {
        let mut state = lock_or_recover(&self.inner.state);
        let previous = state.record_locks.len();
        state.record_locks.retain(|(_, lock)| lock.pid != pid);
        let wait_cancelled = state.record_lock_waits.remove(&pid).is_some();
        let released = state.record_locks.len() != previous;
        if released || wait_cancelled {
            refresh_record_lock_waits(&mut state);
        }
        drop(state);
        if released || wait_cancelled {
            self.inner.wake.notify_all();
        }
        released || wait_cancelled
    }

    fn register_record_lock_wait(
        &self,
        state: &mut FileLockState,
        target: FileLockTarget,
        request: RecordLock,
        blockers: BTreeSet<u32>,
    ) -> FdResult<()> {
        let new_waiter = !state.record_lock_waits.contains_key(&request.pid);
        let waiter_count = state.record_lock_waits.len() + usize::from(new_waiter);
        if new_waiter && waiter_count > self.inner.max_record_locks {
            return Err(FdTableError {
                code: "ENOLCK",
                message: format!(
                    "POSIX record lock waiter limit ({}) derived from limits.resources.maxOpenFds reached; raise limits.resources.maxOpenFds if needed",
                    self.inner.max_record_locks
                ),
            });
        }
        let warning_threshold = (self.inner.max_record_locks.saturating_mul(4) / 5).max(1);
        if !state.warned_near_record_lock_wait_limit && waiter_count >= warning_threshold {
            state.warned_near_record_lock_wait_limit = true;
            eprintln!(
                "[agentos] POSIX record lock waiter usage {}/{} is near the limit derived from limits.resources.maxOpenFds; raise limits.resources.maxOpenFds if needed",
                waiter_count,
                self.inner.max_record_locks
            );
        }
        state.record_lock_waits.insert(
            request.pid,
            RecordLockWait {
                target,
                request,
                blockers,
            },
        );
        if record_lock_wait_cycle(state, request.pid) {
            state.record_lock_waits.remove(&request.pid);
            return Err(FdTableError::deadlock(
                "POSIX record lock wait would create a deadlock",
            ));
        }
        Ok(())
    }

    fn acquire(
        &self,
        owner_id: u64,
        target: FileLockTarget,
        mode: FileLockMode,
        nonblocking: bool,
    ) -> FdResult<()> {
        let mut state = lock_or_recover(&self.inner.state);
        loop {
            let entry = state.entries.entry(target).or_default();
            if entry.can_grant(owner_id, mode) {
                entry.grant(owner_id, mode);
                return Ok(());
            }

            if nonblocking {
                return Err(FdTableError::would_block(
                    "advisory file lock is unavailable",
                ));
            }

            state = wait_or_recover(&self.inner.wake, state);
        }
    }
}

fn record_ranges_overlap(left: RecordLock, right: RecordLock) -> bool {
    left.end.is_none_or(|end| right.start < end) && right.end.is_none_or(|end| left.start < end)
}

fn record_locks_conflict(left: RecordLock, right: RecordLock) -> bool {
    record_ranges_overlap(left, right)
        && (left.lock_type == RecordLockType::Write || right.lock_type == RecordLockType::Write)
}

fn conflicting_record_lock_pids(
    locks: &[(FileLockTarget, RecordLock)],
    target: FileLockTarget,
    request: RecordLock,
) -> BTreeSet<u32> {
    locks
        .iter()
        .filter_map(|(candidate_target, candidate)| {
            (*candidate_target == target
                && candidate.pid != request.pid
                && record_locks_conflict(*candidate, request))
            .then_some(candidate.pid)
        })
        .collect()
}

fn refresh_record_lock_waits(state: &mut FileLockState) {
    let FileLockState {
        record_locks,
        record_lock_waits,
        ..
    } = state;
    for wait in record_lock_waits.values_mut() {
        wait.blockers = conflicting_record_lock_pids(record_locks, wait.target, wait.request);
    }
}

fn record_lock_wait_cycle(state: &FileLockState, requester: u32) -> bool {
    let Some(wait) = state.record_lock_waits.get(&requester) else {
        return false;
    };
    let mut pending = wait.blockers.iter().copied().collect::<Vec<_>>();
    let mut visited = BTreeSet::new();
    while let Some(pid) = pending.pop() {
        if pid == requester {
            return true;
        }
        if !visited.insert(pid) {
            continue;
        }
        if let Some(wait) = state.record_lock_waits.get(&pid) {
            pending.extend(wait.blockers.iter().copied());
        }
    }
    false
}

fn coalesce_record_locks(locks: &mut Vec<(FileLockTarget, RecordLock)>) {
    locks.sort_by_key(|(target, lock)| (*target, lock.pid, lock.lock_type as u8, lock.start));
    let mut merged: Vec<(FileLockTarget, RecordLock)> = Vec::with_capacity(locks.len());
    for (target, lock) in locks.drain(..) {
        if let Some((previous_target, previous)) = merged.last_mut() {
            let touches = previous.end.is_none_or(|end| lock.start <= end);
            if *previous_target == target
                && previous.pid == lock.pid
                && previous.lock_type == lock.lock_type
                && touches
            {
                previous.end = match (previous.end, lock.end) {
                    (None, _) | (_, None) => None,
                    (Some(left), Some(right)) => Some(left.max(right)),
                };
                continue;
            }
        }
        merged.push((target, lock));
    }
    *locks = merged;
}

impl FileLockEntry {
    fn can_grant(&self, owner_id: u64, mode: FileLockMode) -> bool {
        match mode {
            FileLockMode::Shared => self.exclusive.is_none_or(|owner| owner == owner_id),
            FileLockMode::Exclusive => {
                self.exclusive.is_none_or(|owner| owner == owner_id)
                    && self.shared.iter().all(|owner| *owner == owner_id)
            }
        }
    }

    fn grant(&mut self, owner_id: u64, mode: FileLockMode) {
        match mode {
            FileLockMode::Shared => {
                self.exclusive = None;
                self.shared.insert(owner_id);
            }
            FileLockMode::Exclusive => {
                self.shared.retain(|owner| *owner != owner_id);
                self.exclusive = Some(owner_id);
            }
        }
    }

    fn is_empty(&self) -> bool {
        self.exclusive.is_none() && self.shared.is_empty()
    }
}

fn lock_or_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn wait_or_recover<'a, T>(condvar: &Condvar, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
    condvar
        .wait(guard)
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod wasi_preopen_tests {
    use super::*;

    #[test]
    fn kernel_preopen_metadata_follows_descriptor_lifecycle() {
        let mut manager = FdTableManager::new();
        let table = manager.create(7);
        assert!(table.begin_wasi_preopen_initialization());
        assert!(!table.begin_wasi_preopen_initialization());

        let fd = table
            .open_with_details("/workspace", O_DIRECTORY, FILETYPE_DIRECTORY, None)
            .expect("open preopen directory");
        table
            .mark_wasi_preopen(
                fd,
                String::from("/workspace"),
                WASI_PREOPEN_READ_RIGHTS_BASE,
                WASI_PREOPEN_READ_RIGHTS_INHERITING,
            )
            .expect("mark preopen");

        let stat = table.stat(fd).expect("stat preopen");
        assert_eq!(stat.rights, WASI_PREOPEN_READ_RIGHTS_BASE);
        assert_eq!(stat.rights_inheriting, WASI_PREOPEN_READ_RIGHTS_INHERITING);
        assert_eq!(stat.wasi_preopen_path.as_deref(), Some("/workspace"));

        let duplicate = table.dup(fd).expect("duplicate preopen");
        assert_eq!(
            table
                .stat(duplicate)
                .expect("stat duplicate")
                .wasi_preopen_path
                .as_deref(),
            Some("/workspace")
        );
        assert!(table.close(fd));
        assert!(table.stat(fd).is_err());
    }

    #[test]
    fn canonical_layout_resolves_collisions_and_protects_hidden_preopen_aliases() {
        let mut manager = FdTableManager::new();
        let table = manager.create(8);
        let inherited_three = table.open("pipe:three", O_RDWR).expect("open fd 3");
        let inherited_four = table.open("pipe:four", O_RDWR).expect("open fd 4");
        assert_eq!((inherited_three, inherited_four), (3, 4));
        let inherited_ids = (
            table.get(3).expect("fd 3").description.id(),
            table.get(4).expect("fd 4").description.id(),
        );
        let preopen = table
            .open_with_details("/", O_DIRECTORY, FILETYPE_DIRECTORY, None)
            .expect("open preopen directory");
        table
            .mark_wasi_preopen(
                preopen,
                String::from("/"),
                WASI_PREOPEN_WRITE_RIGHTS_BASE,
                WASI_PREOPEN_WRITE_RIGHTS_INHERITING,
            )
            .expect("mark preopen");
        let preopen_id = table.get(preopen).expect("preopen").description.id();

        table
            .canonicalize_initial_wasi_layout()
            .expect("canonicalize initial descriptors");
        assert_eq!(
            table.get(3).expect("visible preopen").description.id(),
            preopen_id
        );
        assert_eq!(
            table.get(4).expect("first inherited").description.id(),
            inherited_ids.0
        );
        assert_eq!(
            table.get(5).expect("second inherited").description.id(),
            inherited_ids.1
        );

        let hidden = WASI_HIDDEN_PREOPEN_FD_TAG | 3;
        assert_eq!(
            table.get(hidden).expect("hidden preopen").description.id(),
            preopen_id
        );
        assert!(table.close(3));
        assert!(table.stat(3).is_err());
        assert_eq!(
            table
                .stat(hidden)
                .expect("hidden preopen survives visible close")
                .wasi_preopen_path
                .as_deref(),
            Some("/")
        );

        // Executor initialization is idempotent and must not recreate a guest
        // fd that the process deliberately closed.
        table
            .canonicalize_initial_wasi_layout()
            .expect("repeat canonicalization");
        assert!(table.stat(3).is_err());

        let mut child = table.fork();
        assert_eq!(
            child.stat(hidden).expect("forked hidden preopen").rights,
            WASI_PREOPEN_WRITE_RIGHTS_BASE
        );
        child.restrict_wasi_preopens(None, None);
        assert!(child.stat(hidden).is_err());
    }

    #[test]
    fn descriptor_rights_survive_dup_fork_and_transfer_exactly() {
        let mut manager = FdTableManager::new();
        let table = manager.create(11);
        let fd = table.open("/file", O_RDWR).expect("open descriptor");
        let base = WASI_RIGHT_FD_READ | WASI_RIGHT_FD_FILESTAT_GET;
        table.set_rights(fd, base, 0).expect("set exact rights");

        let duplicate = table.dup(fd).expect("duplicate descriptor");
        assert_eq!(table.stat(duplicate).expect("dup stat").rights, base);

        let child = table.fork();
        assert_eq!(child.stat(fd).expect("fork stat").rights, base);

        let transfer = table.transfer(fd).expect("transfer descriptor");
        let receiver = manager.create(12);
        let installed = receiver
            .install_transferred(&[transfer], false)
            .expect("install transfer");
        assert_eq!(
            receiver.stat(installed[0]).expect("transfer stat").rights,
            base
        );

        let socket = receiver
            .open_with_details("socket:test", O_RDWR, FILETYPE_SOCKET_STREAM, None)
            .expect("open socket descriptor");
        let socket_default = receiver.stat(socket).expect("socket stat").rights;
        assert_eq!(
            socket_default & (WASI_RIGHT_FD_READ | WASI_RIGHT_FD_WRITE),
            WASI_RIGHT_FD_READ | WASI_RIGHT_FD_WRITE
        );
        receiver
            .set_rights(socket, WASI_RIGHT_FD_READ, 0)
            .expect("restrict socket rights");
        let socket_duplicate = receiver.dup(socket).expect("duplicate socket");
        assert_eq!(
            receiver
                .stat(socket_duplicate)
                .expect("socket duplicate stat")
                .rights,
            WASI_RIGHT_FD_READ
        );
    }

    #[test]
    fn linux_pipe_and_socket_descriptors_synthesize_metadata_mutation_rights() {
        for (flags, filetype) in [
            (O_RDONLY, FILETYPE_PIPE),
            (O_WRONLY, FILETYPE_PIPE),
            (O_RDWR, FILETYPE_SOCKET_STREAM),
            (O_RDWR, FILETYPE_SOCKET_DGRAM),
        ] {
            let (rights, inheriting) = wasi_rights_for_open(flags, filetype);
            assert_ne!(
                rights & WASI_RIGHT_FD_FILESTAT_SET_TIMES,
                0,
                "Linux descriptor type {filetype} must permit fchmod/fchown"
            );
            assert_eq!(inheriting, 0);
        }
    }
}
