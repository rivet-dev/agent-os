use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use web_time::{SystemTime, UNIX_EPOCH};

pub const S_IFREG: u32 = 0o100000;
pub const S_IFDIR: u32 = 0o040000;
pub const S_IFLNK: u32 = 0o120000;
pub const S_IFCHR: u32 = 0o020000;
pub const S_IFBLK: u32 = 0o060000;
pub const S_IFIFO: u32 = 0o010000;
pub const RENAME_NOREPLACE: u32 = 1;
pub const RENAME_EXCHANGE: u32 = 2;
pub const RENAME_WHITEOUT: u32 = 4;

// Each MemoryFileSystem instance gets its own device id, like a Linux
// superblock. Inode numbers are only unique within one instance, so layered
// or mounted compositions need distinct dev values for (dev, ino) file
// identity comparisons to be meaningful. The counter starts above the small
// constants reserved for synthetic device and pipe stats.
static NEXT_MEMORY_FILESYSTEM_DEVICE_ID: AtomicU64 = AtomicU64::new(256);
static NEXT_RENAME_EXCHANGE_ID: AtomicU64 = AtomicU64::new(1);

fn allocate_memory_filesystem_device_id() -> u64 {
    NEXT_MEMORY_FILESYSTEM_DEVICE_ID.fetch_add(1, Ordering::Relaxed)
}

const DEFAULT_UID: u32 = 1000;
const DEFAULT_GID: u32 = 1000;
const DIRECTORY_SIZE: u64 = 4096;
pub const MAX_PATH_LENGTH: usize = 4096;
const MAX_SYMLINK_DEPTH: usize = 40;
pub const XATTR_CREATE: u32 = 1;
pub const XATTR_REPLACE: u32 = 2;
pub const XATTR_NAME_MAX: usize = 255;
pub const XATTR_SIZE_MAX: usize = 64 * 1024;
pub const XATTR_LIST_MAX: usize = 64 * 1024;

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

    pub fn permission_denied(op: &'static str, path: &str) -> Self {
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

pub fn validate_xattr_name(name: &str) -> VfsResult<()> {
    if name.is_empty() || name.len() > XATTR_NAME_MAX || !name.contains('.') || name.contains('\0')
    {
        return Err(VfsError::new(
            "EINVAL",
            format!("invalid extended attribute name: {name:?}"),
        ));
    }
    Ok(())
}

pub fn set_xattr_value(
    xattrs: &mut BTreeMap<String, Vec<u8>>,
    name: &str,
    value: &[u8],
    flags: u32,
) -> VfsResult<()> {
    validate_xattr_name(name)?;
    if flags & !(XATTR_CREATE | XATTR_REPLACE) != 0 || flags == (XATTR_CREATE | XATTR_REPLACE) {
        return Err(VfsError::new(
            "EINVAL",
            format!("invalid xattr flags: {flags}"),
        ));
    }
    if value.len() > XATTR_SIZE_MAX {
        return Err(VfsError::new(
            "E2BIG",
            format!(
                "extended attribute value is {} bytes; Linux-compatible limit is {XATTR_SIZE_MAX} bytes",
                value.len()
            ),
        ));
    }
    let exists = xattrs.contains_key(name);
    if flags == XATTR_CREATE && exists {
        return Err(VfsError::new(
            "EEXIST",
            format!("extended attribute already exists: {name}"),
        ));
    }
    if flags == XATTR_REPLACE && !exists {
        return Err(VfsError::new(
            "ENODATA",
            format!("extended attribute does not exist: {name}"),
        ));
    }
    let list_bytes = xattrs.keys().map(|key| key.len() + 1).sum::<usize>();
    let new_total = list_bytes.saturating_add(if exists { 0 } else { name.len() + 1 });
    if new_total > XATTR_LIST_MAX {
        return Err(VfsError::new(
            "ENOSPC",
            format!(
                "inode extended attribute name list requires {new_total} bytes; Linux-compatible limit is {XATTR_LIST_MAX} bytes"
            ),
        ));
    }
    xattrs.insert(name.to_string(), value.to_vec());
    Ok(())
}

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
    pub atime_nsec: u32,
    pub mtime_ms: u64,
    pub mtime_nsec: u32,
    pub ctime_ms: u64,
    pub ctime_nsec: u32,
    pub birthtime_ms: u64,
    pub ino: u64,
    pub nlink: u64,
    pub uid: u32,
    pub gid: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VirtualTimeSpec {
    pub sec: i64,
    pub nsec: u32,
}

/// Inclusive upper timestamp bound for agentOS filesystem metadata.
///
/// agentOS deliberately uses the traditional unsigned 32-bit seconds range:
/// pre-epoch timestamps are rejected and later timestamps clamp to this bound.
pub const AGENTOS_TIMESTAMP_MAX_SECONDS: i64 = u32::MAX as i64;

impl VirtualTimeSpec {
    pub fn new(sec: i64, nsec: u32) -> VfsResult<Self> {
        if nsec >= 1_000_000_000 {
            return Err(VfsError::new(
                "EINVAL",
                format!("timespec nanoseconds out of range: {nsec}"),
            ));
        }
        Ok(Self { sec, nsec })
    }

    pub fn from_millis(ms: u64) -> Self {
        Self {
            sec: (ms / 1_000) as i64,
            nsec: ((ms % 1_000) * 1_000_000) as u32,
        }
    }

    pub fn to_truncated_millis(self) -> VfsResult<u64> {
        if self.sec < 0 {
            return Err(VfsError::new(
                "EINVAL",
                format!(
                    "negative timestamps are not supported by this filesystem: {}",
                    self.sec
                ),
            ));
        }
        let seconds = u64::try_from(self.sec.min(AGENTOS_TIMESTAMP_MAX_SECONDS)).map_err(|_| {
            VfsError::new("EINVAL", format!("timestamp is out of range: {}", self.sec))
        })?;
        Ok(seconds.saturating_mul(1_000) + (self.nsec as u64 / 1_000_000))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtualUtimeSpec {
    Set(VirtualTimeSpec),
    Now,
    Omit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileExtent {
    pub start: u64,
    pub end: u64,
    pub unwritten: bool,
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
    /// Writes caller-owned bytes into the filesystem.
    ///
    /// This raw VFS primitive does not enforce VM resource policy. Kernel entry
    /// points must preflight file sizes and inode growth before calling it.
    fn write_file(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<()>;
    fn write_file_with_mode(
        &mut self,
        path: &str,
        content: impl Into<Vec<u8>>,
        mode: Option<u32>,
    ) -> VfsResult<()> {
        let _ = mode;
        self.write_file(path, content)
    }
    fn create_file_exclusive(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<()> {
        let content = content.into();
        if self.exists(path) {
            return Err(VfsError::already_exists("open", path));
        }
        self.write_file(path, content)
    }
    fn create_file_exclusive_with_mode(
        &mut self,
        path: &str,
        content: impl Into<Vec<u8>>,
        mode: Option<u32>,
    ) -> VfsResult<()> {
        let _ = mode;
        self.create_file_exclusive(path, content)
    }
    /// Appends caller-owned bytes into the filesystem after checking that the
    /// in-memory file can grow without overflowing addressable memory.
    fn append_file(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<u64> {
        let content = content.into();
        let mut existing = self.read_file(path)?;
        reserve_file_growth(&mut existing, content.len())?;
        existing.extend_from_slice(&content);
        let new_len = existing.len() as u64;
        self.write_file(path, existing)?;
        Ok(new_len)
    }
    fn create_dir(&mut self, path: &str) -> VfsResult<()>;
    fn create_dir_with_mode(&mut self, path: &str, mode: Option<u32>) -> VfsResult<()> {
        let _ = mode;
        self.create_dir(path)
    }
    fn mkdir(&mut self, path: &str, recursive: bool) -> VfsResult<()>;
    fn mknod(&mut self, path: &str, mode: u32, rdev: u64) -> VfsResult<()> {
        let _ = (mode, rdev);
        Err(VfsError::new(
            "EOPNOTSUPP",
            format!("special inode creation is not supported for {path}"),
        ))
    }
    fn mkdir_with_mode(&mut self, path: &str, recursive: bool, mode: Option<u32>) -> VfsResult<()> {
        let _ = mode;
        self.mkdir(path, recursive)
    }
    fn exists(&self, path: &str) -> bool;
    fn stat(&mut self, path: &str) -> VfsResult<VirtualStat>;
    fn remove_file(&mut self, path: &str) -> VfsResult<()>;
    fn remove_dir(&mut self, path: &str) -> VfsResult<()>;
    fn rename(&mut self, old_path: &str, new_path: &str) -> VfsResult<()>;
    fn rename_at2(&mut self, old_path: &str, new_path: &str, flags: u32) -> VfsResult<()> {
        match flags {
            0 => self.rename(old_path, new_path),
            RENAME_NOREPLACE => {
                self.lstat(old_path)?;
                match self.lstat(new_path) {
                    Ok(_) => Err(VfsError::new(
                        "EEXIST",
                        format!("file exists, rename '{old_path}' -> '{new_path}'"),
                    )),
                    Err(error) if error.code() == "ENOENT" => self.rename(old_path, new_path),
                    Err(error) => Err(error),
                }
            }
            RENAME_EXCHANGE => {
                self.lstat(old_path)?;
                self.lstat(new_path)?;
                if normalize_path(old_path) == normalize_path(new_path) {
                    return Ok(());
                }

                let parent = dirname(old_path);
                let temporary = (0..128)
                    .find_map(|_| {
                        let id = NEXT_RENAME_EXCHANGE_ID.fetch_add(1, Ordering::Relaxed);
                        let candidate = if parent == "/" {
                            format!("/.agentos-rename-exchange-{id}")
                        } else {
                            format!("{parent}/.agentos-rename-exchange-{id}")
                        };
                        if self
                            .lstat(&candidate)
                            .is_err_and(|error| error.code() == "ENOENT")
                        {
                            Some(candidate)
                        } else {
                            None
                        }
                    })
                    .ok_or_else(|| {
                        VfsError::new(
                            "EEXIST",
                            "could not allocate a bounded temporary rename-exchange path",
                        )
                    })?;

                self.rename(old_path, &temporary)?;
                if let Err(error) = self.rename(new_path, old_path) {
                    return match self.rename(&temporary, old_path) {
                        Ok(()) => Err(error),
                        Err(rollback) => Err(VfsError::new(
                            "EIO",
                            format!("rename exchange failed: {error}; rollback failed: {rollback}"),
                        )),
                    };
                }
                if let Err(error) = self.rename(&temporary, new_path) {
                    let rollback_destination = self.rename(old_path, new_path);
                    let rollback_source = self.rename(&temporary, old_path);
                    return match (rollback_destination, rollback_source) {
                        (Ok(()), Ok(())) => Err(error),
                        (destination, source) => Err(VfsError::new(
                            "EIO",
                            format!(
                                "rename exchange failed: {error}; rollback destination: {destination:?}; rollback source: {source:?}"
                            ),
                        )),
                    };
                }
                Ok(())
            }
            _ => Err(VfsError::new(
                "EINVAL",
                format!("invalid renameat2 flags: {flags:#x}"),
            )),
        }
    }
    fn realpath(&self, path: &str) -> VfsResult<String>;
    fn symlink(&mut self, target: &str, link_path: &str) -> VfsResult<()>;
    fn read_link(&self, path: &str) -> VfsResult<String>;
    fn lstat(&self, path: &str) -> VfsResult<VirtualStat>;
    fn link(&mut self, old_path: &str, new_path: &str) -> VfsResult<()>;
    fn chmod(&mut self, path: &str, mode: u32) -> VfsResult<()>;
    fn chown(&mut self, path: &str, uid: u32, gid: u32) -> VfsResult<()>;
    fn chown_spec(
        &mut self,
        path: &str,
        uid: u32,
        gid: u32,
        follow_symlinks: bool,
    ) -> VfsResult<()> {
        if !follow_symlinks {
            return Err(VfsError::unsupported(format!(
                "lchown is not supported for '{path}'"
            )));
        }
        self.chown(path, uid, gid)
    }

    fn lchown(&mut self, path: &str, uid: u32, gid: u32) -> VfsResult<()> {
        self.chown(path, uid, gid)
    }
    fn get_xattr(&mut self, path: &str, name: &str, follow_symlinks: bool) -> VfsResult<Vec<u8>> {
        let _ = (name, follow_symlinks);
        Err(VfsError::new(
            "EOPNOTSUPP",
            format!("extended attributes are not supported for {path}"),
        ))
    }
    fn list_xattrs(&mut self, path: &str, follow_symlinks: bool) -> VfsResult<Vec<String>> {
        let _ = follow_symlinks;
        Err(VfsError::new(
            "EOPNOTSUPP",
            format!("extended attributes are not supported for {path}"),
        ))
    }
    fn set_xattr(
        &mut self,
        path: &str,
        name: &str,
        value: Vec<u8>,
        flags: u32,
        follow_symlinks: bool,
    ) -> VfsResult<()> {
        let _ = (name, value, flags, follow_symlinks);
        Err(VfsError::new(
            "EOPNOTSUPP",
            format!("extended attributes are not supported for {path}"),
        ))
    }
    fn remove_xattr(&mut self, path: &str, name: &str, follow_symlinks: bool) -> VfsResult<()> {
        let _ = (name, follow_symlinks);
        Err(VfsError::new(
            "EOPNOTSUPP",
            format!("extended attributes are not supported for {path}"),
        ))
    }
    fn utimes(&mut self, path: &str, atime_ms: u64, mtime_ms: u64) -> VfsResult<()>;
    fn utimes_spec(
        &mut self,
        path: &str,
        atime: VirtualUtimeSpec,
        mtime: VirtualUtimeSpec,
        follow_symlinks: bool,
    ) -> VfsResult<()> {
        if !follow_symlinks {
            return Err(VfsError::unsupported(format!(
                "lutimes is not supported for '{path}'"
            )));
        }
        let existing = match (atime, mtime) {
            (VirtualUtimeSpec::Omit, _) | (_, VirtualUtimeSpec::Omit) => Some(self.stat(path)?),
            _ => None,
        };
        let now = now_ms();
        let atime_ms = resolve_utime_millis(
            atime,
            now,
            existing.as_ref().map(|stat| VirtualTimeSpec {
                sec: (stat.atime_ms / 1_000) as i64,
                nsec: stat.atime_nsec,
            }),
        )?;
        let mtime_ms = resolve_utime_millis(
            mtime,
            now,
            existing.as_ref().map(|stat| VirtualTimeSpec {
                sec: (stat.mtime_ms / 1_000) as i64,
                nsec: stat.mtime_nsec,
            }),
        )?;
        self.utimes(path, atime_ms, mtime_ms)
    }
    /// Resizes a file. VM resource policy must be enforced by the caller.
    fn truncate(&mut self, path: &str, length: u64) -> VfsResult<()>;
    fn sync(&mut self, _path: &str) -> VfsResult<()> {
        Ok(())
    }
    /// Allocates storage for a range without changing existing bytes.
    /// VM resource policy must be enforced by the caller.
    fn allocate(&mut self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        const ALLOCATION_CHUNK_BYTES: u64 = 64 * 1024;

        let end = offset
            .checked_add(length)
            .ok_or_else(|| VfsError::new("EINVAL", "allocation range overflows"))?;
        if length == 0 {
            return Ok(());
        }
        let stat = self.stat(path)?;
        if end > stat.size {
            self.truncate(path, end)?;
        }
        let mut cursor = offset;
        while cursor < end {
            let chunk_len = (end - cursor).min(ALLOCATION_CHUNK_BYTES) as usize;
            let mut bytes = self.pread(path, cursor, chunk_len)?;
            bytes.resize(chunk_len, 0);
            self.pwrite(path, bytes, cursor)?;
            cursor += chunk_len as u64;
        }
        Ok(())
    }
    fn insert_range(&mut self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        validate_shift_range(offset, length)?;
        let size = self.stat(path)?.size;
        if offset >= size {
            return Err(VfsError::new(
                "EINVAL",
                "insert range offset must be before EOF",
            ));
        }
        let tail_len = usize::try_from(size - offset)
            .map_err(|_| VfsError::new("EINVAL", "insert range tail is too large"))?;
        let tail = self.pread(path, offset, tail_len)?;
        self.truncate(
            path,
            size.checked_add(length)
                .ok_or_else(|| VfsError::new("EINVAL", "insert range size overflows"))?,
        )?;
        self.pwrite(path, tail, offset + length)?;
        self.punch_hole(path, offset, length)
    }
    fn collapse_range(&mut self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        validate_shift_range(offset, length)?;
        let size = self.stat(path)?.size;
        let end = offset
            .checked_add(length)
            .ok_or_else(|| VfsError::new("EINVAL", "collapse range overflows"))?;
        if end >= size {
            return Err(VfsError::new(
                "EINVAL",
                "collapse range must end before EOF",
            ));
        }
        let tail_len = usize::try_from(size - end)
            .map_err(|_| VfsError::new("EINVAL", "collapse range tail is too large"))?;
        let tail = self.pread(path, end, tail_len)?;
        self.pwrite(path, tail, offset)?;
        self.truncate(path, size - length)
    }
    /// Zeroes and allocates a byte range, optionally preserving the file size.
    fn zero_range(
        &mut self,
        path: &str,
        offset: u64,
        length: u64,
        keep_size: bool,
    ) -> VfsResult<()> {
        let end = offset
            .checked_add(length)
            .ok_or_else(|| VfsError::new("EINVAL", "zero range overflows"))?;
        if length == 0 {
            return Err(VfsError::new("EINVAL", "zero range length must be nonzero"));
        }
        let original_size = self.stat(path)?.size;
        self.allocate(path, offset, length)?;
        let zero_end = if keep_size {
            end.min(original_size)
        } else {
            end
        };
        let mut cursor = offset.min(zero_end);
        while cursor < zero_end {
            let chunk_len = (zero_end - cursor).min(64 * 1024) as usize;
            self.pwrite(path, vec![0; chunk_len], cursor)?;
            cursor += chunk_len as u64;
        }
        if keep_size && self.stat(path)?.size != original_size {
            self.truncate(path, original_size)?;
        }
        Ok(())
    }
    /// Deallocates a byte range while preserving the file size. Bytes in the
    /// intersecting range read back as zeroes.
    fn punch_hole(&mut self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        let requested_end = offset
            .checked_add(length)
            .ok_or_else(|| VfsError::new("EINVAL", "hole-punch range overflows"))?;
        let size = self.stat(path)?.size;
        let end = requested_end.min(size);
        let mut cursor = offset.min(size);
        while cursor < end {
            let chunk_len = (end - cursor).min(64 * 1024) as usize;
            self.pwrite(path, vec![0; chunk_len], cursor)?;
            cursor += chunk_len as u64;
        }
        Ok(())
    }
    /// Returns allocated byte ranges as half-open `(start, end)` intervals.
    fn allocated_ranges(&mut self, path: &str) -> VfsResult<Vec<(u64, u64)>> {
        Err(VfsError::new(
            "EOPNOTSUPP",
            format!("extent mapping is not supported for {path}"),
        ))
    }
    /// Returns allocated byte ranges whose contents are logically zero until
    /// first written, as half-open `(start, end)` intervals.
    fn unwritten_ranges(&mut self, _path: &str) -> VfsResult<Vec<(u64, u64)>> {
        Ok(Vec::new())
    }
    /// Returns one allocated extent, split at written/unwritten boundaries.
    ///
    /// Implementations with native extent metadata should override this so an
    /// indexed FIEMAP query does not allocate complete range lists.
    fn extent_at(&mut self, path: &str, index: usize) -> VfsResult<Option<FileExtent>> {
        let allocated = self.allocated_ranges(path)?;
        let unwritten = self.unwritten_ranges(path)?;
        Ok(crate::extent::classified_file_extent_at(
            allocated.iter().copied(),
            unwritten.iter().copied(),
            index,
        )
        .map(|extent| FileExtent {
            start: extent.start,
            end: extent.end,
            unwritten: extent.unwritten,
        }))
    }
    fn pread(&mut self, path: &str, offset: u64, length: usize) -> VfsResult<Vec<u8>>;
    /// Writes caller-owned bytes at an offset after checking that the in-memory
    /// file can grow without overflowing addressable memory.
    fn pwrite(&mut self, path: &str, content: impl Into<Vec<u8>>, offset: u64) -> VfsResult<()> {
        let content = content.into();
        let mut existing = self.read_file(path)?;
        let start = checked_file_len(offset, "pwrite offset")?;
        if start > existing.len() {
            resize_file_data(&mut existing, start)?;
        }
        let end = start.checked_add(content.len()).ok_or_else(|| {
            VfsError::new(
                "ENOMEM",
                format!(
                    "pwrite result length overflows addressable memory: offset {offset}, content length {}",
                    content.len()
                ),
            )
        })?;
        if end > existing.len() {
            resize_file_data(&mut existing, end)?;
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
    atime_nsec: u32,
    mtime_ms: u64,
    mtime_nsec: u32,
    ctime_ms: u64,
    ctime_nsec: u32,
    birthtime_ms: u64,
    allocated_extents: Vec<(u64, u64)>,
    unwritten_extents: Vec<(u64, u64)>,
    xattrs: BTreeMap<String, Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryFileSystemSnapshotMetadata {
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub nlink: u64,
    pub ino: u64,
    pub atime_ms: u64,
    #[serde(default)]
    pub atime_nsec: u32,
    pub mtime_ms: u64,
    #[serde(default)]
    pub mtime_nsec: u32,
    pub ctime_ms: u64,
    #[serde(default)]
    pub ctime_nsec: u32,
    pub birthtime_ms: u64,
    #[serde(default)]
    pub allocated_extents: Vec<(u64, u64)>,
    #[serde(default)]
    pub unwritten_extents: Vec<(u64, u64)>,
    #[serde(default)]
    pub xattrs: BTreeMap<String, Vec<u8>>,
}

#[derive(Debug, Clone)]
enum InodeKind {
    File { data: Vec<u8> },
    Directory,
    SymbolicLink { target: String },
    CharacterDevice { rdev: u64 },
    BlockDevice { rdev: u64 },
    Fifo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryFileSystemSnapshotInodeKind {
    File { data: Vec<u8> },
    Directory,
    SymbolicLink { target: String },
    CharacterDevice { rdev: u64 },
    BlockDevice { rdev: u64 },
    Fifo,
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
    device_id: u64,
    path_index: BTreeMap<String, u64>,
    inodes: BTreeMap<u64, Inode>,
    next_ino: u64,
}

impl MemoryFileSystem {
    pub fn new() -> Self {
        let mut filesystem = Self {
            device_id: allocate_memory_filesystem_device_id(),
            path_index: BTreeMap::new(),
            inodes: BTreeMap::new(),
            next_ino: 1,
        };

        let root_ino = filesystem.allocate_inode(InodeKind::Directory, S_IFDIR | 0o755);
        filesystem.path_index.insert(String::from("/"), root_ino);
        filesystem
    }

    pub fn read_dir_filtered_limited<F>(
        &mut self,
        path: &str,
        max_entries: usize,
        mut include: F,
    ) -> VfsResult<Vec<String>>
    where
        F: FnMut(&str) -> bool,
    {
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
            if rest.is_empty() || rest.contains('/') || !include(rest) {
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

    pub fn link_count_in_subtree(&self, ino: u64, path: &str) -> usize {
        let normalized = normalize_path(path);
        let prefix = if normalized == "/" {
            String::from("/")
        } else {
            format!("{normalized}/")
        };

        self.path_index
            .iter()
            .filter(|(candidate_path, candidate_ino)| {
                **candidate_ino == ino
                    && (candidate_path.as_str() == normalized
                        || candidate_path.starts_with(&prefix))
            })
            .count()
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
        let allocated_extents = match &kind {
            InodeKind::File { data } => dense_allocation(data.len() as u64),
            InodeKind::Directory
            | InodeKind::SymbolicLink { .. }
            | InodeKind::CharacterDevice { .. }
            | InodeKind::BlockDevice { .. }
            | InodeKind::Fifo => Vec::new(),
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
                    atime_nsec: 0,
                    mtime_ms: now,
                    mtime_nsec: 0,
                    ctime_ms: now,
                    ctime_nsec: 0,
                    birthtime_ms: now,
                    allocated_extents,
                    unwritten_extents: Vec::new(),
                    xattrs: BTreeMap::new(),
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
            InodeKind::CharacterDevice { .. } | InodeKind::BlockDevice { .. } | InodeKind::Fifo => {
                0
            }
        };

        VirtualStat {
            mode: inode.metadata.mode,
            size,
            blocks: allocated_block_count(&inode.metadata.allocated_extents),
            dev: self.device_id,
            rdev: match inode.kind {
                InodeKind::CharacterDevice { rdev } => rdev,
                InodeKind::BlockDevice { rdev } => rdev,
                _ => 0,
            },
            is_directory: matches!(inode.kind, InodeKind::Directory),
            is_symbolic_link: matches!(inode.kind, InodeKind::SymbolicLink { .. }),
            atime_ms: inode.metadata.atime_ms,
            atime_nsec: inode.metadata.atime_nsec,
            mtime_ms: inode.metadata.mtime_ms,
            mtime_nsec: inode.metadata.mtime_nsec,
            ctime_ms: inode.metadata.ctime_ms,
            ctime_nsec: inode.metadata.ctime_nsec,
            birthtime_ms: inode.metadata.birthtime_ms,
            ino: inode.metadata.ino,
            nlink: inode.metadata.nlink,
            uid: inode.metadata.uid,
            gid: inode.metadata.gid,
        }
    }

    /// Clones the full in-memory filesystem state.
    ///
    /// Callers that expose snapshots outside the kernel must enforce their own
    /// byte and inode limits before reaching this raw clone operation.
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
                                atime_nsec: inode.metadata.atime_nsec,
                                mtime_ms: inode.metadata.mtime_ms,
                                mtime_nsec: inode.metadata.mtime_nsec,
                                ctime_ms: inode.metadata.ctime_ms,
                                ctime_nsec: inode.metadata.ctime_nsec,
                                birthtime_ms: inode.metadata.birthtime_ms,
                                allocated_extents: inode.metadata.allocated_extents.clone(),
                                unwritten_extents: inode.metadata.unwritten_extents.clone(),
                                xattrs: inode.metadata.xattrs.clone(),
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
                                InodeKind::CharacterDevice { rdev } => {
                                    MemoryFileSystemSnapshotInodeKind::CharacterDevice {
                                        rdev: *rdev,
                                    }
                                }
                                InodeKind::BlockDevice { rdev } => {
                                    MemoryFileSystemSnapshotInodeKind::BlockDevice { rdev: *rdev }
                                }
                                InodeKind::Fifo => MemoryFileSystemSnapshotInodeKind::Fifo,
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
            device_id: allocate_memory_filesystem_device_id(),
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
                                atime_nsec: inode.metadata.atime_nsec,
                                mtime_ms: inode.metadata.mtime_ms,
                                mtime_nsec: inode.metadata.mtime_nsec,
                                ctime_ms: inode.metadata.ctime_ms,
                                ctime_nsec: inode.metadata.ctime_nsec,
                                birthtime_ms: inode.metadata.birthtime_ms,
                                allocated_extents: inode.metadata.allocated_extents,
                                unwritten_extents: inode.metadata.unwritten_extents,
                                xattrs: inode.metadata.xattrs,
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
                                MemoryFileSystemSnapshotInodeKind::CharacterDevice { rdev } => {
                                    InodeKind::CharacterDevice { rdev }
                                }
                                MemoryFileSystemSnapshotInodeKind::BlockDevice { rdev } => {
                                    InodeKind::BlockDevice { rdev }
                                }
                                MemoryFileSystemSnapshotInodeKind::Fifo => InodeKind::Fifo,
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
            InodeKind::CharacterDevice { .. } | InodeKind::BlockDevice { .. } | InodeKind::Fifo => {
                Err(VfsError::new(
                    "ENXIO",
                    format!("device I/O requires kernel dispatch: {path}"),
                ))
            }
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
        self.read_dir_filtered_limited(path, max_entries, |_| true)
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
                    inode.metadata.allocated_extents = dense_allocation(existing.len() as u64);
                    inode.metadata.unwritten_extents.clear();
                    inode.metadata.mtime_ms = now;
                    inode.metadata.ctime_ms = now;
                    return Ok(());
                }
                InodeKind::Directory => return Err(VfsError::is_directory("open", path)),
                InodeKind::SymbolicLink { .. } => return Err(VfsError::not_found("open", path)),
                InodeKind::CharacterDevice { .. }
                | InodeKind::BlockDevice { .. }
                | InodeKind::Fifo => {
                    return Err(VfsError::new(
                        "ENXIO",
                        format!("device write requires kernel dispatch: {path}"),
                    ))
                }
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
                let offset = existing.len() as u64;
                reserve_file_growth(existing, data.len())?;
                existing.extend_from_slice(&data);
                allocate_range(
                    &mut inode.metadata.allocated_extents,
                    offset,
                    data.len() as u64,
                );
                remove_extent_range(
                    &mut inode.metadata.unwritten_extents,
                    offset,
                    data.len() as u64,
                );
                inode.metadata.mtime_ms = now;
                inode.metadata.ctime_ms = now;
                Ok(existing.len() as u64)
            }
            InodeKind::Directory => Err(VfsError::is_directory("open", path)),
            InodeKind::SymbolicLink { .. } => Err(VfsError::not_found("open", path)),
            InodeKind::CharacterDevice { .. } | InodeKind::BlockDevice { .. } | InodeKind::Fifo => {
                Err(VfsError::new(
                    "ENXIO",
                    format!("device I/O requires kernel dispatch: {path}"),
                ))
            }
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

    fn mknod(&mut self, path: &str, mode: u32, rdev: u64) -> VfsResult<()> {
        let normalized = self.resolve_path(path, 0)?;
        self.mkdir(&dirname(&normalized), true)?;
        if self.path_index.contains_key(&normalized) {
            return Err(VfsError::already_exists("mknod", path));
        }
        let (kind, type_mode) = match mode & 0o170000 {
            S_IFCHR => (InodeKind::CharacterDevice { rdev }, S_IFCHR),
            S_IFBLK => (InodeKind::BlockDevice { rdev }, S_IFBLK),
            S_IFIFO => (InodeKind::Fifo, S_IFIFO),
            _ => return Err(VfsError::invalid_input("unsupported special inode type")),
        };
        let ino = self.allocate_inode(kind, type_mode | (mode & 0o7777));
        self.path_index.insert(normalized, ino);
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

        if let Some(destination_ino) = self.path_index.get(&new_normalized).copied() {
            let destination_is_directory = matches!(
                self.inodes
                    .get(&destination_ino)
                    .expect("destination path should point at a valid inode")
                    .kind,
                InodeKind::Directory
            );
            match (is_directory, destination_is_directory) {
                (false, true) => return Err(VfsError::is_directory("rename", new_path)),
                (true, false) => return Err(VfsError::not_directory("rename", new_path)),
                _ => {}
            }
        }

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

    fn chown_spec(
        &mut self,
        path: &str,
        uid: u32,
        gid: u32,
        follow_symlinks: bool,
    ) -> VfsResult<()> {
        let inode = self.inode_mut_for_existing_path(path, "chown", follow_symlinks)?;
        inode.metadata.uid = uid;
        inode.metadata.gid = gid;
        inode.metadata.ctime_ms = now_ms();
        Ok(())
    }

    fn lchown(&mut self, path: &str, uid: u32, gid: u32) -> VfsResult<()> {
        let inode = self.inode_mut_for_existing_path(path, "lchown", false)?;
        inode.metadata.uid = uid;
        inode.metadata.gid = gid;
        inode.metadata.ctime_ms = now_ms();
        Ok(())
    }

    fn get_xattr(&mut self, path: &str, name: &str, follow_symlinks: bool) -> VfsResult<Vec<u8>> {
        validate_xattr_name(name)?;
        self.inode_for_existing_path(path, "getxattr", follow_symlinks)?
            .metadata
            .xattrs
            .get(name)
            .cloned()
            .ok_or_else(|| {
                VfsError::new(
                    "ENODATA",
                    format!("extended attribute does not exist: {name}"),
                )
            })
    }

    fn list_xattrs(&mut self, path: &str, follow_symlinks: bool) -> VfsResult<Vec<String>> {
        Ok(self
            .inode_for_existing_path(path, "listxattr", follow_symlinks)?
            .metadata
            .xattrs
            .keys()
            .cloned()
            .collect())
    }

    fn set_xattr(
        &mut self,
        path: &str,
        name: &str,
        value: Vec<u8>,
        flags: u32,
        follow_symlinks: bool,
    ) -> VfsResult<()> {
        let inode = self.inode_mut_for_existing_path(path, "setxattr", follow_symlinks)?;
        set_xattr_value(&mut inode.metadata.xattrs, name, &value, flags)?;
        inode.metadata.ctime_ms = now_ms();
        Ok(())
    }

    fn remove_xattr(&mut self, path: &str, name: &str, follow_symlinks: bool) -> VfsResult<()> {
        validate_xattr_name(name)?;
        let inode = self.inode_mut_for_existing_path(path, "removexattr", follow_symlinks)?;
        if inode.metadata.xattrs.remove(name).is_none() {
            return Err(VfsError::new(
                "ENODATA",
                format!("extended attribute does not exist: {name}"),
            ));
        }
        inode.metadata.ctime_ms = now_ms();
        Ok(())
    }

    fn utimes(&mut self, path: &str, atime_ms: u64, mtime_ms: u64) -> VfsResult<()> {
        let inode = self.inode_mut_for_existing_path(path, "utimes", true)?;
        inode.metadata.atime_ms = atime_ms;
        inode.metadata.atime_nsec = 0;
        inode.metadata.mtime_ms = mtime_ms;
        inode.metadata.mtime_nsec = 0;
        inode.metadata.ctime_ms = now_ms();
        inode.metadata.ctime_nsec = 0;
        Ok(())
    }

    fn utimes_spec(
        &mut self,
        path: &str,
        atime: VirtualUtimeSpec,
        mtime: VirtualUtimeSpec,
        follow_symlinks: bool,
    ) -> VfsResult<()> {
        let stat = if follow_symlinks {
            self.stat(path)?
        } else {
            self.lstat(path)?
        };
        let inode = self.inode_mut_for_existing_path(path, "utimes", follow_symlinks)?;
        let now = now_time_spec();
        let atime = resolve_utime_spec(
            atime,
            now,
            VirtualTimeSpec {
                sec: (stat.atime_ms / 1_000) as i64,
                nsec: stat.atime_nsec,
            },
        )?;
        let mtime = resolve_utime_spec(
            mtime,
            now,
            VirtualTimeSpec {
                sec: (stat.mtime_ms / 1_000) as i64,
                nsec: stat.mtime_nsec,
            },
        )?;
        inode.metadata.atime_ms = atime.to_truncated_millis()?;
        inode.metadata.atime_nsec = atime.nsec;
        inode.metadata.mtime_ms = mtime.to_truncated_millis()?;
        inode.metadata.mtime_nsec = mtime.nsec;
        let ctime = now_time_spec();
        inode.metadata.ctime_ms = ctime.to_truncated_millis()?;
        inode.metadata.ctime_nsec = ctime.nsec;
        Ok(())
    }

    fn truncate(&mut self, path: &str, length: u64) -> VfsResult<()> {
        let inode = self.inode_mut_for_existing_path(path, "truncate", true)?;
        let now = now_ms();
        match &mut inode.kind {
            InodeKind::File { data } => {
                resize_file_data(data, checked_file_len(length, "truncate length")?)?;
                truncate_allocation(&mut inode.metadata.allocated_extents, length);
                truncate_allocation(&mut inode.metadata.unwritten_extents, length);
                inode.metadata.mtime_ms = now;
                inode.metadata.ctime_ms = now;
                Ok(())
            }
            InodeKind::Directory => Err(VfsError::is_directory("truncate", path)),
            InodeKind::SymbolicLink { .. } => Err(VfsError::not_found("truncate", path)),
            InodeKind::CharacterDevice { .. } | InodeKind::BlockDevice { .. } | InodeKind::Fifo => {
                Err(VfsError::new(
                    "ENXIO",
                    format!("cannot truncate character device: {path}"),
                ))
            }
        }
    }

    fn allocate(&mut self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        let end = offset
            .checked_add(length)
            .ok_or_else(|| VfsError::new("EINVAL", "allocation range overflows"))?;
        if length == 0 {
            return Ok(());
        }
        let inode = self.inode_mut_for_existing_path(path, "fallocate", true)?;
        match &mut inode.kind {
            InodeKind::File { data } => {
                let end_len = checked_file_len(end, "allocation range end")?;
                if end_len > data.len() {
                    resize_file_data(data, end_len)?;
                }
                allocate_unwritten_holes(
                    &mut inode.metadata.unwritten_extents,
                    &inode.metadata.allocated_extents,
                    offset,
                    length,
                );
                allocate_range(&mut inode.metadata.allocated_extents, offset, length);
                let now = now_ms();
                inode.metadata.mtime_ms = now;
                inode.metadata.ctime_ms = now;
                Ok(())
            }
            InodeKind::Directory => Err(VfsError::is_directory("fallocate", path)),
            InodeKind::SymbolicLink { .. } => Err(VfsError::not_found("fallocate", path)),
            InodeKind::CharacterDevice { .. } | InodeKind::BlockDevice { .. } | InodeKind::Fifo => {
                Err(VfsError::new(
                    "ENXIO",
                    format!("cannot allocate character device: {path}"),
                ))
            }
        }
    }

    fn insert_range(&mut self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        validate_shift_range(offset, length)?;
        let inode = self.inode_mut_for_existing_path(path, "fallocate", true)?;
        match &mut inode.kind {
            InodeKind::File { data } => {
                let start = checked_file_len(offset, "insert range offset")?;
                let insert_len = checked_file_len(length, "insert range length")?;
                if start >= data.len() {
                    return Err(VfsError::new(
                        "EINVAL",
                        "insert range offset must be before EOF",
                    ));
                }
                data.splice(start..start, std::iter::repeat_n(0, insert_len));
                inode.metadata.allocated_extents =
                    allocation_after_insert(&inode.metadata.allocated_extents, offset, length);
                inode.metadata.unwritten_extents =
                    allocation_after_insert(&inode.metadata.unwritten_extents, offset, length);
                let now = now_ms();
                inode.metadata.mtime_ms = now;
                inode.metadata.ctime_ms = now;
                Ok(())
            }
            InodeKind::Directory => Err(VfsError::is_directory("fallocate", path)),
            InodeKind::SymbolicLink { .. } => Err(VfsError::not_found("fallocate", path)),
            _ => Err(VfsError::new(
                "ENXIO",
                "cannot insert range on special inode",
            )),
        }
    }

    fn collapse_range(&mut self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        validate_shift_range(offset, length)?;
        let end = offset
            .checked_add(length)
            .ok_or_else(|| VfsError::new("EINVAL", "collapse range overflows"))?;
        let inode = self.inode_mut_for_existing_path(path, "fallocate", true)?;
        match &mut inode.kind {
            InodeKind::File { data } => {
                if end >= data.len() as u64 {
                    return Err(VfsError::new(
                        "EINVAL",
                        "collapse range must end before EOF",
                    ));
                }
                let start = checked_file_len(offset, "collapse range offset")?;
                let end = checked_file_len(end, "collapse range end")?;
                data.drain(start..end);
                inode.metadata.allocated_extents =
                    allocation_after_collapse(&inode.metadata.allocated_extents, offset, length);
                inode.metadata.unwritten_extents =
                    allocation_after_collapse(&inode.metadata.unwritten_extents, offset, length);
                let now = now_ms();
                inode.metadata.mtime_ms = now;
                inode.metadata.ctime_ms = now;
                Ok(())
            }
            InodeKind::Directory => Err(VfsError::is_directory("fallocate", path)),
            InodeKind::SymbolicLink { .. } => Err(VfsError::not_found("fallocate", path)),
            _ => Err(VfsError::new(
                "ENXIO",
                "cannot collapse range on special inode",
            )),
        }
    }

    fn zero_range(
        &mut self,
        path: &str,
        offset: u64,
        length: u64,
        keep_size: bool,
    ) -> VfsResult<()> {
        let end = offset
            .checked_add(length)
            .ok_or_else(|| VfsError::new("EINVAL", "zero range overflows"))?;
        if length == 0 {
            return Err(VfsError::new("EINVAL", "zero range length must be nonzero"));
        }
        let inode = self.inode_mut_for_existing_path(path, "fallocate", true)?;
        match &mut inode.kind {
            InodeKind::File { data } => {
                let original_size = data.len() as u64;
                let zero_end = if keep_size {
                    end.min(original_size)
                } else {
                    end
                };
                if !keep_size {
                    let end_len = checked_file_len(end, "zero range end")?;
                    if end_len > data.len() {
                        resize_file_data(data, end_len)?;
                    }
                }
                let start = checked_file_len(offset.min(zero_end), "zero range offset")?;
                let zero_end = checked_file_len(zero_end, "zero range end")?;
                data[start..zero_end].fill(0);
                mark_zero_unwritten(
                    &mut inode.metadata.unwritten_extents,
                    &inode.metadata.allocated_extents,
                    offset,
                    length,
                );
                allocate_range(&mut inode.metadata.allocated_extents, offset, length);
                let now = now_ms();
                inode.metadata.mtime_ms = now;
                inode.metadata.ctime_ms = now;
                Ok(())
            }
            InodeKind::Directory => Err(VfsError::is_directory("fallocate", path)),
            InodeKind::SymbolicLink { .. } => Err(VfsError::not_found("fallocate", path)),
            InodeKind::CharacterDevice { .. } | InodeKind::BlockDevice { .. } | InodeKind::Fifo => {
                Err(VfsError::new("ENXIO", format!("cannot zero range: {path}")))
            }
        }
    }

    fn punch_hole(&mut self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        let requested_end = offset
            .checked_add(length)
            .ok_or_else(|| VfsError::new("EINVAL", "hole-punch range overflows"))?;
        let inode = self.inode_mut_for_existing_path(path, "fallocate", true)?;
        match &mut inode.kind {
            InodeKind::File { data } => {
                let start = checked_file_len(offset.min(data.len() as u64), "hole-punch offset")?;
                let end =
                    checked_file_len(requested_end.min(data.len() as u64), "hole-punch range end")?;
                data[start..end].fill(0);
                punch_allocation(&mut inode.metadata.allocated_extents, offset, length);
                remove_extent_range(&mut inode.metadata.unwritten_extents, offset, length);
                let now = now_ms();
                inode.metadata.mtime_ms = now;
                inode.metadata.ctime_ms = now;
                Ok(())
            }
            InodeKind::Directory => Err(VfsError::is_directory("fallocate", path)),
            InodeKind::SymbolicLink { .. } => Err(VfsError::not_found("fallocate", path)),
            InodeKind::CharacterDevice { .. } | InodeKind::BlockDevice { .. } | InodeKind::Fifo => {
                Err(VfsError::new(
                    "ENXIO",
                    format!("cannot punch character device: {path}"),
                ))
            }
        }
    }

    fn allocated_ranges(&mut self, path: &str) -> VfsResult<Vec<(u64, u64)>> {
        let inode = self.inode_mut_for_existing_path(path, "fiemap", true)?;
        match &inode.kind {
            InodeKind::File { data } => Ok(allocation_byte_ranges(
                &inode.metadata.allocated_extents,
                data.len() as u64,
            )),
            InodeKind::Directory => Err(VfsError::is_directory("fiemap", path)),
            InodeKind::SymbolicLink { .. } => Err(VfsError::not_found("fiemap", path)),
            InodeKind::CharacterDevice { .. } | InodeKind::BlockDevice { .. } | InodeKind::Fifo => {
                Ok(Vec::new())
            }
        }
    }

    fn unwritten_ranges(&mut self, path: &str) -> VfsResult<Vec<(u64, u64)>> {
        let inode = self.inode_mut_for_existing_path(path, "fiemap", true)?;
        match &inode.kind {
            InodeKind::File { data } => Ok(allocation_byte_ranges(
                &inode.metadata.unwritten_extents,
                data.len() as u64,
            )),
            InodeKind::Directory => Err(VfsError::is_directory("fiemap", path)),
            InodeKind::SymbolicLink { .. } => Err(VfsError::not_found("fiemap", path)),
            InodeKind::CharacterDevice { .. } | InodeKind::BlockDevice { .. } | InodeKind::Fifo => {
                Ok(Vec::new())
            }
        }
    }

    fn extent_at(&mut self, path: &str, index: usize) -> VfsResult<Option<FileExtent>> {
        let inode = self.inode_mut_for_existing_path(path, "fiemap", true)?;
        match &inode.kind {
            InodeKind::File { data } => Ok(crate::extent::classified_file_extent_at(
                crate::extent::sector_byte_ranges(
                    inode.metadata.allocated_extents.iter().copied(),
                    data.len() as u64,
                ),
                crate::extent::sector_byte_ranges(
                    inode.metadata.unwritten_extents.iter().copied(),
                    data.len() as u64,
                ),
                index,
            )
            .map(|extent| FileExtent {
                start: extent.start,
                end: extent.end,
                unwritten: extent.unwritten,
            })),
            InodeKind::Directory => Err(VfsError::is_directory("fiemap", path)),
            InodeKind::SymbolicLink { .. } => Err(VfsError::not_found("fiemap", path)),
            InodeKind::CharacterDevice { .. } | InodeKind::BlockDevice { .. } | InodeKind::Fifo => {
                Ok(None)
            }
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
            InodeKind::CharacterDevice { .. } | InodeKind::BlockDevice { .. } | InodeKind::Fifo => {
                Err(VfsError::new(
                    "ENXIO",
                    format!("device I/O requires kernel dispatch: {path}"),
                ))
            }
        }
    }

    fn pwrite(&mut self, path: &str, content: impl Into<Vec<u8>>, offset: u64) -> VfsResult<()> {
        let content = content.into();
        let inode = self.inode_mut_for_existing_path(path, "open", true)?;
        let data = match &mut inode.kind {
            InodeKind::File { data } => data,
            InodeKind::Directory => return Err(VfsError::is_directory("open", path)),
            InodeKind::SymbolicLink { .. } => {
                return Err(VfsError::not_found("open", path));
            }
            InodeKind::CharacterDevice { .. } | InodeKind::BlockDevice { .. } | InodeKind::Fifo => {
                return Err(VfsError::new(
                    "ENXIO",
                    format!("device I/O requires kernel dispatch: {path}"),
                ));
            }
        };
        let start = checked_file_len(offset, "pwrite offset")?;
        let end = start.checked_add(content.len()).ok_or_else(|| {
            VfsError::new(
                "ENOMEM",
                format!(
                    "pwrite result length overflows addressable memory: offset {offset}, content length {}",
                    content.len()
                ),
            )
        })?;
        if end > data.len() {
            resize_file_data(data, end)?;
        }
        data[start..end].copy_from_slice(&content);
        allocate_range(
            &mut inode.metadata.allocated_extents,
            offset,
            content.len() as u64,
        );
        remove_extent_range(
            &mut inode.metadata.unwritten_extents,
            offset,
            content.len() as u64,
        );
        let now = now_ms();
        inode.metadata.mtime_ms = now;
        inode.metadata.ctime_ms = now;
        Ok(())
    }
}

impl Default for MemoryFileSystem {
    fn default() -> Self {
        Self::new()
    }
}

fn resolve_utime_spec(
    spec: VirtualUtimeSpec,
    now: VirtualTimeSpec,
    existing: VirtualTimeSpec,
) -> VfsResult<VirtualTimeSpec> {
    match spec {
        VirtualUtimeSpec::Set(spec) => Ok(spec),
        VirtualUtimeSpec::Now => Ok(now),
        VirtualUtimeSpec::Omit => Ok(existing),
    }
}

fn resolve_utime_millis(
    spec: VirtualUtimeSpec,
    now_ms: u64,
    existing: Option<VirtualTimeSpec>,
) -> VfsResult<u64> {
    match spec {
        VirtualUtimeSpec::Set(spec) => spec.to_truncated_millis(),
        VirtualUtimeSpec::Now => Ok(now_ms),
        VirtualUtimeSpec::Omit => existing
            .ok_or_else(|| VfsError::new("EINVAL", "UTIME_OMIT requires existing metadata"))?
            .to_truncated_millis(),
    }
}

pub fn validate_path(path: &str) -> VfsResult<()> {
    if path.as_bytes().contains(&0) {
        return Err(VfsError::invalid_input("path contains NUL byte"));
    }
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

fn dense_allocation(size: u64) -> Vec<(u64, u64)> {
    if size == 0 {
        Vec::new()
    } else {
        vec![(0, size.div_ceil(512))]
    }
}

fn allocate_range(extents: &mut Vec<(u64, u64)>, offset: u64, length: u64) {
    if length == 0 {
        return;
    }
    let start = offset / 512;
    let end = offset.saturating_add(length).div_ceil(512);
    let mut merged = Vec::with_capacity(extents.len() + 1);
    let mut pending = (start, end);
    for &(extent_start, extent_end) in extents.iter() {
        if extent_end < pending.0 {
            merged.push((extent_start, extent_end));
        } else if pending.1 < extent_start {
            merged.push(pending);
            pending = (extent_start, extent_end);
        } else {
            pending.0 = pending.0.min(extent_start);
            pending.1 = pending.1.max(extent_end);
        }
    }
    merged.push(pending);
    *extents = merged;
}

fn remove_extent_range(extents: &mut Vec<(u64, u64)>, offset: u64, length: u64) {
    if length == 0 {
        return;
    }
    let start = offset / 512;
    let end = offset.saturating_add(length).div_ceil(512);
    *extents = extents
        .iter()
        .flat_map(|&(extent_start, extent_end)| {
            [
                (extent_start, extent_end.min(start)),
                (extent_start.max(end), extent_end),
            ]
            .into_iter()
            .filter(|(part_start, part_end)| part_start < part_end)
        })
        .collect();
}

fn allocate_unwritten_holes(
    unwritten: &mut Vec<(u64, u64)>,
    allocated: &[(u64, u64)],
    offset: u64,
    length: u64,
) {
    if length == 0 {
        return;
    }
    let start = offset / 512;
    let end = offset.saturating_add(length).div_ceil(512);
    let mut cursor = start;
    for &(allocated_start, allocated_end) in allocated {
        if allocated_end <= cursor || allocated_start >= end {
            continue;
        }
        if cursor < allocated_start {
            allocate_range(
                unwritten,
                cursor.saturating_mul(512),
                (allocated_start.min(end) - cursor).saturating_mul(512),
            );
        }
        cursor = cursor.max(allocated_end).min(end);
        if cursor == end {
            return;
        }
    }
    if cursor < end {
        allocate_range(
            unwritten,
            cursor.saturating_mul(512),
            (end - cursor).saturating_mul(512),
        );
    }
}

fn mark_zero_unwritten(
    unwritten: &mut Vec<(u64, u64)>,
    allocated: &[(u64, u64)],
    offset: u64,
    length: u64,
) {
    allocate_unwritten_holes(unwritten, allocated, offset, length);
    let end = offset.saturating_add(length);
    let first_full_block = offset.div_ceil(4096);
    let past_full_block = end / 4096;
    if first_full_block < past_full_block {
        allocate_range(
            unwritten,
            first_full_block * 4096,
            (past_full_block - first_full_block) * 4096,
        );
    }
}

fn truncate_allocation(extents: &mut Vec<(u64, u64)>, size: u64) {
    let end = size.div_ceil(512);
    extents.retain_mut(|(start, extent_end)| {
        *extent_end = (*extent_end).min(end);
        *start < *extent_end
    });
}

fn punch_allocation(extents: &mut Vec<(u64, u64)>, offset: u64, length: u64) {
    let start = offset.div_ceil(512);
    let end = offset.saturating_add(length) / 512;
    if start >= end {
        return;
    }
    *extents = extents
        .iter()
        .flat_map(|&(extent_start, extent_end)| {
            let left = (extent_start, extent_end.min(start));
            let right = (extent_start.max(end), extent_end);
            [left, right]
                .into_iter()
                .filter(|(part_start, part_end)| part_start < part_end)
        })
        .collect();
}

fn validate_shift_range(offset: u64, length: u64) -> VfsResult<()> {
    if length == 0 || !offset.is_multiple_of(512) || !length.is_multiple_of(512) {
        return Err(VfsError::new(
            "EINVAL",
            "insert/collapse range requires a nonzero 512-byte-aligned range",
        ));
    }
    Ok(())
}

fn allocation_after_insert(existing: &[(u64, u64)], offset: u64, length: u64) -> Vec<(u64, u64)> {
    let start = offset / 512;
    let shift = length / 512;
    normalize_extents(existing.iter().flat_map(|&(extent_start, extent_end)| {
        if extent_end <= start {
            vec![(extent_start, extent_end)]
        } else if extent_start >= start {
            vec![(extent_start + shift, extent_end + shift)]
        } else {
            vec![(extent_start, start), (start + shift, extent_end + shift)]
        }
    }))
}

fn allocation_after_collapse(existing: &[(u64, u64)], offset: u64, length: u64) -> Vec<(u64, u64)> {
    let start = offset / 512;
    let end = start + length / 512;
    normalize_extents(existing.iter().flat_map(|&(extent_start, extent_end)| {
        let mut parts = Vec::with_capacity(2);
        if extent_start < start {
            parts.push((extent_start, extent_end.min(start)));
        }
        if extent_end > end {
            parts.push((
                extent_start.max(end) - (end - start),
                extent_end - (end - start),
            ));
        }
        parts
    }))
}

fn normalize_extents(extents: impl IntoIterator<Item = (u64, u64)>) -> Vec<(u64, u64)> {
    let mut merged = Vec::<(u64, u64)>::new();
    for (start, end) in extents.into_iter().filter(|(start, end)| start < end) {
        if let Some(last) = merged.last_mut().filter(|last| start <= last.1) {
            last.1 = last.1.max(end);
        } else {
            merged.push((start, end));
        }
    }
    merged
}

fn allocated_block_count(extents: &[(u64, u64)]) -> u64 {
    extents
        .iter()
        .map(|(start, end)| end.saturating_sub(*start))
        .sum()
}

fn allocation_byte_ranges(extents: &[(u64, u64)], size: u64) -> Vec<(u64, u64)> {
    crate::extent::sector_byte_ranges(extents.iter().copied(), size).collect()
}

fn checked_file_len(value: u64, description: &'static str) -> VfsResult<usize> {
    usize::try_from(value).map_err(|_| {
        VfsError::new(
            "EINVAL",
            format!("{description} exceeds addressable memory: {value}"),
        )
    })
}

fn reserve_file_growth(data: &mut Vec<u8>, additional: usize) -> VfsResult<()> {
    data.try_reserve(additional).map_err(|error| {
        VfsError::new(
            "ENOMEM",
            format!(
                "file growth exceeds addressable memory: current length {}, additional {additional}: {error}",
                data.len()
            ),
        )
    })
}

fn resize_file_data(data: &mut Vec<u8>, new_len: usize) -> VfsResult<()> {
    if new_len > data.len() {
        reserve_file_growth(data, new_len - data.len())?;
    }
    data.resize(new_len, 0);
    Ok(())
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

fn now_time_spec() -> VirtualTimeSpec {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    VirtualTimeSpec {
        sec: now.as_secs() as i64,
        nsec: now.subsec_nanos(),
    }
}
