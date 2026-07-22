use super::{BoundedBytes, BoundedString, BoundedUsize, BoundedVec};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DescriptorWhence {
    Set,
    Current,
    End,
    Data,
    Hole,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DescriptorSyncKind {
    Data,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileRangeOperation {
    Allocate,
    PunchHole,
    Zero,
    Insert,
    Collapse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XattrOperation {
    Get,
    List,
    Set { flags: u32 },
    Remove,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataTarget {
    Descriptor(u32),
    Path { dir_fd: u32, follow_symlinks: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestOpenRights {
    /// The guest supplied Preview1 rights. Explicit zero is a real capability
    /// request and must not be confused with an omitted adapter value.
    Explicit { base: u64, inheriting: u64 },
    /// A non-Preview1 adapter requested Linux-style open semantics. The shared
    /// host layer derives the minimum descriptor rights from the open flags.
    Synthesized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuestOpenSpec {
    pub flags: u32,
    pub mode: Option<u32>,
    pub rights: GuestOpenRights,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilesystemRecordLockKind {
    Read,
    Write,
    Unlock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordLockCommand {
    Query,
    Set,
    Wait,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileTimeUpdate {
    pub atime_ns: Option<u64>,
    pub mtime_ns: Option<u64>,
    pub atime_now: bool,
    pub mtime_now: bool,
}

/// Path metadata changes that must be applied as one admitted host operation.
///
/// This is intentionally executor-neutral. Language adapters may expose a
/// combined `setattr` call, but the sidecar/kernel remain the sole semantic and
/// permission authority for every field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathAttributeUpdate {
    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub atime_ms: Option<u64>,
    pub mtime_ms: Option<u64>,
}

/// One Linux FIEMAP-style extent returned by the kernel.
///
/// The indexed query keeps adapters from asking the sidecar to materialize an
/// unbounded extent list merely to answer the custom `fd_fiemap` ABI one row at
/// a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileExtent {
    pub start: u64,
    pub end: u64,
    pub unwritten: bool,
}

/// Complete semantic fd/path family used by Preview1 and `host_fs`.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum FilesystemOperation {
    ReadFileAt {
        dir_fd: u32,
        path: BoundedString,
        max_bytes: BoundedUsize,
    },
    WriteFileAt {
        dir_fd: u32,
        path: BoundedString,
        bytes: BoundedBytes,
        mode: Option<u32>,
    },
    OpenAt {
        dir_fd: u32,
        path: BoundedString,
        options: GuestOpenSpec,
    },
    OpenTmpfileAt {
        dir_fd: u32,
        path: BoundedString,
        options: GuestOpenSpec,
        linkable: bool,
    },
    Pipe,
    /// Snapshot the process's live kernel fd table. This is read at the point
    /// of a spawn/exec decision; executor adapters must not cache it.
    Snapshot,
    /// Install kernel-owned WASI roots and canonicalize the initial direct
    /// guest fd namespace before any guest code executes.
    CanonicalPreopens,
    Preopen {
        fd: u32,
    },
    Close {
        fd: u32,
    },
    /// Close every guest-visible descriptor at or above `min_fd` as one
    /// kernel-owned table mutation. Missing descriptors are ignored, matching
    /// closefrom(2). Compatibility adapters whose display descriptors differ
    /// from kernel descriptors supply the exact canonical target set instead
    /// of translating the numeric cutoff.
    CloseFrom {
        min_fd: u32,
        exact_fds: Option<BoundedVec<u32>>,
    },
    Renumber {
        from: u32,
        to: u32,
    },
    Duplicate {
        fd: u32,
    },
    DuplicateTo {
        fd: u32,
        target_fd: u32,
    },
    DuplicateMin {
        fd: u32,
        min_fd: u32,
    },
    Move {
        fd: u32,
        replaced_fd: Option<u32>,
    },
    Read {
        fd: u32,
        max_bytes: BoundedUsize,
        offset: Option<u64>,
        deadline_ms: Option<u64>,
    },
    Write {
        fd: u32,
        bytes: BoundedBytes,
        offset: Option<u64>,
        deadline_ms: Option<u64>,
        /// Attempt the unpositioned write once without waiting for capacity.
        /// Adapter decoders choose this progress contract before dispatch.
        nonblocking: bool,
    },
    Seek {
        fd: u32,
        offset: i64,
        whence: DescriptorWhence,
    },
    Sync {
        fd: u32,
        kind: DescriptorSyncKind,
    },
    DescriptorStatus {
        fd: u32,
    },
    DescriptorFileStat {
        fd: u32,
    },
    DescriptorPath {
        fd: u32,
        require_directory: bool,
    },
    DescriptorFdFlags {
        fd: u32,
    },
    SetDescriptorFdFlags {
        fd: u32,
        flags: u32,
    },
    SetDescriptorFlags {
        fd: u32,
        flags: u32,
    },
    SetLength {
        fd: u32,
        length: u64,
    },
    SetPathLength {
        dir_fd: u32,
        path: BoundedString,
        length: u64,
    },
    AdvisoryLock {
        fd: u32,
        operation: u32,
    },
    RecordLock {
        fd: u32,
        command: RecordLockCommand,
        kind: FilesystemRecordLockKind,
        start: u64,
        length: u64,
    },
    CancelRecordLocks,
    NamedPipePeerReady {
        fd: u32,
    },
    ReadDirectory {
        fd: u32,
        cookie: u64,
        max_entries: BoundedUsize,
        max_bytes: BoundedUsize,
    },
    ReadDirectoryAt {
        dir_fd: u32,
        path: BoundedString,
        max_entries: BoundedUsize,
        max_reply_bytes: BoundedUsize,
    },
    Stat {
        target: MetadataTarget,
        path: Option<BoundedString>,
    },
    NodeStatAt {
        dir_fd: u32,
        path: BoundedString,
    },
    NodeLstatAt {
        dir_fd: u32,
        path: BoundedString,
    },
    SetAttributesAt {
        dir_fd: u32,
        path: BoundedString,
        update: PathAttributeUpdate,
        follow_symlinks: bool,
    },
    SetTimes {
        target: MetadataTarget,
        path: Option<BoundedString>,
        update: FileTimeUpdate,
    },
    SetMode {
        target: MetadataTarget,
        path: Option<BoundedString>,
        mode: u32,
    },
    SetOwner {
        target: MetadataTarget,
        path: Option<BoundedString>,
        uid: Option<u32>,
        gid: Option<u32>,
    },
    AccessAt {
        dir_fd: u32,
        path: BoundedString,
        mode: u32,
        effective_ids: bool,
    },
    CreateDirectoryAt {
        dir_fd: u32,
        path: BoundedString,
        mode: u32,
    },
    CreateDirectoriesAt {
        dir_fd: u32,
        path: BoundedString,
        mode: Option<u32>,
    },
    MakeNodeAt {
        dir_fd: u32,
        path: BoundedString,
        mode: u32,
        device: u64,
    },
    LinkAt {
        old_dir_fd: u32,
        old_path: BoundedString,
        follow_old: bool,
        new_dir_fd: u32,
        new_path: BoundedString,
    },
    LinkDescriptorAt {
        fd: u32,
        dir_fd: u32,
        path: BoundedString,
    },
    RenameAt {
        old_dir_fd: u32,
        old_path: BoundedString,
        new_dir_fd: u32,
        new_path: BoundedString,
        flags: u32,
    },
    SymlinkAt {
        target: BoundedString,
        dir_fd: u32,
        path: BoundedString,
    },
    ReadLinkAt {
        dir_fd: u32,
        path: BoundedString,
        max_bytes: BoundedUsize,
    },
    UnlinkAt {
        dir_fd: u32,
        path: BoundedString,
        remove_directory: bool,
    },
    Range {
        fd: u32,
        operation: FileRangeOperation,
        offset: u64,
        length: u64,
        keep_size: bool,
    },
    Extents {
        fd: u32,
        max_entries: BoundedUsize,
    },
    ExtentAt {
        fd: u32,
        index: u32,
    },
    Xattr {
        target: MetadataTarget,
        path: Option<BoundedString>,
        name: Option<BoundedString>,
        value: Option<BoundedBytes>,
        operation: XattrOperation,
        max_result_bytes: BoundedUsize,
    },
    FilesystemStatsAt {
        dir_fd: u32,
        path: BoundedString,
    },
    DescriptorFilesystemStats {
        fd: u32,
    },
    Remount {
        path: BoundedString,
        options: BoundedString,
    },
    StdinRead {
        max_bytes: BoundedUsize,
        timeout_ms: u64,
    },
    StdioWrite {
        fd: u32,
        bytes: BoundedBytes,
    },
    Preopens,
}
