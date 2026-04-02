# Agent OS Runtime Consolidation Spec

## Status

Draft.

Builds on:

- [agent-os-runtime-consolidation-requirements.md](/home/nathan/a5/.agent/research/agent-os-runtime-consolidation-requirements.md)

This document is the implementation-facing follow-on spec. It turns the requirements into a concrete target architecture, migration plan, and test strategy.

## Decision Summary

Agent OS will absorb the legacy Secure-Exec runtime stack directly into this repo and stop depending on it as an external product.

The end state is:

- no `@secure-exec/*` dependencies
- no `secure-exec` binaries, env vars, docs, or user-facing names
- no Python runtime support in scope for this migration
- no public execution-engine plugin abstraction
- no public raw `AgentOs.kernel` escape hatch in the final SDK
- one Agent OS-owned runtime model with three explicit planes:
  - host plane
  - kernel plane
  - execution plane

The kernel becomes a Rust library shared by native and browser builds. Native execution is hosted by a native sidecar process. Browser execution is hosted by a main-thread browser sidecar that owns the kernel and spawns workers only for guest execution.

The migration is incremental. The repo should stay working after each major phase whenever practical. Temporary migration-only adapters are allowed during cutover, but the final state must delete them.

## Goals

- Move the required runtime code from the legacy codebase into this repo.
- Keep the current Agent OS host configuration model roughly intact.
- Preserve feature parity for filesystem behavior, command availability, package injection, browser behavior, and host-managed runtime placement.
- Add scoped Rust tests for each kernel subsystem as it is ported.
- Preserve the ability to use a default shared sidecar or an explicitly created sidecar handle.
- Preserve timing-mitigation behavior across guest JavaScript and guest WebAssembly.
- Migrate the acceptance harness before it is used as a parity gate for the cutover.
- End with a clean Agent OS-owned codebase, not a renamed copy of the old architecture.

## Non-Goals

- Porting or preserving Python support.
- Preserving legacy wire compatibility.
- Preserving the old public runtime package split.
- Keeping old package boundaries just because they existed before.
- Keeping any final `secure-exec` branding in code or docs.

## Research Findings

### 1. Current Agent OS Is Still Deeply Coupled To Secure-Exec

Current `packages/core` still depends directly on:

- `@secure-exec/core`
- `@secure-exec/nodejs`
- `@secure-exec/v8`
- `secure-exec`
- `@rivet-dev/agent-os-posix`
- `@rivet-dev/agent-os-python`

Evidence:

- [packages/core/package.json](/home/nathan/a5/packages/core/package.json)
- [packages/core/src/agent-os.ts](/home/nathan/a5/packages/core/src/agent-os.ts)

The current browser package also still depends on Secure-Exec code and currently rejects `timingMitigation`, which is a direct parity gap:

- [packages/browser/package.json](/home/nathan/a5/packages/browser/package.json)
- [runtime.test.ts](/home/nathan/a5/packages/browser/tests/runtime-driver/runtime.test.ts)
- [runtime-driver.ts](/home/nathan/a5/packages/browser/src/runtime-driver.ts)

There are currently 248 files under `packages/` and `registry/` that still reference `secure-exec` or `@secure-exec/*`.

### 2. The Current Host Configuration Surface Is Already Good Enough To Preserve

The current `AgentOsOptions` shape already expresses the host-controlled surface we want to keep:

- `software`
- `loopbackExemptPorts`
- `moduleAccessCwd`
- `rootFilesystem`
- `mounts`
- `additionalInstructions`
- `scheduleDriver`
- `toolKits`
- `permissions`

Evidence:

- [agent-os.ts](/home/nathan/a5/packages/core/src/agent-os.ts#L173)

This means the migration should preserve the host-facing model rather than redesigning configuration from scratch.

What does not exist today is sidecar selection in the Agent OS SDK. Default shared sidecars and explicit sidecar handles are new host-managed capabilities to add during consolidation, modeled after the legacy sidecar runtime rather than preserved current Agent OS behavior.

### 3. The Legacy Kernel Is A Bounded Port Target

The legacy TypeScript kernel is not infinitely large:

- 12 source files
- 3,667 raw lines
- largest file is `kernel.ts` at 1,098 lines

Source layout:

- [command-registry.ts](/home/nathan/secure-exec-4-rebase/packages/kernel/src/command-registry.ts)
- [device-layer.ts](/home/nathan/secure-exec-4-rebase/packages/kernel/src/device-layer.ts)
- [fd-table.ts](/home/nathan/secure-exec-4-rebase/packages/kernel/src/fd-table.ts)
- [kernel.ts](/home/nathan/secure-exec-4-rebase/packages/kernel/src/kernel.ts)
- [permissions.ts](/home/nathan/secure-exec-4-rebase/packages/kernel/src/permissions.ts)
- [pipe-manager.ts](/home/nathan/secure-exec-4-rebase/packages/kernel/src/pipe-manager.ts)
- [process-table.ts](/home/nathan/secure-exec-4-rebase/packages/kernel/src/process-table.ts)
- [pty.ts](/home/nathan/secure-exec-4-rebase/packages/kernel/src/pty.ts)
- [types.ts](/home/nathan/secure-exec-4-rebase/packages/kernel/src/types.ts)
- [user.ts](/home/nathan/secure-exec-4-rebase/packages/kernel/src/user.ts)
- [vfs.ts](/home/nathan/secure-exec-4-rebase/packages/kernel/src/vfs.ts)

This is large enough to need planning, but small enough to port subsystem-by-subsystem with strong tests.

### 4. The Legacy Kernel Already Has A Strong Behavior Suite

The legacy kernel test surface is substantial:

- 12 test/helper files
- 6,566 raw lines
- dedicated suites for command registry, FD table, device layer, process table, pipes, auth, terminal behavior, resource exhaustion, and integration

Evidence:

- [command-registry.test.ts](/home/nathan/secure-exec-4-rebase/packages/kernel/test/command-registry.test.ts)
- [fd-table.test.ts](/home/nathan/secure-exec-4-rebase/packages/kernel/test/fd-table.test.ts)
- [device-layer.test.ts](/home/nathan/secure-exec-4-rebase/packages/kernel/test/device-layer.test.ts)
- [process-table.test.ts](/home/nathan/secure-exec-4-rebase/packages/kernel/test/process-table.test.ts)
- [pipe-manager.test.ts](/home/nathan/secure-exec-4-rebase/packages/kernel/test/pipe-manager.test.ts)
- [cross-pid-auth.test.ts](/home/nathan/secure-exec-4-rebase/packages/kernel/test/cross-pid-auth.test.ts)
- [shell-terminal.test.ts](/home/nathan/secure-exec-4-rebase/packages/kernel/test/shell-terminal.test.ts)
- [resource-exhaustion.test.ts](/home/nathan/secure-exec-4-rebase/packages/kernel/test/resource-exhaustion.test.ts)
- [kernel-integration.test.ts](/home/nathan/secure-exec-4-rebase/packages/kernel/test/kernel-integration.test.ts)

This suite should be treated as the porting contract for the new Rust kernel.

### 5. The Sidecar Already Has The Right Kind Of Test Coverage

The legacy V8 sidecar tests already cover the categories we still need after renaming:

- protocol framing
- IPC round-trip
- auth and session isolation
- process isolation
- crash containment
- snapshot behavior
- snapshot security

Evidence:

- [ipc-binary.test.ts](/home/nathan/secure-exec-4-rebase/packages/secure-exec-v8/test/ipc-binary.test.ts)
- [ipc-roundtrip.test.ts](/home/nathan/secure-exec-4-rebase/packages/secure-exec-v8/test/ipc-roundtrip.test.ts)
- [ipc-security.test.ts](/home/nathan/secure-exec-4-rebase/packages/secure-exec-v8/test/ipc-security.test.ts)
- [process-isolation.test.ts](/home/nathan/secure-exec-4-rebase/packages/secure-exec-v8/test/process-isolation.test.ts)
- [crash-isolation.test.ts](/home/nathan/secure-exec-4-rebase/packages/secure-exec-v8/test/crash-isolation.test.ts)
- [context-snapshot-behavior.test.ts](/home/nathan/secure-exec-4-rebase/packages/secure-exec-v8/test/context-snapshot-behavior.test.ts)
- [snapshot-security.test.ts](/home/nathan/secure-exec-4-rebase/packages/secure-exec-v8/test/snapshot-security.test.ts)

These should be ported, not reinvented.

### 6. The Repo Already Contains The Native Command Surface We Need To Preserve

The repo already has a large `registry/native` workspace with native command crates and WASI support crates.

Evidence:

- [registry/native](/home/nathan/a5/registry/native)
- [Cargo.toml](/home/nathan/a5/registry/native/Cargo.toml)
- [crates/wasi-ext](/home/nathan/a5/registry/native/crates/wasi-ext)

This means the migration does not need to preserve `packages/posix` as a public product boundary. The preserved behavior is the command surface and runtime behavior, not the package name.

### 7. The Real Acceptance Surface Is Larger Than The Kernel Alone

The migration cannot stop at unit parity. `registry/tests` already exercise:

- kernel/runtime integration
- npm lifecycle and install behavior
- cross-runtime pipes and network
- terminal behavior
- native command behavior
- browser/WASI behavior

Evidence:

- [registry/tests/kernel](/home/nathan/a5/registry/tests/kernel)
- [registry/tests/wasmvm](/home/nathan/a5/registry/tests/wasmvm)

This suite should become the final parity gate after the internal cutover.

## Final Architecture

## Three Planes

### Host Plane

The host plane is the JavaScript SDK owned by Agent OS.

Responsibilities:

- construct Agent OS instances
- choose the default shared sidecar or an explicit sidecar handle
- provide host config such as mounts, software, instructions, permissions, and toolkits
- translate JS filesystem drivers and package descriptors into the sidecar protocol
- expose stable APIs to callers

The host plane does not own per-VM kernel state.

### Kernel Plane

The kernel plane is a per-VM Rust data plane.

Responsibilities:

- VFS state
- FD table
- process table
- permissions and capabilities
- device layer
- pipes and PTY state
- command registry
- runtime accounting and quotas
- filesystem persistence state

The kernel is the source of truth for VM state. It is not V8-specific and is not implemented as a JavaScript shim.

### Execution Plane

The execution plane runs guest JavaScript and guest WebAssembly.

Responsibilities:

- V8 isolate lifecycle
- guest code bootstrap
- stdout/stderr/stream handling
- timing mitigation
- snapshot pool management
- translating guest requests into kernel bridge calls

Execution is not the owner of VM state. Snapshot caches may be shared across VMs in a sidecar, but per-VM kernel state may not.

Guest-visible synchronous semantics are part of the compatibility surface. If guest code currently experiences filesystem access, module loading, process control, or similar operations as synchronous or sync-looking, the migration must preserve that observable behavior unless the public runtime contract is explicitly changed.

## Rust Workspace Structure

The runtime stack is reorganized around four Rust crates:

```text
crates/
  kernel/
  execution/
  sidecar/
  sidecar-browser/
```

Recommended Cargo package and binary naming:

- `crates/kernel` -> Cargo package `agent-os-kernel`
- `crates/execution` -> Cargo package `agent-os-execution`
- `crates/sidecar` -> Cargo package `agent-os-sidecar`, binary `agent-os-sidecar`
- `crates/sidecar-browser` -> Cargo package `agent-os-sidecar-browser`

The directory names stay short. The package and binary names stay branded.

## JavaScript Package Surface

The public npm surface should stay small:

- `@rivet-dev/agent-os`
- `@rivet-dev/agent-os-shell`
- `@rivet-dev/agent-os-registry-types`
- existing registry packages

Optional:

- `@rivet-dev/agent-os-browser` as a thin loader/wrapper if packaging the browser sidecar separately is still useful

Not public:

- standalone kernel package
- standalone execution package
- standalone sidecar-management package
- Python runtime package
- POSIX runtime package

## Intentional Breaking Changes

The migration is allowed to make the following deliberate public-surface changes:

- remove direct public exposure of a live mutable `AgentOs.kernel`
- remove Python runtime support from the consolidated runtime stack
- add explicit sidecar placement APIs to the host SDK

For `AgentOs.kernel` specifically:

- current tests and helpers that reach into `vm.kernel` are treated as migration work, not preserved API contract
- host-facing capabilities such as exec, spawn, mounts, filesystem snapshots, sessions, and diagnostics must remain available through explicit SDK methods or a dedicated admin client
- any temporary compatibility wrapper around `vm.kernel` is migration-only and must be deleted before completion

## Per-Crate Responsibilities

### `kernel`

Owns:

- VM model and identifiers
- VFS
- file descriptors
- process table
- devices
- pipes
- PTY state
- permissions
- command registry
- persistence state
- resource accounting

Does not own:

- V8 isolate creation
- snapshot pool
- worker creation
- wire transport
- JS SDK APIs

### `execution`

Owns the native execution implementation:

- V8 platform setup
- isolate lifecycle
- guest Wasm through V8
- bootstrap code and hardening
- timing mitigation
- shared snapshot pool
- native guest dispatch loop

Does not own:

- VM filesystem state
- permissions
- command registry
- host configuration

### `sidecar`

Owns the native sidecar process:

- protocol server
- VM registry
- composition of `kernel` and `execution`
- host callback dispatch for filesystem drivers, permissions, and persistence
- session and stream lifecycle
- process-wide shared snapshot pool

### `sidecar-browser`

Owns the browser mirror of the sidecar model:

- kernel on the main thread
- browser-side protocol adapter
- worker creation on the main thread
- browser implementation of host and execution bridge traits
- coordinating guest workers
- main-thread bookkeeping for hard worker termination and cleanup

For the initial implementation:

- the browser sidecar stays on the main thread
- the kernel stays on the main thread
- only guest execution runs in workers
- parity-sensitive guest operations must preserve the current sync-looking guest ABI
- `postMessage` alone is not sufficient for sync-looking guest operations such as filesystem access and module loading
- the browser bridge must therefore include a blocking request/reply path, such as `SharedArrayBuffer` plus `Atomics`, for operations that cannot be degraded to async without an explicit ABI change
- if the browser environment cannot provide the required blocking bridge primitives, full-parity sidecar-browser mode is unsupported in that environment

This is required because changing guest-visible sync behavior is not an implementation detail in this system. It is a compatibility break against the current POSIX-like runtime contract.

## Bridge Design

The kernel-facing bridge uses explicit trait methods, not op enums.

Recommended bridge split:

- `FilesystemBridge`
- `PermissionBridge`
- `PersistenceBridge`
- `ClockBridge`
- `RandomBridge`
- `EventBridge`
- `ExecutionBridge`

These may be composed into a higher-level host bridge type, but the leaf interfaces should stay method-oriented.

### `FilesystemBridge`

Responsibilities:

- `read_file`
- `write_file`
- `stat`
- `lstat`
- `read_dir`
- `create_dir`
- `remove_file`
- `remove_dir`
- `rename`
- `symlink`
- `read_link`
- `chmod`
- `truncate`
- `exists`
- optional block-store or chunk-store methods where needed for mounted backends

This is where host-provided JS-backed drivers such as S3, Google Drive, OPFS-backed mounts, and custom VFS adapters are called from native sidecars.

### `PermissionBridge`

Responsibilities:

- resolve host permission policy callbacks
- return allow, deny, or prompt decisions
- enforce VM-scoped capability decisions

### `PersistenceBridge`

Responsibilities:

- load filesystem snapshot/state for a VM
- flush filesystem snapshot/state for a VM

Filesystem persistence stays the only required persisted runtime state in this migration.

### `ClockBridge` And `RandomBridge`

Responsibilities:

- wall clock
- monotonic clock
- timer scheduling hooks if needed at the host boundary
- random byte fill

### `EventBridge`

Responsibilities:

- send structured events from sidecar to host
- diagnostics
- logs
- lifecycle updates

### `ExecutionBridge`

Responsibilities:

- create guest JS execution contexts
- create guest Wasm execution contexts
- stream stdin/stdout/stderr
- kill guest execution
- dispatch guest-to-kernel requests
- deliver async execution events back to the kernel and host

The browser and native implementations differ primarily here.

For browser execution specifically:

- guest termination is defined as hard worker termination plus deterministic main-thread cleanup
- cleanup must clear pending bridge calls, process bookkeeping, and resource accounting for the killed guest
- worker control channels are part of the sandbox boundary and must be hardened against guest access or forged control traffic

## Host Configuration Model

The host configuration model stays host-owned and roughly preserves today’s `AgentOsOptions`.

The migration must preserve:

- `mounts` for filesystem backends
- `software` for package injection
- root filesystem configuration
- `moduleAccessCwd`
- permission configuration
- `scheduleDriver`
- toolkit injection
- OS instruction injection
- loopback exemptions and equivalent host runtime knobs

The migration also adds or formalizes:

- `createSidecar()` or equivalent explicit sidecar handle creation
- passing a sidecar handle into Agent OS construction
- default shared sidecar reuse

This is an intentional split between:

- existing Agent OS config capabilities that must remain available
- new sidecar-placement capabilities that are being added during consolidation

Specific handling notes:

- `rootFilesystem` is preserved as a first-class VM bootstrap input and must be represented explicitly in the sidecar bootstrap protocol
- `moduleAccessCwd` remains a host-facing option even if the host resolves it into package-projection descriptors before crossing the protocol boundary
- `toolKits` remain host-owned; the host may still derive shims, env vars, or RPC ports that are then provided to the sidecar
- `scheduleDriver` remains host-owned and does not have to become a sidecar protocol concept if its behavior remains host-controlled

The intent is to preserve operational behavior, not the exact current class graph.

## Filesystem Drivers And Software Injection

### Filesystem Drivers

The host remains the owner of the mount graph definition.

The sidecar owns mounted VM state, but host-provided drivers still need to work:

- in-memory filesystem drivers
- caller-provided custom drivers
- host directory mounts
- overlay and copy-on-write configurations
- object-storage-backed drivers such as S3
- browser storage backends

This means the native sidecar protocol must support sidecar-to-host filesystem callbacks for JS-defined or JS-owned mounts.

### Software Injection

The host continues to pass `software` inputs.

The current behaviors to preserve are:

- command directories from software packages
- projected npm package roots
- agent metadata and registrations
- registry package inputs
- meta-packages that expand into multiple descriptors

The internal implementation may stop using the current package plumbing, but the behavior must remain available.

## Sidecar Protocol

The protocol is Agent OS-owned and is redesigned cleanly. Legacy compatibility is not required.

### Design Principles

- bidirectional
- VM-scoped
- stream-aware
- typed and versioned
- transport-independent at the schema level
- minimal authority in the host-facing API

### Required Message Categories

Host to sidecar:

- create sidecar session or connect
- create VM
- dispose VM
- provide root filesystem bootstrap configuration
- execute or spawn guest work
- write stdin
- close stdin
- kill guest process
- configure mounts, software, permissions, and instructions
- provide resolved package/module projection descriptors where needed
- request diagnostics or metrics

Sidecar to host:

- filesystem driver calls for host-owned mounts
- permission requests
- persistence load and flush
- structured events
- diagnostics

Execution to sidecar, internal:

- guest lifecycle events
- stdout
- stderr
- exit
- async callbacks
- kernel request dispatch

### Transport

Native:

- Unix domain socket or named-pipe style transport
- framed binary protocol
- single clean Agent OS message schema

Browser:

- same logical API surface
- main-thread sidecar implementation
- no separate OS process
- async control/event coordination over `postMessage`
- a separate blocking request/reply bridge for sync-looking guest operations when parity requires it

The host-facing schema should remain conceptually the same even if the browser path uses direct calls or in-memory dispatch instead of a real socket.

### Required Protocol Invariants

The protocol redesign must still preserve the invariants that make shared sidecars safe:

- authenticated connection setup for native sidecars
- binding of sessions/VMs to the connection or client context that created them
- strict request/response correlation and duplicate-response hardening
- explicit payload and frame size limits
- versioned message schemas
- deterministic cleanup of connection-owned sessions when a client disconnects

For browser sidecars, the transport may differ, but the logical ownership and integrity invariants must remain equivalent.

## Warm Pool And Snapshot Model

The warm pool is a universal concept across kernels inside a sidecar process, but the implementation is not identical across native and browser targets.

That means:

- bootstrap and bridge warm-up data may be shared
- native isolate templates and V8 snapshots may be shared
- timing-mitigation-related bootstrap state may be shared if safe

For native sidecars:

- V8 startup snapshots are the preferred warm-path optimization

For browser sidecars:

- the equivalent concept is a warm bootstrap/module cache, not a literal native-style V8 startup snapshot
- the spec does not assume browser support for native snapshot machinery

That does not mean:

- filesystem state is shared
- FD tables are shared
- permissions are shared
- VM runtime state is snapshotted

Per-VM kernel state must always be created separately.

## Timing Mitigation

Timing mitigation is a hard parity requirement.

It must apply consistently to:

- guest JavaScript
- guest WebAssembly through V8
- native sidecar
- browser sidecar

It must be tested explicitly, not assumed.

At minimum the spec assumes:

- no JS-only shortcut
- no Wasm exemption
- no browser exemption

Browser parity here means the browser implementation must either:

- implement equivalent mitigation behavior for both guest JS and guest Wasm, or
- fail closed and reject the unsupported configuration rather than silently weakening the contract

The same fail-closed rule applies to guest-visible synchronous behavior: if the browser target cannot preserve the required sync-looking guest semantics for a supported feature, that feature must be rejected rather than silently degraded.

## Repo Reorganization

## Target Structure

```text
packages/
  core/            -> publish as @rivet-dev/agent-os
  shell/           -> publish as @rivet-dev/agent-os-shell
  registry-types/  -> publish as @rivet-dev/agent-os-registry-types
  browser/         -> optional thin browser wrapper if needed
  playground/
  dev-shell/

crates/
  kernel/
  execution/
  sidecar/
  sidecar-browser/

registry/
  agent/
  file-system/
  native/
  software/
  tool/
```

## Source Mapping

Legacy source inputs map roughly as follows:

- legacy TS kernel -> design and behavior input for `crates/kernel`
- legacy V8 JS wrapper -> `packages/core/src/sidecar/` host client and tests
- legacy Rust V8 runtime -> `crates/sidecar`
- legacy node bridge/bootstrap/polyfill logic -> `crates/execution` and sidecar bootstrap assets
- current `packages/posix` behavior -> folded into sidecar plus `registry/native`, not preserved as a public package
- current browser worker/runtime code -> `crates/sidecar-browser` plus optional thin JS loader

Temporary staging code is acceptable only during migration. Final state deletes it.

## Testing Strategy

Testing is part of the migration plan, not a cleanup step.

## Test Layers

### 1. Rust Unit Tests

Each kernel subsystem gets scoped unit and integration tests in Rust as it is ported.

Required test files at minimum:

- `crates/kernel/tests/vfs.rs`
- `crates/kernel/tests/fd_table.rs`
- `crates/kernel/tests/process_table.rs`
- `crates/kernel/tests/device_layer.rs`
- `crates/kernel/tests/pipe_manager.rs`
- `crates/kernel/tests/permissions.rs`
- `crates/kernel/tests/command_registry.rs`
- `crates/kernel/tests/pty.rs`
- `crates/kernel/tests/user.rs`
- `crates/kernel/tests/resource_accounting.rs`
- `crates/kernel/tests/kernel_integration.rs`

### 2. Rust Sidecar And Execution Tests

Required native-side tests:

- protocol framing
- request/response round-trip
- session isolation
- process isolation
- crash containment
- snapshot warmup and invalidation
- timing mitigation for JS
- timing mitigation for Wasm
- stream dispatch
- bridge hardening
- payload and frame size limits
- env isolation
- SSRF and network policy enforcement
- resource budgets
- sandbox escape resistance

Recommended files:

- `crates/sidecar/tests/protocol_codec.rs`
- `crates/sidecar/tests/protocol_roundtrip.rs`
- `crates/sidecar/tests/session_isolation.rs`
- `crates/sidecar/tests/process_isolation.rs`
- `crates/sidecar/tests/crash_isolation.rs`
- `crates/sidecar/tests/snapshot_behavior.rs`
- `crates/sidecar/tests/timing_mitigation_js.rs`
- `crates/sidecar/tests/timing_mitigation_wasm.rs`

### 3. JavaScript SDK Contract Tests

These keep the public Agent OS surface honest while internals move.

They must cover:

- config acceptance
- default shared sidecar
- explicit sidecar handle injection
- mounts
- software injection
- session lifecycle
- filesystem APIs
- process management APIs

The existing `packages/core/tests` suite is the starting point, not optional work.

This parity layer must also include the current higher-level JS integration surfaces that depend on the runtime:

- `packages/dev-shell/test`
- `registry/tool/sandbox/tests`

### 4. Browser Contract Tests

Required:

- browser sidecar lifecycle
- worker creation
- guest JS execution
- guest Wasm execution
- sync-looking bridge behavior for filesystem and module loading
- timing mitigation parity
- filesystem driver behavior
- permission validation
- control-channel hardening
- worker termination cleanup semantics

### 5. Registry And Acceptance Tests

The final parity gate is still the higher-level behavior suite:

- `registry/tests/kernel`
- `registry/tests/wasmvm`
- selected `packages/core/tests`

This ensures the migration preserves feature parity instead of only passing new crate-local tests.

## Legacy Test Port Map

The old tests should be ported or re-expressed, not silently dropped.

Kernel:

- `command-registry.test.ts` -> `crates/kernel/tests/command_registry.rs`
- `fd-table.test.ts` -> `crates/kernel/tests/fd_table.rs`
- `device-layer.test.ts` -> `crates/kernel/tests/device_layer.rs`
- `process-table.test.ts` -> `crates/kernel/tests/process_table.rs`
- `pipe-manager.test.ts` -> `crates/kernel/tests/pipe_manager.rs`
- `cross-pid-auth.test.ts` -> `crates/kernel/tests/permissions.rs`
- `shell-terminal.test.ts` -> `crates/kernel/tests/pty.rs`
- `resource-exhaustion.test.ts` -> `crates/kernel/tests/resource_accounting.rs`
- `kernel-integration.test.ts` -> `crates/kernel/tests/kernel_integration.rs`

Sidecar:

- `ipc-binary.test.ts` -> protocol codec tests
- `ipc-roundtrip.test.ts` -> protocol round-trip tests
- `ipc-security.test.ts` -> auth and session isolation tests
- `process-isolation.test.ts` -> sidecar process isolation tests
- `crash-isolation.test.ts` -> crash containment tests
- `context-snapshot-behavior.test.ts` and `snapshot-security.test.ts` -> snapshot behavior tests
- runtime-driver/node `bridge-hardening`, `payload-limits`, `env-leakage`, `ssrf-protection`, `resource-budgets`, and `sandbox-escape` suites -> native sidecar cutover gates

## Migration Progress

- [ ] Phase 1: Internalize legacy runtime source into this repo
- [ ] Phase 2: Migrate the acceptance harness and remove Python from active surfaces
- [ ] Phase 3: Rename and re-own the imported code under Agent OS naming
- [ ] Phase 4: Scaffold the Rust crates and new protocol without changing host semantics
- [ ] Phase 5: Port kernel subsystem 1 with scoped Rust tests
- [ ] Phase 6: Port kernel subsystem 2 with scoped Rust tests
- [ ] Phase 7: Bring up the native sidecar on the new kernel with full bridge/security gates
- [ ] Phase 8: Cut Agent OS host runtime paths over to the new sidecar with command/package parity
- [ ] Phase 9: Bring up the browser sidecar model with parity-safe bridge behavior
- [ ] Phase 10: Delete all legacy code and make parity the only acceptance bar

## Migration Plan

### Phase 1: Internalize Legacy Runtime Source Into This Repo

Objective:

- remove the external dependency on the old repo as a source of truth
- make this repo the only place where runtime work happens

Work:

- copy the needed legacy runtime source into this repo
- prefer copying directly into final target paths where practical
- if temporary staging paths are used, keep them clearly marked as migration-only
- bring over the legacy kernel tests and sidecar tests so they run from this repo
- keep current Agent OS behavior unchanged

Definition of done:

- this repo contains the source currently being pulled from Secure-Exec
- current Agent OS still works
- imported kernel tests run locally from this repo
- imported sidecar tests run locally from this repo

Gating tests:

- current `packages/core` test suite
- imported legacy kernel test suite
- imported legacy sidecar test suite
- selected registry smoke tests

Notes:

- this is the only phase where a temporary staging area for imported legacy code is acceptable
- that staging area must be deleted by the end of the migration

### Phase 2: Migrate The Acceptance Harness And Remove Python From Active Surfaces

Objective:

- keep the real parity bar alive during migration
- remove Python from the active runtime path before later parity gates are defined

Work:

- port `registry/tests/helpers.ts` to Agent OS-owned runtime helpers
- move `TerminalHarness` and similar test-only fixtures into Agent OS-owned test utilities
- remove imports from external legacy repo paths in registry and package tests
- remove Python from `AgentOs.create()`, dev-shell bootstrapping, and other active runtime entrypoints
- remove Python runtime packages, dev dependencies, and parity suites from the in-scope migration surface
- define the post-Python parity matrix explicitly so later “full pass” gates are meaningful

Definition of done:

- the acceptance harness no longer depends on `@secure-exec/*` re-exports or external legacy repo paths
- Python is no longer part of the active Agent OS runtime boot path
- Python-specific tests are either deleted or explicitly marked out of scope from the new parity bar
- the remaining in-scope parity harness still runs

Gating tests:

- selected `registry/tests/kernel` suites running through Agent OS-owned helpers
- selected `registry/tests/wasmvm` suites running through Agent OS-owned helpers
- `packages/core/tests` excluding intentionally removed Python surfaces
- `packages/dev-shell/test` without Python in the active runtime path

### Phase 3: Rename And Re-Own The Imported Code

Objective:

- remove legacy naming early so the rest of the migration builds on the right identity

Work:

- rename packages, crates, binaries, env vars, and JS APIs to Agent OS names
- rename `@rivet-dev/agent-os-core` to `@rivet-dev/agent-os`
- rename the Rust runtime binary to `agent-os-sidecar`
- replace obvious `secure-exec` references in workspace manifests and public docs

Definition of done:

- no new code uses legacy names
- public package names and runtime binaries use Agent OS naming

Gating tests:

- workspace build
- host SDK tests
- sidecar smoke tests
- acceptance-harness smoke tests

### Phase 4: Scaffold New Rust Crates And The New Protocol

Objective:

- establish the final crate boundaries before porting behavior into them

Work:

- add `crates/kernel`
- add `crates/execution`
- add `crates/sidecar`
- add `crates/sidecar-browser`
- define the new Agent OS-owned protocol schema
- add explicit bridge trait skeletons
- add JS host-side sidecar client code in `packages/core`

Definition of done:

- crates exist in the workspace
- protocol types compile
- bridge traits compile
- host-side sidecar client compiles

Gating tests:

- `cargo test` for protocol codec and crate smoke tests
- JS SDK typecheck and smoke tests

### Phase 5: Port Kernel Subsystem Group 1

Objective:

- start the real kernel port with small, bounded modules first

Port in this phase:

- `vfs`
- `fd-table`
- `command-registry`
- `user`

Work:

- port behavior to Rust
- add scoped Rust tests first
- keep the new Rust kernel running only in crate-local tests until behavior is stable

Definition of done:

- Rust versions of these modules pass their own tests
- ported behavior is validated against the old TS test expectations

Gating tests:

- `crates/kernel/tests/vfs.rs`
- `crates/kernel/tests/fd_table.rs`
- `crates/kernel/tests/command_registry.rs`
- `crates/kernel/tests/user.rs`

### Phase 6: Port Kernel Subsystem Group 2

Objective:

- finish the kernel port before switching production paths

Port in this phase:

- `process-table`
- `device-layer`
- `pipe-manager`
- `permissions`
- `pty`
- kernel integration and resource accounting

Definition of done:

- all major kernel subsystems exist in Rust
- all subsystem tests pass
- the Rust kernel can support the minimal VM lifecycle in integration tests

Gating tests:

- `crates/kernel/tests/process_table.rs`
- `crates/kernel/tests/device_layer.rs`
- `crates/kernel/tests/pipe_manager.rs`
- `crates/kernel/tests/permissions.rs`
- `crates/kernel/tests/pty.rs`
- `crates/kernel/tests/resource_accounting.rs`
- `crates/kernel/tests/kernel_integration.rs`

### Phase 7: Bring Up The Native Sidecar On The New Kernel

Objective:

- run native guest execution against the Rust kernel inside the new sidecar

Work:

- port the relevant native bridge/bootstrap logic into `execution`
- connect `sidecar` to `kernel` plus `execution`
- implement host callback dispatch for filesystem drivers, permissions, and persistence
- implement shared snapshot pool
- implement timing mitigation for JS and Wasm
- preserve bridge hardening, payload limits, env isolation, SSRF policy enforcement, and resource-budget behavior

Definition of done:

- native sidecar can create a VM
- native sidecar can execute guest JS
- guest Wasm runs through V8
- timing mitigation tests pass for both
- security and hardening behavior from the old runtime-driver suite is restored on the new path

Gating tests:

- protocol round-trip
- crash isolation
- process isolation
- snapshot behavior
- timing mitigation JS
- timing mitigation Wasm
- bridge hardening
- payload and frame size limits
- env isolation
- SSRF and network policy tests
- resource budget tests
- sandbox escape tests

### Phase 8: Cut Agent OS Host Runtime Paths Over To The New Sidecar With Command/Package Parity

Objective:

- make Agent OS use its own sidecar instead of Secure-Exec code paths without losing command or package behavior in the process

Work:

- replace direct `@secure-exec/*` runtime imports from `packages/core`
- preserve the host config model
- implement default shared sidecar management
- implement explicit sidecar handle injection
- replace direct `vm.kernel` usage in tests/helpers with explicit host or admin APIs
- preserve command discovery and execution in the same phase as the host cutover
- preserve projected package roots and registry software behavior in the same phase as the host cutover
- keep current Agent OS APIs working wherever they remain in scope

Definition of done:

- `packages/core` no longer depends on `@secure-exec/core`, `@secure-exec/nodejs`, or `@secure-exec/v8`
- `packages/core` uses the new Agent OS sidecar client
- current host-facing config still works
- current command availability and software/package behavior also work on the new path
- direct public `AgentOs.kernel` exposure is removed or clearly marked migration-only

Gating tests:

- `packages/core/tests`
- `registry/tests/kernel`
- `registry/tests/wasmvm`
- `packages/dev-shell/test`
- `registry/tool/sandbox/tests`
- command-specific smoke suites

### Phase 9: Bring Up The Browser Sidecar

Objective:

- mirror the sidecar model in the browser without inventing a second architecture

Work:

- compile the Rust kernel for browser use
- run `sidecar-browser` on the main thread
- create guest workers from the main thread
- implement browser bridge traits
- implement a parity-safe blocking bridge path for sync-looking guest operations such as filesystem access and module loading
- preserve control-channel hardening so guest code cannot hijack or forge worker control traffic
- define browser kill semantics as hard worker termination plus deterministic main-thread cleanup
- preserve filesystem-driver behavior and timing mitigation parity, or fail closed where parity is not supportable

Definition of done:

- browser runtime uses the new sidecar-browser model
- browser preserves the current sync-looking guest ABI for the supported surface
- browser timing-mitigation behavior matches the required contract
- browser guest Wasm and guest JS both work through the new model

Gating tests:

- browser runtime contract tests
- browser sync-bridge and module-loading tests
- browser timing mitigation tests
- browser filesystem and permission tests
- browser control-channel hardening tests
- browser termination-cleanup tests

### Phase 10: Delete Legacy Code And Make Parity The Only Bar

Objective:

- finish the migration cleanly

Work:

- delete temporary staging code
- delete legacy TS runtime code that no longer participates in the final architecture
- delete Python runtime code that is out of scope
- delete old wrappers and dead compatibility layers
- remove remaining `secure-exec` references across code, docs, env vars, package names, and examples

Definition of done:

- zero runtime-critical legacy code remains
- zero `@secure-exec/*` dependencies remain
- zero `secure-exec` runtime names remain
- final parity suite passes

Gating tests:

- full post-Python workspace test pass
- full registry parity test pass through the Agent OS-owned harness
- sidecar test pass
- browser test pass

## Working-Increment Rule

Each phase should leave the repo in one of two acceptable states:

- shipping state
- clearly bounded migration branch state with local parity tests already green

Do not merge a phase that only compiles in the abstract but breaks the usable runtime.

Whenever possible:

- import first
- add tests first
- cut over behind existing host APIs
- delete old code only after the new path is green

## Definition Of Done

The migration is done only when all of the following are true:

- Agent OS owns the runtime stack directly
- the kernel is a shared Rust library used by native and browser sidecars
- native execution uses the new sidecar
- browser execution uses the new sidecar-browser model
- current host config capabilities still exist
- new sidecar-placement capabilities exist on the host side
- filesystem drivers and package injection still work
- timing mitigation works for guest JS and guest Wasm
- registry acceptance tests pass on the new runtime path through Agent OS-owned harness code
- direct public `AgentOs.kernel` exposure is gone
- legacy code and naming are deleted

Anything short of that is still migration-in-progress.
