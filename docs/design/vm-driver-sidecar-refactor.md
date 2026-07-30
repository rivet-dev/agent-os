# VM, Driver, and Sidecar Package Refactor

Status: draft; initial naming and dependency decisions locked

Audience: agentOS VM, sidecar, kernel, executor, client, build, and publishing
owners

## 1. Locked package and crate names

Active Rust crates remain physically flat under `crates/`. The `vm-` and
`executor-` prefixes group related packages by name; they do not introduce
nested Cargo workspaces or category directories.

```text
crates/<role>/Cargo.toml
    -> package agentos-<role>
    -> Rust crate agentos_<role>
```

The following renames are in scope:

| Current directory | Current package | Target directory | Target package |
|---|---|---|---|
| `crates/native-sidecar` | `agentos-native-sidecar` | `crates/vm` | `agentos-vm` |
| `crates/kernel` | `agentos-kernel` | `crates/vm-kernel` | `agentos-vm-kernel` |
| `crates/runtime-tokio` | `agentos-runtime-tokio` | `crates/driver-tokio` | `agentos-driver-tokio` |
| `crates/v8-runtime` | `agentos-v8-runtime` | `crates/executor-v8-runtime` | `agentos-executor-v8-runtime` |
| `crates/executor-wasm-common` | `agentos-executor-wasm-common` | `crates/executor-wasm-abi` | `agentos-executor-wasm-abi` |
| `crates/wasm-abi-generator` | `agentos-wasm-abi-generator` | `crates/executor-wasm-abi-generator` | `agentos-executor-wasm-abi-generator` |
| `crates/host-bridge` | `agentos-host-bridge` | `crates/vm-host-interface` | `agentos-vm-host-interface` |
| `crates/native-baseline` | `agentos-native-baseline` | `crates/benchmark-baseline` | `agentos-benchmark-baseline` |

The following names remain unchanged:

- `agentos-sidecar`
- `agentos-sidecar-client`
- `agentos-sidecar-protocol`
- `agentos-vm-config`
- `agentos-vfs-core`
- `agentos-vfs-storage`
- `agentos-resource-accounting`
- `agentos-executor-contract`
- the concrete executor packages
- `agentos-acp-protocol`
- `agentos-rivetkit-ars-client`

No `agentos-host-runtime`, `agentos-host-contract`, or other intermediate
runtime-contract crate is introduced by this refactor. `agentos-driver-tokio`
is the concrete process-owned Tokio driver used by the native agentOS VM.

`agentos-vm-host-interface` is not a sidecar transport. It is the in-process
interface through which the VM requests trusted host facilities. The actual
process boundary remains `agentos-sidecar-protocol` and
`agentos-sidecar-client`.

`agentos-benchmark-baseline` is development-only and remains unpublished. It
provides host and WASM baseline measurements for the differential benchmark
matrix; it is not part of the production dependency graph.

## 2. Target process dependency tree

There is one actual sidecar executable: `agentos-sidecar`.

```text
client process
└── agentos-client
    ├── agentos-sidecar-client
    │   └── agentos-sidecar-protocol
    ├── agentos-vm-config
    └── agentos-acp-protocol

                     framed sidecar protocol
                              │
                              ▼

sidecar process
└── agentos-sidecar                         binary and composition root
    ├── agentos-sidecar-protocol
    ├── agentos-driver-tokio                constructs the one Tokio driver
    │   └── agentos-resource-accounting
    ├── agentos-vm                          VM orchestration library
    │   ├── agentos-driver-tokio            consumes an injected DriverHandle
    │   ├── agentos-vm-config
    │   ├── agentos-vm-kernel
    │   │   ├── agentos-vm-host-interface
    │   │   ├── agentos-resource-accounting
    │   │   └── agentos-vfs-core
    │   ├── agentos-vfs-storage
    │   │   └── agentos-vfs-core
    │   ├── agentos-rivetkit-ars-client
    │   └── agentos-executor-contract
    ├── optional concrete executors
    │   ├── agentos-executor-node-v8
    │   │   ├── agentos-executor-contract
    │   │   └── agentos-executor-v8-runtime
    │   ├── agentos-executor-python-v8-pyodide
    │   │   ├── agentos-executor-contract
    │   │   └── agentos-executor-v8-runtime
    │   ├── agentos-executor-wasm-v8
    │   │   ├── agentos-executor-contract
    │   │   ├── agentos-executor-v8-runtime
    │   │   └── agentos-executor-wasm-abi
    │   └── agentos-executor-wasm-wasmtime
    │       ├── agentos-executor-contract
    │       └── agentos-executor-wasm-abi
    ├── extension registry
    ├── ACP extension
    │   └── agentos-acp-protocol
    └── stdio, fd 3, framing, and connection routing
```

`agentos-sidecar` selects the concrete executor packages through Cargo
features, constructs their registry, and passes that registry into
`agentos-vm`. `agentos-vm` consumes `agentos-executor-contract`; it does not
contain V8, Wasmtime, Pyodide, or Node implementation code.

## 3. Package responsibilities

### 3.1 `agentos-sidecar`

`agentos-sidecar` is the only sidecar process and the native composition
root. It owns:

- the executable entrypoint;
- stdin, stdout, and the inherited fd 3 control lane;
- sidecar wire framing and request routing;
- connection authentication and connection/session ownership;
- extension registration and extension request routing;
- ACP and agent-session orchestration;
- construction of `agentos-driver-tokio`;
- selection and registration of enabled executors; and
- mapping sidecar requests onto the public `agentos-vm` API.

The sidecar does not implement guest Linux semantics, filesystem semantics, or
engine-specific execution.

### 3.2 `agentos-vm`

`agentos-vm` is a library, not a sidecar and not a binary. It owns:

- the active VM registry;
- VM creation, configuration, lookup, and disposal;
- per-VM generation and lifecycle state;
- composition of the kernel, VFS, storage, and executor contract;
- per-VM resource scopes and limits;
- execution coordination and executor dispatch;
- process start, output, exit, signal, and cleanup coordination;
- mounts, packages, layers, overlays, and snapshots;
- VM SQLite handle resolution; and
- public in-process operations used by the sidecar and tests.

Its primary public vocabulary should be `VmManager`, `Vm`, `VmHandle`,
`VmConfig`, `VmId`, `VmGeneration`, `VmEvent`, and `VmError`.

`agentos-vm` does not own:

- sidecar framing or transports;
- host connection/session authentication;
- ACP, agents, prompts, or durable agent sessions;
- the extension registry;
- construction of a Tokio runtime;
- guest syscall implementations;
- V8, Wasmtime, Pyodide, or Node engine internals; or
- concrete VFS backend implementations.

### 3.2.1 Direct embedded VM usage

The Rust architecture must support agentOS as an embeddable virtual OS library
with no sidecar process, client transport, or execution engine.

There are two supported levels:

- `agentos-vm-kernel` provides the virtual OS primitives directly: VFS,
  processes, descriptors, signals, permissions, mounts, sockets, and related
  Linux semantics. It has no Tokio or concrete executor dependency.
- `agentos-vm` with `default-features = false` provides VM lifecycle,
  configuration, storage, mounts, snapshots, and kernel composition without
  Node/V8, Python/Pyodide, WASM/V8, or Wasmtime.

The package must be usable directly:

```toml
[dependencies]
agentos-vm = {
  version = "0.0.1",
  default-features = false,
}
```

```rust
use agentos_vm::{ExecutorRegistry, VmManager};

let vms = VmManager::builder()
    .driver(driver_handle)
    .executors(ExecutorRegistry::empty())
    .build()?;

let vm = vms.create(vm_config).await?;
vm.write_file("/workspace/hello.txt", b"hello").await?;
assert_eq!(vm.read_file("/workspace/hello.txt").await?, b"hello");
```

The checked `embedded_os` example must compile against the public API and run
with `agentos-vm`'s default feature set disabled.
With an empty executor registry, filesystem, process-table, mount, snapshot,
permission, and other OS operations continue to work. A request that actually
needs a language engine fails with a stable typed
`ERR_AGENTOS_EXECUTOR_UNAVAILABLE` error naming the requested runtime. It must
not panic, silently select an engine, or require a sidecar connection.

`agentos-vm` therefore defaults to no executors and accepts an injected
executor registry instead of depending on concrete engines. The
`agentos-sidecar` default feature set enables the standard engines, while each
individual sidecar feature selects exactly its corresponding executor crate.
TypeScript remains native-backed through `@rivet-dev/agentos-core`; this
requirement does not add an in-process TypeScript virtual OS implementation.

### 3.3 `agentos-vm-host-interface`

`agentos-vm-host-interface` is a runtime-neutral, transport-neutral Rust
interface. It owns the request and response types and traits for trusted host
facilities used by the VM:

- filesystem operations;
- permission decisions;
- persistence and snapshot storage;
- clocks and randomness;
- structured host events; and
- execution-engine context and lifecycle operations.

It contains no stdio, fd 3, BARE framing, sidecar connection state, Tokio
runtime construction, V8, or Wasmtime implementation. Both local native
implementations and tests may implement the interface directly.

The primary public traits become:

| Current | Target |
|---|---|
| `HostBridge` | `VmHost` |
| `BridgeTypes` | `VmHostTypes` |
| `FilesystemBridge` | `HostFilesystem` |
| `PermissionBridge` | `HostPermissions` |
| `PersistenceBridge` | `HostPersistence` |
| `ClockBridge` | `HostClock` |
| `RandomBridge` | `HostRandom` |
| `EventBridge` | `HostEvents` |
| `ExecutionBridge` | `HostExecution` |

### 3.4 `agentos-resource-accounting`

`agentos-resource-accounting` remains a runtime-neutral leaf crate. It owns
bounded admission, hierarchical resource ledgers, RAII reservations, queue
tracking, warning thresholds, telemetry observations, and typed limit errors.
It does not own policy defaults or VM lifecycle.

The name remains unchanged. `resource-limits` would omit its reservation and
telemetry responsibilities, while `vm-resource-accounting` would incorrectly
make a process- and executor-wide facility appear VM-specific.

### 3.5 `agentos-driver-tokio`

`agentos-driver-tokio` is the single process-owned trusted work driver. It owns:

- construction and lifetime of the one Tokio runtime;
- its fixed Tokio worker census;
- bounded trusted task spawning and supervision;
- the fixed bounded blocking executor;
- cancellation and shutdown;
- timers and async wakeups;
- native async I/O facilities used by trusted code; and
- cloneable process and VM-scoped driver handles.

It does not own:

- sidecar protocol framing or protocol queue configuration;
- VM identity, VM lifecycle, or executor selection;
- guest process, fd, socket, signal, or filesystem state;
- ACP or extensions; or
- execution of synchronous untrusted guest code on Tokio workers.

The sidecar constructs the driver once and injects its handle into the VM
manager:

```rust
let driver = TokioDriver::new(driver_config)?;

let vms = VmManager::builder()
    .driver(driver.handle())
    .executors(executor_registry)
    .storage(storage_registry)
    .build()?;

let sidecar = Sidecar::new(vms).with_extension(AcpExtension::new());
```

Each VM receives a generation-bound scoped handle derived from the process
driver. No VM, executor, VFS backend, extension, or sidecar subsystem may
construct another Tokio runtime.

### 3.6 `agentos-benchmark-baseline`

`agentos-benchmark-baseline` is an unpublished benchmark binary. It measures
the direct host floor for process, filesystem, DNS, TCP, Unix socket, UDP,
HTTP, pipe, CPU, timer, and allocation operations. Its `wasm32-wasip1` build
measures the supported subset through the VM WASM lane.

The benchmark harness, not this binary, computes aggregate statistics and the
agentOS emulation tax. No production crate or package may depend on it.

## 4. Remove the second sidecar vocabulary

The repository currently exposes both `agentos-native-sidecar` and
`agentos-sidecar` as if they were separate sidecar products. The target has
one process boundary and one sidecar binary.

Remove:

- the `agentos-native-sidecar` Cargo package name;
- the `agentos-native-sidecar` executable;
- the `agentos-native-sidecar` protocol schema name;
- `AGENTOS_NATIVE_SIDECAR_BIN`;
- the `@rivet-dev/agentos-runtime-sidecar` binary resolver;
- native-sidecar-specific release artifacts and platform packages;
- native-sidecar-specific benchmark configuration; and
- tests and documentation that describe a second sidecar product.

Keep:

- the `agentos-sidecar` executable;
- the `agentos-sidecar` protocol schema name;
- `AGENTOS_SIDECAR_BIN`;
- the `@rivet-dev/agentos-sidecar` binary resolver and platform packages; and
- one set of sidecar build, smoke-test, CI, benchmark, and publish paths.

Low-level VM clients and high-level ACP clients use the same sidecar binary.
The extension registry permits the sidecar to expose ACP without introducing
ACP or agent dependencies into `agentos-vm`.

This repository has no protocol backward-compatibility requirement. Remove the
old names outright; do not add binary aliases, environment-variable fallbacks,
duplicate artifacts, or dual protocol schema names.

## 5. Replace vague `common` and unqualified `runtime` names

The V8 support crate is shared specifically by V8-backed executors, so its name
must show that relationship:

```text
agentos-v8-runtime
    -> agentos-executor-v8-runtime
```

The WASM shared crate contains the engine-neutral agentOS WASM ABI, generated
imports, validation, profiles, limits, and stable WASM error types. `common`
does not identify that responsibility:

```text
agentos-executor-wasm-common
    -> agentos-executor-wasm-abi
```

The generator follows the same namespace:

```text
agentos-wasm-abi-generator
    -> agentos-executor-wasm-abi-generator
```

These crates must remain engine-neutral. `executor-wasm-abi` must not depend on
V8 or Wasmtime, and the generator must not become a runtime dependency of
production executors.

The VM-to-host interface and benchmark floor also receive responsibility-based
names:

```text
agentos-host-bridge
    -> agentos-vm-host-interface

agentos-native-baseline
    -> agentos-benchmark-baseline
```

`bridge` is reserved for an actual transport or protocol boundary. `native`
does not describe a benchmark that is also compiled into a WASM comparison
lane.

## 6. Rust type and module cleanup

Package renames are incomplete if public types continue to encode the old
architecture.

Required type changes:

| Current | Target |
|---|---|
| `NativeSidecar` | split into `Sidecar` and `VmManager` |
| `NativeSidecarConfig` | split into `SidecarConfig` and VM configuration/manager options |
| `NativeSidecarBridge` | remove or replace with a VM-owned bridge bound that does not mention sidecars |
| `SidecarRuntime` | `TokioDriver` |
| `RuntimeContext` | `DriverHandle` |
| VM-scoped `RuntimeContext` | `VmDriverHandle` |
| `RuntimeConfig` | `DriverConfig` |
| `RuntimeBuildError` | `DriverBuildError` |
| `RuntimeMetrics` | `DriverMetrics` |
| `RuntimeMetricsSnapshot` | `DriverMetricsSnapshot` |
| `RuntimeResourceConfig` | `DriverResourceConfig` |
| `HostBridge` | `VmHost` |
| `BridgeTypes` | `VmHostTypes` |
| `FilesystemBridge` | `HostFilesystem` |
| `PermissionBridge` | `HostPermissions` |
| `PersistenceBridge` | `HostPersistence` |
| `ClockBridge` | `HostClock` |
| `RandomBridge` | `HostRandom` |
| `EventBridge` | `HostEvents` |
| `ExecutionBridge` | `HostExecution` |

The split of `NativeSidecar` is behavioral ownership, not a search-and-replace:

- connection, protocol, extension, and transport fields move to `Sidecar`;
- VM maps, VM lifecycle, kernel composition, and execution coordination move
  to `VmManager`; and
- sidecar request handlers call `VmManager` through its public in-process API.

Remove `native_sidecar` and `runtime_tokio` from:

- Rust module paths and test names;
- tracing targets;
- generated protocol schema identifiers;
- environment variables;
- npm binary resolvers;
- CI artifact names;
- benchmark result metadata; and
- architecture documentation.

Use `agentos_vm`, `agentos_driver_tokio`, and `agentos_sidecar` consistently.

## 7. Flatten the TypeScript package graph

The TypeScript layer should expose a small number of meaningful distribution
boundaries. Source modules remain organized internally, but implementation
details do not receive separate npm packages.

### 7.1 Current graph

```text
@rivet-dev/agentos
└── @rivet-dev/agentos-core
    ├── @rivet-dev/agentos-runtime-core
    │   └── @rivet-dev/agentos-runtime-sidecar
    │       └── platform binary packages
    └── @rivet-dev/agentos-sidecar
        └── platform binary packages

@rivet-dev/agentos-posix
└── @rivet-dev/agentos-core

@rivet-dev/agentos-vm-test-harness
└── @rivet-dev/agentos-core

@rivet-dev/agentos-test-harness
└── @rivet-dev/agentos-vm-test-harness
```

This graph contains duplicate sidecar resolvers, an empty POSIX package, two
test-harness packages, and a public `runtime-core` package whose implementation
is already consumed as part of `agentos-core`.

### 7.2 Target graph

```text
@rivet-dev/agentos
└── @rivet-dev/agentos-core
    └── @rivet-dev/agentos-sidecar
        └── platform binary packages

@rivet-dev/agentos-test-harness        private
└── @rivet-dev/agentos-core

@rivet-dev/agentos-benchmarks          private
├── @rivet-dev/agentos-core
└── @rivet-dev/agentos
```

The three production boundaries are:

- `@rivet-dev/agentos`: RivetKit, actor, React, and orchestration integration;
- `@rivet-dev/agentos-core`: VM APIs, ACP/session APIs, sidecar client,
  protocol, VM configuration, and Node integration; and
- `@rivet-dev/agentos-sidecar`: the platform-specific native binary resolver.

Do not merge `agentos-core` into `agentos`. Low-level VM consumers must not
inherit RivetKit, React, or actor dependencies. Keep the sidecar resolver
separate because its platform-specific optional dependencies and release
artifacts have a distinct packaging lifecycle.

### 7.3 Package moves and removals

| Current | Target | Action |
|---|---|---|
| `packages/runtime-core` | `packages/core` | merge source, generated code, assets, scripts, and tests |
| `packages/runtime-sidecar` | — | remove in favor of the canonical sidecar resolver |
| `packages/sidecar-binary` | `packages/sidecar` | rename directory; keep package `@rivet-dev/agentos-sidecar` |
| `packages/posix` | — | remove the empty package; POSIX behavior remains kernel-owned |
| `packages/vm-test-harness` | `packages/test-harness` | merge into the single private test harness |
| `test-harness` | `packages/test-harness` | move under the flat package root |
| `packages/typescript` | `packages/core` | merge the private TypeScript VM helpers |
| `packages/runtime-benchmarks` | `packages/benchmarks` | rename private tooling package to `@rivet-dev/agentos-benchmarks` |
| `packages/browser` | `archive/browser/packages/browser` | archive and remove from the active workspace |
| `packages/runtime-browser` | `archive/browser/packages/runtime-browser` | archive and remove from the active workspace |
| `packages/playground` | `archive/browser/packages/playground` | archive because its worker and frontend depend entirely on the dormant browser packages |

No compatibility packages, re-export-only packages, duplicate binary
resolvers, or npm aliases remain. Browser reference sources do not constrain
the active package graph and must stay outside builds, CI, publication, and
behavioral parity.

### 7.4 Target TypeScript directory structure

```text
packages/
├── agentos/               @rivet-dev/agentos
├── core/                  @rivet-dev/agentos-core
├── sidecar/               @rivet-dev/agentos-sidecar
├── test-harness/          @rivet-dev/agentos-test-harness       private
├── benchmarks/            @rivet-dev/agentos-benchmarks         private
├── build-tools/           @rivet-dev/agentos-build-tools        private
├── agentos-apps/
├── agentos-sandbox/
├── agentos-toolchain/
├── eve/
├── flue/
├── node-pty/
└── shell/

archive/browser/packages/
├── browser/
├── runtime-browser/
└── playground/
```

Package flattening does not require flattening every source file. The merged
`agentos-core` keeps a small number of responsibility-based internal folders:

```text
packages/core/src/
├── index.ts
├── agent-os.ts
├── types.ts
├── session-api.ts
├── language-execution.ts
├── code-execution.ts
├── filesystem-snapshot.ts
├── layers.ts
├── sidecar/
│   ├── client.ts
│   ├── process.ts
│   ├── rpc-client.ts
│   ├── framing.ts
│   ├── protocol.ts
│   ├── payload-codec.ts
│   ├── event-buffer.ts
│   └── errors.ts
├── generated/
│   ├── protocol/
│   └── vm-config/
├── cron/
└── internal/
    ├── runtime-compat.ts
    └── typescript-tools.ts
```

There must be one generated protocol and VM-config tree after the merge. The
single private test harness owns runtime factories, WASM fixture discovery,
terminal helpers, conformance helpers, and test filesystems. Production
packages must not depend on it.

## 8. Decisions deliberately deferred

The VFS packages retain their current names until their final responsibility is
decided. In particular, this document does not decide whether actor SQLite,
snapshots, package storage, and other VM persistence belong in
`vfs-storage` or a broader future VM-storage package.

Moving the native mounted-filesystem Tokio adapter out of `vfs-core` remains a
valid dependency cleanup, but it is not allowed to expand this package rename
into a VFS behavior rewrite.

## 9. Implementation checklist

- [ ] Rename `runtime-tokio` to `driver-tokio` and update its public types.
- [ ] Move sidecar protocol configuration out of `driver-tokio`.
- [ ] Rename `kernel` to `vm-kernel`.
- [ ] Move sidecar transport, connection, protocol, and extension ownership out
      of the current `sidecar` crate.
- [ ] Rename the remaining VM orchestration library to `agentos-vm`.
- [ ] Make `agentos-vm` library-only.
- [ ] Make `agentos-sidecar` the only sidecar executable and native composition
      root.
- [ ] Preserve the extension mechanism and keep ACP out of `agentos-vm`.
- [ ] Move executor feature selection to the `agentos-sidecar` composition
      root.
- [ ] Make `agentos-vm` default to no executors and accept an injected,
      possibly empty executor registry.
- [ ] Return typed `ERR_AGENTOS_EXECUTOR_UNAVAILABLE` errors for execution
      requests against an empty or incomplete registry.
- [ ] Add a checked Rust example that uses `agentos-vm` directly as an
      embeddable OS with no sidecar, client, or executors.
- [ ] Keep `agentos-vm-kernel` free of Tokio and concrete executor
      dependencies.
- [ ] Rename `v8-runtime` to `executor-v8-runtime`.
- [ ] Rename `executor-wasm-common` to `executor-wasm-abi`.
- [ ] Rename `wasm-abi-generator` to `executor-wasm-abi-generator`.
- [ ] Rename `host-bridge` to `vm-host-interface` and update its public traits.
- [ ] Rename `native-baseline` to `benchmark-baseline`.
- [ ] Keep `resource-accounting` runtime-neutral and unchanged in name.
- [ ] Remove `agentos-native-sidecar`, `AGENTOS_NATIVE_SIDECAR_BIN`, and the
      duplicate sidecar resolver/artifacts.
- [ ] Merge all production `agentos-runtime-core` functionality, generated
      code, assets, scripts, and tests into `agentos-core`.
- [ ] Remove the `@rivet-dev/agentos-runtime-core` package without a
      compatibility re-export.
- [ ] Merge the private TypeScript VM/compiler helpers into `agentos-core`.
- [ ] Merge `agentos-vm-test-harness` and the root test harness into
      `packages/test-harness`.
- [ ] Remove the empty `agentos-posix` package.
- [ ] Rename `packages/sidecar-binary` to `packages/sidecar` and remove the
      duplicate `agentos-sidecar` package family.
- [ ] Rename `packages/runtime-benchmarks` to `packages/benchmarks`.
- [ ] Archive browser TypeScript packages under `archive/browser/packages/`
      and remove them from the active pnpm/Turbo/publish graph.
- [ ] Update all TypeScript imports to the flattened `agentos-core`,
      `agentos-sidecar`, and test-harness surfaces.
- [ ] Ensure `agentos-core` has one protocol generator, one VM-config generator,
      and no duplicate generated type trees.
- [ ] Update Cargo metadata, lockfile, publish order, CI, scripts, generated
      paths, documentation, and architecture guards.
- [ ] Update Rust and TypeScript protocol schema names to
      `agentos-sidecar`.
- [ ] Run the complete workspace, feature-matrix, executor-conformance,
      sidecar, VM, protocol, publish, smoke, parity, and benchmark validation
      suites.
- [ ] Run `cargo check -p agentos-vm --no-default-features` and compile/run the
      direct embedded-VM example in CI.
