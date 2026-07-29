use super::overlay_fs::{OverlayFileSystem, OverlayMode};
use super::usage::{
    RootFilesystemResourceLimits, DEFAULT_MAX_FILESYSTEM_BYTES, DEFAULT_MAX_INODE_COUNT,
};
use super::vfs::{
    normalize_path, MemoryFileSystem, VfsError, VfsResult, VirtualFileSystem, VirtualStat,
    VirtualUtimeSpec, MAX_PATH_LENGTH,
};
use crate::posix::vfs::VirtualDirEntry;
use base64::Engine;
use serde::Deserialize;
use std::collections::BTreeSet;

// The base filesystem fixture is staged into OUT_DIR by build.rs: copied from
// the canonical `packages/agentos-core/fixtures/base-filesystem.json`
// during in-tree builds, or from the vendored `assets/base-filesystem.json`
// copy bundled in the published crate.
const BUNDLED_BASE_FILESYSTEM_JSON: &str =
    include_str!(concat!(env!("OUT_DIR"), "/base-filesystem.json"));
pub const ROOT_FILESYSTEM_SNAPSHOT_FORMAT: &str = "agentos_filesystem_snapshot_v1";
const LEGACY_AGENTOS_ROOT_FILESYSTEM_SNAPSHOT_FORMAT: &str = "agentos_filesystem_snapshot_v1";
const ROOT_FILESYSTEM_SNAPSHOT_FIXED_OVERHEAD_BYTES: usize = 4 * 1024;
const ROOT_FILESYSTEM_SNAPSHOT_ENTRY_OVERHEAD_BYTES: usize = MAX_PATH_LENGTH + 1024;
const DEFAULT_ROOT_DIRECTORIES: &[&str] = &[
    "/",
    "/dev",
    "/proc",
    "/tmp",
    "/bin",
    "/lib",
    "/sbin",
    "/boot",
    "/etc",
    "/root",
    "/run",
    "/srv",
    "/sys",
    "/opt",
    "/mnt",
    "/media",
    "/home",
    "/usr",
    "/usr/bin",
    "/usr/games",
    "/usr/include",
    "/usr/lib",
    "/usr/libexec",
    "/usr/man",
    "/usr/local",
    "/usr/local/bin",
    "/usr/sbin",
    "/usr/share",
    "/usr/share/man",
    "/var",
    "/var/cache",
    "/var/empty",
    "/var/lib",
    "/var/lock",
    "/var/log",
    "/var/run",
    "/var/spool",
    "/var/tmp",
    "/etc/agentos",
];
const KERNEL_RESERVED_BOOTSTRAP_PATH_PREFIXES: &[&str] = &["/dev", "/proc", "/sys"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootFilesystemError {
    message: String,
}

impl RootFilesystemError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for RootFilesystemError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RootFilesystemError {}

impl From<VfsError> for RootFilesystemError {
    fn from(error: VfsError) -> Self {
        Self::new(error.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilesystemEntryKind {
    File,
    Directory,
    Symlink,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesystemEntry {
    pub path: String,
    pub kind: FilesystemEntryKind,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub content: Option<Vec<u8>>,
    pub target: Option<String>,
}

impl FilesystemEntry {
    pub fn directory(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            kind: FilesystemEntryKind::Directory,
            mode: 0o755,
            uid: 0,
            gid: 0,
            content: None,
            target: None,
        }
    }

    pub fn file(path: impl Into<String>, content: impl Into<Vec<u8>>) -> Self {
        Self {
            path: path.into(),
            kind: FilesystemEntryKind::File,
            mode: 0o644,
            uid: 0,
            gid: 0,
            content: Some(content.into()),
            target: None,
        }
    }

    pub fn symlink(path: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            kind: FilesystemEntryKind::Symlink,
            mode: 0o777,
            uid: 0,
            gid: 0,
            content: None,
            target: Some(target.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootFilesystemSnapshot {
    pub entries: Vec<FilesystemEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RootFilesystemImportLimits {
    pub max_encoded_snapshot_bytes: Option<usize>,
    pub max_filesystem_bytes: Option<u64>,
    pub max_inode_count: Option<usize>,
}

impl RootFilesystemImportLimits {
    pub fn from_resource_limits(limits: &impl RootFilesystemResourceLimits) -> Self {
        Self {
            max_encoded_snapshot_bytes: encoded_snapshot_limit(
                limits.max_filesystem_bytes(),
                limits.max_inode_count(),
            ),
            max_filesystem_bytes: limits.max_filesystem_bytes(),
            max_inode_count: limits.max_inode_count(),
        }
    }
}

impl Default for RootFilesystemImportLimits {
    fn default() -> Self {
        Self {
            max_encoded_snapshot_bytes: encoded_snapshot_limit(
                Some(DEFAULT_MAX_FILESYSTEM_BYTES),
                Some(DEFAULT_MAX_INODE_COUNT),
            ),
            max_filesystem_bytes: Some(DEFAULT_MAX_FILESYSTEM_BYTES),
            max_inode_count: Some(DEFAULT_MAX_INODE_COUNT),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootFilesystemMode {
    Ephemeral,
    ReadOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootFilesystemDescriptor {
    pub mode: RootFilesystemMode,
    pub disable_default_base_layer: bool,
    pub lowers: Vec<RootFilesystemSnapshot>,
    pub bootstrap_entries: Vec<FilesystemEntry>,
}

impl Default for RootFilesystemDescriptor {
    fn default() -> Self {
        Self {
            mode: RootFilesystemMode::Ephemeral,
            disable_default_base_layer: false,
            lowers: Vec::new(),
            bootstrap_entries: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub struct RootFileSystem {
    overlay: OverlayFileSystem,
    mode: RootFilesystemMode,
    bootstrap_finished: bool,
}

impl RootFileSystem {
    pub fn from_descriptor(
        descriptor: RootFilesystemDescriptor,
    ) -> Result<Self, RootFilesystemError> {
        Self::from_descriptor_with_import_limits(descriptor, &RootFilesystemImportLimits::default())
    }

    pub fn from_descriptor_with_import_limits(
        descriptor: RootFilesystemDescriptor,
        limits: &RootFilesystemImportLimits,
    ) -> Result<Self, RootFilesystemError> {
        let mut lower_snapshots = descriptor.lowers.clone();
        if !descriptor.disable_default_base_layer {
            lower_snapshots.push(load_bundled_base_snapshot_with_limits(limits)?);
        } else if lower_snapshots.is_empty() {
            lower_snapshots.push(minimal_root_snapshot());
        }
        validate_descriptor_import_limits(
            &lower_snapshots,
            &descriptor.bootstrap_entries,
            limits,
            "root filesystem descriptor",
        )?;

        let lowers = lower_snapshots
            .iter()
            .map(snapshot_to_memory_filesystem)
            .collect::<Result<Vec<_>, _>>()?;

        let mut root = Self {
            overlay: OverlayFileSystem::new(lowers, OverlayMode::Ephemeral),
            mode: descriptor.mode,
            bootstrap_finished: false,
        };
        root.apply_bootstrap_entries(&descriptor.bootstrap_entries)?;
        Ok(root)
    }

    pub fn apply_bootstrap_entries(
        &mut self,
        entries: &[FilesystemEntry],
    ) -> Result<(), RootFilesystemError> {
        if self.bootstrap_finished {
            return Err(RootFilesystemError::new(
                "root filesystem bootstrap is already finished",
            ));
        }

        for entry in sort_entries(entries.to_vec()) {
            if is_kernel_reserved_bootstrap_path(&entry.path) {
                continue;
            }
            apply_entry(&mut self.overlay, &entry)?;
        }
        Ok(())
    }

    pub fn finish_bootstrap(&mut self) {
        if self.bootstrap_finished {
            return;
        }
        self.bootstrap_finished = true;
        if self.mode == RootFilesystemMode::ReadOnly {
            self.overlay.lock_writes();
        }
    }

    pub fn snapshot(&mut self) -> Result<RootFilesystemSnapshot, RootFilesystemError> {
        Ok(RootFilesystemSnapshot {
            entries: snapshot_virtual_filesystem(&mut self.overlay, "/")?,
        })
    }

    pub fn check_rename_copy_up_limits(
        &mut self,
        old_path: &str,
        new_path: &str,
        max_bytes: Option<u64>,
        max_inodes: Option<usize>,
    ) -> VfsResult<()> {
        self.overlay
            .check_rename_copy_up_limits(old_path, new_path, max_bytes, max_inodes)
    }
}

impl VirtualFileSystem for RootFileSystem {
    fn read_file(&mut self, path: &str) -> VfsResult<Vec<u8>> {
        self.overlay.read_file(path)
    }

    fn read_dir(&mut self, path: &str) -> VfsResult<Vec<String>> {
        self.overlay.read_dir(path)
    }

    fn read_dir_limited(&mut self, path: &str, max_entries: usize) -> VfsResult<Vec<String>> {
        self.overlay.read_dir_limited(path, max_entries)
    }

    fn read_dir_with_types(&mut self, path: &str) -> VfsResult<Vec<VirtualDirEntry>> {
        self.overlay.read_dir_with_types(path)
    }

    fn write_file(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<()> {
        self.overlay.write_file(path, content.into())
    }

    fn create_file_exclusive(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<()> {
        self.overlay.create_file_exclusive(path, content.into())
    }

    fn append_file(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<u64> {
        self.overlay.append_file(path, content.into())
    }

    fn create_dir(&mut self, path: &str) -> VfsResult<()> {
        self.overlay.create_dir(path)
    }

    fn mkdir(&mut self, path: &str, recursive: bool) -> VfsResult<()> {
        self.overlay.mkdir(path, recursive)
    }

    fn mknod(&mut self, path: &str, mode: u32, rdev: u64) -> VfsResult<()> {
        self.overlay.mknod(path, mode, rdev)
    }

    fn exists(&self, path: &str) -> bool {
        self.overlay.exists(path)
    }

    fn stat(&mut self, path: &str) -> VfsResult<VirtualStat> {
        self.overlay.stat(path)
    }

    fn remove_file(&mut self, path: &str) -> VfsResult<()> {
        self.overlay.remove_file(path)
    }

    fn remove_dir(&mut self, path: &str) -> VfsResult<()> {
        self.overlay.remove_dir(path)
    }

    fn rename(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        self.overlay.rename(old_path, new_path)
    }

    fn realpath(&self, path: &str) -> VfsResult<String> {
        self.overlay.realpath(path)
    }

    fn symlink(&mut self, target: &str, link_path: &str) -> VfsResult<()> {
        self.overlay.symlink(target, link_path)
    }

    fn read_link(&self, path: &str) -> VfsResult<String> {
        self.overlay.read_link(path)
    }

    fn lstat(&self, path: &str) -> VfsResult<VirtualStat> {
        self.overlay.lstat(path)
    }

    fn link(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        self.overlay.link(old_path, new_path)
    }

    fn chmod(&mut self, path: &str, mode: u32) -> VfsResult<()> {
        self.overlay.chmod(path, mode)
    }

    fn chown(&mut self, path: &str, uid: u32, gid: u32) -> VfsResult<()> {
        self.overlay.chown(path, uid, gid)
    }

    fn chown_spec(
        &mut self,
        path: &str,
        uid: u32,
        gid: u32,
        follow_symlinks: bool,
    ) -> VfsResult<()> {
        self.overlay.chown_spec(path, uid, gid, follow_symlinks)
    }

    fn lchown(&mut self, path: &str, uid: u32, gid: u32) -> VfsResult<()> {
        self.overlay.lchown(path, uid, gid)
    }

    fn get_xattr(&mut self, path: &str, name: &str, follow_symlinks: bool) -> VfsResult<Vec<u8>> {
        self.overlay.get_xattr(path, name, follow_symlinks)
    }

    fn list_xattrs(&mut self, path: &str, follow_symlinks: bool) -> VfsResult<Vec<String>> {
        self.overlay.list_xattrs(path, follow_symlinks)
    }

    fn set_xattr(
        &mut self,
        path: &str,
        name: &str,
        value: Vec<u8>,
        flags: u32,
        follow_symlinks: bool,
    ) -> VfsResult<()> {
        self.overlay
            .set_xattr(path, name, value, flags, follow_symlinks)
    }

    fn remove_xattr(&mut self, path: &str, name: &str, follow_symlinks: bool) -> VfsResult<()> {
        self.overlay.remove_xattr(path, name, follow_symlinks)
    }

    fn utimes(&mut self, path: &str, atime_ms: u64, mtime_ms: u64) -> VfsResult<()> {
        self.overlay.utimes(path, atime_ms, mtime_ms)
    }

    fn utimes_spec(
        &mut self,
        path: &str,
        atime: VirtualUtimeSpec,
        mtime: VirtualUtimeSpec,
        follow_symlinks: bool,
    ) -> VfsResult<()> {
        self.overlay
            .utimes_spec(path, atime, mtime, follow_symlinks)
    }

    fn truncate(&mut self, path: &str, length: u64) -> VfsResult<()> {
        self.overlay.truncate(path, length)
    }

    fn allocate(&mut self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        self.overlay.allocate(path, offset, length)
    }

    fn insert_range(&mut self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        self.overlay.insert_range(path, offset, length)
    }

    fn collapse_range(&mut self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        self.overlay.collapse_range(path, offset, length)
    }

    fn zero_range(
        &mut self,
        path: &str,
        offset: u64,
        length: u64,
        keep_size: bool,
    ) -> VfsResult<()> {
        self.overlay.zero_range(path, offset, length, keep_size)
    }

    fn punch_hole(&mut self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        self.overlay.punch_hole(path, offset, length)
    }

    fn allocated_ranges(&mut self, path: &str) -> VfsResult<Vec<(u64, u64)>> {
        self.overlay.allocated_ranges(path)
    }

    fn unwritten_ranges(&mut self, path: &str) -> VfsResult<Vec<(u64, u64)>> {
        self.overlay.unwritten_ranges(path)
    }

    fn pread(&mut self, path: &str, offset: u64, length: usize) -> VfsResult<Vec<u8>> {
        self.overlay.pread(path, offset, length)
    }

    fn pwrite(&mut self, path: &str, content: impl Into<Vec<u8>>, offset: u64) -> VfsResult<()> {
        self.overlay.pwrite(path, content, offset)
    }
}

#[derive(Debug, Deserialize)]
struct RawBaseFilesystemSnapshot {
    filesystem: RawFilesystemEntries,
}

#[derive(Debug, Deserialize)]
struct RawFilesystemEntries {
    entries: Vec<RawFilesystemEntry>,
}

#[derive(Debug, Deserialize)]
struct RawFilesystemEntry {
    path: String,
    #[serde(rename = "type")]
    kind: RawFilesystemEntryKind,
    mode: String,
    uid: u32,
    gid: u32,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    encoding: Option<String>,
    #[serde(default)]
    target: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RawFilesystemEntryKind {
    File,
    Directory,
    Symlink,
}

#[derive(Debug, Deserialize)]
struct RawSnapshotExport {
    format: String,
    filesystem: RawFilesystemEntries,
}

#[derive(Debug, serde::Serialize)]
struct SnapshotExport<'a> {
    format: &'static str,
    filesystem: SnapshotFilesystem<'a>,
}

#[derive(Debug, serde::Serialize)]
struct SnapshotFilesystem<'a> {
    entries: Vec<SerializedFilesystemEntry<'a>>,
}

#[derive(Debug, serde::Serialize)]
struct SerializedFilesystemEntry<'a> {
    path: &'a str,
    #[serde(rename = "type")]
    kind: &'static str,
    mode: String,
    uid: u32,
    gid: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    encoding: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<&'a str>,
}

pub fn encode_snapshot(snapshot: &RootFilesystemSnapshot) -> Result<Vec<u8>, RootFilesystemError> {
    let serialized_entries = snapshot
        .entries
        .iter()
        .map(|entry| SerializedFilesystemEntry {
            path: &entry.path,
            kind: match entry.kind {
                FilesystemEntryKind::File => "file",
                FilesystemEntryKind::Directory => "directory",
                FilesystemEntryKind::Symlink => "symlink",
            },
            mode: format!("{:o}", entry.mode),
            uid: entry.uid,
            gid: entry.gid,
            content: entry
                .content
                .as_ref()
                .map(|bytes| base64::engine::general_purpose::STANDARD.encode(bytes)),
            encoding: entry.content.as_ref().map(|_| "base64"),
            target: entry.target.as_deref(),
        })
        .collect::<Vec<_>>();

    serde_json::to_vec(&SnapshotExport {
        format: ROOT_FILESYSTEM_SNAPSHOT_FORMAT,
        filesystem: SnapshotFilesystem {
            entries: serialized_entries,
        },
    })
    .map_err(|error| RootFilesystemError::new(format!("serialize root snapshot: {error}")))
}

pub fn decode_snapshot(bytes: &[u8]) -> Result<RootFilesystemSnapshot, RootFilesystemError> {
    decode_snapshot_with_import_limits(bytes, &RootFilesystemImportLimits::default())
}

pub fn decode_snapshot_with_import_limits(
    bytes: &[u8],
    limits: &RootFilesystemImportLimits,
) -> Result<RootFilesystemSnapshot, RootFilesystemError> {
    validate_encoded_snapshot_size(bytes, limits, "root snapshot")?;
    let raw: RawSnapshotExport = serde_json::from_slice(bytes)
        .map_err(|error| RootFilesystemError::new(format!("parse root snapshot: {error}")))?;
    if !is_supported_root_filesystem_snapshot_format(&raw.format) {
        return Err(RootFilesystemError::new(format!(
            "unsupported root snapshot format: {}",
            raw.format
        )));
    }
    raw_entries_to_snapshot(raw.filesystem.entries, limits, "root snapshot")
}

pub fn is_supported_root_filesystem_snapshot_format(format: &str) -> bool {
    format == ROOT_FILESYSTEM_SNAPSHOT_FORMAT
        || format == LEGACY_AGENTOS_ROOT_FILESYSTEM_SNAPSHOT_FORMAT
}

pub fn load_bundled_base_snapshot_with_limits(
    limits: &RootFilesystemImportLimits,
) -> Result<RootFilesystemSnapshot, RootFilesystemError> {
    validate_encoded_snapshot_size(
        BUNDLED_BASE_FILESYSTEM_JSON.as_bytes(),
        limits,
        "bundled base filesystem",
    )?;
    let raw: RawBaseFilesystemSnapshot = serde_json::from_str(BUNDLED_BASE_FILESYSTEM_JSON)
        .map_err(|error| {
            RootFilesystemError::new(format!("parse bundled base filesystem: {error}"))
        })?;
    raw_entries_to_snapshot(raw.filesystem.entries, limits, "bundled base filesystem")
}

fn minimal_root_snapshot() -> RootFilesystemSnapshot {
    let mut entries = DEFAULT_ROOT_DIRECTORIES
        .iter()
        .map(|path| FilesystemEntry::directory(*path))
        .collect::<Vec<_>>();
    entries.push(FilesystemEntry::file("/usr/bin/env", Vec::new()));
    RootFilesystemSnapshot { entries }
}

fn convert_raw_entry(raw: RawFilesystemEntry) -> Result<FilesystemEntry, RootFilesystemError> {
    let content = match raw.content {
        Some(content) => match raw.encoding.as_deref() {
            Some("base64") => Some(
                base64::engine::general_purpose::STANDARD
                    .decode(content)
                    .map_err(|error| {
                        RootFilesystemError::new(format!(
                            "decode base64 content for {}: {error}",
                            raw.path
                        ))
                    })?,
            ),
            Some("utf8") | None => Some(content.into_bytes()),
            Some(other) => {
                return Err(RootFilesystemError::new(format!(
                    "unsupported content encoding for {}: {other}",
                    raw.path
                )));
            }
        },
        None => None,
    };

    Ok(FilesystemEntry {
        path: raw.path,
        kind: match raw.kind {
            RawFilesystemEntryKind::File => FilesystemEntryKind::File,
            RawFilesystemEntryKind::Directory => FilesystemEntryKind::Directory,
            RawFilesystemEntryKind::Symlink => FilesystemEntryKind::Symlink,
        },
        mode: u32::from_str_radix(&raw.mode, 8).map_err(|error| {
            RootFilesystemError::new(format!("parse mode {}: {error}", raw.mode))
        })?,
        uid: raw.uid,
        gid: raw.gid,
        content,
        target: raw.target,
    })
}

fn raw_entries_to_snapshot(
    raw_entries: Vec<RawFilesystemEntry>,
    limits: &RootFilesystemImportLimits,
    context: &str,
) -> Result<RootFilesystemSnapshot, RootFilesystemError> {
    if let Some(limit) = limits.max_inode_count {
        if raw_entries.len() > limit {
            return Err(RootFilesystemError::new(format!(
                "{context} contains {} entries, exceeding limit {limit}",
                raw_entries.len()
            )));
        }
    }

    let entries = raw_entries
        .into_iter()
        .map(convert_raw_entry)
        .collect::<Result<Vec<_>, _>>()?;
    validate_entry_import_limits(&entries, limits, context)?;
    Ok(RootFilesystemSnapshot { entries })
}

pub fn validate_snapshot_import_limits(
    snapshot: &RootFilesystemSnapshot,
    limits: &RootFilesystemImportLimits,
    context: &str,
) -> Result<(), RootFilesystemError> {
    validate_entry_import_limits(&snapshot.entries, limits, context)
}

fn validate_descriptor_import_limits(
    lowers: &[RootFilesystemSnapshot],
    bootstrap_entries: &[FilesystemEntry],
    limits: &RootFilesystemImportLimits,
    context: &str,
) -> Result<(), RootFilesystemError> {
    let explicit_entry_count = lowers
        .iter()
        .map(|snapshot| snapshot.entries.len())
        .sum::<usize>()
        .saturating_add(bootstrap_entries.len());
    let mut inode_paths = BTreeSet::new();
    for snapshot in lowers {
        collect_materialized_entry_paths(&snapshot.entries, &mut inode_paths);
    }
    collect_materialized_entry_paths(bootstrap_entries, &mut inode_paths);
    let inode_count = inode_paths.len();
    if let Some(limit) = limits.max_inode_count {
        if explicit_entry_count > limit {
            return Err(RootFilesystemError::new(format!(
                "{context} contains {explicit_entry_count} entries, exceeding limit {limit}"
            )));
        }

        if inode_count > limit {
            return Err(RootFilesystemError::new(format!(
                "{context} contains {inode_count} entries, exceeding limit {limit}"
            )));
        }
    }

    let mut bytes = 0_u64;
    for snapshot in lowers {
        bytes = bytes.saturating_add(entry_content_bytes(&snapshot.entries));
    }
    bytes = bytes.saturating_add(entry_content_bytes(bootstrap_entries));
    if let Some(limit) = limits.max_filesystem_bytes {
        if bytes > limit {
            return Err(RootFilesystemError::new(format!(
                "{context} contains {bytes} bytes, exceeding limit {limit}"
            )));
        }
    }
    Ok(())
}

fn validate_entry_import_limits(
    entries: &[FilesystemEntry],
    limits: &RootFilesystemImportLimits,
    context: &str,
) -> Result<(), RootFilesystemError> {
    if let Some(limit) = limits.max_inode_count {
        if entries.len() > limit {
            return Err(RootFilesystemError::new(format!(
                "{context} contains {} entries, exceeding limit {limit}",
                entries.len()
            )));
        }

        let inode_count = materialized_entry_inode_count(entries);
        if inode_count > limit {
            return Err(RootFilesystemError::new(format!(
                "{context} contains {inode_count} entries, exceeding limit {limit}"
            )));
        }
    }

    let bytes = entry_content_bytes(entries);
    if let Some(limit) = limits.max_filesystem_bytes {
        if bytes > limit {
            return Err(RootFilesystemError::new(format!(
                "{context} contains {bytes} bytes, exceeding limit {limit}"
            )));
        }
    }
    Ok(())
}

fn validate_encoded_snapshot_size(
    bytes: &[u8],
    limits: &RootFilesystemImportLimits,
    context: &str,
) -> Result<(), RootFilesystemError> {
    if let Some(limit) = limits.max_encoded_snapshot_bytes {
        if bytes.len() > limit {
            return Err(RootFilesystemError::new(format!(
                "{context} contains {} encoded bytes, exceeding limit {limit}",
                bytes.len()
            )));
        }
    }
    Ok(())
}

fn entry_content_bytes(entries: &[FilesystemEntry]) -> u64 {
    entries.iter().fold(0_u64, |total, entry| {
        total.saturating_add(match entry.kind {
            FilesystemEntryKind::File => entry
                .content
                .as_ref()
                .map(|content| usize_to_u64(content.len()))
                .unwrap_or(0),
            FilesystemEntryKind::Directory => 0,
            FilesystemEntryKind::Symlink => entry
                .target
                .as_ref()
                .map(|target| usize_to_u64(target.len()))
                .unwrap_or(0),
        })
    })
}

fn materialized_entry_inode_count(entries: &[FilesystemEntry]) -> usize {
    let mut paths = BTreeSet::new();
    collect_materialized_entry_paths(entries, &mut paths);
    paths.len()
}

fn collect_materialized_entry_paths(entries: &[FilesystemEntry], paths: &mut BTreeSet<String>) {
    for entry in entries {
        collect_materialized_path(&entry.path, paths);
    }
}

fn collect_materialized_path(path: &str, paths: &mut BTreeSet<String>) {
    let normalized = normalize_path(path);
    paths.insert(normalized.clone());

    let mut parent = String::new();
    let segments = normalized
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    for segment in segments.iter().take(segments.len().saturating_sub(1)) {
        parent.push('/');
        parent.push_str(segment);
        paths.insert(parent.clone());
    }
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

const fn u64_limit_to_usize(value: u64) -> usize {
    if value > usize::MAX as u64 {
        usize::MAX
    } else {
        value as usize
    }
}

const fn encoded_snapshot_limit(
    max_filesystem_bytes: Option<u64>,
    max_inode_count: Option<usize>,
) -> Option<usize> {
    let Some(max_filesystem_bytes) = max_filesystem_bytes else {
        return None;
    };

    Some(
        u64_limit_to_usize(max_filesystem_bytes)
            .saturating_mul(2)
            .saturating_add(match max_inode_count {
                Some(max_inode_count) => {
                    max_inode_count.saturating_mul(ROOT_FILESYSTEM_SNAPSHOT_ENTRY_OVERHEAD_BYTES)
                }
                None => 0,
            })
            .saturating_add(ROOT_FILESYSTEM_SNAPSHOT_FIXED_OVERHEAD_BYTES),
    )
}

fn snapshot_to_memory_filesystem(
    snapshot: &RootFilesystemSnapshot,
) -> Result<MemoryFileSystem, RootFilesystemError> {
    let mut filesystem = MemoryFileSystem::new();
    for entry in sort_entries(snapshot.entries.clone()) {
        apply_entry_to_memory_filesystem(&mut filesystem, &entry)?;
    }
    Ok(filesystem)
}

fn apply_entry_to_memory_filesystem(
    filesystem: &mut MemoryFileSystem,
    entry: &FilesystemEntry,
) -> Result<(), RootFilesystemError> {
    ensure_parent_directories(filesystem, &entry.path)?;

    match entry.kind {
        FilesystemEntryKind::Directory => {
            filesystem.mkdir(&entry.path, true)?;
            filesystem.chmod(&entry.path, entry.mode)?;
            filesystem.chown(&entry.path, entry.uid, entry.gid)?;
        }
        FilesystemEntryKind::File => {
            filesystem.write_file(&entry.path, entry.content.clone().unwrap_or_default())?;
            filesystem.chmod(&entry.path, entry.mode)?;
            filesystem.chown(&entry.path, entry.uid, entry.gid)?;
        }
        FilesystemEntryKind::Symlink => {
            let Some(target) = entry.target.as_deref() else {
                return Err(RootFilesystemError::new(format!(
                    "missing symlink target for {}",
                    entry.path
                )));
            };
            filesystem.symlink_with_metadata(
                target,
                &entry.path,
                entry.mode,
                entry.uid,
                entry.gid,
            )?;
        }
    }

    Ok(())
}

fn apply_entry(
    filesystem: &mut impl VirtualFileSystem,
    entry: &FilesystemEntry,
) -> Result<(), RootFilesystemError> {
    ensure_parent_directories(filesystem, &entry.path)?;

    match entry.kind {
        FilesystemEntryKind::Directory => {
            filesystem.mkdir(&entry.path, true)?;
            filesystem.chmod(&entry.path, entry.mode)?;
            filesystem.chown(&entry.path, entry.uid, entry.gid)?;
        }
        FilesystemEntryKind::File => {
            filesystem.write_file(&entry.path, entry.content.clone().unwrap_or_default())?;
            filesystem.chmod(&entry.path, entry.mode)?;
            filesystem.chown(&entry.path, entry.uid, entry.gid)?;
        }
        FilesystemEntryKind::Symlink => {
            let Some(target) = entry.target.as_deref() else {
                return Err(RootFilesystemError::new(format!(
                    "missing symlink target for {}",
                    entry.path
                )));
            };
            filesystem.symlink(target, &entry.path)?;
        }
    }

    Ok(())
}

fn ensure_parent_directories(
    filesystem: &mut impl VirtualFileSystem,
    path: &str,
) -> Result<(), RootFilesystemError> {
    let normalized = normalize_path(path);
    let mut current = String::new();
    let segments = normalized
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();

    for segment in segments.iter().take(segments.len().saturating_sub(1)) {
        current.push('/');
        current.push_str(segment);

        if filesystem.exists(&current) {
            continue;
        }

        filesystem.create_dir(&current)?;
        filesystem.chmod(&current, 0o755)?;
        filesystem.chown(&current, 0, 0)?;
    }

    Ok(())
}

fn sort_entries(mut entries: Vec<FilesystemEntry>) -> Vec<FilesystemEntry> {
    entries.sort_by(|left, right| {
        let depth_left = if left.path == "/" {
            0
        } else {
            left.path.split('/').filter(|part| !part.is_empty()).count()
        };
        let depth_right = if right.path == "/" {
            0
        } else {
            right
                .path
                .split('/')
                .filter(|part| !part.is_empty())
                .count()
        };
        depth_left
            .cmp(&depth_right)
            .then_with(|| left.path.cmp(&right.path))
    });
    entries
}

fn snapshot_virtual_filesystem(
    filesystem: &mut impl VirtualFileSystem,
    root_path: &str,
) -> Result<Vec<FilesystemEntry>, RootFilesystemError> {
    let mut entries = Vec::new();
    snapshot_path(filesystem, root_path, &mut entries)?;
    Ok(entries)
}

fn snapshot_path(
    filesystem: &mut impl VirtualFileSystem,
    path: &str,
    entries: &mut Vec<FilesystemEntry>,
) -> Result<(), RootFilesystemError> {
    let stat = if path == "/" {
        filesystem.stat(path)?
    } else {
        filesystem.lstat(path)?
    };

    if stat.is_symbolic_link {
        entries.push(FilesystemEntry {
            path: path.to_owned(),
            kind: FilesystemEntryKind::Symlink,
            mode: stat.mode,
            uid: stat.uid,
            gid: stat.gid,
            content: None,
            target: Some(filesystem.read_link(path)?),
        });
        return Ok(());
    }

    if stat.is_directory {
        entries.push(FilesystemEntry {
            path: path.to_owned(),
            kind: FilesystemEntryKind::Directory,
            mode: stat.mode,
            uid: stat.uid,
            gid: stat.gid,
            content: None,
            target: None,
        });

        let mut children = filesystem
            .read_dir_with_types(path)?
            .into_iter()
            .map(|entry| entry.name)
            .filter(|name| name != "." && name != "..")
            .collect::<Vec<_>>();
        children.sort();

        for child in children {
            let child_path = if path == "/" {
                format!("/{child}")
            } else {
                format!("{path}/{child}")
            };
            snapshot_path(filesystem, &child_path, entries)?;
        }
        return Ok(());
    }

    entries.push(FilesystemEntry {
        path: path.to_owned(),
        kind: FilesystemEntryKind::File,
        mode: stat.mode,
        uid: stat.uid,
        gid: stat.gid,
        content: Some(filesystem.read_file(path)?),
        target: None,
    });
    Ok(())
}

fn is_kernel_reserved_bootstrap_path(path: &str) -> bool {
    let normalized = normalize_path(path);
    KERNEL_RESERVED_BOOTSTRAP_PATH_PREFIXES
        .iter()
        .any(|prefix| normalized == *prefix || normalized.starts_with(&format!("{prefix}/")))
}
