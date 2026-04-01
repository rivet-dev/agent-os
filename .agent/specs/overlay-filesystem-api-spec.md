# Overlay Filesystem API

## Status

Reviewed after adversarial pass. This spec assumes breaking API changes are acceptable.

## Summary

The default VM filesystem should behave like a Docker/container root filesystem:

- the VM root is an overlay filesystem
- there is exactly one writable upper layer for the live VM
- there are zero or more immutable lower snapshot layers beneath it
- the built-in base filesystem snapshot from bundled `base-filesystem.json` is included by default unless explicitly disabled
- extra mounts behave like Docker volumes or bind mounts: they are separate mount boundaries, not additional layers inside the root overlay

This spec defines the public API and user model for that behavior.

It is intentionally separate from [overlay-metadata-proposal.md](/home/nathan/a3/.agent/specs/overlay-metadata-proposal.md), which covers how overlay semantics should be implemented in the metadata engine itself.

## Goals

1. Make the default root filesystem an overlay filesystem by default.
2. Match Linux OverlayFS and Docker mental models closely enough that behavior is predictable.
3. Distinguish clearly between:
   - a mounted filesystem
   - a layer
   - storage plumbing used inside a layer
4. Keep the high-level API simple for ordinary users.
5. Keep the low-level API explicit for advanced users and filesystem driver authors.

## Non-Goals

1. Exposing raw metadata-store or block-store objects in the main `AgentOs.create()` API.
2. Making arbitrary mounted `VirtualFileSystem` implementations automatically usable as overlay layers.
3. Supporting multiple writable layers in one overlay view.
4. OCI import/export in phase 1.

## Normative Model

### Root Filesystem

The VM root filesystem is an overlay filesystem by default.

Conceptually:

```text
/
  = overlay(
      upper = writable live layer,
      lowers = [zero or more frozen snapshots, including the bundled base snapshot by default]
    )
```

That is the same shape used by Linux OverlayFS and Docker `overlay2`:

- one writable upper
- zero or more read-only lowers
- one merged mounted view

### Extra Mounts

Extra mounts are separate mount boundaries.

Examples:

- `/workspace` mounted from a host directory
- `/data` mounted from another overlay filesystem
- `/proc` and `/dev` mounted as special kernel filesystems

These behave like Linux bind mounts or Docker volumes:

- they hide the underlying path while mounted
- they are not absorbed into the parent overlay's layer stack
- cross-mount rename/link and similar inode-moving operations keep normal mount semantics like `EXDEV`

### Middle Layers

"Middle layers" in a Docker-like stack are just lower snapshot layers with precedence.

Example:

```text
upper   = writable live diff
lower0  = most recent frozen snapshot
lower1  = older frozen snapshot
lower2  = base filesystem snapshot
```

They are not additional writable uppers.

## API Principles

### Principle 1: Root Should Be Config, Not Manual Composition

Most users should not call `createOverlayFilesystem()` just to get a normal VM root.

The root should be overlay-backed by default via `AgentOs.create(...)` config.

### Principle 2: Mounts and Layers Must Be Different Concepts

Mounts are kernel-visible mounted filesystems.

Layers are overlay-building blocks.

Do not collapse these into one generic interface.

### Principle 3: Layers Share One Base Interface

A writable upper layer and a frozen snapshot layer should be the same underlying concept with different invariants.

Users should not need to implement "the layer twice."

### Principle 4: Storage Plumbing Is Lower-Level Than Layer APIs

Metadata stores and block stores are implementation details of layers.

High-level users should work with layer handles and filesystem configs, not raw metadata/block objects.

## Public API Proposal

### 1. Root Filesystem Configuration

Add an explicit root filesystem config to `AgentOs.create()`.

Proposed shape:

```ts
type OverlayFilesystemMode = "ephemeral" | "read-only";

type RootSnapshotExport = { kind: "snapshot-export"; source: unknown };

type RootLowerInput =
  | { kind: "bundled-base-filesystem" }
  | RootSnapshotExport;

interface RootFilesystemConfig {
  type?: "overlay"; // default
  mode?: OverlayFilesystemMode; // default "ephemeral"
  disableDefaultBaseLayer?: boolean; // default false
  lowers?: RootLowerInput[]; // highest-precedence lower first
}

interface AgentOsOptions {
  rootFilesystem?: RootFilesystemConfig;
  mounts?: MountConfig[];
  // existing options omitted
}
```

Default behavior:

- if `rootFilesystem` is omitted, AgentOs creates:
  - an ephemeral writable upper layer backed by a filesystem layer rooted in a tmp dir in the default internal temp-dir-backed layer store
  - one lower layer from the bundled built base filesystem artifact imported from `base-filesystem.json`
- if `rootFilesystem.mode` is `"read-only"`, AgentOs mounts the lowers without a writable upper
- if `disableDefaultBaseLayer` is not set, the bundled base snapshot is appended as the deepest lower layer
- if `disableDefaultBaseLayer` is `true`, the lower stack comes only from `rootFilesystem.lowers`
- if `disableDefaultBaseLayer` is `true` and no lowers are provided, the root overlay has no lower layers
- if `rootFilesystem.lowers` is provided, `lowers[0]` is the highest-precedence lower and the last entry is the deepest/base lower
- AgentOs imports each `RootLowerInput` into the internal root store before composing `/`

Documentation requirement:

- `disableDefaultBaseLayer` must be documented in the `AgentOs.create()` API docs because it changes the default root shape substantially

Bundled base-layer requirement:

- core should statically import the built base filesystem JSON artifact into the bundle
- callers should not need to load `base-filesystem.json` manually
- the default root path should not depend on a user-visible async import step for the base layer
- `RootFilesystemConfig` intentionally accepts root-lower inputs rather than arbitrary `SnapshotLayerHandle`s because the root store remains core-owned in phase 1

Equivalent conceptual expansion:

```ts
rootFilesystem: {
  type: "overlay",
  mode: "ephemeral",
  disableDefaultBaseLayer: false,
}
```

### 2. Mount API

Mounts should support both plain mounted filesystems and declarative overlay mounts.

```ts
interface PlainMountConfig {
  path: string;
  driver: VirtualFileSystem;
  readOnly?: boolean;
}

interface OverlayMountConfig {
  path: string;
  filesystem: {
    type: "overlay";
    store: LayerStore;
    mode?: OverlayFilesystemMode; // default "ephemeral"
    lowers: SnapshotLayerHandle[]; // highest-precedence lower first
  };
}

type MountConfig = PlainMountConfig | OverlayMountConfig;
```

This keeps one consistent user model:

- plain mounts for host-dir, proc/dev, and custom VFS drivers
- declarative overlay mounts for paths like `/data`
- no separate overlay-mount helper is required in phase 1

Implementation rule:

- plain mounts take an existing `VirtualFileSystem`
- overlay mounts are resolved through the provided `LayerStore`
- `OverlayMountConfig` is the preferred public AgentOs mount surface for overlay filesystems
- prebuilding an overlay `VirtualFileSystem` and passing it through `PlainMountConfig` is a lower-level/internal pattern rather than the primary public API
- if `filesystem.mode` is omitted or `"ephemeral"`, AgentOs creates one fresh writable upper by calling `filesystem.store.createWritableLayer()` and owns that upper for the lifetime of the mount
- if `filesystem.mode` is `"read-only"`, AgentOs creates no upper layer for the mount
- the auto-created writable upper for a declarative overlay mount is not exposed directly in phase 1

### 3. Layer Handle API

Introduce explicit layer handles for overlay construction.

```ts
interface LayerHandle {
  kind: "writable" | "snapshot";
  storeId: string;
  layerId: string;
}

interface WritableLayerHandle extends LayerHandle {
  kind: "writable";
  leaseId: string;
}

interface SnapshotLayerHandle extends LayerHandle {
  kind: "snapshot";
}
```

Important point:

- `WritableLayerHandle` and `SnapshotLayerHandle` are not separate storage models
- they are constrained forms of the same underlying layer abstraction
- `LayerHandle` is an opaque, store-bound handle returned by a `LayerStore`; callers should not manually construct one from plain data
- `storeId` defines the compatibility domain for overlay composition
- all layers in one overlay view must come from the same `storeId`
- writable handles are leased capabilities; raw `layerId` alone is not enough to reopen an active writer safely
- snapshot handles are reopenable descriptors within the same compatible store
- snapshot handles may be serialized as descriptors; writable handles should be treated as live capabilities rather than durable IDs

### 4. Layer Store API

Advanced users and backend authors need a way to create, open, import, and seal layers.

Proposed shape:

```ts
interface LayerStore {
  readonly storeId: string;
  createWritableLayer(): Promise<WritableLayerHandle>;
  importSnapshot(source: SnapshotImportSource): Promise<SnapshotLayerHandle>;
  openSnapshotLayer(layerId: string): Promise<SnapshotLayerHandle>;
  sealLayer(layer: WritableLayerHandle): Promise<SnapshotLayerHandle>;
  createOverlayFilesystem(
    options:
      | {
          mode?: "ephemeral";
          upper: WritableLayerHandle;
          lowers: SnapshotLayerHandle[];
        }
      | {
          mode: "read-only";
          lowers: SnapshotLayerHandle[];
        },
  ): VirtualFileSystem;
}
```

Possible snapshot import sources in phase 1:

- base filesystem JSON artifact
- explicit snapshot export/import format

This is the right level for:

- SQLite-backed local layers
- SQLite metadata + S3 block-store layers
- future cloud/persistent stores

Core ownership rule:

- `LayerStore` should be a core API in `packages/core`
- backend packages implement or return `LayerStore`
- AgentOs uses a core-owned default internal temp-dir-backed `LayerStore` for the root overlay in phase 1
- public custom root-store injection for `/` is deferred in phase 1

Lifecycle rules:

- `sealLayer()` creates a new immutable snapshot layer ID from the current visible tree
- `sealLayer()` invalidates the writable handle it sealed
- a writable layer may be attached to at most one active overlay view
- writable layers are not reopened by raw `layerId` while active
- writable layers are single-writer only in phase 1
- the default root writable layer is a tmp-dir-backed filesystem layer managed by core's internal root `LayerStore`

### 5. Snapshot Import Source

`SnapshotImportSource` is intentionally backend-agnostic but phase 1 only needs to guarantee our internal snapshot/base-artifact inputs.

```ts
type SnapshotImportSource =
  | { kind: "base-filesystem-artifact"; source: unknown }
  | { kind: "snapshot-export"; source: unknown };
```

The exact payload shape is backend-specific. The important requirement is that importing produces a `SnapshotLayerHandle`.

Phase-1 note:

- the default root base snapshot should be bundled into core from `base-filesystem.json`
- callers should not need to load that JSON manually
- OCI import/export is deferred and tracked in `TODO.md`

### 6. Root Snapshot Lifecycle

If the root is an overlay filesystem by default, AgentOs needs a high-level lifecycle API for it.

Proposed shape:

```ts
interface AgentOs {
  snapshotRootFilesystem(): Promise<RootSnapshotExport>;
}
```

Semantics:

- creates a new frozen snapshot-export descriptor from the current visible root tree
- does not require callers to manipulate the live root upper handle directly
- the returned value is reusable in `rootFilesystem.lowers` or via `LayerStore.importSnapshot(...)`

This remains async because sealing the live root into a durable snapshot may involve metadata and block-store work even though the default base snapshot itself is bundled into core synchronously at build time.

## What Users Implement

There should be two extension surfaces.

### A. Mounted Filesystem Drivers

If a user wants to mount a filesystem directly, they implement:

```ts
interface VirtualFileSystem { ... }
```

Examples:

- host directory projection
- special kernel filesystems
- a custom remote filesystem

These are mountable.

They are not automatically valid overlay layers.

### B. Overlay Storage Backends

If a user wants a filesystem to participate in layered overlay semantics, they should implement the layer/storage side:

- a `LayerStore`
- and, internally, whatever metadata/block abstractions the layer engine needs

Examples:

- SQLite metadata + local block store
- SQLite metadata + S3 block store
- future Postgres metadata + object storage block store

These produce `LayerHandle`s that can be used to build an overlay filesystem.

### Backend Author API

Backend packages should expose a factory that returns a `LayerStore`.

Example:

```ts
function createLayerStore(config: BackendSpecificConfig): LayerStore;
```

Core should depend on:

- `LayerStore`
- `WritableLayerHandle`
- `SnapshotLayerHandle`

Core should not depend on backend-specific metadata or block-store types.

## What Users Should Not Implement Twice

Users should not implement:

- one interface for writable layers
- another interface for snapshot layers

Instead:

- implement the storage engine once
- represent writable vs frozen as layer state/invariants

Example lifecycle:

```ts
const upper = await store.createWritableLayer();
const snapshot = await store.sealLayer(upper);
```

Same layer model, different state.

## Metadata Ownership

### Each Layer Owns Metadata

Each layer should own the metadata needed to describe the filesystem tree represented by that layer.

That means:

- a layer has its own inode/dentry/chunk mapping state
- layer identity is explicit
- layers are not anonymous slices of some parent mount table

Important clarification:

- this is logical ownership, not necessarily one physical database per layer
- a `LayerStore` may keep multiple layers in one shared metadata engine or one shared block store
- the requirement is that layer identity, isolation, and durability are explicit in the storage model
- inode allocation, dentries, and chunk references must all be scoped by `layer_id`
- cross-layer references are forbidden except through explicit provenance/import rules

### Writable Upper Layer

The active writable upper layer is where live overlay state lives:

- whiteouts
- opaque directories
- copy-up provenance
- writable dentries/inodes/chunk updates

### Frozen Lower Layers

Frozen lower layers are snapshot layers suitable for use as overlay lowers.

They should not carry live overlay runtime state such as:

- active whiteouts
- active opaque markers
- live copy-up bookkeeping

If a writable layer is sealed into a reusable lower snapshot, the resulting snapshot must behave like an ordinary immutable lower layer. The internal storage encoding is an implementation detail.

### Middle Layers

Middle layers follow the same rule as any other lower:

- they use the same `LayerHandle` abstraction
- they are just frozen snapshots with higher precedence than older lowers

So yes:

- they can use the same interface
- they just are not writable

## Why a Plain Mounted VFS Is Not a Layer

Overlay construction needs semantics that a generic mounted `VirtualFileSystem` does not provide:

- layer identity
- sealing/freezing
- whiteout rules
- copy-up destination control
- provenance
- fsck/validation
- snapshot import/export

So this should be invalid:

```ts
someStore.createOverlayFilesystem({
  upper: someArbitraryVirtualFileSystem,
  lowers: [someOtherVirtualFileSystem],
});
```

The overlay builder should consume explicit layer handles, not arbitrary mounted filesystems.

## S3, Host Mounts, and Other Examples

### S3-Backed Overlay Filesystem

S3 can support overlay filesystems if it is used as block storage under a layer store.

Good model:

- durable metadata backend is present
- S3 stores blocks/chunks
- the layer store produces layer handles
- `LayerStore.createOverlayFilesystem(...)` builds the merged VFS from those handles

Bad model:

- "mount raw S3 VFS and expect the parent/root overlay to absorb it"
- "use S3 alone as the layer backend without durable metadata"

### Host Directory Mount

A host directory mount is a direct mount boundary, not a layer.

It should be treated like a Docker bind mount:

- mountable at a path
- separate from root overlay internals
- not implicitly usable as a lower or upper layer

### Another Overlay Mount

This is valid:

```ts
const dataStore = createLayerStore({
  // backend-specific config
});

const dataBase = await dataStore.importSnapshot(seedSnapshot);

await AgentOs.create({
  mounts: [
    {
      path: "/data",
      filesystem: {
        type: "overlay",
        store: dataStore,
        lowers: [dataBase],
      },
    },
  ],
});
```

Here:

- `/` is one overlay filesystem
- `/data` is another overlay filesystem
- they are still separate mount boundaries

## Recommended High-Level Examples

### Example 1: Default Root

```ts
const vm = await AgentOs.create();
```

Behavior:

- root is an overlay filesystem automatically
- lower base layer comes from the bundled built base filesystem artifact from `base-filesystem.json`
- upper layer is a fresh writable layer in the internal temp-dir-backed layer store

### Example 2: Root With Extra Middle Layers

```ts
const vm = await AgentOs.create({
  rootFilesystem: {
    disableDefaultBaseLayer: true,
    lowers: [
      snapshotExportA,
      snapshotExportB,
      { kind: "bundled-base-filesystem" },
    ],
  },
});
```

Interpretation:

- `snapshotExportA` has higher precedence than `snapshotExportB`
- `snapshotExportB` has higher precedence than the bundled base filesystem lower
- because `disableDefaultBaseLayer` is `true`, AgentOs does not append another bundled base lower underneath these inputs
- AgentOs still creates one writable upper layer automatically for the live VM

### Example 3: Root Plus Host Bind Mount

```ts
const vm = await AgentOs.create({
  mounts: [
    { path: "/workspace", driver: hostDirFs, readOnly: false },
  ],
});
```

Interpretation:

- `/workspace` is a separate mount boundary
- root overlay does not see through it

### Example 4: Root Plus S3-Backed Layered Mount

```ts
const dataStore = createLayerStore({
  // durable metadata backend + S3 block store config
});

const seedBase = await dataStore.importSnapshot(seedSnapshot);

const vm = await AgentOs.create({
  mounts: [
    {
      path: "/data",
      filesystem: {
        type: "overlay",
        store: dataStore,
        lowers: [seedBase],
      },
    },
  ],
});
```

Interpretation:

- `/data` has overlay semantics internally
- `/data` is still a separate mount from `/`

## Invariants

1. One overlay view has exactly one writable upper layer, or zero uppers in explicit read-only mode.
2. Lower layers are frozen snapshots.
3. Lower ordering is explicit: `lowers[0]` is the highest-precedence lower.
4. Middle layers are just higher-precedence frozen lower layers.
5. Mounts and layers are distinct concepts.
6. Arbitrary mounted `VirtualFileSystem` drivers are not valid overlay layers unless they explicitly expose layer handles through a layer store.
7. Root is an overlay filesystem by default.
8. Cross-mount rename/link and similar inode-moving operations retain normal mount semantics rather than overlay-specific magic.
9. A writable layer may be attached to at most one active overlay view.
10. The bundled base filesystem snapshot is included by default unless `disableDefaultBaseLayer` is set.
11. The default root writable upper lives in the core-owned temp-dir-backed layer store unless a later design explicitly adds custom root-store injection.
12. Public overlay-mount configuration is declarative in phase 1; no separate overlay-mount helper API is required.

## Deferred

1. OCI import/export support for overlay layers and snapshots is deferred beyond phase 1 and tracked in `TODO.md`.
2. Public custom root-store injection for `/` is deferred beyond phase 1; root store ownership remains in core for now.
