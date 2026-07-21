//! Filesystem methods + path guards + supporting types + the in-process [`VirtualFileSystem`] mount
//! contract.
//!
//! Ported from `packages/core/src/agent-os.ts` (fs methods + `_assertSafeAbsolutePath` /
//! `_assertWritableAbsolutePath`), `runtime-compat.ts` (`VirtualStat`, `VirtualFileSystem`), and
//! `filesystem-snapshot.ts` (snapshot export types).
//!
//! Parity notes: every method runs the path guards first; `mkdir` recursive uses the WRITABLE guard,
//! non-recursive uses the SAFE guard. `writeFile` does NOT create parents; `writeFiles` DOES. Batch
//! methods NEVER reject (per-entry error strings). Snapshot wire format keeps octal-string `mode`
//! and `utf8`/`base64` content verbatim.

use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

use agentos_sidecar_client::wire::{
    self, GuestFilesystemCallRequest, GuestFilesystemOperation, GuestFilesystemResultResponse,
    GuestFilesystemStat, RootFilesystemEntry, RootFilesystemEntryEncoding, RootFilesystemEntryKind,
};

use crate::agent_os::AgentOs;
use crate::error::ClientError;

// ---------------------------------------------------------------------------
// Supporting types
// ---------------------------------------------------------------------------

/// `string | Uint8Array` file content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileContent {
    Text(String),
    Bytes(Vec<u8>),
}

impl From<String> for FileContent {
    fn from(value: String) -> Self {
        FileContent::Text(value)
    }
}

impl From<&str> for FileContent {
    fn from(value: &str) -> Self {
        FileContent::Text(value.to_string())
    }
}

impl From<Vec<u8>> for FileContent {
    fn from(value: Vec<u8>) -> Self {
        FileContent::Bytes(value)
    }
}

impl From<&[u8]> for FileContent {
    fn from(value: &[u8]) -> Self {
        FileContent::Bytes(value.to_vec())
    }
}

/// An entry returned by `readdir_recursive`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirEntry {
    pub path: String,
    #[serde(rename = "type")]
    pub entry_type: DirEntryType,
    pub size: u64,
}

/// The type of a directory entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DirEntryType {
    File,
    Directory,
    Symlink,
}

/// Options for `readdir_recursive`. `max_depth` None = unlimited, Some(0) = immediate children only;
/// `exclude` matches basenames at any depth.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReaddirRecursiveOptions {
    pub max_depth: Option<u32>,
    pub exclude: Vec<String>,
}

/// A batch write entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchWriteEntry {
    pub path: String,
    pub content: FileContent,
}

/// Result of a single batch write (never an `Err`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchWriteResult {
    pub path: String,
    pub success: bool,
    pub error: Option<String>,
}

/// Result of a single batch read (never an `Err`). `content` is None on failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchReadResult {
    pub path: String,
    pub content: Option<Vec<u8>>,
    pub error: Option<String>,
}

/// Options for `mkdir`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MkdirOptions {
    pub recursive: bool,
}

/// Options for `remove`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RemoveOptions {
    pub recursive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DynamicMountDescriptor {
    pub path: String,
    pub plugin: crate::config::MountPlugin,
    #[serde(default)]
    #[serde(rename = "readOnly")]
    pub read_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountInfo {
    pub path: String,
    pub kind: String,
    #[serde(rename = "readOnly")]
    pub read_only: bool,
}

/// Stat result. 16 fields; `*_ms` time fields are `f64` (JS ms, possibly fractional).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VirtualStat {
    pub mode: u32,
    pub size: u64,
    pub blocks: u64,
    pub dev: u64,
    pub rdev: u64,
    #[serde(rename = "isDirectory")]
    pub is_directory: bool,
    #[serde(rename = "isSymbolicLink")]
    pub is_symbolic_link: bool,
    #[serde(rename = "atimeMs")]
    pub atime_ms: f64,
    #[serde(rename = "mtimeMs")]
    pub mtime_ms: f64,
    #[serde(rename = "ctimeMs")]
    pub ctime_ms: f64,
    #[serde(rename = "birthtimeMs")]
    pub birthtime_ms: f64,
    pub ino: u64,
    pub nlink: u64,
    pub uid: u32,
    pub gid: u32,
}

/// A directory entry with a known type, returned by `read_dir_with_types` on the mount contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualDirEntry {
    pub name: String,
    pub is_directory: bool,
    pub is_symbolic_link: bool,
}

// ---------------------------------------------------------------------------
// Snapshot export wire types (octal-string mode, utf8/base64 content)
// ---------------------------------------------------------------------------

/// `{ kind: "snapshot-export"; source }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootSnapshotExport {
    pub kind: SnapshotExportKind,
    pub source: FilesystemSnapshotExport,
}

/// The literal `"snapshot-export"` tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SnapshotExportKind {
    #[serde(rename = "snapshot-export")]
    SnapshotExport,
}

/// `{ format: "agentos-filesystem-snapshot-v1"; filesystem: { entries } }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesystemSnapshotExport {
    pub format: String,
    pub filesystem: FilesystemSnapshotEntries,
}

/// `{ entries: FilesystemEntry[] }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesystemSnapshotEntries {
    pub entries: Vec<FilesystemEntry>,
}

/// A single snapshot entry. `mode` is an OCTAL STRING (e.g. `"0755"`). `content` is utf8 or base64.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesystemEntry {
    pub path: String,
    #[serde(rename = "type")]
    pub entry_type: DirEntryType,
    pub mode: String,
    pub uid: u32,
    pub gid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoding: Option<FilesystemEntryEncoding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

/// Snapshot content encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FilesystemEntryEncoding {
    Utf8,
    Base64,
}

// ---------------------------------------------------------------------------
// VirtualFileSystem mount contract (in-process trait object for mount_fs)
// ---------------------------------------------------------------------------

/// The 25-method mount backend contract. A `mount_fs` driver implements this trait; it is a live
/// in-process object and cannot cross an RPC boundary.
///
/// TODO(parity: confirm exact method set/signatures against runtime-compat.ts before first impl).
#[async_trait]
pub trait VirtualFileSystem: Send + Sync {
    async fn read_file(&self, path: &str) -> Result<Vec<u8>>;
    async fn read_text_file(&self, path: &str) -> Result<String>;
    async fn read_dir(&self, path: &str) -> Result<Vec<String>>;
    async fn read_dir_with_types(&self, path: &str) -> Result<Vec<VirtualDirEntry>>;
    async fn write_file(&self, path: &str, content: &[u8]) -> Result<()>;
    async fn create_dir(&self, path: &str) -> Result<()>;
    async fn mkdir(&self, path: &str, recursive: bool) -> Result<()>;
    async fn exists(&self, path: &str) -> Result<bool>;
    async fn stat(&self, path: &str) -> Result<VirtualStat>;
    async fn lstat(&self, path: &str) -> Result<VirtualStat>;
    async fn remove_file(&self, path: &str) -> Result<()>;
    async fn remove_dir(&self, path: &str) -> Result<()>;
    async fn rename(&self, from: &str, to: &str) -> Result<()>;
    async fn realpath(&self, path: &str) -> Result<String>;
    async fn symlink(&self, target: &str, path: &str) -> Result<()>;
    async fn readlink(&self, path: &str) -> Result<String>;
    async fn link(&self, existing: &str, new_path: &str) -> Result<()>;
    async fn chmod(&self, path: &str, mode: u32) -> Result<()>;
    async fn chown(&self, path: &str, uid: u32, gid: u32) -> Result<()>;
    async fn utimes(&self, path: &str, atime_ms: f64, mtime_ms: f64) -> Result<()>;
    async fn truncate(&self, path: &str, len: u64) -> Result<()>;
    async fn pread(&self, path: &str, offset: u64, length: u64) -> Result<Vec<u8>>;
    async fn pwrite(&self, path: &str, offset: u64, data: &[u8]) -> Result<u64>;
}

// ---------------------------------------------------------------------------
// Path guards
// ---------------------------------------------------------------------------

impl AgentOs {
    /// Posix-normalize a path the same way Node's `path.posix.normalize` does.
    ///
    /// Matches Node semantics: collapse `.`/`..` segments and duplicate separators, preserve a
    /// trailing slash when present, keep a leading slash for absolute paths, and return `.` for an
    /// empty result. Above-root `..` segments on an absolute path are discarded; on a relative path
    /// they are retained.
    pub(crate) fn posix_normalize(path: &str) -> String {
        if path.is_empty() {
            return String::from(".");
        }

        let is_absolute = path.starts_with('/');
        let trailing_slash = path.ends_with('/');

        let mut segments: Vec<&str> = Vec::new();
        for part in path.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    match segments.last().copied() {
                        Some(last) if last != ".." => {
                            segments.pop();
                        }
                        Some(_) | None => {
                            // Retain leading `..` only on relative paths; on absolute paths the
                            // segment is silently discarded (cannot go above root).
                            if !is_absolute {
                                segments.push("..");
                            }
                        }
                    }
                }
                other => segments.push(other),
            }
        }

        let mut joined = segments.join("/");
        if joined.is_empty() {
            if is_absolute {
                return String::from("/");
            }
            return String::from(".");
        }

        if trailing_slash {
            joined.push('/');
        }
        if is_absolute {
            let mut absolute = String::from("/");
            absolute.push_str(&joined);
            absolute
        } else {
            joined
        }
    }

    /// Throws `PathNotAbsolute` if not absolute, `PathNotNormalized` if not in normalized form.
    pub(crate) fn assert_safe_absolute_path(path: &str) -> std::result::Result<(), ClientError> {
        if !path.starts_with('/') {
            return Err(ClientError::PathNotAbsolute(path.to_string()));
        }
        if Self::posix_normalize(path) != path {
            return Err(ClientError::PathNotNormalized(path.to_string()));
        }
        Ok(())
    }

    /// Runs the safe guard, then rejects writes to read-only paths.
    pub(crate) fn assert_writable_absolute_path(
        path: &str,
    ) -> std::result::Result<(), ClientError> {
        Self::assert_safe_absolute_path(path)?;
        if path == "/proc"
            || path.starts_with("/proc/")
            || path == "/etc/agentos"
            || path.starts_with("/etc/agentos/")
        {
            return Err(ClientError::PathReadOnly(path.to_string()));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Internal helpers (guest filesystem RPC + path joins)
// ---------------------------------------------------------------------------

impl AgentOs {
    /// Render a batch-method error the way the TypeScript `AgentOs` surfaces `err.message` into
    /// `BatchWriteResult.error` / `BatchReadResult.error`. The error may be a bare [`ClientError`]
    /// (path guards) or an [`anyhow::Error`] wrapping one (kernel RPC failures via
    /// [`Self::guest_fs_call`]), so downcast to recover the exact TS message; otherwise fall back to
    /// the anyhow chain string.
    fn batch_error_message(err: &anyhow::Error) -> String {
        match err.downcast_ref::<ClientError>() {
            Some(client_error) => client_error.batch_message(),
            None => err.to_string(),
        }
    }

    /// Build the VM-scoped ownership for guest filesystem RPCs.
    fn fs_vm_scope(&self) -> wire::OwnershipScope {
        wire::OwnershipScope::VmOwnership(wire::VmOwnership {
            connection_id: self.connection_id().to_string(),
            session_id: self.wire_session_id().to_string(),
            vm_id: self.vm_id().to_string(),
        })
    }

    /// Join a parent directory with a child basename the way the TS fs code does (special-casing the
    /// root so it does not produce a leading `//`).
    fn join_child(dir: &str, child: &str) -> String {
        if dir == "/" {
            format!("/{child}")
        } else {
            format!("{dir}/{child}")
        }
    }

    /// Issue a single guest filesystem RPC and return the typed result, mapping a sidecar
    /// `Rejected` response into a [`ClientError::Kernel`] so the errno `code` survives for parity.
    async fn guest_fs_call(
        &self,
        request: GuestFilesystemCallRequest,
    ) -> Result<GuestFilesystemResultResponse> {
        let scope = self.fs_vm_scope();
        let response = self
            .transport()
            .request_wire(
                scope,
                wire::RequestPayload::GuestFilesystemCallRequest(request),
            )
            .await
            .context("guest filesystem call failed")?;
        match response {
            wire::ResponsePayload::GuestFilesystemResultResponse(result) => Ok(result),
            wire::ResponsePayload::RejectedResponse(rejected) => {
                Err(ClientError::from_rejection(rejected).into())
            }
            other => Err(anyhow::anyhow!(
                "unexpected response to guest filesystem call: {other:?}"
            )),
        }
    }

    /// A guest filesystem call carrying only an operation + path (the common case).
    fn fs_request(
        operation: GuestFilesystemOperation,
        path: impl Into<String>,
    ) -> GuestFilesystemCallRequest {
        GuestFilesystemCallRequest {
            operation,
            path: path.into(),
            destination_path: None,
            target: None,
            content: None,
            encoding: None,
            recursive: false,
            max_depth: None,
            mode: None,
            uid: None,
            gid: None,
            atime_ms: None,
            mtime_ms: None,
            len: None,
            offset: None,
        }
    }

    /// Convert a wire [`GuestFilesystemStat`] into the public [`VirtualStat`] (`*_ms` widened to
    /// `f64` to match JS millisecond precision).
    fn virtual_stat_from(stat: GuestFilesystemStat) -> VirtualStat {
        VirtualStat {
            mode: stat.mode,
            size: stat.size,
            blocks: stat.blocks,
            dev: stat.dev,
            rdev: stat.rdev,
            is_directory: stat.is_directory,
            is_symbolic_link: stat.is_symbolic_link,
            atime_ms: stat.atime_ms as f64,
            mtime_ms: stat.mtime_ms as f64,
            ctime_ms: stat.ctime_ms as f64,
            birthtime_ms: stat.birthtime_ms as f64,
            ino: stat.ino,
            nlink: stat.nlink,
            uid: stat.uid,
            gid: stat.gid,
        }
    }

    // --- low-level kernel ops (each maps to one guest filesystem RPC) ---

    /// Mirrors TS `decodeGuestFilesystemContent`: a missing `content` field is a hard error
    /// (`sidecar returned no file content for <path>`, fail-by-default), `base64` is decoded, and
    /// any other/absent encoding is treated as utf8 bytes.
    async fn kernel_read_file(&self, path: &str) -> Result<Vec<u8>> {
        let result = self
            .guest_fs_call(Self::fs_request(GuestFilesystemOperation::ReadFile, path))
            .await?;
        let content = result
            .content
            .with_context(|| format!("sidecar returned no file content for {path}"))?;
        match result.encoding {
            Some(RootFilesystemEntryEncoding::Base64) => BASE64
                .decode(content.as_bytes())
                .context("decoding base64 file content"),
            Some(RootFilesystemEntryEncoding::Utf8) | None => Ok(content.into_bytes()),
        }
    }

    /// Mirrors TS `encodeGuestFilesystemContent`: string content is sent verbatim with NO `encoding`
    /// field (the sidecar defaults absent encoding to utf8); byte content is base64-encoded and
    /// carries `encoding: "base64"`.
    async fn kernel_write_file(&self, path: &str, content: &FileContent) -> Result<()> {
        let (encoded, encoding) = match content {
            FileContent::Text(text) => (text.clone(), None),
            FileContent::Bytes(bytes) => (
                BASE64.encode(bytes),
                Some(RootFilesystemEntryEncoding::Base64),
            ),
        };
        let mut request = Self::fs_request(GuestFilesystemOperation::WriteFile, path);
        request.content = Some(encoded);
        request.encoding = encoding;
        self.guest_fs_call(request).await?;
        Ok(())
    }

    /// Single-level directory creation. Mirrors TS `kernel.mkdir(path)` (no options), which the
    /// native client maps to the `create_dir` guest filesystem operation. This backs BOTH
    /// `AgentOs::mkdir` (non-recursive) and every `_mkdirp` component, so it always emits
    /// [`GuestFilesystemOperation::CreateDir`] (never `Mkdir`, which the native client reserves for
    /// the recursive `kernel.mkdir(path, { recursive: true })` shape that this code path never uses).
    async fn kernel_mkdir(&self, path: &str) -> Result<()> {
        self.guest_fs_call(Self::fs_request(GuestFilesystemOperation::CreateDir, path))
            .await?;
        Ok(())
    }

    async fn kernel_exists(&self, path: &str) -> Result<bool> {
        let result = self
            .guest_fs_call(Self::fs_request(GuestFilesystemOperation::Exists, path))
            .await?;
        Ok(result.exists.unwrap_or(false))
    }

    async fn kernel_readdir(&self, path: &str) -> Result<Vec<String>> {
        let result = self
            .guest_fs_call(Self::fs_request(GuestFilesystemOperation::ReadDir, path))
            .await?;
        // agentos's READ_DIR now returns rich entries (`entries:
        // list<GuestDirEntry>` with name + is_directory + is_symbolic_link);
        // this name-only accessor projects the basenames. The richer fields back
        // the typed [`Self::read_dir_with_types`] path.
        Ok(result
            .entries
            .unwrap_or_default()
            .into_iter()
            .map(|entry| entry.name)
            .collect())
    }

    async fn kernel_readdir_recursive(
        &self,
        path: &str,
        max_depth: Option<u32>,
    ) -> Result<Vec<wire::GuestDirEntry>> {
        let mut request = Self::fs_request(GuestFilesystemOperation::ReadDirRecursive, path);
        request.max_depth = max_depth;
        let result = self.guest_fs_call(request).await?;
        Ok(result.entries.unwrap_or_default())
    }

    async fn kernel_stat(&self, path: &str) -> Result<VirtualStat> {
        let result = self
            .guest_fs_call(Self::fs_request(GuestFilesystemOperation::Stat, path))
            .await?;
        let stat = result.stat.context("stat response missing stat payload")?;
        Ok(Self::virtual_stat_from(stat))
    }

    async fn kernel_lstat(&self, path: &str) -> Result<VirtualStat> {
        let result = self
            .guest_fs_call(Self::fs_request(GuestFilesystemOperation::Lstat, path))
            .await?;
        let stat = result.stat.context("lstat response missing stat payload")?;
        Ok(Self::virtual_stat_from(stat))
    }

    async fn kernel_remove_path(&self, path: &str, recursive: bool) -> Result<()> {
        let mut request = Self::fs_request(GuestFilesystemOperation::Remove, path);
        request.recursive = recursive;
        self.guest_fs_call(request).await?;
        Ok(())
    }

    async fn kernel_move_path(&self, from: &str, to: &str) -> Result<()> {
        let mut request = Self::fs_request(GuestFilesystemOperation::Move, from);
        request.destination_path = Some(to.to_string());
        request.recursive = true;
        self.guest_fs_call(request).await?;
        Ok(())
    }

    /// Recursively create directories (`mkdir -p`). Uses the WRITABLE guard, then walks each path
    /// component and creates the ones that do not yet exist (mirrors TS `_mkdirp`).
    async fn mkdirp(&self, path: &str) -> Result<()> {
        Self::assert_writable_absolute_path(path)?;
        let mut current = String::new();
        for part in path.split('/').filter(|p| !p.is_empty()) {
            current.push('/');
            current.push_str(part);
            if !self.kernel_exists(&current).await? {
                self.kernel_mkdir(&current).await?;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Filesystem methods
// ---------------------------------------------------------------------------

impl AgentOs {
    /// Read a file's raw bytes (no decode).
    pub async fn read_file(&self, path: &str) -> Result<Vec<u8>> {
        Self::assert_safe_absolute_path(path)?;
        self.kernel_read_file(path).await
    }

    /// Write a file. Writable-path guard; does NOT auto-create parents; `Text` -> UTF-8.
    pub async fn write_file(&self, path: &str, content: impl Into<FileContent>) -> Result<()> {
        Self::assert_writable_absolute_path(path)?;
        let content = content.into();
        self.kernel_write_file(path, &content).await
    }

    /// Batch write. Sequential; never rejects (per-entry error); auto-creates parent dirs.
    pub async fn write_files(&self, entries: Vec<BatchWriteEntry>) -> Vec<BatchWriteResult> {
        let mut results = Vec::with_capacity(entries.len());
        for entry in entries {
            let outcome: Result<()> = async {
                Self::assert_writable_absolute_path(&entry.path)?;
                // Create parent directories as needed. TS slices off everything after the last `/`;
                // for a path like `/foo` this yields an empty parent which is skipped.
                if let Some(idx) = entry.path.rfind('/') {
                    let parent = &entry.path[..idx];
                    if !parent.is_empty() {
                        self.mkdirp(parent).await?;
                    }
                }
                self.kernel_write_file(&entry.path, &entry.content).await?;
                Ok(())
            }
            .await;
            match outcome {
                Ok(()) => results.push(BatchWriteResult {
                    path: entry.path,
                    success: true,
                    error: None,
                }),
                Err(err) => results.push(BatchWriteResult {
                    path: entry.path,
                    success: false,
                    error: Some(Self::batch_error_message(&err)),
                }),
            }
        }
        results
    }

    /// Batch read. Sequential; never rejects; `content` None on failure.
    pub async fn read_files(&self, paths: Vec<String>) -> Vec<BatchReadResult> {
        let mut results = Vec::with_capacity(paths.len());
        for path in paths {
            let outcome: Result<Vec<u8>> = async {
                Self::assert_safe_absolute_path(&path)?;
                self.kernel_read_file(&path).await
            }
            .await;
            match outcome {
                Ok(content) => results.push(BatchReadResult {
                    path,
                    content: Some(content),
                    error: None,
                }),
                Err(err) => results.push(BatchReadResult {
                    path,
                    content: None,
                    error: Some(Self::batch_error_message(&err)),
                }),
            }
        }
        results
    }

    /// Make a directory. Recursive -> writable guard + mkdirp; non-recursive -> safe guard + single
    /// level. The guard asymmetry is load-bearing.
    pub async fn mkdir(&self, path: &str, options: MkdirOptions) -> Result<()> {
        if options.recursive {
            return self.mkdirp(path).await;
        }
        Self::assert_writable_absolute_path(path)?;
        self.kernel_mkdir(path).await
    }

    /// List basenames (may include `.`/`..`).
    pub async fn readdir(&self, path: &str) -> Result<Vec<String>> {
        Self::assert_safe_absolute_path(path)?;
        self.kernel_readdir(path).await
    }

    /// List directory entries with their resolved type, mirroring the TS `readDirWithTypes` used by
    /// the ACP `fs/readDir` host request. `.`/`..` are filtered by the caller. A symlink is reported
    /// as a symlink (lstat-style, not followed); other entries are stat'd as directory vs file.
    pub(crate) async fn acp_read_dir_with_types(&self, path: &str) -> Result<Vec<VirtualDirEntry>> {
        Self::assert_safe_absolute_path(path)?;
        let names = self.kernel_readdir(path).await?;
        let mut entries = Vec::with_capacity(names.len());
        for name in names {
            if name == "." || name == ".." {
                continue;
            }
            let full_path = Self::join_child(path, &name);
            let stat = self.kernel_lstat(&full_path).await?;
            entries.push(VirtualDirEntry {
                name,
                is_directory: stat.is_directory,
                is_symbolic_link: stat.is_symbolic_link,
            });
        }
        Ok(entries)
    }

    /// Typed directory listing: each child reported with its resolved type. agentos's native
    /// `READ_DIR` returns basenames only (`entries: list<str>`), so the type of each entry is derived
    /// with a per-child `lstat` (a symlink is reported as such, lstat-style, not followed). Goes
    /// through the kernel, so mounts are listed correctly. `.`/`..` are filtered.
    pub async fn read_dir_with_types(&self, path: &str) -> Result<Vec<VirtualDirEntry>> {
        self.acp_read_dir_with_types(path).await
    }

    /// Recursive BFS listing; symlinks recorded but NOT descended; a stat failure aborts the call.
    pub async fn readdir_recursive(
        &self,
        path: &str,
        options: ReaddirRecursiveOptions,
    ) -> Result<Vec<DirEntry>> {
        Self::assert_safe_absolute_path(path)?;
        let exclude: std::collections::HashSet<&str> =
            options.exclude.iter().map(String::as_str).collect();
        let entries = self
            .kernel_readdir_recursive(path, options.max_depth)
            .await?;
        let mut excluded_prefixes: Vec<String> = Vec::new();
        let mut results: Vec<DirEntry> = Vec::new();

        for entry in entries {
            if excluded_prefixes.iter().any(|prefix| {
                entry.path == *prefix || entry.path.starts_with(&format!("{prefix}/"))
            }) {
                continue;
            }
            if exclude.contains(entry.name.as_str()) {
                if entry.is_directory && !entry.is_symbolic_link {
                    excluded_prefixes.push(entry.path);
                }
                continue;
            }

            let entry_type = if entry.is_symbolic_link {
                DirEntryType::Symlink
            } else if entry.is_directory {
                DirEntryType::Directory
            } else {
                DirEntryType::File
            };
            results.push(DirEntry {
                path: entry.path,
                entry_type,
                size: entry.size,
            });
        }

        Ok(results)
    }

    /// Return typed immediate children using one sidecar filesystem operation.
    pub async fn readdir_entries(&self, path: &str) -> Result<Vec<VirtualDirEntry>> {
        Self::assert_safe_absolute_path(path)?;
        Ok(self
            .kernel_readdir_recursive(path, Some(0))
            .await?
            .into_iter()
            .map(|entry| VirtualDirEntry {
                name: entry.name,
                is_directory: entry.is_directory,
                is_symbolic_link: entry.is_symbolic_link,
            })
            .collect())
    }

    /// Stat (follows symlinks).
    pub async fn stat(&self, path: &str) -> Result<VirtualStat> {
        Self::assert_safe_absolute_path(path)?;
        self.kernel_stat(path).await
    }

    /// Existence check. Safe-path guard still errors; missing path -> false.
    pub async fn exists(&self, path: &str) -> Result<bool> {
        Self::assert_safe_absolute_path(path)?;
        self.kernel_exists(path).await
    }

    /// Export the root filesystem snapshot. Octal-string mode + utf8/base64 content verbatim.
    pub async fn export_root_filesystem(&self, max_bytes: usize) -> Result<RootSnapshotExport> {
        if max_bytes == 0 {
            return Err(ClientError::Sidecar("max_bytes must be greater than zero".into()).into());
        }
        let scope = self.fs_vm_scope();
        let max_bytes_u64 = u64::try_from(max_bytes)
            .map_err(|_| ClientError::Sidecar("max_bytes exceeds u64".into()))?;
        let response = self
            .transport()
            .request_wire(
                scope,
                wire::RequestPayload::SnapshotRootFilesystemRequest(
                    wire::SnapshotRootFilesystemRequest {
                        max_bytes: max_bytes_u64,
                    },
                ),
            )
            .await
            .context("snapshot root filesystem failed")?;
        let snapshot = match response {
            wire::ResponsePayload::RootFilesystemSnapshotResponse(snapshot) => snapshot,
            wire::ResponsePayload::RejectedResponse(rejected) => {
                return Err(ClientError::from_rejection(rejected).into());
            }
            other => {
                return Err(anyhow::anyhow!(
                    "unexpected response to snapshot root filesystem: {other:?}"
                ));
            }
        };

        let entries = snapshot
            .entries
            .into_iter()
            .map(Self::snapshot_entry_from)
            .collect::<Result<Vec<_>>>()?;

        let snapshot = RootSnapshotExport {
            kind: SnapshotExportKind::SnapshotExport,
            source: FilesystemSnapshotExport {
                format: String::from("agentos-filesystem-snapshot-v1"),
                filesystem: FilesystemSnapshotEntries { entries },
            },
        };
        let size = serde_json::to_vec(&snapshot)
            .context("serializing root filesystem export for bound check")?
            .len();
        if size > max_bytes {
            return Err(ClientError::Sidecar(format!(
				"root filesystem export is {size} bytes, limit is {max_bytes}; raise max_bytes to export this filesystem"
			)).into());
        }
        Ok(snapshot)
    }

    /// Mount a portable sidecar-owned filesystem descriptor.
    pub async fn mount_fs(&self, descriptor: DynamicMountDescriptor) -> Result<()> {
        Self::assert_safe_absolute_path(&descriptor.path)?;
        let config = descriptor
            .plugin
            .config
            .unwrap_or_else(|| serde_json::json!({}));
        let plugin_id = descriptor.plugin.id;
        let mount = wire::MountDescriptor {
            guest_path: descriptor.path,
            guest_source: plugin_id.clone(),
            guest_fstype: plugin_id.clone(),
            read_only: descriptor.read_only,
            plugin: wire::MountPluginDescriptor {
                id: plugin_id,
                config: serde_json::to_string(&config)
                    .context("serializing dynamic mount config")?,
            },
        };
        {
            let mut mounts = self.inner().dynamic_mounts.lock();
            if mounts
                .iter()
                .any(|existing| existing.guest_path == mount.guest_path)
            {
                return Err(ClientError::Sidecar(format!(
                    "mount already exists: {}",
                    mount.guest_path
                ))
                .into());
            }
            mounts.push(mount);
        }
        if let Err(error) = self.reconfigure_dynamic_mounts().await {
            self.inner().dynamic_mounts.lock().pop();
            return Err(error);
        }
        Ok(())
    }

    pub async fn unmount_fs(&self, path: &str) -> Result<()> {
        Self::assert_safe_absolute_path(path)?;
        let removed = {
            let mut mounts = self.inner().dynamic_mounts.lock();
            mounts
                .iter()
                .position(|mount| mount.guest_path == path)
                .map(|index| (index, mounts.remove(index)))
        };
        let Some((index, mount)) = removed else {
            return Ok(());
        };
        if let Err(error) = self.reconfigure_dynamic_mounts().await {
            self.inner().dynamic_mounts.lock().insert(index, mount);
            return Err(error);
        }
        Ok(())
    }

    pub async fn list_mounts(&self) -> Result<Vec<MountInfo>> {
        let response = self
            .transport()
            .request_wire(self.fs_vm_scope(), wire::RequestPayload::ListMountsRequest)
            .await?;
        match response {
            wire::ResponsePayload::ListMountsResponse(response) => Ok(response
                .mounts
                .into_iter()
                .map(|mount| MountInfo {
                    path: mount.path,
                    kind: mount.kind,
                    read_only: mount.read_only,
                })
                .collect()),
            wire::ResponsePayload::RejectedResponse(rejected) => {
                Err(ClientError::from_rejection(rejected).into())
            }
            other => Err(ClientError::Sidecar(format!(
                "unexpected list mounts response: {other:?}"
            ))
            .into()),
        }
    }

    async fn reconfigure_dynamic_mounts(&self) -> Result<()> {
        let inner = self.inner();
        let config = &inner.config;
        let mounts = inner.dynamic_mounts.lock().clone();
        let response = self
            .transport()
            .request_wire(
                self.fs_vm_scope(),
                wire::RequestPayload::ConfigureVmRequest(wire::ConfigureVmRequest {
                    mounts,
                    software: Vec::new(),
                    permissions: Some(crate::agent_os::permissions_policy(config)),
                    module_access_cwd: None,
                    instructions: config.additional_instructions.clone().into_iter().collect(),
                    projected_modules: Vec::new(),
                    command_permissions: std::collections::HashMap::new(),
                    loopback_exempt_ports: config.loopback_exempt_ports.clone(),
                    packages: crate::agent_os::build_package_descriptors(config),
                    packages_mount_at: config.packages_mount_at.clone().unwrap_or_default(),
                    bootstrap_commands: Vec::new(),
                    binding_shim_commands: Vec::new(),
                }),
            )
            .await?;
        match response {
            wire::ResponsePayload::VmConfiguredResponse(_) => Ok(()),
            wire::ResponsePayload::RejectedResponse(rejected) => {
                Err(ClientError::from_rejection(rejected).into())
            }
            other => Err(ClientError::Sidecar(format!(
                "unexpected dynamic mount reconfigure response: {other:?}"
            ))
            .into()),
        }
    }

    /// Move a path through the sidecar primitive. The kernel attempts rename first, then falls back
    /// to recursive copy+remove on EXDEV.
    pub async fn move_path(&self, from: &str, to: &str) -> Result<()> {
        Self::assert_writable_absolute_path(from)?;
        Self::assert_writable_absolute_path(to)?;
        self.kernel_move_path(from, to).await
    }

    /// Delete a path through the sidecar primitive. Non-recursive directory deletes preserve
    /// ENOTEMPTY semantics.
    pub async fn remove(&self, path: &str, options: RemoveOptions) -> Result<()> {
        Self::assert_writable_absolute_path(path)?;
        self.kernel_remove_path(path, options.recursive).await
    }

    /// Convert a wire [`RootFilesystemEntry`] into the public snapshot [`FilesystemEntry`],
    /// preserving the octal-string `mode` and verbatim utf8/base64 `content`/`target`.
    ///
    /// Mirrors TS `convertSidecarRootSnapshotEntries` + `toSnapshotModeString` exactly:
    /// - `mode` falls back kind-dependently when absent (directory 0o755, symlink 0o777, file 0o644).
    /// - file entries ALWAYS carry `content` (defaulting to `""`) and `encoding` (defaulting to
    ///   `utf8`); directory/symlink entries carry neither.
    /// - symlink entries REQUIRE a `target`; a missing target is a hard error (fail-by-default),
    ///   matching the TS `throw`.
    fn snapshot_entry_from(entry: RootFilesystemEntry) -> Result<FilesystemEntry> {
        let entry_type = match entry.kind {
            RootFilesystemEntryKind::File => DirEntryType::File,
            RootFilesystemEntryKind::Directory => DirEntryType::Directory,
            RootFilesystemEntryKind::Symlink => DirEntryType::Symlink,
        };
        // Kind-dependent permission-bit fallback, then octal string with leading `0` masked to the
        // permission bits, matching TS `toSnapshotModeString`.
        let fallback_mode = match entry.kind {
            RootFilesystemEntryKind::Directory => 0o755,
            RootFilesystemEntryKind::Symlink => 0o777,
            RootFilesystemEntryKind::File => 0o644,
        };
        let mode = format!("0{:o}", entry.mode.unwrap_or(fallback_mode) & 0o7777);
        let uid = entry.uid.unwrap_or(0);
        let gid = entry.gid.unwrap_or(0);

        match entry.kind {
            RootFilesystemEntryKind::File => {
                let encoding = match entry.encoding {
                    Some(RootFilesystemEntryEncoding::Utf8) | None => FilesystemEntryEncoding::Utf8,
                    Some(RootFilesystemEntryEncoding::Base64) => FilesystemEntryEncoding::Base64,
                };
                Ok(FilesystemEntry {
                    path: entry.path,
                    entry_type,
                    mode,
                    uid,
                    gid,
                    content: Some(entry.content.unwrap_or_default()),
                    encoding: Some(encoding),
                    target: None,
                })
            }
            RootFilesystemEntryKind::Symlink => {
                let target = entry.target.with_context(|| {
                    format!(
                        "sidecar root snapshot for {} is missing a symlink target",
                        entry.path
                    )
                })?;
                Ok(FilesystemEntry {
                    path: entry.path,
                    entry_type,
                    mode,
                    uid,
                    gid,
                    content: None,
                    encoding: None,
                    target: Some(target),
                })
            }
            RootFilesystemEntryKind::Directory => Ok(FilesystemEntry {
                path: entry.path,
                entry_type,
                mode,
                uid,
                gid,
                content: None,
                encoding: None,
                target: None,
            }),
        }
    }
}
