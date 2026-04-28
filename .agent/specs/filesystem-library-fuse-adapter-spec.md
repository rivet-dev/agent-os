# Filesystem Library and Experimental FUSE Adapter Spec

## Summary

Split the current filesystem substrate out of `crates/kernel` into a standalone Rust library centered on overlay composition and pluggable mounted filesystems.

That shared library must work in three embeddings:

1. as the primary root filesystem substrate for Agent OS VMs
2. as a mounted subtree inside Agent OS
3. as a standalone filesystem or subtree exported through an experimental FUSE adapter without Agent OS

The FUSE work is **not** an Agent OS feature flag. It is a separate consumer of the same filesystem library.

## Why

Right now the codebase mixes three different concerns:

1. generic filesystem semantics
2. Agent OS kernel-specific synthetic behavior
3. mount/plugin integration details

That makes reuse ugly and makes the current "filesystem" abstraction more root-shaped than it should be.

The actual primary reusable value is:

- in-memory filesystem semantics
- overlay composition
- mount routing
- snapshot/root assembly
- pluggable filesystem backends

The reusable library should own those pieces. Agent OS and FUSE should sit on top as consumers.

## Goals

1. Extract a standalone Rust filesystem library from `crates/kernel`.
2. Make overlay + mount/plugin composition the center of the design.
3. Support non-root embeddings through a first-class subtree/scoped view abstraction.
4. Keep Agent OS kernel-specific pseudo-filesystems and permission logic out of the shared library.
5. Build an experimental Linux-only FUSE adapter that mounts the shared filesystem library without depending on Agent OS.
6. Preserve current Agent OS behavior while improving crate boundaries.

## Non-Goals

1. Binding Agent OS itself to FUSE.
2. Making guest code talk to host FUSE directly.
3. Shipping cross-platform FUSE support in phase 1.
4. Solving durable storage or object-store backends in this spec beyond defining trait boundaries.
5. Redesigning the entire kernel or process model.
6. Preserving every current type name if better names produce a cleaner split.

## Current State

Today the core filesystem code lives in `crates/kernel`:

- `vfs.rs` defines `VirtualFileSystem`, `VfsError`, `VirtualStat`, and `MemoryFileSystem`
- `overlay_fs.rs` implements overlay behavior, but it is hard-wired to `MemoryFileSystem`
- `root_fs.rs` assembles the VM root from lower snapshots and bootstrap entries
- `mount_table.rs` implements path-based mount routing and a separate `MountedFileSystem` trait
- `mount_plugin.rs` implements mount plugin registry logic
- `device_layer.rs` and procfs behavior in `kernel.rs` inject kernel-specific pseudo-filesystems
- `permissions.rs` wraps the filesystem with VM-aware access control

Two design problems jump out:

1. The reusable substrate is mixed with kernel-only behavior.
2. The API shape is still implicitly root-first. `RootFileSystem` assumes `/`, and overlay metadata behavior assumes instance root semantics rather than explicit subtree views.

## Design Principles

### 1. Filesystem and VM root are different concepts

The shared library defines filesystem semantics. Agent OS decides how to use one as the VM root.

### 2. Overlay and mounts are the center

The shared library exists primarily to support overlay composition and pluggable mounted filesystems. Everything else is secondary.

### 3. Root-relative internally, subtree-capable externally

Each filesystem instance can still treat its own logical root as `/`. Reuse comes from wrapping that filesystem in a scoped view that re-roots a prefix as a new logical `/`.

### 4. One real filesystem trait

The library should not keep two near-duplicate traits for "filesystem" and "mounted filesystem" unless there is a hard capability difference. Phase 1 assumes there is not.

### 5. Kernel-specific pseudo-filesystems stay out

`/dev`, `/proc`, permission policy, VM identity, process-table-backed synthetic paths, and resource-accounting hooks belong in Agent OS integration, not in the shared filesystem library.

### 6. FUSE is just another adapter

The FUSE adapter should depend on the shared library the same way Agent OS does. If the design requires special Agent OS-only filesystem APIs to make FUSE work, the split failed.

## Target Package Layout

Recommended phase-1 workspace layout:

```text
crates/filesystem/
  src/
    lib.rs
    types.rs
    path.rs
    memory_fs.rs
    overlay.rs
    mount_table.rs
    mount_plugin.rs
    scoped.rs
    snapshot.rs
    root_builder.rs

crates/filesystem-fuse/
  src/
    lib.rs
    adapter.rs
    inode_table.rs
    main.rs (optional binary crate instead)

crates/kernel/
  src/
    kernel.rs
    device_layer.rs
    permissions.rs
    ...
```

Recommended crate names:

- `agent-os-filesystem`
- `agent-os-filesystem-fuse`

If you want cleaner generic naming later, do it after the split works.

## Layering

### Layer 1: Shared filesystem core

This layer belongs in `crates/filesystem`.

It owns:

- the main filesystem trait
- path normalization and validation helpers
- common error/stat/directory-entry types
- the reference in-memory filesystem
- overlay semantics
- mount-table routing
- plugin traits for mounted filesystems
- snapshot import/export
- subtree/scoped filesystem views
- root assembly helpers that are generic filesystem conveniences, not Agent OS policy

It does **not** own:

- `/dev`
- `/proc`
- VM-specific permissions
- process table integration
- file-descriptor-backed pseudo paths
- kernel resource accounting

### Layer 2: Agent OS integration

This layer stays in `crates/kernel`.

It owns:

- `KernelVm`
- synthetic `/dev`
- procfs behavior
- permission wrappers tied to `vm_id`
- process/FD/socket-coupled filesystem projections
- kernel-specific mount policy
- resource accounting and Linux-compatibility glue

This layer depends on the shared filesystem library.

### Layer 3: Experimental FUSE adapter

This layer lives in its own crate and depends only on the shared filesystem library plus a FUSE crate.

It owns:

- host mount/unmount lifecycle
- mapping FUSE operations onto the shared filesystem trait
- inode and lookup bookkeeping required by FUSE
- optional read-only and debug/export modes

It must not depend on `agent-os-kernel`.

## Core API Proposal

### One filesystem trait

Replace the current split between `VirtualFileSystem` and `MountedFileSystem` with one core trait in the shared library.

Proposed shape:

```rust
pub trait FileSystem: Send + 'static {
    fn read_file(&mut self, path: &str) -> FsResult<Vec<u8>>;
    fn read_dir(&mut self, path: &str) -> FsResult<Vec<String>>;
    fn read_dir_with_types(&mut self, path: &str) -> FsResult<Vec<DirEntry>>;
    fn write_file(&mut self, path: &str, content: Vec<u8>) -> FsResult<()>;
    fn create_file_exclusive(&mut self, path: &str, content: Vec<u8>) -> FsResult<()>;
    fn append_file(&mut self, path: &str, content: Vec<u8>) -> FsResult<u64>;
    fn create_dir(&mut self, path: &str) -> FsResult<()>;
    fn mkdir(&mut self, path: &str, recursive: bool) -> FsResult<()>;
    fn exists(&self, path: &str) -> bool;
    fn stat(&mut self, path: &str) -> FsResult<Stat>;
    fn lstat(&self, path: &str) -> FsResult<Stat>;
    fn remove_file(&mut self, path: &str) -> FsResult<()>;
    fn remove_dir(&mut self, path: &str) -> FsResult<()>;
    fn rename(&mut self, old_path: &str, new_path: &str) -> FsResult<()>;
    fn realpath(&self, path: &str) -> FsResult<String>;
    fn symlink(&mut self, target: &str, link_path: &str) -> FsResult<()>;
    fn read_link(&self, path: &str) -> FsResult<String>;
    fn link(&mut self, old_path: &str, new_path: &str) -> FsResult<()>;
    fn chmod(&mut self, path: &str, mode: u32) -> FsResult<()>;
    fn chown(&mut self, path: &str, uid: u32, gid: u32) -> FsResult<()>;
    fn utimes(&mut self, path: &str, atime_ms: u64, mtime_ms: u64) -> FsResult<()>;
    fn truncate(&mut self, path: &str, length: u64) -> FsResult<()>;
    fn pread(&mut self, path: &str, offset: u64, length: usize) -> FsResult<Vec<u8>>;
    fn shutdown(&mut self) -> FsResult<()> { Ok(()) }
}
```

Notes:

- The `&mut self` model is acceptable in phase 1 because it matches current code and keeps the migration sane.
- FUSE may later want an `Arc<Mutex<dyn FileSystem>>` or a lock-sharded wrapper. That is an adapter concern, not a reason to keep the current split-brain traits.

### Shared types

The shared library should define and own:

- `FsError`
- `FsResult<T>`
- `FileType`
- `DirEntry`
- `Stat`
- path normalization and validation helpers

The current `VfsError`, `VirtualDirEntry`, and `VirtualStat` move here with better names if desired.

## Scoped Filesystem View

### Problem

Current code is root-first. Overlay and root assembly assume they are describing `/`, and there is no first-class subtree view type.

That is the wrong abstraction if the same filesystem library must support:

- full root in Agent OS
- mounted subtree in Agent OS
- mounting `/workspace` or `/data` through FUSE

### Solution

Add a first-class wrapper that exposes a subtree of any filesystem as a new logical root.

Proposed shape:

```rust
pub struct ScopedFileSystem<F> {
    inner: F,
    prefix: String,
}

impl<F: FileSystem> ScopedFileSystem<F> {
    pub fn new(inner: F, prefix: impl Into<String>) -> FsResult<Self>;
}
```

Semantics:

- logical `/` in the scoped view maps to `prefix` in the backing filesystem
- logical `/a/b` maps to `{prefix}/a/b`
- `read_dir("/")` lists the children of the backing prefix
- `realpath("/")` returns `/` in the scoped view, not the backing prefix
- `rename`, `link`, `symlink`, and `realpath` must never escape the scoped root
- attempts to escape via `..` or symlink traversal fail closed

This wrapper is the key to making the library actually reusable instead of pretending.

### Why this should be a wrapper, not a special case

If subtrees are modeled by ad hoc path-prefix logic in the FUSE adapter or in Agent OS mount glue, the behavior will drift and get weird fast. The subtree view needs one implementation with conformance tests.

## Overlay Filesystem

### Responsibility

The shared library overlay implementation owns:

- one writable upper or read-only mode
- zero or more lower layers
- whiteouts
- opaque directories
- copy-up rules
- merged `readdir`
- cross-directory rename behavior within an overlay instance

### Required cleanup

The current overlay implementation is tied to `MemoryFileSystem`.

That must change.

Phase-1 proposal:

```rust
pub struct OverlayFileSystem {
    lowers: Vec<Box<dyn FileSystem>>,
    upper: Option<Box<dyn FileSystem>>,
    writes_locked: bool,
}
```

That is the blunt, workable version. If object-safety or clone constraints get annoying, use a concrete adapter enum or `Box<dyn FileSystemHandle>` type. Do not keep `MemoryFileSystem` hardcoded and call the result "pluggable."

### Internal metadata

The overlay implementation may continue to store whiteouts and opaque-directory metadata under a reserved hidden path inside the overlay instance, but that path is internal implementation detail.

Rules:

- it must stay hidden from user-visible directory listings
- it must not leak into snapshot exports
- it must not be treated as VM-root-specific behavior
- it must work correctly when the entire overlay is wrapped in `ScopedFileSystem`

The current `/.agent-os-overlay` convention is acceptable as an implementation detail inside one filesystem instance. It is not acceptable as a public API assumption.

## Mount Table

### Responsibility

The mount table belongs in the shared library.

It owns:

- longest-prefix path resolution
- mounted subtree dispatch
- parent directory entry merging
- cross-mount `EXDEV`
- read-only mount enforcement
- root mount bookkeeping

This is generic filesystem behavior. It should not live only in the kernel.

### API proposal

```rust
pub struct MountTable {
    // root mount is always present at "/"
}

impl MountTable {
    pub fn new(root: impl FileSystem) -> Self;
    pub fn mount(
        &mut self,
        path: &str,
        filesystem: impl FileSystem,
        options: MountOptions,
    ) -> FsResult<()>;
    pub fn unmount(&mut self, path: &str) -> FsResult<()>;
    pub fn mounts(&self) -> &[MountEntry];
}

pub struct MountOptions {
    pub read_only: bool,
    pub name: String,
}
```

### Subpath mounting

Subpath mounting is a first-class use case.

Examples:

- Agent OS mounts a workspace backend at `/workspace`
- FUSE mounts a scoped view of `/workspace` as a standalone host-visible root
- A standalone consumer mounts an overlay at `/data`

The mount table already wants this model. The extracted crate should make it explicit.

## Mount Plugins

### Responsibility

The shared library should define plugin/factory traits for mounted filesystems. Agent OS may own one registry instance, but the traits themselves are reusable.

Proposed shape:

```rust
pub trait FileSystemPluginFactory<C>: Send + Sync {
    fn plugin_id(&self) -> &'static str;
    fn open(&self, request: OpenFileSystemPluginRequest<'_, C>) -> Result<Box<dyn FileSystem>, PluginError>;
}

pub struct OpenFileSystemPluginRequest<'a, C> {
    pub mount_path: &'a str,
    pub config: &'a C,
    pub read_only: bool,
}
```

### Plugin categories

There are two useful plugin categories:

1. mounted filesystem plugins
2. storage plugins used by future durable filesystem implementations

This spec only requires a clean trait boundary for mounted filesystem plugins. It does not require solving storage plugins yet.

## Snapshot and Root Assembly

### Responsibility

The shared library should keep snapshot import/export and overlay-root assembly helpers, but those helpers should be generic filesystem conveniences rather than Agent OS policy objects.

The current `RootFileSystem` name implies "the one true VM root." That is too grand and too Agent OS-shaped.

Recommended direction:

- move snapshot entry types into the shared library
- keep bundled-base import logic out of the shared library
- rename `RootFileSystem` to something like `OverlayRootBuilder`, `SnapshotRootFileSystem`, or keep the current name internally but document it as a convenience wrapper

### What stays out

These do not belong in the shared library:

- bundled import of `packages/core/fixtures/base-filesystem.json`
- Agent OS default root-directory policy
- suppressing bootstrap entries because `/proc` or `/dev` are kernel-owned in Agent OS

Those are Agent OS integration decisions.

### Shared-library assembly model

The shared library should provide generic helpers:

```rust
pub struct FilesystemSnapshot {
    pub entries: Vec<FilesystemEntry>,
}

pub struct OverlayRootDescriptor {
    pub mode: OverlayMode,
    pub lowers: Vec<FilesystemSnapshot>,
    pub bootstrap_entries: Vec<FilesystemEntry>,
}
```

Agent OS can then provide its own wrapper that appends the bundled base layer and filters kernel-reserved paths.

## Agent OS Integration Plan

### What moves out of `crates/kernel`

Move these to `crates/filesystem`:

- `vfs.rs` core types and in-memory filesystem
- `overlay_fs.rs`
- `mount_table.rs`
- `mount_plugin.rs`
- snapshot/root-assembly pieces from `root_fs.rs`

### What stays in `crates/kernel`

- `kernel.rs`
- `device_layer.rs`
- procfs logic in `kernel.rs`
- `permissions.rs`
- resource-accounting integration
- process-table / FD-table / socket-table integration

### Integration shape after the split

Conceptually:

```text
KernelVm
  -> PermissionedFileSystem<
       DeviceLayer<
         MountTable<
           shared filesystem root
         >
       >
     >
  + procfs dispatch in kernel-owned paths
```

That is still an Agent OS concern. The shared filesystem library remains oblivious to VM-specific synthetic paths.

### Agent OS root setup

Agent OS should own:

- choosing the default base filesystem snapshot
- appending Agent OS bootstrap entries
- filtering or overriding `/dev`, `/proc`, `/sys`
- wrapping the resulting filesystem in permissions/device/proc behavior

The shared library should only provide the mechanics needed to build that root.

## Experimental FUSE Adapter

### Scope

Phase 1 is Linux-only and experimental.

The adapter may be either:

- a library crate with a small CLI binary
- or a single binary crate

Recommended CLI shape:

```text
agent-os-filesystem-fuse mount \
  --mountpoint /tmp/fs \
  --source memory-demo

agent-os-filesystem-fuse mount \
  --mountpoint /tmp/workspace \
  --scoped-prefix /workspace \
  --source overlay-demo
```

### Adapter requirements

The FUSE adapter must support:

- mounting the full filesystem view
- mounting a scoped subtree view
- read-only mode
- clean unmount/shutdown
- correct file, directory, and symlink semantics for the supported operations

### Recommended crate choice

Use a maintained Rust FUSE crate with low-level request handling if needed. The exact crate choice is implementation detail; the spec only requires Linux compatibility and a clean separation from Agent OS.

### FUSE inode model

FUSE cares about inode stability, lookups, and forgets in ways the current shared filesystem API does not expose directly.

The adapter should own a translation layer:

- map filesystem paths to FUSE inode ids
- cache lookup counts
- refresh stats on demand
- keep path/inode mappings coherent across rename/unlink as well as practical for an experimental adapter

This bookkeeping belongs in the FUSE adapter, not in the shared filesystem library.

### Supported operation set

Phase 1 should support:

- lookup
- getattr
- readlink
- opendir
- readdir
- open
- read
- mkdir
- create
- write
- unlink
- rmdir
- rename
- symlink
- setattr subset needed for chmod/truncate/utimens

Skip xattrs, file locking, and advanced FUSE features in phase 1 unless they come nearly free.

### Concurrency model

The shared filesystem library can remain `&mut self` internally in phase 1. The FUSE adapter should wrap the filesystem instance in a synchronization primitive and serialize mutations as needed.

That is not glamorous, but it is sufficient for an experimental adapter and avoids forcing premature rewrites of the core library.

## Compatibility and Migration

### Phase 1: Mechanical extraction

1. Create `crates/filesystem`.
2. Move shared types and `MemoryFileSystem` there.
3. Move `OverlayFileSystem`, `MountTable`, and mount plugin traits there.
4. Adjust `crates/kernel` imports.
5. Keep behavior identical where possible.

### Phase 2: API cleanup

1. Collapse `VirtualFileSystem` and `MountedFileSystem` into one trait.
2. Rename shared types if needed.
3. Introduce `ScopedFileSystem`.
4. Make overlay generic over filesystem backends instead of `MemoryFileSystem`.

### Phase 3: Agent OS integration cleanup

1. Replace old imports throughout `crates/kernel` and sidecar code.
2. Move Agent OS-only root assembly policy out of the shared crate.
3. Keep `/dev` and procfs kernel-owned.

### Phase 4: Experimental FUSE adapter

1. Create `crates/filesystem-fuse`.
2. Add a demo source and mount CLI.
3. Implement scoped subtree mounts.
4. Validate behavior against the shared library conformance tests.

## Testing Strategy

### Shared library tests

Move generic filesystem tests out of `crates/kernel/tests` into `crates/filesystem/tests`:

- in-memory filesystem conformance
- overlay whiteout/opaque-dir behavior
- overlay rename/copy-up behavior
- mount-table dispatch
- cross-mount `EXDEV`
- scoped subtree behavior
- snapshot round-trips

### New mandatory scoped-view tests

Add direct tests for:

- `ScopedFileSystem("/")` behaves the same as the underlying filesystem
- scoped root `readdir("/")` exposes only the subtree
- `realpath("/")` returns `/`
- rename/link/symlink cannot escape the scoped root
- overlay hidden metadata remains hidden when viewed through a scoped subtree

These tests are non-negotiable. Without them the whole "not root-only" claim is fluff.

### Agent OS tests

Keep kernel-owned tests in `crates/kernel/tests`:

- `/dev` behavior
- procfs behavior
- permission checks
- resource-accounting interactions
- root bootstrap filtering for kernel-reserved paths

### FUSE tests

Add Linux-only integration tests for the experimental adapter:

- mount full root and verify basic CRUD
- mount scoped subtree and verify only that subtree is visible
- verify symlink and rename behavior
- verify read-only mode

Mark the slow FUSE tests `#[ignore]` if needed so normal CI does not turn into a hostage situation.

## Documentation Requirements

When this lands:

1. Document the shared filesystem library as a standalone Rust crate.
2. Document Agent OS as one consumer of that library.
3. Document the FUSE adapter as experimental and host-side.
4. Do not describe FUSE as part of guest execution.

## Risks

### 1. Fake genericity

If overlay stays hard-coded to `MemoryFileSystem`, the split is cosmetic.

### 2. Trait confusion

If both `VirtualFileSystem` and `MountedFileSystem` survive with tiny differences, the API will stay muddy and every adapter will need glue for no good reason.

### 3. Root leakage

If subtree support is implemented only in the FUSE adapter or only in Agent OS mount glue, behavior will diverge and path semantics will rot.

### 4. Agent OS contamination

If `/dev`, `/proc`, permissions, or VM-specific bootstrap policy move into the shared crate, the library stops being reusable.

### 5. FUSE pressure on the core API

FUSE has real inode and lookup semantics. The adapter should absorb that complexity first. Do not contort the shared library around low-level FUSE details unless repeated pain proves it is necessary.

## Recommended Decisions

These should be treated as settled for this spec:

1. The primary shared abstraction is a generic filesystem library, not an Agent OS root implementation.
2. Overlay + pluggable mounted filesystems are the main use case.
3. `ScopedFileSystem` is required in phase 2, not optional future polish.
4. `/dev`, `/proc`, and VM permissions stay in Agent OS.
5. The experimental FUSE adapter is Linux-only in phase 1.
6. The FUSE adapter depends on the shared filesystem library, never on `agent-os-kernel`.

## End State

After this work:

- `crates/filesystem` is the reusable filesystem substrate
- Agent OS uses it as the base for VM filesystem composition
- FUSE can mount either the full filesystem or a scoped subtree without Agent OS
- kernel-only behavior remains in the kernel

That is the split we actually want. Anything mushier just rebrands the current knot.
