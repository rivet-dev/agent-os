# Agent OS Runtime Consolidation Requirements

## Status

Requirements document only. This is not a spec.

## Context

Agent OS currently depends on a legacy runtime stack for kernel, sidecar, and execution behavior.

The direction from this discussion is to stop using that legacy stack as an external dependency and move the relevant runtime, kernel, and sidecar functionality directly into Agent OS in order to simplify the architecture and preserve existing behavior.

This consolidation phase is focused on V8 and native functionality. Python is out of scope for the initial merge.

This document captures the requirements that a later architecture doc and implementation plan must satisfy.

## Scope

This document covers:

- runtime consolidation into Agent OS
- sidecar process model
- VM isolation model
- kernel ownership and boundaries
- Rust crate structure
- naming and package consolidation
- top-level project/package structure
- browser parity requirements
- persistence requirements

This document does not define:

- wire formats
- concrete Rust or TypeScript module layout below the package/crate boundary
- syscall ABI details
- migration sequencing

## Core Direction

Agent OS must absorb the legacy runtime stack rather than continuing to depend on externally owned runtime packages.

The resulting system must preserve existing functionality while simplifying ownership:

- Agent OS becomes the product and implementation boundary.
- The sidecar becomes part of Agent OS.
- The kernel becomes part of Agent OS.
- JavaScript and guest WebAssembly execution become built-in runtime capabilities of Agent OS.
- The shared kernel implementation is written in Rust and reused across native and browser builds.
- The final consolidated system must not retain legacy product naming or package branding.

## Architectural Planes

The consolidated system must remain explicit about the separation between three planes:

### Host Plane

The host plane is the JavaScript SDK surface used by callers.

The host plane is responsible for:

- creating and disposing Agent OS instances
- preserving the current host-facing VM configuration surface for mounts, software, root filesystem setup, permissions, toolkits, and related options
- introducing sidecar-handle selection and creation as a new host-managed capability during consolidation
- creating logical VMs
- choosing whether a VM uses the default shared sidecar or an explicitly supplied sidecar handle
- issuing host-to-sidecar control requests

The host plane is not the owner of per-VM kernel state.

### Kernel Plane

The kernel plane is the authoritative per-VM data plane.

The kernel plane owns VM state and exposes a generic interface to the execution plane.

The kernel plane must remain separate from both the host control surface and the execution internals.

### Execution Plane

The execution plane is the built-in runtime layer that runs JavaScript and guest WebAssembly.

The execution plane is responsible for:

- V8 isolate lifecycle
- guest JavaScript execution
- guest WebAssembly execution through V8
- calling into the kernel through the generic kernel interface

The execution plane must not become the owner of kernel state.

For the native sidecar, execution is provided by a dedicated native execution layer.

For the browser sidecar, execution is provided through browser primitives exposed through the browser bridge rather than through the native execution crate.

For the initial browser implementation, the kernel and browser-side sidecar live on the main thread, while only guest execution runs in Web Workers or equivalent browser worker primitives.

## Definitions

### Host

The host is the JavaScript SDK side of Agent OS.

The host manages Agent OS from outside the sidecar process.

### VM

A VM is a logical Agent OS execution environment.

A VM is not required to map 1:1 to an OS process.

Each VM must have its own isolated kernel state even when multiple VMs share the same sidecar process.

### Sidecar

A sidecar is the runtime host process used to execute one or more logical VMs.

The sidecar is allowed to host multiple VMs in the same process for performance reasons.

### Kernel

The kernel is the authoritative per-VM data plane.

The kernel owns VM state and exposes a generic interface to execution subsystems.

The kernel must not be collapsed into V8-specific logic.

The shared kernel implementation is the Rust code that is compiled for both the native sidecar and the browser-side sidecar build.

## Requirements

### 1. Consolidation

- Agent OS must stop depending on the legacy runtime stack as an external runtime dependency.
- Legacy runtime functionality needed by Agent OS must be moved into Agent OS directly.
- The consolidated system must maintain 1:1 functionality with the current behavior baseline.
- The final consolidated system must not ship with leftover legacy-branded runtime dependencies.

### 2. Kernel Ownership

- The kernel must remain a distinct data-plane component.
- The kernel must be implemented as a shared Rust library used by both native and browser sidecar implementations.
- The kernel must expose a generic interface to execution subsystems.
- The kernel must not be modeled as a V8-specific bridge layer.
- The kernel must remain the source of truth for per-VM state.
- The kernel plane must remain separate from the host plane and the execution plane.

Per-VM state includes at least:

- filesystem state
- file descriptor state
- process state
- permissions and capability state
- runtime resource/accounting state

### 3. Execution Model

- Agent OS will no longer preserve a pluggable public “execution engines” abstraction as a primary architecture concept.
- JavaScript and guest WebAssembly support will be baked into the runtime implementation.
- Guest WebAssembly must use V8's WebAssembly support.
- Even with built-in execution support, the kernel boundary must stay generic and separate from engine internals.
- Guest-visible synchronous semantics for filesystem, module loading, process control, and similar runtime operations are part of the compatibility surface wherever current behavior depends on them.
- Internal implementation may change, but observable guest behavior must not silently shift from synchronous or sync-looking to asynchronous semantics unless the public runtime contract is explicitly redefined.
- Timing mitigation must be implemented consistently across guest JavaScript and guest WebAssembly execution.
- Timing mitigation must not be treated as a JavaScript-only feature or as a V8-only special case.
- The native implementation may use a dedicated `execution` crate.
- The browser implementation does not need to use the native `execution` crate and may instead satisfy the same kernel-facing interface through browser primitives and browser-specific bridge code.

### 4. VM-to-Process Mapping

- A VM does not need to run in its own OS process.
- Multiple VMs may share the same sidecar process.
- A shared-process model is expected and acceptable.
- Snapshot-based fast startup is a required design input for the shared-process model.

### 5. Isolation Model

- Each VM must have isolated kernel state even when multiple VMs share one sidecar process.
- Sharing a sidecar process must not imply sharing filesystem state, FD state, process tables, or permissions between VMs.
- A crash of a shared sidecar process may take down every VM in that shard. This is expected and acceptable.

Snapshotting and bootstrap acceleration may be shared across VMs, but per-VM kernel state must never be stored inside shared snapshots.

### 6. Configurable Process Isolation

- Agent OS must support configurable process placement for VMs.
- The final design must support a default shared-sidecar process.
- Agent OS must also support manually creating a sidecar reference/handle and passing it into Agent OS construction so callers can control placement.
- The design must allow callers to choose which logical OS instances share a sidecar process.

This host-side configuration model must remain on the JavaScript SDK side rather than moving into VM internals.

This requirement exists to preserve the intended operational model for the consolidated runtime rather than forcing either:

- one process per VM, or
- one global process for all VMs

### 7. Host-Side Configuration Surface

- Agent OS must keep the current host-side configuration model for the existing Agent OS options such as `mounts`, `software`, `rootFilesystem`, `moduleAccessCwd`, `scheduleDriver`, `toolKits`, and `permissions`.
- The host must add a default shared-sidecar path as part of the consolidation.
- The host must add an explicit sidecar creation/reference path that can be passed into Agent OS construction.
- VM placement decisions must remain host-controlled configuration, not guest-controlled runtime behavior.

This document does not require the final SDK shape to be byte-for-byte identical, but it does require the same existing Agent OS configuration capabilities to remain available on the host side while adding sidecar-placement control as a new host capability.

### 8. Public SDK Compatibility Boundary

- Direct public exposure of a live mutable `AgentOs.kernel` object is not required to survive the consolidation.
- Removing raw kernel exposure from the public SDK is an intentional breaking change if equivalent host-facing capabilities remain available through explicit APIs.
- If a temporary compatibility shim for `AgentOs.kernel` exists during migration, it must be treated as migration-only and deleted before completion.

### 9. Host-to-Sidecar Protocol

- Agent OS must define a clean, simple host-to-sidecar protocol under Agent OS ownership.
- There is no requirement to preserve legacy wire compatibility.
- The protocol must support both the native sidecar and the browser-side sidecar model.
- The protocol must support VM lifecycle, root filesystem bootstrap configuration, stream transport, filesystem-related host interactions, permissions, and execution control.
- The protocol must preserve the security and isolation invariants required for shared sidecars, including authenticated connection setup, session ownership/binding, payload or frame size limits, and response integrity.

This document does not define the protocol in detail, but it does require a single Agent OS-owned protocol design rather than carrying forward legacy protocol shape for compatibility reasons.

### 10. Bridge Interface Shape

- The kernel-facing bridge API must use explicit trait methods rather than generic operation enums.
- Filesystem bridge APIs must expose distinct operations such as read, write, stat, readdir, mkdir, remove, and rename rather than a single `fs_call(op)` entrypoint.
- Execution bridge APIs must expose distinct guest lifecycle and IO methods rather than a single generic command or operation enum entrypoint.
- Bridge interfaces should be split by concern so they remain readable and testable.
- It is acceptable to compose multiple smaller traits such as filesystem, permissions, persistence, clocks, events, and execution rather than placing all methods on one trait.

This requirement applies to the internal kernel/bridge interface shape. It does not require the external host-to-sidecar transport protocol to mirror the exact same method granularity on the wire.

### 11. Browser Parity

- Agent OS must preserve browser parity.
- Browser parity means API parity and behavioral parity, not identical implementation details.
- The browser implementation may differ internally from the native sidecar implementation.
- Browser support must preserve the same logical VM and kernel model as far as user-visible behavior is concerned.
- The primary expected difference between native and browser implementations is the bridge between the kernel and execution/host primitives.
- In the initial browser implementation, the kernel and `sidecar-browser` run on the main thread.
- In the initial browser implementation, worker creation is main-thread-owned.
- Browser workers are guest execution containers, not owners of kernel state.
- The browser implementation must preserve the current sync-looking guest ABI for filesystem, module-loading, and similar guest-facing operations, or else explicitly redefine that ABI and update the public/browser contract in the same migration phase.
- `postMessage` by itself is not sufficient as the browser execution bridge for parity-sensitive synchronous guest operations.
- Browser worker control channels must remain part of the sandbox boundary and require explicit hardening against guest access or forgery.
- Browser support must preserve timing-mitigation semantics for both guest JavaScript and guest WebAssembly even if the implementation differs from native.

### 12. Persistence

- Persistence behavior must remain unchanged from the current baseline.
- Filesystem state is the only persistence requirement captured by this discussion.
- No new persistence requirements are introduced here for other runtime state.

### 13. Filesystem Driver And Mount Surface

- Agent OS must preserve the current filesystem driver and mount model as part of the host-controlled API surface.
- The host must continue to control filesystem attachment and mount configuration.
- Consolidation must not remove the ability to mount different filesystem backends into a VM.
- The design must preserve current filesystem-driver functionality even if internal implementation details change.
- The host-side config surface must continue to support passing concrete filesystem driver instances/objects into VM mount configuration.

At minimum, the consolidated system must preserve the existing categories of filesystem support that Agent OS exposes today:

- in-memory filesystems
- caller-provided/custom filesystems
- host directory mounts
- overlay/copy-on-write mounts
- object-storage-backed filesystems
- browser storage backends needed for browser parity

This document does not require the exact same internal package split for filesystem drivers, but it does require the same user-visible capabilities to remain available.
In particular, drivers such as the S3-backed filesystem must continue to work as host-provided filesystem backends rather than being dropped during consolidation.

### 14. Provided Commands And Command Surface

- Agent OS must preserve the current provided command surface as a functional requirement.
- Consolidation must not silently remove or rename the command interfaces currently provided to VMs.
- The design must preserve command resolution behavior expected by the current VM model.
- The design must preserve the ability for packaged software/command sets to provide executable commands inside the VM.

This applies to:

- built-in shell entrypoints
- the currently provided POSIX/WASM command surface
- JavaScript-projected tools and agents that appear as executable software inside the VM
- registry-provided software packages and command bundles

Internal implementation may change, but the current externally visible command behavior must be preserved unless a later, explicit product decision changes it.

### 15. Software Package Injection Surface

- Agent OS must preserve the current host-side software/package injection model.
- The host must continue to be able to pass software descriptors/packages into the Agent OS `software` configuration at VM creation time.
- Consolidation must preserve the current ability for passed-in software to affect VM command availability, projected package roots, and agent/tool registration behavior.
- The design must preserve support for direct package descriptors as well as bundled/meta-package inputs that expand to multiple software entries.

At minimum, the consolidated system must preserve the existing categories of software input behavior exposed today:

- WASM command packages that contribute command directories
- tool packages that project required npm package roots into the VM
- agent packages that project required npm package roots into the VM and register agent metadata
- registry packages passed directly through the host config surface
- array/meta-package inputs that expand into multiple software descriptors

This requirement preserves the current host-side `software` configuration capability even if the internal implementation stops using the current legacy package plumbing.

### 16. Removed Concepts

- “Web instance” is not part of this requirements document and should not drive the initial architecture.
- The new requirements should be written without depending on a separate web-instance abstraction.

### 17. Naming And Branding Consolidation

- The final consolidated system must not retain legacy product naming.
- Public package names, internal package names, binary names, environment variable names, log labels, docs, and user-facing symbols must use Agent OS naming.
- No final public package or binary should use legacy naming.
- Compatibility may be preserved at the protocol-behavior level, but not by leaving legacy product names in place.

At minimum, the final state must eliminate legacy naming from:

- npm package names
- Rust crate and binary names
- environment variables
- host SDK APIs
- sidecar-management APIs
- repository documentation and examples

### 18. Rust Crate Structure Target

The consolidated runtime implementation should be organized around four Rust crates:

```text
crates/
  kernel/
  execution/
  sidecar/
  sidecar-browser/
```

This structure implies the following requirements:

- `kernel` is a shared Rust library that applies to both native and browser builds.
- `execution` is a native-only Rust library that provides the native execution layer.
- `sidecar` imports `kernel` and `execution` and provides the native sidecar implementation and bridge.
- `sidecar-browser` imports `kernel` and provides the browser-side sidecar implementation, using browser bindings plus worker coordination primitives to provide the execution bridge.
- In the initial implementation, `sidecar-browser` runs on the main thread and is responsible for creating and coordinating browser workers.
- The browser-side implementation must mimic the sidecar model rather than introducing a separate browser-only architecture.
- The browser-side implementation may use Rust-to-browser bindings and JavaScript glue where required, but the kernel-facing interface must remain aligned with the native sidecar.
- The only intentionally divergent layer between native and browser implementations should be the bridge between the kernel and execution/host primitives.

### 19. Top-Level Project Structure Target

The simplified top-level project structure should be organized around Agent OS ownership rather than the legacy package split.

The target top-level structure is:

```text
packages/
  core/            -> publish as @rivet-dev/agent-os
  shell/           -> publish as @rivet-dev/agent-os-shell
  registry-types/  -> publish as @rivet-dev/agent-os-registry-types

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

This structure implies the following requirements:

- The current host SDK package must be renamed from `@rivet-dev/agent-os-core` to `@rivet-dev/agent-os`.
- Registry packages should remain under the current Agent OS naming scheme.
- The repository should not preserve the old runtime/core/v8/browser/posix package map as a public product boundary.
- The initial consolidated design does not need separate public packages for Python support or for a standalone POSIX execution-engine abstraction.
- Native command/runtime assets may remain under `registry/native` or another Agent OS-owned internal boundary without being exposed as separate execution-engine products.
- Any JavaScript package used for browser integration may be a thin wrapper or loader around the browser-side sidecar implementation rather than a distinct runtime implementation layer.

### 20. JavaScript Package Surface Target

The JavaScript/npm surface should remain minimal and should not mirror the Rust crate split.

At minimum, the public JavaScript package surface should include:

- `@rivet-dev/agent-os`
- `@rivet-dev/agent-os-shell`
- `@rivet-dev/agent-os-registry-types`
- existing Agent OS registry packages under `registry/agent`, `registry/file-system`, `registry/software`, and `registry/tool`

The initial consolidated design does not require separate public npm packages for:

- a standalone kernel package
- a standalone execution-engine package
- a standalone Python runtime package
- a standalone POSIX runtime package

If browser loading requires a separate JavaScript wrapper package, it must use Agent OS naming and act as a thin wrapper around the browser-side sidecar implementation rather than as a separate browser runtime architecture.

## Constraints

- The design must keep the kernel boundary explicit after consolidation.
- The design must keep the host plane, kernel plane, and execution plane conceptually separate.
- The design must preserve existing Agent OS host configuration ergonomics while adding shared-sidecar ergonomics.
- The design must not require a 1:1 VM-to-process mapping.
- The design must not break browser support in pursuit of native-side simplification.
- The design must preserve current filesystem persistence semantics.
- The design must add and preserve a host-controlled sidecar configuration model.
- The design must define a new clean Agent OS-owned host-to-sidecar protocol.
- The design must preserve shared-sidecar security invariants even though the protocol is redesigned.
- The design must use explicit bridge trait methods rather than op-enum based bridge APIs.
- The design must preserve the current filesystem driver/mount capabilities.
- The design must preserve the current provided command surface.
- The design must preserve the current host-side software/package injection capabilities.
- The design must remove legacy-branded names from the final consolidated system.
- The design must simplify the public package surface rather than mirroring the legacy package split.
- The design must organize the runtime implementation around the four Rust crates described above.
- The design must preserve consistent timing-mitigation semantics across guest JavaScript and guest WebAssembly.

## Non-Goals

The following are not requirements from this discussion:

- designing a new public execution-engine plugin API
- introducing persistence for non-filesystem runtime state
- requiring dedicated process isolation for every VM
- defining a public web-instance abstraction
- requiring identical browser and native internals
- preserving Python as part of the first consolidation phase
- preserving the legacy public runtime package split as-is
- preserving legacy wire compatibility

## Implications For The Follow-On Spec

The later spec should assume:

- a host control plane in the JavaScript SDK
- one logical kernel instance per VM
- a distinct per-VM kernel data plane
- a built-in execution plane for V8 JavaScript and guest WebAssembly
- one or more VMs per sidecar process
- configurable VM placement across sidecars
- sidecar selection is a new host capability to add to the existing Agent OS config surface
- built-in V8-backed JavaScript and WebAssembly execution
- a generic kernel interface separating kernel state from runtime internals
- direct public `AgentOs.kernel` access may be removed as an intentional breaking change
- explicit bridge traits with method-per-operation APIs rather than generic op-enum dispatch
- a shared Rust `kernel` crate compiled for both native and browser-side builds
- a native-only `execution` crate
- a native `sidecar` crate that composes `kernel` and `execution`
- a `sidecar-browser` crate that composes `kernel` with browser bindings and browser-specific bridge code
- a new Agent OS-owned host-to-sidecar protocol rather than legacy protocol compatibility
- shared-sidecar protocol invariants such as authentication, session binding, and response integrity
- a browser bridge that does more than plain `postMessage` for sync-looking guest operations
- consistent timing-mitigation semantics for both guest JavaScript and guest WebAssembly
- the existing filesystem driver and mount capabilities remain available
- the existing provided command surface remains available
- the existing host-side software/package injection capabilities remain available
- browser parity at the API/behavior level
- Python-specific runtime surfaces and tests may be removed from the final parity bar because Python is intentionally out of scope
- unchanged filesystem persistence semantics
- a simplified public package surface centered on `@rivet-dev/agent-os`
- no final legacy-branded runtime packages or binaries

## Summary

Agent OS should absorb the legacy runtime stack, keep a clear separation between the host plane, kernel plane, and execution plane, preserve the current Agent OS host configuration surface while adding shared-sidecar-plus-explicit-handle placement control, define a new clean Agent OS-owned host-to-sidecar protocol with explicit shared-sidecar security invariants, organize the runtime around four Rust crates (`kernel`, `execution`, `sidecar`, and `sidecar-browser`), keep the kernel as a shared per-VM Rust data plane, allow removal of direct public `AgentOs.kernel` exposure as an intentional breaking change, use explicit bridge traits with method-per-operation APIs, and run JavaScript and guest WebAssembly as built-in execution capabilities with only the bridge layer differing between native and browser implementations.
