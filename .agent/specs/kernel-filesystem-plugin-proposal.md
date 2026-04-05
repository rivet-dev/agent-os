# Kernel-Native Filesystem Driver Proposal

## Summary

Move filesystem driver execution out of the TypeScript runtime layer and into the native kernel/sidecar path, with a plugin registry for non-core drivers.

The main goal is to remove the hot-path JS boundary for filesystem operations. Today the expensive cases are mount-backed paths, where guest code ends up paying for:

1. guest runtime -> kernel bridge
2. kernel -> JS `VirtualFileSystem`
3. JS driver -> host API / remote API

The target state is:

1. guest runtime -> native kernel VFS
2. native kernel mount table -> native filesystem plugin
3. plugin talks directly to local OS / remote service

That keeps the syscall path entirely native for built-in and plugin-backed filesystems.

## Current State

### What is already native

- `crates/kernel/src/vfs.rs` owns the core in-memory VFS semantics.
- `crates/kernel/src/kernel.rs` owns the kernel VM, permissions, FDs, PTYs, and process table.
- `crates/sidecar/src/service.rs` already hosts the native kernel and exposes it through the sidecar scaffold.

### What is still JS-bound

- `packages/core/src/agent-os.ts` resolves `mounts` into JS `VirtualFileSystem` instances.
- The live published kernel surface is still the TypeScript package in `packages/kernel-legacy-staging`.
- The sandbox mount in `registry/tool/sandbox/src/filesystem.ts` is a JS wrapper over the `sandbox-agent` SDK.
- The sidecar scaffold still uses `HostFilesystem<B>` as a bridge-backed root filesystem; it does not have a native mount/plugin dispatcher yet.

### Why this matters

- For host-dir, sandbox, and any future remote/persistent mount, every read/write/stat crosses back into JS.
- `pread` on the sandbox backend currently downloads the whole file, because the SDK only exposes full-file reads.
- Mount handling is not yet represented in the Rust kernel. The sidecar protocol has `MountDescriptor`, but the scaffold currently only stores that config; it does not apply it.

## Recommendation

Use a **statically registered native plugin system**, not dynamic shared-library loading.

That means:

- plugin crates live in the monorepo and are compiled into the sidecar/native runtime
- plugin instances are selected by string id plus structured config
- TypeScript packages become thin config/descriptor wrappers instead of implementation hosts

I would explicitly avoid `.so`/`.dylib`/`.dll` loading for the first version. It creates packaging, versioning, security, and npm distribution problems that are not worth it here.

## Required End State

This proposal is not a dual-stack design. The required end state is:

- the filesystem hot path runs in the native kernel/plugin layer
- the current JavaScript filesystem driver implementations are deleted
- docs in the external docs repo the user referred to as `~/r8` are updated to describe the native plugin architecture and to remove stale references to JS-backed filesystem drivers

On this machine `~/r8` does not exist, but there is a local checkout at `/home/nathan/rivet-8`. I am treating that as the likely local equivalent for proposal purposes.

## Target Architecture

### 1. Native mount table in the kernel

Add a mount-dispatch layer in Rust:

- root filesystem remains a native `VirtualFileSystem`
- mounted filesystems are stored in a `MountTable`
- path resolution does longest-prefix mount matching
- cross-mount rename/link returns `EXDEV`
- mount points appear in parent directory listings
- read-only enforcement happens in the native mount layer, not in JS wrappers

Recommended structure:

```text
crates/kernel/src/
  mount_table.rs
  mount_plugin.rs
  overlay_fs.rs
  root_fs.rs
```

The kernel should own one composed filesystem view:

```text
KernelVm
  -> RootFs
       - native root overlay
       - mounted sub-filesystems
       - synthetic mountpoint directory projection
```

### 2. Native plugin registry

Add a registry that maps plugin ids to factories:

```rust
pub trait FileSystemPluginFactory: Send + Sync {
    fn plugin_id(&self) -> &'static str;
    fn open(&self, request: OpenFileSystemPluginRequest) -> Result<Box<dyn MountedFileSystem>, PluginError>;
}

pub trait MountedFileSystem: VirtualFileSystem + Send {
    fn capabilities(&self) -> FileSystemCapabilities;
    fn shutdown(&mut self) -> Result<(), PluginError> { Ok(()) }
}
```

Recommended runtime model:

- the sidecar owns the plugin registry
- `ConfigureVmRequest` carries declarative mount specs
- the sidecar instantiates plugin mounts during VM configuration
- the kernel sees only native `MountedFileSystem` trait objects

### 3. Two plugin families

Do not treat every filesystem concern as the same thing.

There are two distinct families:

#### A. Mount plugins

These back a mounted subtree directly:

- `memory`
- `host_dir`
- `sandbox_agent`
- `overlay`

These should implement `MountedFileSystem` directly.

#### B. Storage plugins

These back persistent chunk/block storage for the main VFS:

- `sqlite_metadata`
- `s3_block_store`
- `google_drive_block_store`

These should not be mounted directly. They should be consumed by a native `ChunkedVfs` implementation.

That gives a clean split:

- mount plugins serve mounted paths
- storage plugins serve durable VFS internals

## Public API Shape

The core public API should shift from “pass a live JS filesystem object” to “pass a declarative native mount spec”.

### New preferred API

```ts
const vm = await AgentOs.create({
  mounts: [
    {
      path: "/workspace",
      plugin: {
        id: "host_dir",
        config: {
          hostPath: "/tmp/project",
          readOnly: false,
        },
      },
    },
    {
      path: "/sandbox",
      plugin: {
        id: "sandbox_agent",
        config: {
          baseUrl: sandbox.baseUrl,
          token: sandbox.token,
          basePath: "/",
          readOnly: false,
        },
      },
    },
  ],
});
```

### Compatibility path

Keep the current JS object form temporarily:

```ts
{
  path: "/data",
  driver: someVirtualFileSystem
}
```

But treat it as a slow fallback:

- map it to a `js_bridge` mount plugin internally
- mark it deprecated
- keep it only for arbitrary caller-provided custom filesystems and browser-only cases
- do not let any first-party filesystem package continue to use this path after its native replacement lands

That gives a migration path without blocking advanced users.

## Kernel and Sidecar Changes

### Kernel changes

1. Add `MountTable` and native mount prefix dispatch.
2. Move read-only mount enforcement into Rust.
3. Move root overlay behavior into Rust.
4. Make directory listings merge native root entries and mount point names.
5. Return `EXDEV` natively for cross-mount rename/link.

### Sidecar changes

1. Extend `MountDescriptor` so it can actually instantiate plugins.
2. Add serde-backed config payloads per plugin.
3. During `ConfigureVm`, resolve each mount spec through the plugin registry and attach it to the VM kernel.
4. Keep `HostFilesystem<B>` only for:
   - root bootstrap if needed during migration
   - legacy `js_bridge` plugin
   - browser placement where native plugins are unavailable

Recommended protocol evolution:

```rust
pub struct MountDescriptor {
    pub guest_path: String,
    pub plugin_id: String,
    pub read_only: bool,
    pub config_json: serde_json::Value,
}
```

## Built-In vs Plugin Ownership

### Make these built-in

These are hot-path, foundational, or too core to externalize:

- root overlay
- in-memory filesystem
- read-only wrapper
- synthetic bootstrap/root filesystem
- mount table / dispatch layer

### Make these plugins

These are host- or service-specific:

- host directory projection
- sandbox agent mount
- S3 block store
- Google Drive block store

These should be native Rust plugin crates only. We should not keep parallel JavaScript driver implementations after the migration is complete.

## Explicit Cleanup Requirements

The proposal requires deleting the old JavaScript filesystem driver implementations after parity is reached. Do not leave them in the repo as dormant alternatives.

### Delete these JavaScript implementation surfaces

- `packages/core/src/backends/host-dir-backend.ts`
- `packages/core/src/backends/overlay-backend.ts`
- `registry/tool/sandbox/src/filesystem.ts`
- the TypeScript implementation packages under:
  - `registry/file-system/s3`
  - `registry/file-system/google-drive`

### Replace with these native surfaces

- native host-dir plugin crate
- native overlay/root mount implementation in `crates/kernel`
- native sandbox-agent filesystem plugin crate
- native S3 block-store plugin crate
- native Google Drive block-store plugin crate

### Compatibility policy

- Keep temporary JS compatibility wrappers only if they serialize declarative plugin configs and contain no live filesystem logic.
- If a wrapper still performs filesystem operations itself, it has not met the target state and must be deleted.
- Delete JS-driver-specific tests once the equivalent native plugin coverage exists, or rewrite those tests to target the native plugin path.
- The only allowed long-term JS fallback is the generic `js_bridge` path for user-supplied custom filesystems. It is not an allowed long-term implementation strategy for first-party packages.

## Sandbox Mount Proposal

This is the hardest part and needs its own native client crate.

### Why sandbox is hard

The current sandbox mount is not just “a filesystem driver”. It depends on the TypeScript `sandbox-agent` SDK, which currently provides:

- auth and base URL handling
- filesystem helper methods
- HTTP request construction
- error translation

If the kernel plugin needs to be native, that SDK layer has to be rebuilt in Rust.

### Recommended implementation

Add:

```text
crates/sandbox-agent-client/
crates/fs-plugin-sandbox-agent/
```

#### `crates/sandbox-agent-client`

This should be a minimal client, not a full port of the entire TS SDK.

It only needs the subset required for filesystem mounting:

- `list_fs_entries`
- `read_fs_file`
- `write_fs_file`
- `delete_fs_entry`
- `mkdir_fs`
- `move_fs`
- `stat_fs`
- optionally `upload_fs_batch`

It should support:

- `base_url`
- optional bearer token
- optional extra headers
- request timeout
- structured error parsing

It should not try to port ACP session management in the first phase.

#### `crates/fs-plugin-sandbox-agent`

This crate adapts the client to the kernel plugin interface and preserves current filesystem semantics:

- no symlink support
- no hard-link support
- no chmod/chown/utimes
- `truncate` support
- `pread` support

### Important gap: current sandbox API is not enough for a good native port

The current server API exposes full-file reads via `GET /v1/fs/file`.

That means a naive native plugin would still have to:

- download the entire file for `pread`
- download the entire file for partial reads from shells/tools

That keeps the worst sandbox performance problem intact.

I would not ship the sandbox plugin without adding one of these server capabilities:

1. `GET /v1/fs/file-range?path=...&offset=...&length=...`
2. `Range` header support on `GET /v1/fs/file`
3. ACP extension method for ranged file reads

My recommendation is option 2 or 3. Either is acceptable. Option 2 is simpler for a small native client.

### Sandbox plugin caching

Even after removing JS, remote sandbox mounts will still be network-bound. The native plugin should include small, explicit caches:

- metadata cache with short TTL
- directory listing cache with invalidation on write/mkdir/delete/move
- read cache for small files
- optional read-ahead for ranged reads

The cache should be correctness-first:

- write-through
- invalidate parent directories on mutation
- invalidate both source and destination parents on rename/move

## Host Directory Plugin Proposal

Port `createHostDirBackend` into a native plugin.

That plugin should preserve the current guarantees:

- canonicalize the host root at open time
- reject path traversal
- reject symlink escapes
- enforce read-only at the plugin boundary

This is a good first plugin because:

- it is local, not remote
- it exercises path-resolution and mount dispatch
- it replaces a hot JS boundary with a straightforward native path

## Persistent Storage Plugin Proposal

After mount plugins are working, move durable storage drivers under a native `ChunkedVfs` architecture.

Recommended sequence:

1. native `FsMetadataStore`
2. native `FsBlockStore`
3. native `ChunkedVfs`
4. port `sqlite_metadata`
5. port `s3_block_store`
6. port `google_drive_block_store`

I would not block the mount-plugin work on this. It is related, but not required for the first performance win.

## Migration Plan

### Phase 1: native mount table

- add Rust mount dispatch
- add native read-only wrapper
- move root overlay into Rust
- add mount table tests for precedence, readdir merge, and `EXDEV`

### Phase 2: host-dir plugin

- implement `host_dir` native plugin
- add declarative mount config in `packages/core`
- keep JS `driver` mounts as fallback during migration only
- delete `packages/core/src/backends/host-dir-backend.ts` once the native host-dir plugin is wired through the public API

### Phase 3: sandbox-agent client + plugin

- add minimal Rust sandbox-agent filesystem client
- add `sandbox_agent` mount plugin
- add ranged-read support to sandbox-agent server if missing
- port the current sandbox mount tests to the native path
- delete `registry/tool/sandbox/src/filesystem.ts` once the native sandbox plugin reaches parity

### Phase 4: compatibility and deprecation

- route existing `createSandboxFs` and host-dir helper APIs to declarative plugin configs where possible
- keep `js_bridge` only for arbitrary custom `VirtualFileSystem`
- add warnings/docs that JS-backed mounts are slower and legacy
- delete any remaining JS filesystem driver code that still executes filesystem operations
- delete or reduce old filesystem driver packages so they are no longer implementation packages
- remove any stale exports that expose JS filesystem backends as first-class APIs
- reject any migration as incomplete if a first-party filesystem package still depends on the JS fallback path

### Phase 5: durable storage plugins

- native metadata/block-store interfaces
- native `ChunkedVfs`
- S3 / Google Drive plugin ports
- delete `registry/file-system/s3` and `registry/file-system/google-drive` as TypeScript implementation packages once the native block-store plugins are in place

### Phase 6: docs cleanup in `~/r8`

- update the external docs repo at `~/r8`
- if `~/r8` is not present locally, use `/home/nathan/rivet-8` as the local checkout to edit
- remove references that describe filesystem drivers as JavaScript/runtime-level packages
- document that filesystem drivers now run in the native kernel/plugin layer
- update any sandbox/filesystem/mounting pages to describe the new declarative plugin mount model
- call out any behavior changes for custom mounts, browser placement, and compatibility wrappers
- treat the docs update as a migration completion requirement, not optional cleanup
- at minimum update:
  - `/home/nathan/rivet-8/website/src/content/docs/actors/sandbox.mdx`
  - `/home/nathan/rivet-8/docs/docs/actors/sandbox.mdx`
  - `/home/nathan/rivet-8/website/src/content/posts/2026-01-28-sandbox-agent-sdk/page.mdx`

## Testing Plan

### Kernel tests

- mount precedence
- root + mount separation
- `readdir("/")` includes mount points
- `EXDEV` for cross-mount rename/link
- read-only enforcement
- unmount behavior

### Sandbox plugin tests

Reuse the current conformance surface from `registry/tool/sandbox/tests`:

- filesystem driver conformance
- VM integration with `/sandbox`
- create/read/write/delete/mkdir/move/stat
- `truncate`
- `pread`

Add new tests for:

- auth failure
- timeout handling
- directory cache invalidation
- ranged reads

### Migration safety tests

For a transition period, run the same test matrix against:

- `js_bridge` sandbox mount
- native sandbox plugin

That makes it easy to prove parity before removing the JS path.

### Cleanup verification tests

Add explicit checks that fail if the deleted JS implementation surfaces still exist or are still exported:

- no public export path should expose `createHostDirBackend` as a live implementation
- no public export path should expose the sandbox JS VFS implementation as the preferred path
- no package build should depend on `registry/file-system/s3` or `registry/file-system/google-drive` as live runtime implementations after the native migration lands

## Risks

### 1. Sync kernel trait vs remote filesystem latency

The Rust `VirtualFileSystem` trait is synchronous today. Remote filesystem plugins will block the caller thread.

That is acceptable for a first pass, but it means:

- sandbox/S3/Drive plugins should use tight timeouts
- plugin work may need dedicated worker threads later
- a future async VFS may still be worth doing

I would not make async VFS a prerequisite for this migration.

### 2. Browser placement

Native plugins are a native-sidecar story. Browser placement cannot load host-dir or sandbox native plugins.

Recommended behavior:

- native placements use native plugins
- browser placements keep the JS fallback path
- API surface stays the same, but capability availability depends on placement

### 3. Packaging

If every npm package tries to ship its own compiled Rust binary independently, packaging will get messy.

Recommended packaging model:

- one sidecar binary
- plugin crates compiled into it behind Cargo features
- TypeScript packages only supply config descriptors and docs

## Recommendation on Scope

If the goal is the fastest path to real performance wins, I would scope the first implementation to:

1. native mount table in Rust
2. native host-dir plugin
3. native sandbox-agent plugin
4. JS fallback only for arbitrary user-supplied custom filesystems, never for first-party driver packages

That gets the important win without blocking on a full durable-storage rewrite.

## Concrete Deliverables

### Rust crates

```text
crates/kernel
  src/mount_table.rs
  src/mount_plugin.rs
  src/overlay_fs.rs

crates/sandbox-agent-client
crates/fs-plugin-host-dir
crates/fs-plugin-sandbox-agent
```

### TypeScript changes

```text
packages/core
  - new declarative native mount config types
  - mount serialization into sidecar protocol
  - legacy js_bridge fallback
```

### Deletions

```text
delete packages/core/src/backends/host-dir-backend.ts
delete packages/core/src/backends/overlay-backend.ts
delete registry/tool/sandbox/src/filesystem.ts
delete registry/file-system/s3 as a TypeScript implementation package
delete registry/file-system/google-drive as a TypeScript implementation package
```

### Docs changes

```text
~/r8
  - update filesystem / sandbox / mount docs to describe native kernel plugins
  - remove stale references to JS filesystem driver packages
  - document the compatibility status of js_bridge fallback mounts
  - ship these docs changes as part of the migration, not as a later follow-up
  - at minimum update `website/src/content/docs/actors/sandbox.mdx`, `docs/docs/actors/sandbox.mdx`, and `website/src/content/posts/2026-01-28-sandbox-agent-sdk/page.mdx` in the local `/home/nathan/rivet-8` checkout if `~/r8` is missing
```

### Protocol changes

- `MountDescriptor` must carry plugin id + structured config
- `ConfigureVm` must instantiate mounts, not just store metadata

## Final Call

The right design is:

- **native mount table in the kernel**
- **statically registered native filesystem plugins**
- **declarative mount configs from TypeScript**
- **JS bridge kept only as a fallback for arbitrary user-supplied custom filesystems**
- **first-party JavaScript filesystem driver packages deleted or reduced to non-runtime config wrappers**
- **docs in `~/r8` updated in the same migration workstream**

The sandbox mount should be treated as a first-class native plugin backed by a small Rust `sandbox-agent` filesystem client. That is the only way to get the performance win you want without keeping the filesystem hot path dependent on JS.
