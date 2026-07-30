use crate::engine::error::{VfsError, VfsResult};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use web_time::{SystemTime, UNIX_EPOCH};

pub const MAX_PATH: usize = 4096;
pub const MAX_SYMLINK_DEPTH: usize = 40;
pub const DEFAULT_INLINE_THRESHOLD: usize = 64 * 1024;
pub const DEFAULT_CHUNK_SIZE: u32 = 4 * 1024 * 1024;
pub const XATTR_CREATE: u32 = 1;
pub const XATTR_REPLACE: u32 = 2;
pub(crate) const INTERNAL_XATTR_PREFIX: &str = "agentos.internal.";
pub(crate) const INODE_RDEV_XATTR: &str = "agentos.internal.rdev";
pub(crate) const INODE_UNWRITTEN_EXTENTS_XATTR: &str = "agentos.internal.unwritten_extents";
pub const XATTR_NAME_MAX: usize = 255;
pub const XATTR_SIZE_MAX: usize = 64 * 1024;
pub const XATTR_LIST_MAX: usize = 64 * 1024;

pub const S_IFREG: u32 = 0o100000;
pub const S_IFDIR: u32 = 0o040000;
pub const S_IFLNK: u32 = 0o120000;
pub const S_IFCHR: u32 = 0o020000;
pub const S_IFBLK: u32 = 0o060000;
pub const S_IFIFO: u32 = 0o010000;

pub(crate) fn decode_unwritten_extents(
    xattrs: &BTreeMap<String, Vec<u8>>,
) -> VfsResult<Vec<(u64, u64)>> {
    Ok(unwritten_sector_ranges(xattrs)?.collect())
}

pub(crate) fn unwritten_sector_ranges(
    xattrs: &BTreeMap<String, Vec<u8>>,
) -> VfsResult<impl Iterator<Item = (u64, u64)> + Clone + '_> {
    let encoded = xattrs
        .get(INODE_UNWRITTEN_EXTENTS_XATTR)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if !encoded.len().is_multiple_of(16) {
        return Err(VfsError::new(
            "EIO",
            "corrupt internal unwritten-extent metadata",
        ));
    }
    let mut prior_end = None;
    for chunk in encoded.chunks_exact(16) {
        let (start, end) = decode_unwritten_extent(chunk);
        if start >= end || prior_end.is_some_and(|prior_end| prior_end >= start) {
            return Err(VfsError::new(
                "EIO",
                "corrupt internal unwritten-extent ordering",
            ));
        }
        prior_end = Some(end);
    }
    Ok(encoded.chunks_exact(16).map(decode_unwritten_extent))
}

fn decode_unwritten_extent(chunk: &[u8]) -> (u64, u64) {
    let start = u64::from_le_bytes(chunk[..8].try_into().expect("eight-byte extent start"));
    let end = u64::from_le_bytes(chunk[8..].try_into().expect("eight-byte extent end"));
    (start, end)
}

pub(crate) fn encode_unwritten_extents(
    xattrs: &mut BTreeMap<String, Vec<u8>>,
    extents: &[(u64, u64)],
) {
    if extents.is_empty() {
        xattrs.remove(INODE_UNWRITTEN_EXTENTS_XATTR);
        return;
    }
    let mut encoded = Vec::with_capacity(extents.len() * 16);
    for (start, end) in extents {
        encoded.extend_from_slice(&start.to_le_bytes());
        encoded.extend_from_slice(&end.to_le_bytes());
    }
    xattrs.insert(String::from(INODE_UNWRITTEN_EXTENTS_XATTR), encoded);
}

pub(crate) fn unwritten_byte_ranges(
    xattrs: &BTreeMap<String, Vec<u8>>,
    size: u64,
) -> VfsResult<Vec<(u64, u64)>> {
    Ok(crate::extent::sector_byte_ranges(unwritten_sector_ranges(xattrs)?, size).collect())
}

fn normalize_sector_extents(mut extents: Vec<(u64, u64)>) -> Vec<(u64, u64)> {
    extents.retain(|(start, end)| start < end);
    extents.sort_unstable();
    let mut normalized: Vec<(u64, u64)> = Vec::with_capacity(extents.len());
    for (start, end) in extents {
        if let Some((_, previous_end)) = normalized.last_mut() {
            if start <= *previous_end {
                *previous_end = (*previous_end).max(end);
                continue;
            }
        }
        normalized.push((start, end));
    }
    normalized
}

pub(crate) fn unwritten_after_zero(
    existing: &[(u64, u64)],
    allocated: &[(u64, u64)],
    offset: u64,
    length: u64,
) -> Vec<(u64, u64)> {
    if length == 0 {
        return existing.to_vec();
    }
    let end = offset.saturating_add(length);
    let mut unwritten = unwritten_after_allocate(existing, allocated, offset, length);
    let first_full_block = offset.div_ceil(4096);
    let past_full_block = end / 4096;
    if first_full_block < past_full_block {
        unwritten.push((first_full_block * 8, past_full_block * 8));
    }
    normalize_sector_extents(unwritten)
}

pub(crate) fn unwritten_after_write(
    existing: &[(u64, u64)],
    offset: u64,
    length: u64,
) -> Vec<(u64, u64)> {
    if length == 0 {
        return existing.to_vec();
    }
    let start = offset / 512;
    let end = offset.saturating_add(length).div_ceil(512);
    existing
        .iter()
        .flat_map(|&(extent_start, extent_end)| {
            [
                (extent_start, extent_end.min(start)),
                (extent_start.max(end), extent_end),
            ]
            .into_iter()
            .filter(|(part_start, part_end)| part_start < part_end)
        })
        .collect()
}

pub(crate) fn unwritten_after_truncate(existing: &[(u64, u64)], size: u64) -> Vec<(u64, u64)> {
    let end = size.div_ceil(512);
    existing
        .iter()
        .filter_map(|(start, extent_end)| {
            let extent_end = (*extent_end).min(end);
            (*start < extent_end).then_some((*start, extent_end))
        })
        .collect()
}

pub(crate) fn unwritten_after_allocate(
    existing: &[(u64, u64)],
    allocated: &[(u64, u64)],
    offset: u64,
    length: u64,
) -> Vec<(u64, u64)> {
    if length == 0 {
        return existing.to_vec();
    }
    let start = offset / 512;
    let end = offset.saturating_add(length).div_ceil(512);
    let mut holes = Vec::new();
    let mut cursor = start;
    for &(allocated_start, allocated_end) in allocated {
        if allocated_end <= cursor || allocated_start >= end {
            continue;
        }
        if cursor < allocated_start {
            holes.push((cursor, allocated_start.min(end)));
        }
        cursor = cursor.max(allocated_end).min(end);
        if cursor == end {
            break;
        }
    }
    if cursor < end {
        holes.push((cursor, end));
    }
    normalize_sector_extents(existing.iter().copied().chain(holes).collect())
}

pub(crate) fn unwritten_after_insert(
    existing: &[(u64, u64)],
    offset: u64,
    length: u64,
) -> Vec<(u64, u64)> {
    let start = offset / 512;
    let shift = length / 512;
    normalize_sector_extents(
        existing
            .iter()
            .flat_map(|&(extent_start, extent_end)| {
                if extent_end <= start {
                    vec![(extent_start, extent_end)]
                } else if extent_start >= start {
                    vec![(extent_start + shift, extent_end + shift)]
                } else {
                    vec![(extent_start, start), (start + shift, extent_end + shift)]
                }
            })
            .collect(),
    )
}

pub(crate) fn unwritten_after_collapse(
    existing: &[(u64, u64)],
    offset: u64,
    length: u64,
) -> Vec<(u64, u64)> {
    let start = offset / 512;
    let end = start + length / 512;
    let shift = length / 512;
    normalize_sector_extents(
        existing
            .iter()
            .flat_map(|&(extent_start, extent_end)| {
                let mut parts = Vec::new();
                if extent_start < start {
                    parts.push((extent_start, extent_end.min(start)));
                }
                if extent_end > end {
                    parts.push((extent_start.max(end) - shift, extent_end - shift));
                }
                parts
            })
            .collect(),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Timespec {
    pub sec: i64,
    pub nsec: u32,
}

impl Timespec {
    pub fn now() -> Self {
        let duration = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Self {
            sec: duration.as_secs() as i64,
            nsec: duration.subsec_nanos(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum InodeType {
    File,
    Directory,
    Symlink,
    CharacterDevice,
    BlockDevice,
    Fifo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Storage {
    Inline(Vec<u8>),
    Chunked { chunk_size: u32 },
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InodeMeta {
    pub ino: u64,
    pub kind: InodeType,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub size: u64,
    pub nlink: u64,
    pub atime: Timespec,
    pub mtime: Timespec,
    pub ctime: Timespec,
    pub birthtime: Timespec,
    pub storage: Storage,
    pub symlink_target: Option<String>,
    #[serde(default)]
    pub allocated_extents: Vec<(u64, u64)>,
    #[serde(default)]
    pub xattrs: BTreeMap<String, Vec<u8>>,
}

impl InodeMeta {
    pub fn to_stat(&self) -> VirtualStat {
        let type_bits = match self.kind {
            InodeType::File => S_IFREG,
            InodeType::Directory => S_IFDIR,
            InodeType::Symlink => S_IFLNK,
            InodeType::CharacterDevice => S_IFCHR,
            InodeType::BlockDevice => S_IFBLK,
            InodeType::Fifo => S_IFIFO,
        };
        VirtualStat {
            mode: type_bits | (self.mode & 0o7777),
            size: self.size,
            blocks: self
                .allocated_extents
                .iter()
                .map(|(start, end)| end.saturating_sub(*start))
                .sum(),
            rdev: inode_rdev(&self.xattrs),
            is_directory: self.kind == InodeType::Directory,
            is_symbolic_link: self.kind == InodeType::Symlink,
            atime: self.atime,
            mtime: self.mtime,
            ctime: self.ctime,
            birthtime: self.birthtime,
            ino: self.ino,
            nlink: self.nlink,
            uid: self.uid,
            gid: self.gid,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dentry {
    pub name: String,
    pub ino: u64,
    pub kind: InodeType,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DentryStat {
    pub name: String,
    pub meta: InodeMeta,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VirtualStat {
    pub mode: u32,
    pub size: u64,
    pub blocks: u64,
    pub rdev: u64,
    pub is_directory: bool,
    pub is_symbolic_link: bool,
    pub atime: Timespec,
    pub mtime: Timespec,
    pub ctime: Timespec,
    pub birthtime: Timespec,
    pub ino: u64,
    pub nlink: u64,
    pub uid: u32,
    pub gid: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SnapshotId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BlockKey(pub String);

impl BlockKey {
    pub fn from_content(data: &[u8]) -> Self {
        Self(blake3::hash(data).to_hex().to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkRange {
    pub start: u64,
    pub end: Option<u64>,
}

impl ChunkRange {
    pub fn all() -> Self {
        Self {
            start: 0,
            end: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkRef {
    pub index: u64,
    pub key: BlockKey,
    pub len: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkEdit {
    pub index: u64,
    pub key: BlockKey,
    pub len: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateInodeAttrs {
    pub kind: InodeType,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub storage: Storage,
    pub symlink_target: Option<String>,
    pub xattrs: BTreeMap<String, Vec<u8>>,
}

impl CreateInodeAttrs {
    pub fn file(mode: u32, uid: u32, gid: u32, storage: Storage) -> Self {
        Self {
            kind: InodeType::File,
            mode,
            uid,
            gid,
            storage,
            symlink_target: None,
            xattrs: BTreeMap::new(),
        }
    }

    pub fn directory(mode: u32, uid: u32, gid: u32) -> Self {
        Self {
            kind: InodeType::Directory,
            mode,
            uid,
            gid,
            storage: Storage::None,
            symlink_target: None,
            xattrs: BTreeMap::new(),
        }
    }

    pub fn symlink(target: String, uid: u32, gid: u32) -> Self {
        Self {
            kind: InodeType::Symlink,
            mode: 0o777,
            uid,
            gid,
            storage: Storage::None,
            symlink_target: Some(target),
            xattrs: BTreeMap::new(),
        }
    }

    pub fn character_device(mode: u32, uid: u32, gid: u32, rdev: u64) -> Self {
        Self {
            kind: InodeType::CharacterDevice,
            mode,
            uid,
            gid,
            storage: Storage::None,
            symlink_target: None,
            xattrs: BTreeMap::from([(String::from(INODE_RDEV_XATTR), rdev.to_le_bytes().to_vec())]),
        }
    }

    pub fn special_node(mode: u32, uid: u32, gid: u32, rdev: u64) -> VfsResult<Self> {
        let kind = match mode & 0o170000 {
            S_IFCHR => InodeType::CharacterDevice,
            S_IFBLK => InodeType::BlockDevice,
            S_IFIFO => InodeType::Fifo,
            _ => return Err(VfsError::einval("unsupported special inode type")),
        };
        let mut attrs = Self {
            kind,
            mode: mode & 0o7777,
            uid,
            gid,
            storage: Storage::None,
            symlink_target: None,
            xattrs: BTreeMap::new(),
        };
        if matches!(kind, InodeType::CharacterDevice | InodeType::BlockDevice) {
            attrs
                .xattrs
                .insert(String::from(INODE_RDEV_XATTR), rdev.to_le_bytes().to_vec());
        }
        Ok(attrs)
    }
}

pub(crate) fn inode_rdev(xattrs: &BTreeMap<String, Vec<u8>>) -> u64 {
    xattrs
        .get(INODE_RDEV_XATTR)
        .and_then(|value| value.as_slice().try_into().ok())
        .map(u64::from_le_bytes)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InodePatch {
    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub atime: Option<Timespec>,
    pub mtime: Option<Timespec>,
    pub size: Option<u64>,
    pub storage: Option<Storage>,
    pub allocated_extents: Option<Vec<(u64, u64)>>,
    pub xattrs: Option<BTreeMap<String, Vec<u8>>>,
}

pub fn validate_xattr_name(name: &str) -> VfsResult<()> {
    if name.starts_with(INTERNAL_XATTR_PREFIX) {
        return Err(VfsError::new(
            "EOPNOTSUPP",
            format!("reserved extended attribute namespace: {name}"),
        ));
    }
    if name.len() > XATTR_NAME_MAX {
        return Err(VfsError::new(
            "ERANGE",
            format!(
                "extended attribute name is {} bytes; maximum is {XATTR_NAME_MAX}",
                name.len()
            ),
        ));
    }
    if name.is_empty() || !name.contains('.') || name.contains('\0') {
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
        return Err(VfsError::einval(format!("invalid xattr flags: {flags}")));
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
        return Err(VfsError::eexist(name));
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectMeta {
    pub size: u64,
    #[serde(default)]
    pub allocated_extents: Vec<(u64, u64)>,
    pub atime: Timespec,
    pub mtime: Timespec,
    pub ctime: Timespec,
    pub birthtime: Timespec,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub kind: InodeType,
    pub symlink_target: Option<String>,
    #[serde(default)]
    pub link_id: Option<String>,
    #[serde(default)]
    pub xattrs: BTreeMap<String, Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileExtent {
    pub start: u64,
    pub end: u64,
    pub unwritten: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectEntry {
    pub name: String,
    pub size: u64,
    pub mtime: Timespec,
    pub is_prefix: bool,
}

pub fn validate_path(path: &str) -> VfsResult<()> {
    if path.is_empty() || !path.starts_with('/') {
        return Err(VfsError::einval(format!("path must be absolute: {path}")));
    }
    if path.len() > MAX_PATH {
        return Err(VfsError::enametoolong(path));
    }
    Ok(())
}

pub fn normalize_path(path: &str) -> VfsResult<String> {
    validate_path(path)?;
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            name => parts.push(name),
        }
    }
    if parts.is_empty() {
        Ok("/".to_string())
    } else {
        Ok(format!("/{}", parts.join("/")))
    }
}

pub fn parent_and_name(path: &str) -> VfsResult<(String, String)> {
    let normalized = normalize_path(path)?;
    if normalized == "/" {
        return Err(VfsError::einval("root has no parent"));
    }
    let (parent, name) = normalized
        .rsplit_once('/')
        .ok_or_else(|| VfsError::einval(format!("invalid path: {path}")))?;
    let parent = if parent.is_empty() { "/" } else { parent };
    Ok((parent.to_string(), name.to_string()))
}
