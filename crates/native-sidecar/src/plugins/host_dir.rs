use nix::errno::Errno;
use nix::fcntl::{readlinkat, renameat, AtFlags, OFlag};
use nix::libc;

// The universal resolver (the `confine` module in this file) never returns a
// metadata-only `O_PATH`
// handle (macOS has no `O_PATH`, and gVisor does not honor it as an anchor); a
// read-only open stands in as the metadata anchor and every operation is
// performed fd-relative (`fstat`/`fchmod`/`fchown`/`futimens`), so `O_RDONLY` is
// the portable anchor open mode.
const O_PATH_ANCHOR: OFlag = OFlag::O_RDONLY;
use agentos_execution::{
    GuestModuleReader, LocalModuleResolutionCache, ModuleFsReader, ModuleResolveMode,
    ModuleResolver,
};
use agentos_kernel::mount_plugin::{
    FileSystemPluginFactory, OpenFileSystemPluginRequest, PluginError,
};
use agentos_kernel::mount_table::{
    MountedFileSystem, MountedVirtualFileSystem, ReadOnlyFileSystem,
};
use agentos_kernel::resource_accounting::DEFAULT_MAX_PREAD_BYTES;
use agentos_kernel::vfs::{
    normalize_path, VfsError, VfsResult, VirtualDirEntry, VirtualFileSystem, VirtualStat,
    VirtualTimeSpec, VirtualUtimeSpec,
};
use nix::sys::stat::{fchmod, fstat, fstatat, mkdirat, utimensat, Mode, SFlag, UtimensatFlags};
use nix::sys::time::TimeSpec;
use nix::unistd::{fchownat, linkat, symlinkat, unlinkat, Gid, Uid, UnlinkatFlags};
use serde::Deserialize;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, OwnedFd, RawFd};
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use vfs::posix::TarFileSystem;

const MAX_HOST_DIR_READ_BYTES: usize = DEFAULT_MAX_PREAD_BYTES;

/// Host-mount confinement: one universal resolve-beneath implementation.
///
/// Host-backed mounts (both `host_dir` plugin mounts and the mapped-runtime
/// host paths in [`crate::filesystem`]) expose real host directories to guest
/// code. Every guest-supplied path must resolve *strictly beneath* its mount
/// root: `..` must never ascend above the root and symlinks (relative or
/// absolute) must never escape it. This module is that escape boundary, so it is
/// security-critical.
///
/// # Why there is no `openat2(RESOLVE_BENEATH)`
///
/// Linux offers `openat2(dirfd, path, RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS)`,
/// which performs an atomic, kernel-enforced resolve-beneath open. We used to
/// rely on it on Linux and fall back to a cap-std walk on macOS (which has no
/// such syscall). That split is deleted.
///
/// `openat2` is **not usable as our portable primitive**:
///
///   * **gVisor / runsc (Rivet Compute) does not support it.** Under gVisor the
///     `openat2(RESOLVE_BENEATH)` resolve fails, so a guest `chdir`/`stat` into
///     a host mount returns `ENOENT` even though the path exists. This is the
///     bug that motivated deleting it: it passes under runc (a real kernel) and
///     fails under runsc (a foreign kernel).
///   * **macOS has no `openat2` at all**, which is why a second implementation
///     existed in the first place.
///
/// Maintaining two implementations of a security boundary doubles the surface
/// for escape bugs. Instead this module implements a single manual
/// resolve-beneath walk that runs identically on Linux, macOS, and gVisor:
///
///   * It descends one component at a time with plain `openat(2)` (via
///     [`rustix`], which returns an owned fd without `unsafe`, satisfying this
///     crate's `#![forbid(unsafe_code)]`), anchoring every hop on the parent's
///     file descriptor. `openat(2)` is universally supported, including gVisor.
///   * `..` is resolved by popping an ancestor-fd stack we keep ourselves — we
///     never ask the kernel to resolve `..` (that would be racy under directory
///     renames). This is what makes the walk escape-safe under concurrent
///     mutation of the mount contents by guest code. Intermediate directories
///     are opened `O_RDONLY` as traversal anchors; a search-only directory
///     (execute without read for a non-root sidecar uid) falls back to an
///     `O_PATH` anchor on Linux (see [`open_dir_anchor`]) — a *traversal* anchor
///     only, never a metadata/leaf anchor.
///   * Every real `openat` uses `O_NOFOLLOW`, so a component that is swapped for
///     a symlink after we `lstat` it fails closed with `ELOOP` instead of being
///     followed. Symlinks are instead expanded manually and re-resolved beneath
///     the root; absolute symlink targets and absolute components are rejected
///     as escapes (matching `RESOLVE_BENEATH`).
///   * Results are used as file descriptors, never as recovered path strings, so
///     there is no `/proc/self/fd/N`, `/dev/fd/N`, or `fcntl(F_GETPATH)`
///     dependency. The [`Resolved::real_path`] we return is *diagnostic /
///     logical only* (error messages, best-effort realpath) and must never be
///     re-opened as an authority handle.
///
/// This is not byte-for-byte identical to `openat2` under pathological rename
/// races (the kernel may return `EAGAIN`/`EXDEV` where our fd-anchored walk sees
/// a valid sequence of states), but for our threat model — guest-controlled
/// mount contents, symlink swaps, concurrent guest mutation — it provides the
/// same confinement guarantee.
pub(crate) mod confine {
    use nix::errno::Errno;
    use nix::fcntl::OFlag;
    use nix::sys::stat::Mode;
    use rustix::fs::{self as rfs, AtFlags, FileType, OFlags};
    use std::ffi::{OsStr, OsString};
    use std::os::fd::{BorrowedFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Component, Path, PathBuf};

    /// Maximum number of symlinks expanded during a single resolution, matching
    /// the conventional `MAXSYMLINKS`/`ELOOP` budget.
    const MAX_SYMLINK_EXPANSIONS: u32 = 40;

    /// A path resolved strictly beneath a mount root.
    #[derive(Debug)]
    pub(crate) struct Resolved {
        /// Owned file descriptor for the resolved object. This is the authority
        /// handle: operate on it (fd-relative `*at` calls, `fstat`,
        /// `read`/`write`, `File::from`), never on [`Resolved::real_path`].
        pub(crate) fd: OwnedFd,
        /// The real host path the walk arrived at. **Diagnostic / logical only**
        /// — it is only accurate in a quiescent filesystem, so it must not be
        /// re-opened as an authority handle. Safe to use for error messages and
        /// as a seed for re-resolving children beneath the same root.
        pub(crate) real_path: PathBuf,
    }

    /// A path resolved strictly beneath a mount root down to its *parent
    /// directory*, WITHOUT opening the final component. Returned by
    /// [`resolve_parent_beneath`] so metadata syscalls run fd-relative against
    /// `(parent, name)` (`fstatat`/`fchownat`/`utimensat`) instead of opening the
    /// leaf `O_RDONLY` — a leaf open would require READ permission that POSIX does
    /// not require for these ops, which regresses a non-root sidecar (see the
    /// module guardrail).
    #[derive(Debug)]
    pub(crate) struct ResolvedParent {
        /// Owned fd for the parent directory of the resolved object (or the
        /// resolved directory itself when `name` is `None`). Confined beneath the
        /// root and anchored per-hop with `O_NOFOLLOW`, so a `*at` call against it
        /// cannot traverse outside the mount.
        pub(crate) parent: OwnedFd,
        /// Final component name to pass to the `*at` syscall. `None` when the path
        /// resolved to the root/an ancestor directory itself (no final
        /// component); operate on `parent` directly (e.g. `fstat`).
        pub(crate) name: Option<OsString>,
    }

    /// What the walk does with the final resolved component.
    #[derive(Clone, Copy)]
    enum Leaf {
        /// Open the final component with these flags/mode; yields [`Resolved`].
        Open(OFlags, rfs::Mode),
        /// Open the final component as a DIRECTORY traversal anchor via
        /// [`open_dir_anchor`] (`O_DIRECTORY | O_RDONLY`, with the Linux `O_PATH`
        /// fallback for a search-only dir); yields [`Resolved`]. Used to open a
        /// parent directory purely as an `*at` anchor (never to `readdir` it), so
        /// a search-only parent does not spuriously fail `EACCES`.
        OpenDirAnchor,
        /// Do not open the final component; yield its parent dir fd + name
        /// ([`ResolvedParent`]). The final symlink is followed (stat semantics),
        /// so callers see the real target's parent.
        Parent,
    }

    enum Resolution {
        Opened {
            fd: OwnedFd,
            real_path: PathBuf,
        },
        Parent {
            parent: OwnedFd,
            name: Option<OsString>,
        },
    }

    /// Directory entry classification returned by [`read_dir`], derived from the
    /// directory's own file descriptor.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum EntryKind {
        Directory,
        Symlink,
        Other,
    }

    /// An owned path segment used by the resolver work stack. Owning the
    /// segments lets us splice expanded symlink targets in front of the
    /// remaining path.
    enum Segment {
        Current,
        Parent,
        Name(OsString),
    }

    fn errno(err: rustix::io::Errno) -> Errno {
        Errno::from_raw(err.raw_os_error())
    }

    fn nix_oflag_to_rustix(flags: OFlag) -> OFlags {
        // Preserve every caller-set bit (no silent truncation); the resolver
        // adds `NOFOLLOW`/`CLOEXEC` itself and inspects the well-known
        // access/creation bits below.
        OFlags::from_bits_retain(flags.bits() as u32)
    }

    fn nix_mode_to_rustix(mode: Mode) -> rfs::Mode {
        rfs::Mode::from_bits_retain(mode.bits())
    }

    /// Push the components of `path` onto `stack` so that the first component is
    /// popped first. Returns `EXDEV` if the path is absolute (an escape).
    fn push_path_segments(stack: &mut Vec<Segment>, path: &Path) -> Result<(), Errno> {
        let mut segments = Vec::new();
        for component in path.components() {
            match component {
                Component::CurDir => segments.push(Segment::Current),
                Component::ParentDir => segments.push(Segment::Parent),
                Component::Normal(name) => segments.push(Segment::Name(name.to_os_string())),
                // An absolute path or Windows-style prefix escapes the root;
                // this is exactly what `RESOLVE_BENEATH` rejects.
                Component::RootDir | Component::Prefix(_) => return Err(Errno::EXDEV),
            }
        }
        // Reverse so `Vec::pop` yields the leftmost component first.
        stack.extend(segments.into_iter().rev());
        Ok(())
    }

    fn open_root(root: &Path) -> Result<OwnedFd, Errno> {
        // The mount root is a trusted, configured host path; follow it normally
        // but require it to be a directory.
        match rfs::open(
            root,
            OFlags::DIRECTORY | OFlags::RDONLY | OFlags::CLOEXEC,
            rfs::Mode::empty(),
        ) {
            Ok(fd) => Ok(fd),
            // A search-only root (execute without read for the sidecar uid) can
            // still be traversed via an `O_PATH` anchor on Linux; see
            // [`open_dir_anchor`].
            #[cfg(target_os = "linux")]
            Err(rustix::io::Errno::ACCESS) => rfs::open(
                root,
                OFlags::DIRECTORY | OFlags::PATH | OFlags::CLOEXEC,
                rfs::Mode::empty(),
            )
            .map_err(errno),
            Err(err) => Err(errno(err)),
        }
    }

    /// Open an INTERMEDIATE directory to use as a *traversal anchor* — never for
    /// reading its entries. Prefers `O_RDONLY`; a search-only directory (mode
    /// `0711`/`0111`: execute without read for the sidecar uid) cannot be opened
    /// `O_RDONLY`, so on Linux fall back to `O_PATH`, which needs only search
    /// access yet still works as an `openat`/`*at` dirfd anchor — restoring the
    /// traversal `openat2`/kernel-walk gave a non-root sidecar. macOS has no
    /// `O_PATH` and keeps `O_RDONLY`-only behaviour (its pre-existing limitation).
    ///
    /// This `O_PATH` is a directory *traversal* anchor ONLY: it is used solely as
    /// the `dirfd` argument to `statat`/`openat`/`readlinkat`, never
    /// `read`/`readdir`/`fchmod`, so the module's metadata-anchor prohibition
    /// still holds. `O_NOFOLLOW` is added by [`open_child`] so a component swapped
    /// to a symlink after we `lstat`ed it fails closed.
    fn open_dir_anchor(parent: &OwnedFd, name: &OsStr) -> Result<OwnedFd, Errno> {
        match open_child(
            parent,
            name,
            OFlags::DIRECTORY | OFlags::RDONLY,
            rfs::Mode::empty(),
        ) {
            #[cfg(target_os = "linux")]
            Err(Errno::EACCES) => open_child(
                parent,
                name,
                OFlags::DIRECTORY | OFlags::PATH,
                rfs::Mode::empty(),
            ),
            other => other,
        }
    }

    /// Resolve `relative` strictly beneath `root` and open it with `flags`/`mode`.
    ///
    /// This is the universal replacement for `openat2(root, relative,
    /// RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS)`. `flags`/`mode` are the flags
    /// for the *final* component (intermediate directories are always opened
    /// `O_DIRECTORY | O_RDONLY | O_NOFOLLOW`, falling back to `O_PATH` for a
    /// search-only dir; see [`open_dir_anchor`]). An attempt to escape the root
    /// (via `..`, an absolute component, or an escaping symlink) fails with
    /// `EXDEV`, matching the kernel's `RESOLVE_BENEATH` behaviour.
    pub(crate) fn resolve_beneath(
        root: &Path,
        relative: &Path,
        flags: OFlag,
        mode: Mode,
    ) -> Result<Resolved, Errno> {
        match resolve_impl(
            root,
            relative,
            Leaf::Open(nix_oflag_to_rustix(flags), nix_mode_to_rustix(mode)),
        )? {
            Resolution::Opened { fd, real_path } => Ok(Resolved { fd, real_path }),
            Resolution::Parent { .. } => {
                unreachable!("Leaf::Open always yields Resolution::Opened")
            }
        }
    }

    /// Resolve `relative` strictly beneath `root` to a DIRECTORY fd, opened as a
    /// traversal anchor via [`open_dir_anchor`] (so a search-only final dir uses
    /// the Linux `O_PATH` fallback instead of failing `EACCES`). The result is
    /// only safe to use as an `*at` dirfd anchor, never to `readdir`.
    pub(crate) fn resolve_dir_anchor_beneath(
        root: &Path,
        relative: &Path,
    ) -> Result<Resolved, Errno> {
        match resolve_impl(root, relative, Leaf::OpenDirAnchor)? {
            Resolution::Opened { fd, real_path } => Ok(Resolved { fd, real_path }),
            Resolution::Parent { .. } => {
                unreachable!("Leaf::OpenDirAnchor always yields Resolution::Opened")
            }
        }
    }

    /// Resolve `relative` strictly beneath `root`, following the final symlink
    /// (stat semantics), and return the resolved object's *parent* directory fd
    /// plus its final name — WITHOUT opening the leaf. Metadata callers then run
    /// `fstatat`/`fchownat`/`utimensat` against `(parent, name)`, which needs only
    /// search access on the parent, not read on the leaf. See [`ResolvedParent`].
    pub(crate) fn resolve_parent_beneath(
        root: &Path,
        relative: &Path,
    ) -> Result<ResolvedParent, Errno> {
        match resolve_impl(root, relative, Leaf::Parent)? {
            Resolution::Parent { parent, name } => Ok(ResolvedParent { parent, name }),
            Resolution::Opened { .. } => {
                unreachable!("Leaf::Parent always yields Resolution::Parent")
            }
        }
    }

    /// The one universal resolve-beneath walk shared by [`resolve_beneath`] and
    /// [`resolve_parent_beneath`] — a single security-critical implementation, per
    /// the module guardrail. `leaf` selects whether the final component is opened
    /// or returned as `(parent, name)`.
    fn resolve_impl(root: &Path, relative: &Path, leaf: Leaf) -> Result<Resolution, Errno> {
        // For `Leaf::Parent` the final component is followed like `stat`, so use
        // empty flags (no `NOFOLLOW`/`O_EXCL`) for the follow decision.
        let rflags = match leaf {
            Leaf::Open(flags, _) => flags,
            // Both follow the final symlink (no `NOFOLLOW`/`O_EXCL`); a parent
            // dir path may legitimately end in a symlink-to-directory.
            Leaf::OpenDirAnchor => OFlags::DIRECTORY | OFlags::RDONLY,
            Leaf::Parent => OFlags::empty(),
        };

        // Ancestor fd stack: `dirs[0]` is the root, `dirs.last()` is the
        // directory we are currently resolving relative to. `..` pops this stack
        // instead of asking the kernel to resolve `..`, which would be unsafe
        // under renames.
        let mut dirs: Vec<OwnedFd> = vec![open_root(root)?];
        let mut real_path = root.to_path_buf();

        let mut stack: Vec<Segment> = Vec::new();
        push_path_segments(&mut stack, relative)?;

        let mut symlink_expansions: u32 = 0;

        while let Some(segment) = stack.pop() {
            let name = match segment {
                Segment::Current => continue,
                Segment::Parent => {
                    if dirs.len() == 1 {
                        // `..` at the root escapes.
                        return Err(Errno::EXDEV);
                    }
                    dirs.pop();
                    real_path.pop();
                    continue;
                }
                Segment::Name(name) => name,
            };

            let is_last = stack.is_empty();
            // The final component does not follow a symlink when the caller
            // asked for `O_NOFOLLOW`, or when `O_CREAT | O_EXCL` is set (POSIX
            // requires `O_EXCL` to fail on an existing final symlink rather than
            // follow it).
            let final_no_follow =
                rflags.contains(OFlags::NOFOLLOW) || rflags.contains(OFlags::EXCL | OFlags::CREATE);
            let follow_symlink = !(is_last && final_no_follow);

            let parent = dirs.last().expect("dirs is never empty");

            // Ordinary intermediate directories are overwhelmingly the hot
            // path (module loading and project metadata scans). Opening with
            // O_NOFOLLOW already proves the component was not swapped to a
            // symlink, so avoid a redundant lstat before that open. On the
            // exceptional symlink/non-directory path, fall through to the
            // classifier below to preserve the existing expansion and error
            // semantics.
            if !is_last {
                match open_dir_anchor(parent, name.as_os_str()) {
                    Ok(child) => {
                        dirs.push(child);
                        real_path.push(&name);
                        continue;
                    }
                    Err(Errno::ELOOP | Errno::ENOTDIR) => {}
                    Err(error) => return Err(error),
                }
            }

            match rfs::statat(parent, name.as_os_str(), AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) => {
                    let is_symlink = FileType::from_raw_mode(stat.st_mode) == FileType::Symlink;
                    if is_symlink && follow_symlink {
                        symlink_expansions += 1;
                        if symlink_expansions > MAX_SYMLINK_EXPANSIONS {
                            return Err(Errno::ELOOP);
                        }
                        let target = read_link_at(parent, name.as_os_str())?;
                        if target.is_absolute() {
                            // Absolute symlink target escapes the root.
                            return Err(Errno::EXDEV);
                        }
                        // Splice the target's components in front of the
                        // remaining path, to be resolved from the symlink's own
                        // directory.
                        push_path_segments(&mut stack, &target)?;
                        continue;
                    }

                    if is_last {
                        return match leaf {
                            Leaf::Open(flags, mode) => {
                                let fd = open_child(parent, name.as_os_str(), flags, mode)?;
                                real_path.push(&name);
                                Ok(Resolution::Opened { fd, real_path })
                            }
                            Leaf::OpenDirAnchor => {
                                let fd = open_dir_anchor(parent, name.as_os_str())?;
                                real_path.push(&name);
                                Ok(Resolution::Opened { fd, real_path })
                            }
                            Leaf::Parent => {
                                let parent = dirs.pop().expect("dirs is never empty");
                                Ok(Resolution::Parent {
                                    parent,
                                    name: Some(name),
                                })
                            }
                        };
                    }

                    let child = open_dir_anchor(parent, name.as_os_str())?;
                    dirs.push(child);
                    real_path.push(&name);
                }
                Err(rustix::io::Errno::NOENT) if is_last => {
                    if let Leaf::Open(flags, mode) = leaf {
                        if flags.contains(OFlags::CREATE) {
                            // Creating the final component.
                            let fd = open_child(parent, name.as_os_str(), flags, mode)?;
                            real_path.push(&name);
                            return Ok(Resolution::Opened { fd, real_path });
                        }
                    }
                    return Err(Errno::ENOENT);
                }
                Err(err) => return Err(errno(err)),
            }
        }

        // The path was empty or `.`/`..`-only and resolved to a directory
        // already on the stack (typically the root). Return that directory.
        let fd = dirs.pop().expect("dirs is never empty");
        match leaf {
            Leaf::Open(_, _) | Leaf::OpenDirAnchor => Ok(Resolution::Opened { fd, real_path }),
            Leaf::Parent => Ok(Resolution::Parent {
                parent: fd,
                name: None,
            }),
        }
    }

    /// Open a single already-validated child component. Always adds `O_NOFOLLOW`
    /// (so a component swapped to a symlink after we `lstat`ed it fails closed)
    /// and `O_CLOEXEC`.
    fn open_child(
        parent: &OwnedFd,
        name: &OsStr,
        flags: OFlags,
        mode: rfs::Mode,
    ) -> Result<OwnedFd, Errno> {
        rfs::openat(
            parent,
            name,
            flags | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            mode,
        )
        .map_err(errno)
    }

    fn read_link_at(dirfd: &OwnedFd, name: &OsStr) -> Result<PathBuf, Errno> {
        let target = rfs::readlinkat(dirfd, name, Vec::new()).map_err(errno)?;
        Ok(PathBuf::from(
            OsStr::from_bytes(target.as_bytes()).to_os_string(),
        ))
    }

    /// Enumerate the entries of an open directory fd, classifying each without a
    /// path-based re-walk. Directory entries whose `d_type` is unknown are
    /// resolved with an fd-anchored `fstatat`, so enumeration stays confined to
    /// the directory file descriptor (never a recovered path string). `.` and
    /// `..` are omitted.
    pub(crate) fn read_dir(dir: BorrowedFd<'_>) -> Result<Vec<(OsString, EntryKind)>, Errno> {
        let iter = rfs::Dir::read_from(dir).map_err(errno)?;
        let mut entries = Vec::new();
        for entry in iter {
            let entry = entry.map_err(errno)?;
            let name = entry.file_name();
            if name.to_bytes() == b"." || name.to_bytes() == b".." {
                continue;
            }
            let name = OsStr::from_bytes(name.to_bytes()).to_os_string();
            let kind = match entry.file_type() {
                FileType::Directory => EntryKind::Directory,
                FileType::Symlink => EntryKind::Symlink,
                FileType::Unknown => {
                    // Resolve via the directory fd (fd-anchored, no path re-walk).
                    match rfs::statat(dir, name.as_os_str(), AtFlags::SYMLINK_NOFOLLOW) {
                        Ok(stat) => match FileType::from_raw_mode(stat.st_mode) {
                            FileType::Directory => EntryKind::Directory,
                            FileType::Symlink => EntryKind::Symlink,
                            _ => EntryKind::Other,
                        },
                        // A classification `statat` can fail for benign,
                        // non-fault reasons: the entry vanished after `readdir`
                        // yielded it (`ENOENT`), or the directory is
                        // readable-but-not-searchable so the type simply cannot be
                        // determined (`EACCES`/`EPERM` — common on `DT_UNKNOWN`
                        // filesystems like the gVisor gofer). Treat those as
                        // `Other` (unknown type) so a plain listing still returns
                        // the names. Any OTHER errno is a real fault and must
                        // propagate rather than be swallowed into a misclassification.
                        Err(
                            rustix::io::Errno::NOENT
                            | rustix::io::Errno::ACCESS
                            | rustix::io::Errno::PERM,
                        ) => EntryKind::Other,
                        Err(err) => return Err(errno(err)),
                    }
                }
                _ => EntryKind::Other,
            };
            entries.push((name, kind));
        }
        Ok(entries)
    }
}

/// An owned file descriptor resolved strictly beneath a mount root by
/// [`confine::resolve_beneath`]. All operations go through the fd (fd-relative
/// `*at` calls, `fstat`, fd `read`/`write`) — never a recovered path string — so
/// they stay confined to the resolved object and TOCTOU-safe. The `OwnedFd`
/// closes the descriptor on drop.
#[derive(Debug)]
struct AnchoredFd {
    fd: OwnedFd,
    /// The real host path the resolve-beneath walk arrived at. **Diagnostic /
    /// logical only** (see [`confine::Resolved::real_path`]): safe for error
    /// messages and for re-expressing the resolved path as a guest path, but it
    /// must never be re-opened as an authority handle.
    real_path: PathBuf,
}

impl AnchoredFd {
    /// `fchmod` the resolved object. Used only by `chmod`, which still opens the
    /// leaf (`O_RDONLY | O_NOFOLLOW`) rather than going through the parent-fd +
    /// `*at` path the other metadata ops use: `fchmodat` has no
    /// `AT_SYMLINK_NOFOLLOW`, so a leaf-free `chmod` cannot close the
    /// check-then-mutate symlink-swap race. `chmod` therefore accepts the
    /// non-root read requirement in exchange for TOCTOU safety (see the module
    /// guardrail / `crates/native-sidecar/CLAUDE.md`).
    fn set_mode(&self, mode: u32) -> Result<(), Errno> {
        fchmod(self.as_raw_fd(), Mode::from_bits_truncate(mode as _))
    }

    /// Consume the handle, yielding a [`std::fs::File`] over the owned fd (no
    /// path re-open).
    fn into_file(self) -> File {
        File::from(self.fd)
    }
}

impl AsRawFd for AnchoredFd {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
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

pub(crate) trait HostDirReadLimitContext {
    fn host_dir_max_read_bytes(&self) -> Option<usize>;
}

impl HostDirReadLimitContext for () {
    fn host_dir_max_read_bytes(&self) -> Option<usize> {
        Some(MAX_HOST_DIR_READ_BYTES)
    }
}

impl<Context> FileSystemPluginFactory<Context> for HostDirMountPlugin
where
    Context: HostDirReadLimitContext,
{
    fn plugin_id(&self) -> &'static str {
        "host_dir"
    }

    fn open(
        &self,
        request: OpenFileSystemPluginRequest<'_, Context>,
    ) -> Result<Box<dyn MountedFileSystem>, PluginError> {
        let max_read_bytes = request.context.host_dir_max_read_bytes();
        self.open_with_read_limit(request, max_read_bytes)
    }
}

impl HostDirMountPlugin {
    fn open_with_read_limit<Context>(
        &self,
        request: OpenFileSystemPluginRequest<'_, Context>,
        max_read_bytes: Option<usize>,
    ) -> Result<Box<dyn MountedFileSystem>, PluginError> {
        let config: HostDirMountConfig = serde_json::from_value(request.config.clone())
            .map_err(|error| PluginError::invalid_input(error.to_string()))?;
        let filesystem = HostDirFilesystem::new_with_read_limit(&config.host_path, max_read_bytes)?;
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
    max_read_bytes: Option<usize>,
}

impl HostDirFilesystem {
    #[allow(dead_code)]
    pub(crate) fn new(host_path: impl AsRef<Path>) -> VfsResult<Self> {
        Self::new_with_read_limit(host_path, Some(MAX_HOST_DIR_READ_BYTES))
    }

    pub(crate) fn new_with_read_limit(
        host_path: impl AsRef<Path>,
        max_read_bytes: Option<usize>,
    ) -> VfsResult<Self> {
        let host_path_str = host_path.as_ref().to_string_lossy().into_owned();
        let canonical_root = fs::canonicalize(host_path.as_ref())
            .map_err(|error| io_error_to_vfs("open", &host_path_str, error))?;
        let metadata = fs::metadata(&canonical_root)
            .map_err(|error| io_error_to_vfs("stat", &host_path_str, error))?;
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
            max_read_bytes,
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

    /// Open `relative` strictly beneath the mount root via the universal
    /// resolve-beneath walk in [`confine`]. See that module for why `openat2`
    /// is not used.
    fn open_beneath(&self, relative: &Path, flags: OFlag, mode: Mode) -> VfsResult<AnchoredFd> {
        let relative_display = relative.display().to_string();
        let resolved =
            confine::resolve_beneath(&self.host_root, relative, flags, mode).map_err(|error| {
                match error {
                    Errno::EXDEV => VfsError::access_denied(
                        "open",
                        &relative_display,
                        Some("path escapes host directory"),
                    ),
                    other => io_error_to_vfs("open", &relative_display, nix_to_io(other)),
                }
            })?;
        Ok(AnchoredFd {
            fd: resolved.fd,
            real_path: resolved.real_path,
        })
    }

    fn open_directory_beneath(&self, relative: &Path) -> VfsResult<AnchoredFd> {
        self.open_beneath(
            relative,
            OFlag::O_DIRECTORY | OFlag::O_RDONLY,
            Mode::empty(),
        )
    }

    /// Open `relative` as a directory *anchor* for `*at` calls, using the
    /// search-only-dir `O_PATH` fallback (see [`confine::open_dir_anchor`]). Use
    /// this — not [`open_directory_beneath`] — when the fd is only an `*at`
    /// anchor (parent for `fstatat`/`fchownat`/`unlinkat`/…), never `readdir`ed,
    /// so a search-only parent does not spuriously fail `EACCES`. `readdir` paths
    /// must keep [`open_directory_beneath`] so listing a search-only dir still
    /// reports `EACCES`.
    fn open_dir_anchor_beneath(&self, relative: &Path) -> VfsResult<AnchoredFd> {
        let relative_display = relative.display().to_string();
        let resolved =
            confine::resolve_dir_anchor_beneath(&self.host_root, relative).map_err(|error| {
                match error {
                    Errno::EXDEV => VfsError::access_denied(
                        "open",
                        &relative_display,
                        Some("path escapes host directory"),
                    ),
                    other => io_error_to_vfs("open", &relative_display, nix_to_io(other)),
                }
            })?;
        Ok(AnchoredFd {
            fd: resolved.fd,
            real_path: resolved.real_path,
        })
    }

    fn host_path_for_fd(&self, fd: &AnchoredFd, virtual_path: &str) -> VfsResult<PathBuf> {
        // The resolve-beneath walk already confined `fd` under the root and
        // recorded the logical host path it arrived at; re-check it defensively
        // before handing it back for path re-expression.
        self.ensure_within_root(&fd.real_path, virtual_path)?;
        Ok(fd.real_path.clone())
    }

    /// `lstat` the leaf through the confined parent fd and reject a symlink leaf
    /// with `EPERM` — metadata mutations must not follow symlinks (a followed
    /// symlink could retarget the op onto an escaped host path). Needs only search
    /// permission on the parent, never read on the leaf.
    fn reject_symlink_leaf(
        &self,
        parent_dir: &AnchoredFd,
        name: &std::ffi::OsStr,
        normalized: &str,
        op: &'static str,
    ) -> VfsResult<()> {
        let stat = fstatat(
            Some(parent_dir.as_raw_fd()),
            name,
            AtFlags::AT_SYMLINK_NOFOLLOW,
        )
        .map_err(|error| io_error_to_vfs(op, normalized, nix_to_io(error)))?;
        if stat.st_mode & SFlag::S_IFMT.bits() == SFlag::S_IFLNK.bits() {
            return Err(VfsError::new(
                "EPERM",
                format!("{op} '{normalized}': metadata operations do not follow symlinks"),
            ));
        }
        Ok(())
    }

    // `open(O_NOFOLLOW)` on a symlink fails outright, so we cannot open the link
    // itself as an anchor and inspect it. Instead detect the symlink directly:
    // `lstat` the final component through the resolved parent fd and reject it
    // with `EPERM`; otherwise open the (non-symlink) target as the metadata
    // anchor. `O_NOFOLLOW` on the anchor open closes the lstat→open race (a
    // component swapped to a symlink after the check fails closed with `ELOOP`).
    //
    // This is used ONLY by `chmod` (see [`AnchoredFd::set_mode`]); `chown`/
    // `utimes`/`stat` avoid the leaf open entirely (parent-fd + `*at`).
    fn open_metadata_beneath(&self, path: &str, op: &'static str) -> VfsResult<AnchoredFd> {
        // The mount root has no final component to lstat and is always a
        // directory (never a symlink), so open it directly as the anchor.
        // (`split_parent` would otherwise reject `/` with EINVAL.)
        let (_, root_relative) = self.relative_virtual_path(path);
        if root_relative.file_name().is_none() {
            return self.open_beneath(&root_relative, O_PATH_ANCHOR, Mode::empty());
        }
        let (parent_dir, _, name, normalized) = self.split_parent(path, false)?;
        self.reject_symlink_leaf(&parent_dir, name.as_os_str(), &normalized, op)?;
        let (_, relative) = self.relative_virtual_path(path);
        self.open_beneath(&relative, O_PATH_ANCHOR | OFlag::O_NOFOLLOW, Mode::empty())
    }

    fn ensure_directory_tree(
        &self,
        relative_dir: &Path,
        virtual_path: &str,
        mode: u32,
    ) -> VfsResult<()> {
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
                Mode::from_bits_truncate(mode as _),
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
            self.ensure_directory_tree(&parent, &normalized, 0o755)?;
        }
        // The parent fd is used only as an `*at` anchor by every `split_parent`
        // caller (never `readdir`ed), so use the search-only `O_PATH` fallback:
        // `chown`/`utimes`/`remove`/`rename`/… must work through a search-only
        // parent directory, matching POSIX (they need only search on the parent).
        let parent_dir = self.open_dir_anchor_beneath(&parent)?;
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

    fn existing_utime_specs(
        &self,
        parent_dir: &AnchoredFd,
        name: &std::ffi::OsStr,
        virtual_path: &str,
        follow_symlinks: bool,
    ) -> VfsResult<(VirtualTimeSpec, VirtualTimeSpec)> {
        let flags = if follow_symlinks {
            AtFlags::empty()
        } else {
            AtFlags::AT_SYMLINK_NOFOLLOW
        };
        let stat = fstatat(Some(parent_dir.as_raw_fd()), name, flags)
            .map_err(|error| io_error_to_vfs("utimes", virtual_path, nix_to_io(error)))?;
        let atime = VirtualTimeSpec::new(
            stat.st_atime,
            stat.st_atime_nsec.clamp(0, 999_999_999) as u32,
        )?;
        let mtime = VirtualTimeSpec::new(
            stat.st_mtime,
            stat.st_mtime_nsec.clamp(0, 999_999_999) as u32,
        )?;
        Ok((atime, mtime))
    }

    fn resolve_utime_timespec(spec: VirtualUtimeSpec, existing: VirtualTimeSpec) -> TimeSpec {
        match spec {
            VirtualUtimeSpec::Set(spec) => TimeSpec::new(spec.sec, spec.nsec as libc::c_long),
            VirtualUtimeSpec::Now => TimeSpec::new(0, libc::UTIME_NOW),
            VirtualUtimeSpec::Omit => TimeSpec::new(existing.sec, libc::UTIME_OMIT),
        }
    }

    fn apply_utimens(
        &self,
        path: &str,
        atime: VirtualUtimeSpec,
        mtime: VirtualUtimeSpec,
        follow_symlinks: bool,
    ) -> VfsResult<()> {
        let (parent_dir, _, name, normalized) = self.split_parent(path, false)?;
        if follow_symlinks {
            // `utimes` (follow) rejects a symlink leaf, matching `chmod`/`chown`;
            // the richer `lutimes` path (`follow_symlinks == false`) instead
            // operates on the symlink itself via `NoFollowSymlink` below.
            self.reject_symlink_leaf(&parent_dir, name.as_os_str(), &normalized, "utimes")?;
        }
        // Read existing times (for `Omit`) and apply the new times both with
        // `NoFollowSymlink`. For `follow_symlinks == true` the leaf was just
        // confirmed non-symlink, so nofollow is behaviourally identical while
        // additionally closing the check→mutate symlink-swap race (a leaf swapped
        // to a symlink afterwards is operated on as the symlink, staying confined,
        // instead of following it to an escaped host path).
        let existing = match (atime, mtime) {
            (VirtualUtimeSpec::Omit, _) | (_, VirtualUtimeSpec::Omit) => Some(
                self.existing_utime_specs(&parent_dir, name.as_os_str(), &normalized, false)?,
            ),
            _ => None,
        };
        let existing_atime = existing
            .as_ref()
            .map(|(atime, _)| *atime)
            .unwrap_or(VirtualTimeSpec { sec: 0, nsec: 0 });
        let existing_mtime = existing
            .as_ref()
            .map(|(_, mtime)| *mtime)
            .unwrap_or(VirtualTimeSpec { sec: 0, nsec: 0 });
        let times = [
            Self::resolve_utime_timespec(atime, existing_atime),
            Self::resolve_utime_timespec(mtime, existing_mtime),
        ];
        utimensat(
            Some(parent_dir.as_raw_fd()),
            name.as_os_str(),
            &times[0],
            &times[1],
            UtimensatFlags::NoFollowSymlink,
        )
        .map_err(|error| io_error_to_vfs("utimes", &normalized, nix_to_io(error)))
    }

    fn stat_from_metadata(metadata: fs::Metadata) -> VirtualStat {
        let atime_ms = metadata.atime().max(0) as u64 * 1_000
            + (metadata.atime_nsec().max(0) as u64 / 1_000_000);
        let atime_nsec = metadata.atime_nsec().clamp(0, 999_999_999) as u32;
        let mtime_ms = metadata.mtime().max(0) as u64 * 1_000
            + (metadata.mtime_nsec().max(0) as u64 / 1_000_000);
        let mtime_nsec = metadata.mtime_nsec().clamp(0, 999_999_999) as u32;
        let ctime_ms = metadata.ctime().max(0) as u64 * 1_000
            + (metadata.ctime_nsec().max(0) as u64 / 1_000_000);
        let ctime_nsec = metadata.ctime_nsec().clamp(0, 999_999_999) as u32;
        VirtualStat {
            mode: metadata.mode(),
            size: metadata.size(),
            blocks: metadata.blocks(),
            dev: metadata.dev(),
            rdev: metadata.rdev(),
            is_directory: metadata.is_dir(),
            is_symbolic_link: metadata.file_type().is_symlink(),
            atime_ms,
            atime_nsec,
            mtime_ms,
            mtime_nsec,
            ctime_ms,
            ctime_nsec,
            birthtime_ms: ctime_ms,
            ino: metadata.ino(),
            nlink: metadata.nlink(),
            uid: metadata.uid(),
            gid: metadata.gid(),
        }
    }

    #[allow(clippy::unnecessary_cast)]
    fn stat_from_file_stat(stat: nix::sys::stat::FileStat) -> VirtualStat {
        let file_type = SFlag::from_bits_truncate(stat.st_mode);
        let atime_ms =
            stat.st_atime.max(0) as u64 * 1_000 + (stat.st_atime_nsec.max(0) as u64 / 1_000_000);
        let atime_nsec = stat.st_atime_nsec.clamp(0, 999_999_999) as u32;
        let mtime_ms =
            stat.st_mtime.max(0) as u64 * 1_000 + (stat.st_mtime_nsec.max(0) as u64 / 1_000_000);
        let mtime_nsec = stat.st_mtime_nsec.clamp(0, 999_999_999) as u32;
        let ctime_ms =
            stat.st_ctime.max(0) as u64 * 1_000 + (stat.st_ctime_nsec.max(0) as u64 / 1_000_000);
        let ctime_nsec = stat.st_ctime_nsec.clamp(0, 999_999_999) as u32;

        VirtualStat {
            // Widen for platform differences: mode_t/dev_t/nlink_t are narrower
            // on macOS (u16/i32/u16) than on Linux.
            mode: stat.st_mode as u32,
            size: stat.st_size as u64,
            blocks: stat.st_blocks as u64,
            dev: stat.st_dev as u64,
            rdev: stat.st_rdev as u64,
            is_directory: file_type == SFlag::S_IFDIR,
            is_symbolic_link: file_type == SFlag::S_IFLNK,
            atime_ms,
            atime_nsec,
            mtime_ms,
            mtime_nsec,
            ctime_ms,
            ctime_nsec,
            birthtime_ms: ctime_ms,
            ino: stat.st_ino,
            // st_nlink is u64 on x86_64 but u32 on aarch64 / u16 on macOS; widen.
            nlink: stat.st_nlink as u64,
            uid: stat.st_uid,
            gid: stat.st_gid,
        }
    }

    fn write_all_at(
        &self,
        file: &File,
        content: &[u8],
        mut offset: u64,
        path: &str,
    ) -> VfsResult<()> {
        let mut written = 0usize;
        while written < content.len() {
            let bytes_written = file
                .write_at(&content[written..], offset)
                .map_err(|error| io_error_to_vfs("write", path, error))?;
            if bytes_written == 0 {
                return Err(io_error_to_vfs(
                    "write",
                    path,
                    io::Error::new(io::ErrorKind::WriteZero, "failed to write whole buffer"),
                ));
            }

            written += bytes_written;
            offset = offset.checked_add(bytes_written as u64).ok_or_else(|| {
                VfsError::new("EINVAL", format!("pwrite offset overflow: {path}"))
            })?;
        }

        Ok(())
    }

    fn check_read_length(&self, path: &str, length: usize) -> VfsResult<()> {
        if let Some(limit) = self.max_read_bytes {
            if length <= limit {
                return Ok(());
            }

            return Err(VfsError::new(
                "EINVAL",
                format!("read length {length} exceeds host_dir limit {limit}: {path}"),
            ));
        }

        Ok(())
    }

    fn check_full_read_metadata(&self, path: &str, size: u64) -> VfsResult<()> {
        if let Some(limit) = self.max_read_bytes {
            if size <= limit as u64 {
                return Ok(());
            }

            return Err(VfsError::new(
                "EINVAL",
                format!("file size {size} exceeds host_dir read limit {limit}: {path}"),
            ));
        }

        Ok(())
    }

    fn read_to_end_bounded(&self, file: &mut File, path: &str) -> VfsResult<Vec<u8>> {
        let mut buffer = Vec::new();
        match self.max_read_bytes {
            Some(limit) => {
                Read::by_ref(file)
                    .take((limit as u64).saturating_add(1))
                    .read_to_end(&mut buffer)
                    .map_err(|error| io_error_to_vfs("open", path, error))?;
            }
            None => {
                file.read_to_end(&mut buffer)
                    .map_err(|error| io_error_to_vfs("open", path, error))?;
            }
        }
        self.check_read_length(path, buffer.len())?;
        Ok(buffer)
    }

    fn write_file_with_creation_mode(
        &mut self,
        path: &str,
        content: Vec<u8>,
        file_mode: u32,
    ) -> VfsResult<()> {
        let (_, relative) = self.relative_virtual_path(path);
        if let Some(parent) = relative.parent() {
            self.ensure_directory_tree(parent, path, 0o755)?;
        }
        let handle = self.open_beneath(
            &relative,
            OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_TRUNC,
            Mode::from_bits_truncate(file_mode as _),
        )?;
        let mut file = handle.into_file();
        file.write_all(&content)
            .map_err(|error| io_error_to_vfs("write", path, error))
    }

    fn create_dir_with_creation_mode(&mut self, path: &str, mode: u32) -> VfsResult<()> {
        let (normalized, relative) = self.relative_virtual_path(path);
        if relative.file_name().is_none() {
            return Err(VfsError::new(
                "EEXIST",
                format!("mkdir '{normalized}': File exists"),
            ));
        }
        let (parent_dir, _, name, normalized) = self.split_parent(path, false)?;
        mkdirat(
            Some(parent_dir.as_raw_fd()),
            name.as_os_str(),
            Mode::from_bits_truncate(mode as _),
        )
        .map_err(|error| io_error_to_vfs("mkdir", &normalized, nix_to_io(error)))
    }

    fn mkdir_with_creation_mode(
        &mut self,
        path: &str,
        recursive: bool,
        mode: u32,
    ) -> VfsResult<()> {
        if recursive {
            let (normalized, relative) = self.relative_virtual_path(path);
            self.ensure_directory_tree(&relative, &normalized, mode)
        } else {
            self.create_dir_with_creation_mode(path, mode)
        }
    }
}

impl VirtualFileSystem for HostDirFilesystem {
    fn read_file(&mut self, path: &str) -> VfsResult<Vec<u8>> {
        let (_, relative) = self.relative_virtual_path(path);
        let handle = self.open_beneath(&relative, OFlag::O_RDONLY, Mode::empty())?;
        let mut file = handle.into_file();
        self.check_full_read_metadata(
            path,
            file.metadata()
                .map_err(|error| io_error_to_vfs("open", path, error))?
                .len(),
        )?;
        self.read_to_end_bounded(&mut file, path)
    }

    fn read_dir(&mut self, path: &str) -> VfsResult<Vec<String>> {
        let (_, relative) = self.relative_virtual_path(path);
        let directory = self.open_directory_beneath(&relative)?;
        let mut entries = confine::read_dir(directory.fd.as_fd())
            .map_err(|error| io_error_to_vfs("readdir", path, nix_to_io(error)))?
            .into_iter()
            .map(|(name, _)| name.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        entries.sort();
        Ok(entries)
    }

    fn read_dir_with_types(&mut self, path: &str) -> VfsResult<Vec<VirtualDirEntry>> {
        let (_, relative) = self.relative_virtual_path(path);
        let directory = self.open_directory_beneath(&relative)?;
        let mut entries = confine::read_dir(directory.fd.as_fd())
            .map_err(|error| io_error_to_vfs("readdir", path, nix_to_io(error)))?
            .into_iter()
            .map(|(name, kind)| VirtualDirEntry {
                name: name.to_string_lossy().into_owned(),
                is_directory: kind == confine::EntryKind::Directory,
                is_symbolic_link: kind == confine::EntryKind::Symlink,
            })
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(entries)
    }

    fn write_file(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<()> {
        self.write_file_with_creation_mode(path, content.into(), 0o644)
    }

    fn write_file_with_mode(
        &mut self,
        path: &str,
        content: impl Into<Vec<u8>>,
        mode: Option<u32>,
    ) -> VfsResult<()> {
        self.write_file_with_creation_mode(path, content.into(), mode.unwrap_or(0o666))
    }

    fn create_dir(&mut self, path: &str) -> VfsResult<()> {
        self.create_dir_with_creation_mode(path, 0o755)
    }

    fn create_dir_with_mode(&mut self, path: &str, mode: Option<u32>) -> VfsResult<()> {
        self.create_dir_with_creation_mode(path, mode.unwrap_or(0o777))
    }

    fn mkdir(&mut self, path: &str, recursive: bool) -> VfsResult<()> {
        self.mkdir_with_creation_mode(path, recursive, 0o755)
    }

    fn mkdir_with_mode(&mut self, path: &str, recursive: bool, mode: Option<u32>) -> VfsResult<()> {
        self.mkdir_with_creation_mode(path, recursive, mode.unwrap_or(0o777))
    }

    fn exists(&self, path: &str) -> bool {
        // Resolve to the parent + leaf (following symlinks), which needs only
        // search access — `open_beneath(O_RDONLY)` would falsely report a
        // statable-but-unreadable file as missing under a non-root sidecar.
        let (_, relative) = self.relative_virtual_path(path);
        confine::resolve_parent_beneath(&self.host_root, &relative).is_ok()
    }

    fn stat(&mut self, path: &str) -> VfsResult<VirtualStat> {
        // `fstatat`/`fstat` against the confined parent fd — no leaf open, so no
        // READ permission is required on the target (POSIX `stat` needs none).
        // The walk already followed and confined the final symlink, so the leaf
        // name is a real, non-symlink target of the resolved parent.
        let (_, relative) = self.relative_virtual_path(path);
        let resolved = confine::resolve_parent_beneath(&self.host_root, &relative).map_err(
            |error| match error {
                Errno::EXDEV => {
                    VfsError::access_denied("stat", path, Some("path escapes host directory"))
                }
                other => io_error_to_vfs("stat", path, nix_to_io(other)),
            },
        )?;
        let stat = match &resolved.name {
            Some(name) => fstatat(
                Some(resolved.parent.as_raw_fd()),
                name.as_os_str(),
                AtFlags::AT_SYMLINK_NOFOLLOW,
            ),
            None => fstat(resolved.parent.as_raw_fd()),
        }
        .map_err(|error| io_error_to_vfs("stat", path, nix_to_io(error)))?;
        Ok(Self::stat_from_file_stat(stat))
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
        let file = self.open_beneath(&relative, O_PATH_ANCHOR, Mode::empty())?;
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
        if normalize_path(path) == "/" {
            return self
                .host_root_dir
                .metadata()
                .map(Self::stat_from_metadata)
                .map_err(|error| io_error_to_vfs("lstat", path, error));
        }

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
        let handle = self.open_metadata_beneath(path, "chmod")?;
        handle
            .set_mode(mode)
            .map_err(|error| io_error_to_vfs("chmod", path, nix_to_io(error)))
    }

    fn chown(&mut self, path: &str, uid: u32, gid: u32) -> VfsResult<()> {
        // Parent-fd + `fchownat` (no leaf open → no read requirement). Reject a
        // symlink leaf, then use `AT_SYMLINK_NOFOLLOW`: for the confirmed
        // non-symlink leaf this is behaviourally a plain `chown`, and it also
        // closes the check→mutate symlink-swap race (a leaf swapped to a symlink
        // afterwards is `lchown`ed in place, staying confined, never followed to
        // an escaped host path).
        let (parent_dir, _, name, normalized) = self.split_parent(path, false)?;
        self.reject_symlink_leaf(&parent_dir, name.as_os_str(), &normalized, "chown")?;
        fchownat(
            Some(parent_dir.as_raw_fd()),
            name.as_os_str(),
            Some(Uid::from_raw(uid)),
            Some(Gid::from_raw(gid)),
            AtFlags::AT_SYMLINK_NOFOLLOW,
        )
        .map_err(|error| VfsError::new(error_code(&error), error.to_string()))
    }

    fn utimes(&mut self, path: &str, atime_ms: u64, mtime_ms: u64) -> VfsResult<()> {
        self.apply_utimens(
            path,
            VirtualUtimeSpec::Set(VirtualTimeSpec::from_millis(atime_ms)),
            VirtualUtimeSpec::Set(VirtualTimeSpec::from_millis(mtime_ms)),
            true,
        )
    }

    fn utimes_spec(
        &mut self,
        path: &str,
        atime: VirtualUtimeSpec,
        mtime: VirtualUtimeSpec,
        follow_symlinks: bool,
    ) -> VfsResult<()> {
        self.apply_utimens(path, atime, mtime, follow_symlinks)
    }

    fn truncate(&mut self, path: &str, length: u64) -> VfsResult<()> {
        let (_, relative) = self.relative_virtual_path(path);
        let handle = self.open_beneath(&relative, OFlag::O_WRONLY, Mode::empty())?;
        handle
            .into_file()
            .set_len(length)
            .map_err(|error| io_error_to_vfs("truncate", path, error))
    }

    fn pread(&mut self, path: &str, offset: u64, length: usize) -> VfsResult<Vec<u8>> {
        self.check_read_length(path, length)?;
        let (_, relative) = self.relative_virtual_path(path);
        let handle = self.open_beneath(&relative, OFlag::O_RDONLY, Mode::empty())?;
        let file = handle.into_file();
        let mut buffer = vec![0; length];
        let bytes_read = file
            .read_at(&mut buffer, offset)
            .map_err(|error| io_error_to_vfs("open", path, error))?;
        buffer.truncate(bytes_read);
        Ok(buffer)
    }

    fn pwrite(&mut self, path: &str, content: impl Into<Vec<u8>>, offset: u64) -> VfsResult<()> {
        let (_, relative) = self.relative_virtual_path(path);
        let handle = self.open_beneath(&relative, OFlag::O_WRONLY, Mode::empty())?;
        let file = handle.into_file();
        let content = content.into();
        self.write_all_at(&file, &content, offset, path)
    }
}

/// One read-only `host_dir`/`module_access` mount, keyed by its guest mount
/// point. The filesystem reads mount-relative virtual paths (e.g. `/foo/index.js`
/// for a mount at `/root/node_modules`).
// `dead_code` is allowed because `host_dir.rs` is also `#[path]`-included by
// `tests/host_dir.rs`, whose test compilation exercises only the filesystem
// plugin and not the module-reader path (which the real lib build does use).
#[allow(dead_code)]
#[derive(Clone)]
struct HostDirModuleMount {
    /// Normalized guest mount point, e.g. `/root/node_modules`.
    guest_prefix: String,
    filesystem: ModuleMountBackend,
}

/// Backing filesystem for one module-reader mount: an anchored host dir
/// (`host_dir`/`module_access` mounts) or a packed `.aospkg` tar
/// (`agentos_packages` package-version leaves). Both are read-only and safe to
/// use off the service loop — the tar reader serves mmap-backed byte ranges
/// from the shared identity-keyed archive cache and never touches the kernel.
#[allow(dead_code)]
#[derive(Clone)]
enum ModuleMountBackend {
    Host(HostDirFilesystem),
    Tar(TarFileSystem),
}

#[allow(dead_code)]
impl ModuleMountBackend {
    fn realpath(&self, path: &str) -> VfsResult<String> {
        match self {
            Self::Host(fs) => fs.realpath(path),
            Self::Tar(fs) => VirtualFileSystem::realpath(fs, path),
        }
    }

    fn read_file(&mut self, path: &str) -> VfsResult<Vec<u8>> {
        match self {
            Self::Host(fs) => fs.read_file(path),
            Self::Tar(fs) => VirtualFileSystem::read_file(fs, path),
        }
    }

    fn stat(&mut self, path: &str) -> VfsResult<VirtualStat> {
        match self {
            Self::Host(fs) => fs.stat(path),
            Self::Tar(fs) => VirtualFileSystem::stat(fs, path),
        }
    }

    fn exists(&self, path: &str) -> bool {
        match self {
            Self::Host(fs) => fs.exists(path),
            Self::Tar(fs) => VirtualFileSystem::exists(fs, path),
        }
    }
}

#[allow(dead_code)]
impl HostDirModuleMount {
    /// If `guest_path` falls under this mount, return the mount-relative virtual
    /// path (always absolute, e.g. `/foo/index.js`).
    fn relative_virtual_path(&self, guest_path: &str) -> Option<String> {
        if guest_path == self.guest_prefix {
            return Some(String::from("/"));
        }
        let prefix_with_sep = if self.guest_prefix == "/" {
            String::from("/")
        } else {
            format!("{}/", self.guest_prefix)
        };
        let rest = guest_path.strip_prefix(&prefix_with_sep)?;
        Some(format!("/{rest}"))
    }

    /// Re-express a mount-relative virtual path (e.g. `/foo/index.js`) as a guest
    /// path under this mount (e.g. `/root/node_modules/foo/index.js`).
    fn guest_path_for_relative(&self, relative: &str) -> String {
        let trimmed = relative.trim_start_matches('/');
        if self.guest_prefix == "/" {
            if trimmed.is_empty() {
                String::from("/")
            } else {
                format!("/{trimmed}")
            }
        } else if trimmed.is_empty() {
            self.guest_prefix.clone()
        } else {
            format!("{}/{trimmed}", self.guest_prefix)
        }
    }
}

/// A `Send`-able, clonable, read-only [`ModuleFsReader`] over one or more mounted
/// `host_dir`/`module_access` filesystems. It lets module resolution run on the
/// V8 bridge thread — concurrently with the sidecar service loop — while still
/// reading exactly the mounted `node_modules` tree the guest sees (anchored
/// resolve-beneath with escaping-symlink refusal; see [`confine`]), instead of
/// the host-direct path translator.
///
/// It never touches the `&mut` kernel, so a large cold-start module graph cannot
/// serialize behind / starve work on the service-loop thread (e.g. an ACP
/// `session/new` bootstrap awaiting the adapter's response on that same loop).
#[allow(dead_code)]
#[derive(Clone)]
pub(crate) struct HostDirModuleReader {
    /// Mounts sorted longest-`guest_prefix`-first so the most specific mount
    /// wins (mirrors the kernel mount table's longest-prefix dispatch).
    mounts: Vec<HostDirModuleMount>,
}

#[allow(dead_code)]
impl HostDirModuleReader {
    /// Build a reader from `(guest_path, host_path)` pairs for the VM's read-only
    /// `host_dir`/`module_access` mounts. Mounts whose host root cannot be opened
    /// are skipped. Returns `None` if no usable mount remains, so callers fall
    /// back to the service-loop kernel reader.
    pub(crate) fn from_mounts<I, G, H>(mounts: I) -> Option<Self>
    where
        I: IntoIterator<Item = (G, H)>,
        G: AsRef<str>,
        H: AsRef<Path>,
    {
        let mut entries = mounts
            .into_iter()
            .filter_map(|(guest_path, host_path)| {
                let filesystem = HostDirFilesystem::new_with_read_limit(
                    host_path.as_ref(),
                    Some(MAX_HOST_DIR_READ_BYTES),
                )
                .ok()?;
                Some(HostDirModuleMount {
                    guest_prefix: normalize_path(guest_path.as_ref()),
                    filesystem: ModuleMountBackend::Host(filesystem),
                })
            })
            .collect::<Vec<_>>();
        if entries.is_empty() {
            return None;
        }
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.guest_prefix.len()));
        entries.dedup_by(|left, right| left.guest_prefix == right.guest_prefix);
        Some(Self { mounts: entries })
    }

    /// Build a reader from host-dir pairs plus packed package-version tar
    /// mounts (`(guest_path, aospkg_path, tar_root)`), so module resolution
    /// reads packed `node_modules` content directly from the `.aospkg` mount
    /// index off the service loop. Unopenable mounts are skipped.
    pub(crate) fn from_mounts_and_package_tars<I, G, H>(
        mounts: I,
        package_tars: Vec<(String, String, String)>,
    ) -> Option<Self>
    where
        I: IntoIterator<Item = (G, H)>,
        G: AsRef<str>,
        H: AsRef<Path>,
    {
        let mut entries = mounts
            .into_iter()
            .filter_map(|(guest_path, host_path)| {
                let filesystem = HostDirFilesystem::new_with_read_limit(
                    host_path.as_ref(),
                    Some(MAX_HOST_DIR_READ_BYTES),
                )
                .ok()?;
                Some(HostDirModuleMount {
                    guest_prefix: normalize_path(guest_path.as_ref()),
                    filesystem: ModuleMountBackend::Host(filesystem),
                })
            })
            .collect::<Vec<_>>();
        entries.extend(
            package_tars
                .into_iter()
                .filter_map(|(guest_path, tar_path, root)| {
                    let filesystem = TarFileSystem::open_at(&tar_path, &root).ok()?;
                    Some(HostDirModuleMount {
                        guest_prefix: normalize_path(&guest_path),
                        filesystem: ModuleMountBackend::Tar(filesystem),
                    })
                }),
        );
        if entries.is_empty() {
            return None;
        }
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.guest_prefix.len()));
        entries.dedup_by(|left, right| left.guest_prefix == right.guest_prefix);
        Some(Self { mounts: entries })
    }

    /// Find the index of the most-specific mount containing `guest_path` and the
    /// corresponding mount-relative virtual path.
    fn mount_index_for(&self, guest_path: &str) -> Option<(usize, String)> {
        let normalized = normalize_path(guest_path);
        self.mounts.iter().enumerate().find_map(|(index, mount)| {
            mount
                .relative_virtual_path(&normalized)
                .map(|rel| (index, rel))
        })
    }
}

impl ModuleFsReader for HostDirModuleReader {
    fn canonical_guest_path(&mut self, guest_path: &str) -> Option<String> {
        let (index, relative) = self.mount_index_for(guest_path)?;
        let mount = &self.mounts[index];
        // `realpath` returns a mount-relative virtual path; re-express it as a
        // guest path so the resolver keeps operating in the guest namespace.
        let resolved = mount.filesystem.realpath(&relative).ok()?;
        Some(mount.guest_path_for_relative(&resolved))
    }

    fn read_to_string(&mut self, guest_path: &str) -> Option<String> {
        let (index, relative) = self.mount_index_for(guest_path)?;
        let bytes = self.mounts[index].filesystem.read_file(&relative).ok()?;
        String::from_utf8(bytes).ok()
    }

    fn path_is_dir(&mut self, guest_path: &str) -> Option<bool> {
        let (index, relative) = self.mount_index_for(guest_path)?;
        // `stat` follows symlinks (O_PATH, no O_NOFOLLOW), so a symlinked package
        // directory reports as a directory just like `fs.statSync` would.
        self.mounts[index]
            .filesystem
            .stat(&relative)
            .ok()
            .map(|stat| stat.is_directory)
    }

    fn path_exists(&mut self, guest_path: &str) -> bool {
        match self.mount_index_for(guest_path) {
            Some((index, relative)) => self.mounts[index].filesystem.exists(&relative),
            None => false,
        }
    }
}

/// Session-thread module reader: the mounted `HostDirModuleReader` plus a
/// persistent resolution cache, so the V8 isolate thread can both resolve
/// specifiers and read source DIRECTLY (same mount + resolve-beneath
/// confinement, same `ModuleResolver` semantics as the bridge), skipping the
/// per-module `_resolveModule`/`_loadFile` bridge round-trips.
pub(crate) struct SessionModuleReader {
    reader: HostDirModuleReader,
    cache: LocalModuleResolutionCache,
}

impl SessionModuleReader {
    pub(crate) fn new(reader: HostDirModuleReader) -> Self {
        Self {
            reader,
            cache: LocalModuleResolutionCache::default(),
        }
    }
}

impl GuestModuleReader for SessionModuleReader {
    fn read_module_source(&mut self, resolved_guest_path: &str) -> Option<String> {
        self.reader.read_to_string(resolved_guest_path)
    }

    fn resolve_module(&mut self, specifier: &str, referrer: &str) -> Option<String> {
        // Mirror the bridge's `_resolveModule` exactly: import mode, same reader,
        // same persisted cache.
        let reader: &mut dyn ModuleFsReader = &mut self.reader;
        let mut resolver = ModuleResolver::new(reader, &mut self.cache);
        resolver.resolve_module(specifier, referrer, ModuleResolveMode::Import)
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
mod tar_module_reader_tests {
    use super::*;
    use agentos_execution::{ModuleResolveMode, ModuleResolver};

    #[test]
    fn tar_reader_resolves_packed_node_modules() {
        let aospkg = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../software/pi/dist/package.aospkg");
        if !aospkg.is_file() {
            eprintln!("skip: pi aospkg not built");
            return;
        }
        let mut reader = HostDirModuleReader::from_mounts_and_package_tars(
            Vec::<(String, std::path::PathBuf)>::new(),
            vec![(
                String::from("/opt/agentos/pkgs/pi/0.2.1"),
                aospkg.to_string_lossy().into_owned(),
                String::from("/"),
            )],
        )
        .expect("reader");
        let probe = "/opt/agentos/pkgs/pi/0.2.1/node_modules/@anthropic-ai/sdk/package.json";
        assert!(reader.path_exists(probe), "packed package.json must exist");
        assert!(
            reader.read_to_string(probe).is_some(),
            "packed package.json must read"
        );
        let mut cache = Default::default();
        let dyn_reader: &mut dyn ModuleFsReader = &mut reader;
        let mut resolver = ModuleResolver::new(dyn_reader, &mut cache);
        let resolved = resolver.resolve_module(
            "@anthropic-ai/sdk",
            "/opt/agentos/pkgs/pi/0.2.1/node_modules/@agentos-software/pi/dist/adapter.js",
            ModuleResolveMode::Require,
        );
        assert!(
            resolved.is_some(),
            "require-mode resolution from the packed tar"
        );
        let resolved_import = resolver.resolve_module(
            "@anthropic-ai/sdk",
            "/opt/agentos/pkgs/pi/0.2.1/node_modules/@agentos-software/pi/dist/adapter.js",
            ModuleResolveMode::Import,
        );
        assert!(
            resolved_import.is_some(),
            "import-mode resolution from the packed tar"
        );
        assert!(
            resolved.is_some(),
            "must resolve @anthropic-ai/sdk from the packed tar"
        );
    }
}
