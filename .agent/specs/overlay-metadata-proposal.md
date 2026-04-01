# Durable Overlay Filesystem in Core Metadata

## Status

Reviewed after adversarial pass. This proposal assumes breaking changes are acceptable.

## Summary

We should replace the current wrapper-based overlay in `packages/core/src/backends/overlay-backend.ts` with a metadata-native overlay implementation in core.

Today the overlay implementation is:

- one lower `VirtualFileSystem`
- one upper `VirtualFileSystem`
- an in-memory `Set<string>` of whiteouts

That is not durable. Deletes of lower-layer files exist only in process memory, not in persistent metadata. The replacement should store all overlay semantics in the metadata layer itself:

- immutable, materialized lower layers
- one writable upper layer per view
- durable whiteouts
- durable opaque directories
- durable copy-up provenance
- crash-safe interaction with block storage

In our runtime model, whiteouts and opaque markers are live only in the active upper layer. Frozen lower snapshots are materialized trees and must not contain live whiteout or opaque markers.

The goal is to match Linux OverlayFS behavior as closely as our API allows, not to build a generic copy-on-write filesystem that only resembles it.

## Sources

Primary references that define the target semantics:

- Linux OverlayFS docs: https://docs.kernel.org/filesystems/overlayfs.html
- Linux kernel doc mirror: https://www.kernel.org/doc/html/latest/filesystems/overlayfs.html
- Docker `overlay2` docs: https://docs.docker.com/engine/storage/drivers/overlayfs-driver/
- OCI layer format: https://raw.githubusercontent.com/opencontainers/image-spec/main/layer.md

Relevant subsections:

- Upper/lower/workdir: https://www.kernel.org/doc/html/latest/filesystems/overlayfs.html#upper-and-lower
- Whiteouts and opaque directories: https://www.kernel.org/doc/html/latest/filesystems/overlayfs.html#whiteouts-and-opaque-directories
- Non-directories and copy-up: https://www.kernel.org/doc/html/latest/filesystems/overlayfs.html#non-directories
- Renaming directories / `EXDEV`: https://www.kernel.org/doc/html/latest/filesystems/overlayfs.html#renaming-directories
- Inode properties / `xino`: https://www.kernel.org/doc/html/latest/filesystems/overlayfs.html#inode-properties
- `xino`: https://www.kernel.org/doc/html/latest/filesystems/overlayfs.html#xino
- `metacopy`: https://www.kernel.org/doc/html/latest/filesystems/overlayfs.html#metadata-only-copy-up

## What OverlayFS Does

Linux OverlayFS presents:

- one writable upper layer
- one or more read-only lower layers
- one merged view

The behaviors we should treat as normative:

1. Upper beats lower for any non-directory name collision.
2. Directory names merge unless the upper directory is opaque.
3. Deleting a lower entry records a whiteout in upper.
4. Replacing lower directory contents while keeping the directory uses an opaque upper directory.
5. Writing or mutating metadata on a lower non-directory triggers copy-up first.
6. By default, renaming a lower or merged directory returns `EXDEV`.
7. OverlayFS can optionally preserve more hardlink behavior with `index=on`; without that feature, lower hardlinks may diverge after copy-up.
8. OCI layers serialize deletes as `.wh.<name>` and opaque directories as `.wh..wh..opq`.

Important caller-visible details from the kernel docs:

- Only name lists merge. Directory metadata comes from upper if upper exists.
- Non-directory `st_dev` / `st_ino` may come from the visible source object and may change after copy-up unless `xino` is used.
- `readdir(3)` has cursor caching behavior that depends on open directory handles.

## Explicit Compatibility Decisions

These choices make phase 1 concrete and keep it aligned with real OverlayFS behavior.

1. Phase 1 should match OverlayFS with `index=off` for lower hardlinks.
2. Phase 1 should add `dev` to `VirtualStat`. Without `st_dev`, we cannot represent OverlayFS-like object identity accurately.
3. Phase 1 does not need a kernel-style `workdir`.
4. Phase 1 does not need to expose full POSIX `seekdir`/offset behavior at the public API boundary, but the metadata layer should still use view-bound directory handles so merged `readdir` snapshots are explicit and stable per open handle.
5. Frozen lower layers in our runtime are materialized trees, not live OCI diff layers. OCI whiteout and opaque markers are consumed during import and are not re-interpreted from lower layers at lookup time.

Decision 3 needs explanation:

- Linux OverlayFS needs `workdir` because it implements copy-up and rename over a real upper filesystem.
- Our equivalent should be SQLite transactions for metadata plus staged block writes and deferred block GC for data.
- That gives us the same correctness property: no visible partial mutation after crash.

## Why the Current Model Is Wrong

The current model is structurally incorrect for durable overlay semantics:

1. Lower-layer deletes are transient.
2. Overlay state is split across lower fs, upper fs, and wrapper-local memory.
3. There is no durable representation of whiteouts or opaque directories.
4. Copy-up provenance is implicit.
5. There is no clean path to persistent overlays, snapshots, or OCI diff import/export.

## Breaking Changes We Should Make

### Replace `FsMetadataStore` With a Layer-Aware API

The current metadata model assumes one canonical tree:

- one inode namespace
- one dentry table
- one symlink table
- one chunk map

That is the wrong abstraction for overlay semantics. The new API should be explicitly view-aware.

Minimum shape:

```ts
interface OverlayView {
  viewId: number;
  upperLayerId: number;
  lowerLayerIds: number[]; // highest-precedence lower first
}

interface NodeRef {
  viewId: number;
  layerId: number;
  ino: number;
}
```

Recommended direction:

- add a new `LayeredMetadataStore` interface
- replace plain inode arguments/returns with `NodeRef`
- keep `transaction<T>(fn)` as a first-class API contract for multi-table overlay mutations
- update `ChunkedVFS` to operate in the context of a view-bound metadata session, not a raw global store
- make metadata operations explicitly view-bound or layer-bound so per-layer inode numbers are never ambiguous at call sites
- make merged-directory iteration a first-class operation so `readdir` caching semantics are explicit instead of accidental
- keep an adapter only if we need temporary compatibility with old single-layer callers

### Extend `VirtualStat`

Current `VirtualStat` only exposes `ino`. OverlayFS semantics depend on the `st_dev` / `st_ino` pair.

Required breaking change:

```ts
interface VirtualStat {
  dev: number;
  ino: number;
  mode: number;
  size: number;
  isDirectory: boolean;
  isSymbolicLink: boolean;
  atimeMs: number;
  mtimeMs: number;
  ctimeMs: number;
  birthtimeMs: number;
  nlink: number;
  uid: number;
  gid: number;
}
```

Phase 1 identity policy:

- directories report overlay-view `dev`
- non-directories report identity from the currently visible source object
- non-directory `dev` / `ino` may change after copy-up, matching OverlayFS without `xino`

If we later want `xino`-like stability, that is a separate feature.

### Versioning Must Also Become Layer-Aware

If versioning stays, the current `FsMetadataStoreVersioning` shape is no longer sufficient because it keys everything by bare `ino`.

Required breaking changes:

- add `layerId` to `createVersion`
- add `layerId` to `getVersion`
- add `layerId` to `listVersions`
- add `layerId` to `getVersionChunkMap`
- add `layerId` to `deleteVersions`
- add `layerId` to `restoreVersion`

Until `metacopy` exists, the public API should still behave as if `storageMode` is effectively `inline | chunked`.

## Proposed Schema

The schema below is the durable phase-1 model. Optional phase-2 tables are called out separately.

### 0. Schema Metadata

```sql
CREATE TABLE schema_meta (
  key               TEXT PRIMARY KEY,
  value             TEXT NOT NULL
);
```

Required keys:

- `schema_version`
- `feature_flags`

Version mismatches must fail closed. Migration is one-way.

### 1. Layers

```sql
CREATE TABLE layers (
  layer_id          INTEGER PRIMARY KEY AUTOINCREMENT,
  kind              TEXT NOT NULL CHECK(kind IN ('base', 'snapshot', 'upper')),
  writable          INTEGER NOT NULL CHECK(writable IN (0, 1)),
  frozen            INTEGER NOT NULL DEFAULT 0 CHECK(frozen IN (0, 1)),
  state             TEXT NOT NULL CHECK(state IN ('draft', 'active', 'sealed', 'deleted')),
  created_at_ms     INTEGER NOT NULL,
  sealed_at_ms      INTEGER,
  description       TEXT
);
```

Notes:

- lower layers must be `frozen=1`
- upper layers are writable only while their view is active
- every layer root is always inode `1` in that layer
- enforce with triggers: `kind='upper'` implies `writable=1 AND frozen=0`, and `kind IN ('base','snapshot')` implies `writable=0 AND frozen=1`

Using `ino=1` per layer keeps root resolution simple because the primary key is `(layer_id, ino)`.

### 1.1 Layer Inode Counters

```sql
CREATE TABLE layer_counters (
  layer_id          INTEGER PRIMARY KEY,
  next_ino          INTEGER NOT NULL,
  FOREIGN KEY (layer_id) REFERENCES layers(layer_id) ON DELETE CASCADE
);
```

This replaces the old single global allocator. New inode numbers are allocated per layer inside the same transaction that creates the inode.

### 2. Overlay Views

```sql
CREATE TABLE overlay_views (
  view_id                 INTEGER PRIMARY KEY AUTOINCREMENT,
  upper_layer_id          INTEGER NOT NULL,
  state                   TEXT NOT NULL DEFAULT 'active'
                           CHECK(state IN ('active', 'sealed', 'deleted')),
  created_at_ms           INTEGER NOT NULL,
  sealed_at_ms            INTEGER,
  description             TEXT,
  UNIQUE (upper_layer_id),
  FOREIGN KEY (upper_layer_id) REFERENCES layers(layer_id)
);

CREATE TABLE overlay_view_lowers (
  view_id                 INTEGER NOT NULL,
  lower_order             INTEGER NOT NULL,
  lower_layer_id          INTEGER NOT NULL,
  PRIMARY KEY (view_id, lower_order),
  UNIQUE (view_id, lower_layer_id),
  FOREIGN KEY (view_id) REFERENCES overlay_views(view_id) ON DELETE CASCADE,
  FOREIGN KEY (lower_layer_id) REFERENCES layers(layer_id)
);
```

Rules:

- one upper layer belongs to at most one active view
- lower layers may be shared across many views
- `lower_order=0` is the highest-precedence lower layer
- once a layer is referenced as a lower layer, it must never become writable again
- enforce with triggers: `lower_layer_id` must never equal `upper_layer_id`, lowers must be frozen/non-writable, and uppers must be writable/non-frozen

### 3. Inodes

```sql
CREATE TABLE inodes (
  layer_id                INTEGER NOT NULL,
  ino                     INTEGER NOT NULL,
  type                    TEXT NOT NULL CHECK(type IN ('file', 'directory', 'symlink')),
  mode                    INTEGER NOT NULL,
  uid                     INTEGER NOT NULL DEFAULT 0,
  gid                     INTEGER NOT NULL DEFAULT 0,
  size                    INTEGER NOT NULL DEFAULT 0,
  nlink                   INTEGER NOT NULL DEFAULT 0,
  atime_ms                INTEGER NOT NULL,
  mtime_ms                INTEGER NOT NULL,
  ctime_ms                INTEGER NOT NULL,
  birthtime_ms            INTEGER NOT NULL,
  storage_mode            TEXT NOT NULL
                           CHECK(storage_mode IN ('inline', 'chunked', 'metacopy')),
  inline_content          BLOB,

  overlay_opaque          INTEGER NOT NULL DEFAULT 0 CHECK(overlay_opaque IN (0, 1)),

  origin_layer_id         INTEGER,
  origin_ino              INTEGER,

  data_origin_layer_id    INTEGER,
  data_origin_ino         INTEGER,

  redirect_path           TEXT,

  PRIMARY KEY (layer_id, ino),
  FOREIGN KEY (layer_id) REFERENCES layers(layer_id),
  FOREIGN KEY (origin_layer_id, origin_ino) REFERENCES inodes(layer_id, ino),
  FOREIGN KEY (data_origin_layer_id, data_origin_ino) REFERENCES inodes(layer_id, ino),

  CHECK (overlay_opaque = 0 OR type = 'directory'),
  CHECK ((origin_layer_id IS NULL) = (origin_ino IS NULL)),
  CHECK ((data_origin_layer_id IS NULL) = (data_origin_ino IS NULL)),
  CHECK (redirect_path IS NULL OR type = 'directory'),
  CHECK (
    (storage_mode = 'inline' AND type = 'file' AND data_origin_layer_id IS NULL AND data_origin_ino IS NULL) OR
    (storage_mode = 'chunked' AND type = 'file' AND inline_content IS NULL AND data_origin_layer_id IS NULL AND data_origin_ino IS NULL) OR
    (storage_mode = 'metacopy' AND type = 'file' AND inline_content IS NULL AND data_origin_layer_id IS NOT NULL AND data_origin_ino IS NOT NULL) OR
    (type IN ('directory', 'symlink') AND storage_mode = 'inline' AND inline_content IS NULL AND data_origin_layer_id IS NULL AND data_origin_ino IS NULL)
  )
);
```

Meaning of overlay-specific columns:

- `overlay_opaque`: only meaningful on upper-layer directories
- `origin_layer_id` / `origin_ino`: durable copy-up provenance
- `data_origin_layer_id` / `data_origin_ino`: reserved for future `metacopy`
- `redirect_path`: reserved for future `redirect_dir`; if enabled later it stores the original overlay-root-absolute path after directory-only copy-up

Phase-1 rules:

- `storage_mode='metacopy'` is reserved but must not be emitted
- whiteout rows may exist only in active upper layers
- only upper-layer directories may carry `overlay_opaque=1`
- `redirect_path`, if present in a later phase, must be a normalized absolute overlay path
- `storage_mode` should default to `inline` on inode creation and be promoted to `chunked` by write paths, so phase-1 callers do not need to pass it explicitly
- `chunks` may exist only for file inodes and `symlinks` may exist only for symlink inodes; enforce with write-path checks plus `fsck`, and add triggers if we want DB-level enforcement

### 4. Directory Entries

```sql
CREATE TABLE dentries (
  layer_id                INTEGER NOT NULL,
  parent_ino              INTEGER NOT NULL,
  name                    TEXT NOT NULL,
  entry_kind              TEXT NOT NULL CHECK(entry_kind IN ('normal', 'whiteout')),
  child_ino               INTEGER,

  PRIMARY KEY (layer_id, parent_ino, name),

  FOREIGN KEY (layer_id, parent_ino) REFERENCES inodes(layer_id, ino),
  FOREIGN KEY (layer_id, child_ino) REFERENCES inodes(layer_id, ino),

  CHECK (
    (entry_kind = 'normal' AND child_ino IS NOT NULL) OR
    (entry_kind = 'whiteout' AND child_ino IS NULL)
  ),
  CHECK (name <> '' AND name <> '.' AND name <> '..' AND instr(name, '/') = 0)
);

CREATE INDEX idx_dentries_child
  ON dentries(layer_id, child_ino);
```

This is the core durability change.

A lower-layer delete is represented as:

- a row in the upper layer
- same logical parent path and `name`
- attached to the corresponding upper-layer parent directory inode
- `entry_kind='whiteout'`

That is the durable equivalent of an OverlayFS whiteout.

Important write-path rule:

- if the parent directory exists only in lower, the implementation must first create or copy-up the ancestor directory chain in upper so the whiteout row has a real upper-layer parent inode

### 5. Symlinks

```sql
CREATE TABLE symlinks (
  layer_id                INTEGER NOT NULL,
  ino                     INTEGER NOT NULL,
  target                  TEXT NOT NULL,
  PRIMARY KEY (layer_id, ino),
  FOREIGN KEY (layer_id, ino) REFERENCES inodes(layer_id, ino)
);
```

### 6. Chunk Mapping

```sql
CREATE TABLE chunks (
  layer_id                INTEGER NOT NULL,
  ino                     INTEGER NOT NULL,
  chunk_index             INTEGER NOT NULL,
  block_key               TEXT NOT NULL,
  PRIMARY KEY (layer_id, ino, chunk_index),
  FOREIGN KEY (layer_id, ino) REFERENCES inodes(layer_id, ino)
);
```

This stays close to the current design. The main change is layer scoping.

### 7. Pending Block Operations

```sql
CREATE TABLE pending_block_ops (
  txn_id                  TEXT NOT NULL,
  block_key               TEXT NOT NULL,
  op                      TEXT NOT NULL
                           CHECK(op IN ('publish', 'retire')),
  created_at_ms           INTEGER NOT NULL,
  PRIMARY KEY (txn_id, block_key, op)
);
```

Purpose:

- SQLite savepoints do not roll back block-store side effects
- recovery must know which block publishes and retirements were in flight
- crash before cleanup may leak blocks temporarily, but must never create visible metadata pointing at missing data

### 8. Optional `index=on` Support (Phase 2)

Phase 1 should match OverlayFS with `index=off`. That means lower hardlinks may diverge after one name is copied up.

If we later want `index=on`-style behavior, add:

```sql
CREATE TABLE copy_up_index (
  upper_layer_id          INTEGER NOT NULL,
  lower_layer_id          INTEGER NOT NULL,
  lower_ino               INTEGER NOT NULL,
  upper_ino               INTEGER NOT NULL,
  PRIMARY KEY (upper_layer_id, lower_layer_id, lower_ino),
  UNIQUE (upper_layer_id, upper_ino),
  FOREIGN KEY (upper_layer_id, upper_ino) REFERENCES inodes(layer_id, ino),
  FOREIGN KEY (lower_layer_id, lower_ino) REFERENCES inodes(layer_id, ino)
);
```

This is not needed for phase-1 correctness.

### 9. Optional Versions Table

If versioning remains, it must also be layer-scoped:

```sql
CREATE TABLE versions (
  layer_id                INTEGER NOT NULL,
  ino                     INTEGER NOT NULL,
  version                 INTEGER NOT NULL,
  size                    INTEGER NOT NULL,
  created_at_ms           INTEGER NOT NULL,
  storage_mode            TEXT NOT NULL,
  inline_content          BLOB,
  chunk_map               TEXT,
  PRIMARY KEY (layer_id, ino, version),
  FOREIGN KEY (layer_id, ino) REFERENCES inodes(layer_id, ino)
);
```

## How Resolution Works

Path resolution in a view should be:

1. Start from `(upper_layer_id, 1)` and from `(lower_layer_id, 1)` for each lower layer.
2. For each path component:
   - check upper first
   - if upper has a whiteout for the name: stop with `ENOENT`
   - if upper has a normal non-directory: choose upper and stop lower lookup for that name
   - if upper has a normal directory:
     - if the upper directory is opaque: choose upper only
     - otherwise merge with matching lower directories
   - if upper has no entry:
     - if the highest-precedence matching lower entry is a non-directory, choose that visible lower entry
     - if one or more matching lower entries are directories, merge all matching lower directories in precedence order until a non-directory in a higher-precedence lower blocks the name
3. For merged directories:
   - upper names come first
   - lower names are appended only if not shadowed by upper or by a whiteout
4. Directory metadata comes from upper if an upper directory exists; lower directory metadata is hidden
5. Merged `readdir` results should be cached per opened directory handle and rebuilt after reopen or rewind, matching the OverlayFS model rather than recalculating mid-stream

This matches the kernel rule that only name lists merge while directory metadata comes from upper.

Important invariant:

- lower layers are materialized trees
- live whiteouts and opaque markers in frozen lower layers are invalid and must be rejected by validation/fsck

## How Mutations Work

### Create New File

- ensure ancestor directories exist in upper first
- create upper inode
- create upper dentry
- no lower mutation

### Write Existing Upper File

- modify upper inode and upper chunk map only

### Write Existing Lower File

- resolve visible lower object
- copy it up first
- write only to upper copy
- lower remains unchanged

### `chmod` / `chown` / `utimes` / `truncate` on Lower File

Phase 1:

- full copy-up

Phase 2:

- optional metadata-only copy-up using `metacopy`

### Delete Upper-Only File

- remove upper dentry
- remove upper inode if link count reaches zero
- record unreachable blocks in `pending_block_ops`
- no whiteout required

### Delete Upper File That Shadows Lower Content

- remove the upper dentry
- remove the upper inode if link count reaches zero
- insert a whiteout for the same `parent/name` in the same transaction
- the lower object must remain hidden; merged lookup returns `ENOENT`

### Delete Lower-Only File

- ensure ancestor directories exist in upper first
- insert upper whiteout row
- do not mutate lower

### Delete Lower or Merged Directory

Default semantics:

- `removeDir` on a non-empty merged view returns `ENOTEMPTY`
- if the visible merged directory is empty, ensure ancestor directories exist in upper first and insert a whiteout at the parent/name
- if the directory has upper participation and lower content beneath the same name, remove the upper state and publish the whiteout atomically

### Delete Directory Contents But Keep Directory

- create upper directory if needed
- remove any upper children being deleted
- mark the upper directory `overlay_opaque=1`

This matches the semantic difference between:

- whiteout: hide the named lower entry itself
- opaque directory: keep the directory, hide lower children

### Rename

- rename upper-only file or upper-only directory within upper normally
- rename lower-only or merged directory returns `EXDEV` by default
- future phase 2 may add `redirect_dir`

### Hardlinks

Phase 1 rule:

- creating a hardlink to a lower non-directory triggers copy-up first
- once one lower hardlinked name is copied up, alias preservation is not guaranteed across the remaining lower names

That is consistent with OverlayFS `index=off`.

## Durability Protocol

This is the part the original draft was too vague about. SQLite metadata is transactional, but block storage is not.

Phase-1 write ordering should be:

### Create / overwrite / copy-up

1. Write new blocks to fresh block-store keys first.
2. Record pending `publish` / `retire` entries in `pending_block_ops`.
3. Commit metadata in one SQLite transaction so the inode and chunk map point at the new keys.
4. Finalize the `pending_block_ops` journal and retire old keys after commit on a best-effort basis.

Crash outcomes:

- crash before metadata commit: leaked blocks only, no visible partial file
- crash after metadata commit but before GC: visible file is correct, old blocks leak temporarily

### Delete / truncate

1. Commit metadata first, removing or replacing the chunk references.
2. Record pending `retire` entries in `pending_block_ops`.
3. Delete them asynchronously.

The invariant is:

- visible metadata must never reference a block that is missing
- leaks are acceptable temporarily
- partial visible state is not

Required SQLite settings for this contract:

- `journal_mode=WAL`
- `synchronous=FULL`
- `foreign_keys=ON`

## OCI Mapping

Internal SQL encoding and OCI tar encoding are not the same thing. The important requirement is semantic equivalence.

Canonical import rules:

- `.wh.<name>` becomes one whiteout row for `<name>`
- `.wh..wh..opq` becomes `overlay_opaque=1` on the containing upper directory

Canonicalization rules:

- if a layer contains both `.wh.<name>` and a normal `<name>`, the whiteout is redundant because whiteouts only apply to lower layers; keep the normal entry
- if a layer contains `.wh..wh..opq` and explicit child whiteouts for lower-only children, accept both but canonicalize to the opaque directory plus the remaining actual upper entries
- import order must not matter; OCI explicitly requires opaque handling to be order-independent
- reject ordinary OCI export/import paths whose basename begins with `.wh.` unless they are actual whiteout markers; OCI reserves that prefix
- OCI markers are consumed during import; frozen lower layers in our runtime are materialized trees, not live OCI diff layers

Raw-layer note:

- if a frozen layer is inspected through a raw layer API, it is a materialized tree without overlay whiteout/opaque markers
- `entry_kind='whiteout'` and `overlay_opaque=1` are only valid in active upper layers

## Why This Matches OverlayFS Closely

### Whiteouts

OverlayFS uses upper whiteouts to hide lower names. Our equivalent is a durable upper-layer whiteout dentry row.

### Opaque Directories

OverlayFS uses an opaque marker on upper directories. Our equivalent is `overlay_opaque=1` on upper directories.

### Copy-Up

OverlayFS copies up on the first operation that requires write access or metadata mutation. Our equivalent is full copy-up into upper before that operation.

### `EXDEV` on Lower or Merged Directory Rename

OverlayFS returns `EXDEV` by default. We should do the same.

### `index=off` Hardlink Behavior

OverlayFS without `index=on` can break lower hardlink aliasing on copy-up. Phase 1 should document and match that instead of accidentally promising stronger behavior than the kernel default.

### No Separate Workdir

Linux needs `workdir` because its implementation runs over a real upper filesystem. We can replace that with:

- SQLite transactions
- staged block writes
- deferred block GC

That preserves the same correctness property without a separate visible filesystem layer.

## Recommended Phases

### Phase 1: Required Parity

Implement:

- upper + multiple lowers
- durable whiteouts
- durable opaque directories
- full copy-up on the first operation requiring write access or metadata mutation
- full copy-up on metadata mutation
- merged directory listing
- `EXDEV` for lower and merged directory rename
- `VirtualStat.dev`
- durable overlay views in SQLite
- staged block writes plus durable `pending_block_ops`

Do not implement yet:

- `redirect_dir`
- `metacopy`
- `index=on`
- export-grade origin verification

### Phase 2: Closer Parity

Implement:

- `redirect_dir`
- `metacopy`
- `index=on` equivalent using `copy_up_index`
- OCI import/export helpers
- optional `xino`-like stable identity if we decide we need it

## Test Matrix

This is the minimum set. Missing items are gaps.

### A. View Construction

1. Create a view with one upper and one lower.
2. Create a view with one upper and multiple lowers.
3. Reject a view with a writable lower.
4. Reject duplicate lower order.
5. Reject a frozen upper.
6. Re-open a persisted view after restart.
7. Verify every layer root is inode `1`.
8. Reject reuse of the same upper by two active views.
9. Reject duplicate `(view_id, lower_layer_id)` rows.

### B. Basic Visibility

1. Read file that exists only in lower.
2. Read file that exists only in upper.
3. Upper file hides lower file of same name.
4. Upper symlink hides lower file of same name.
5. Upper file hides lower directory of same name.
6. Upper directory merges with lower directory of same name.
7. Directory metadata comes from upper when upper directory exists.

### C. Whiteouts

1. Delete lower-only file creates one upper whiteout row.
2. Deleted lower-only file does not resolve in merged view.
3. Deleted lower-only file is still accessible in the raw lower layer.
4. Deleting lower-only file does not create a copied-up inode.
5. Whiteout hides matching lower file in directory listing.
6. Whiteout hides matching lower directory in directory listing.
7. Whiteout survives restart.
8. Recreating the same path removes the whiteout and exposes the new upper entry.
9. Delete lower-only file whose parent exists only in lower first creates the ancestor upper directory chain.
10. Lower layers never contain `entry_kind='whiteout'` rows.

### D. Opaque Directories

1. Opaque upper directory hides lower children.
2. Opaque upper directory still exposes upper children.
3. Non-opaque upper directory merges lower children.
4. Opaque marker survives restart.
5. Listing an opaque directory does not leak lower names.
6. OCI import of redundant explicit whiteouts beneath an opaque directory canonicalizes correctly.
7. Lower layers never contain `overlay_opaque=1`.

### E. Copy-Up

1. First operation requiring write access or metadata mutation on a lower file performs copy-up.
2. Second write to the same file does not copy-up again.
3. Copy-up preserves mode, uid, gid, timestamps, symlink target, and file size.
4. Copy-up ensures ancestor directories exist in upper.
5. Metadata mutation on lower file also triggers copy-up.
6. Hardlink creation against lower file triggers copy-up first.
7. Symlink creation does not trigger copy-up.
8. Phase 1 explicitly allows one lower hardlink to diverge from its lower siblings after copy-up.

### F. Directory Behavior

1. `mkdir -p` on existing lower directory is a no-op.
2. `mkdir -p` on lower symlink-to-directory does not replace the symlink.
3. New upper directory is visible immediately.
4. Empty merged directory lists correctly.
5. Merged directory with both upper and lower names returns a deduplicated list.
6. Merged directory listings remain stable for the lifetime of an opened directory handle and are rebuilt after reopen or rewind.

### G. Rename Semantics

1. Rename upper-only file works.
2. Rename lower-only file copies up then removes the old visible name.
3. Rename upper-only directory works.
4. Rename lower-only directory returns `EXDEV`.
5. Rename merged directory returns `EXDEV`.
6. Future `redirect_dir` tests record redirect metadata and resolve correctly.
7. Rename of a lower hardlinked file does not promise alias preservation in phase 1.
8. Crash during lower-directory rename fallback does not leave half-copied directories or stray whiteouts.

### H. Delete Semantics

1. Delete upper-only file removes upper dentry and upper inode if link count reaches zero.
2. Delete lower-only file creates whiteout only.
3. Deleting an upper file that shadowed lower removes the upper state and leaves a whiteout, so merged lookup returns `ENOENT`.
4. Delete lower-only empty directory creates whiteout at parent/name.
5. Delete contents of merged directory while keeping directory sets opaque marker.
6. Recursive delete on mixed upper/lower subtree behaves deterministically.
7. `removeDir` on a merged directory with any visible child returns `ENOTEMPTY`.
8. Crash during opaque-directory replacement does not leave both lower children visible and upper opaque metadata committed.

### I. Symlinks and Hardlinks

1. Lower symlink resolves through overlay.
2. Copy-up of a symlink preserves target.
3. Hardlink to lower file copies up source first.
4. Hardlink counts remain correct within upper.
5. Phase 1 documents divergence of lower hardlinks after copy-up.
6. Phase 2 `index=on` tests preserve linked aliases across copy-up.

### J. Multiple Lower Layers

1. Highest-precedence lower wins over deeper lower for non-directories.
2. Upper whiteout hides all matching names from all lowers.
3. Opaque upper directory hides all lower children beneath it.
4. Same-named directories across multiple lowers merge in precedence order.
5. Copy-up provenance points to the first visible lower object.
6. Frozen lower snapshots are materialized trees and do not contain overlay whiteouts or opaque markers.

### K. Persistence and Recovery

1. Restart after whiteout preserves delete.
2. Restart after copy-up preserves upper data.
3. Restart after opaque marker preserves hidden lower children.
4. Crash during whiteout insertion rolls back cleanly.
5. Crash during copy-up never leaves partial visible state.
6. Crash during copy-up with chunked data does not leave a visible inode pointing at missing blocks.
7. Crash after block writes but before metadata commit leaks blocks only.
8. Crash after metadata commit but before GC leaves leaked old blocks only.
9. Kill-9 or simulated power-fail during whiteout, copy-up, rename, snapshot seal, and view attach reopens as either all-old or all-new state.

### L. Transactions and Concurrency

1. Whiteout insertion is atomic.
2. Copy-up and dentry replacement are atomic.
3. Rename sequence is atomic.
4. Directory listing never returns both a whiteout and the hidden lower name.
5. Constraint violations reject impossible states.
6. Concurrent copy-up of the same visible lower file does not produce two visible upper files.
7. Concurrent delete and write of the same visible lower path resolves to one committed winner.
8. Concurrent whiteout and recreate of the same path never exposes the lower entry in between.
9. Concurrent readers see either the old visible object or the new one, never a half-published mixed state.

### M. Impossible-State Tests

These must be rejected by schema, write-path checks, or `fsck`.

1. Whiteout row with child inode set.
2. Normal dentry row without child inode.
3. `overlay_opaque=1` on non-directory inode.
4. Chunk rows for symlink or directory inodes.
5. Dentry referencing child inode in a different layer.
6. Upper layer modified after it becomes sealed or frozen.
7. Lower layer referenced by a view while still writable.
8. Invalid dentry names: empty string, `.`, `..`, or names containing `/`.
9. `origin_layer_id` without `origin_ino`, or vice versa.
10. Opaque directory flag on a non-upper layer directory.
11. Whiteout row on a non-upper layer.

### N. `fsck` and Invariant Checks

1. Every layer has exactly one root inode at `(layer_id, 1)`.
2. Every active view upper is writable and not frozen.
3. Every referenced lower is frozen.
4. Every `origin_layer_id` / `origin_ino` pair resolves.
5. Every `pending_block_ops` entry is replayable or safely discardable on recovery.
6. No chunk rows exist for non-file inodes.
7. No symlink row exists for a non-symlink inode.
8. No lower layer contains a whiteout row or an opaque marker.

### O. OCI Interoperability

1. Export whiteout row as `.wh.<name>`.
2. Import `.wh.<name>` as upper whiteout row.
3. Export explicit whiteouts by default for opaque directory replacement.
4. Import opaque whiteout as `overlay_opaque=1`.
5. Accept explicit whiteouts and opaque whiteout forms.
6. Import order of `.wh..wh..opq` and sibling entries does not matter.
7. Importing both `.wh.<name>` and `<name>` in the same layer canonicalizes to the normal entry because whiteouts only apply to lower layers.
8. OCI export/import rejects ordinary paths whose basename begins with `.wh.` because OCI reserves that prefix for whiteout markers.

### P. Linux Differential Oracle

1. Replay the same operation corpus against kernel OverlayFS and the metadata implementation.
2. Compare visible tree shape, `stat` results, rename/unlink/rmdir outcomes, and directory listing results.
3. Document every intentional deviation instead of allowing silent drift.

## Comparison to the Current Core Metadata

Current SQLite metadata model:

- one inode namespace
- one dentry table
- one symlink table
- one chunk map
- optional versions

Missing today:

- no layer ordering
- no overlay view object
- no durable whiteouts
- no durable opaque directories
- no copy-up provenance
- no crash-safe overlay delete semantics

Proposed model adds:

- explicit layers
- explicit overlay views
- layer-scoped inodes and dentries
- durable whiteout rows
- durable opaque directories
- explicit lower ordering
- explicit pending block journal for crash safety

## Migration Safety

1. Ship an offline one-way migrator from the legacy single-tree metadata format to the layered schema.
2. Make schema-version mismatches fail closed.
3. Before cutover, export a legacy snapshot/base-filesystem artifact so there is a downgrade escape hatch outside the new DB format.

## Recommendation

We should implement this as a new layered metadata subsystem, not as an incremental patch on top of the in-memory whiteout wrapper.

Recommended path:

1. Keep the wrapper only as temporary compatibility for existing tests.
2. Add layer-aware SQLite metadata with the phase-1 schema above.
3. Extend `VirtualStat` with `dev`.
4. Update `ChunkedVFS` to resolve against a view-bound metadata session and use `NodeRef` instead of plain inode numbers internally.
5. Move base root filesystems and future snapshots onto immutable materialized lower layers plus durable upper metadata.
6. Gate rollout on `schema_version` checks, mount-time `fsck`, and the Linux differential test corpus.

That is the cleanest route to durable overlay behavior that still maps directly onto Linux OverlayFS semantics.

## Open Questions

1. Do we want to ship code for only one lower layer first while keeping the schema multi-lower from day one?
2. Do we want full copy-up only in phase 1, or do we want `metacopy` immediately?
3. Is OverlayFS `index=off` behavior sufficient permanently, or do we want a later `index=on` equivalent?
4. Do we need OCI import/export in the first implementation, or only the internal durable model?
5. Do we want an adapter for the old `FsMetadataStore` API, or do we replace it outright?
