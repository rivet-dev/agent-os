# Browser Sidecar E2E Proposal

## Summary

Implement browser support by finishing the original `sidecar-browser` design instead of extending the current JavaScript-only browser runtime path.

The intended architecture already exists in the original runtime-consolidation spec:

- the kernel stays on the browser main thread
- only guest execution runs in workers
- parity-sensitive guest operations keep a sync-looking ABI through a blocking bridge
- unsupported browser environments fail closed instead of silently degrading

Today the repo has pieces of that design, but they are split:

- `crates/sidecar-browser` has the right service shape and worker-facing kernel ownership model
- `packages/browser` has the working browser sync bridge, worker protocol, timing-mitigation behavior, and permission wiring
- `packages/browser/tests` exercise that path through a Node-hosted `Worker` shim, not a real browser
- `packages/playground` proves some packaging and static-serving concerns, but it is not a stable runtime contract harness

The proposal is to connect those pieces into one browser runtime path and make a real browser E2E suite the acceptance gate.

## Source Context

The original architecture already describes the correct direction:

- `.agent/research/agent-os-runtime-consolidation-spec.md`
  - `sidecar-browser`
  - `Phase 9: Bring Up The Browser Sidecar`

The current implementation seams worth reusing are:

- `crates/sidecar-browser/src/service.rs`
- `crates/sidecar-browser/src/lib.rs`
- `packages/browser/src/runtime-driver.ts`
- `packages/browser/src/worker.ts`
- `packages/browser/src/sync-bridge.ts`
- `packages/browser/src/worker-protocol.ts`
- `packages/browser/src/worker-adapter.ts`
- `packages/playground/backend/server.ts`

## Problem Statement

Current browser support is not in the final architectural state and is not tested end to end in a real browser.

The main gaps are:

1. The browser package is still a JavaScript runtime-driver path, not a thin host wrapper over `sidecar-browser`.
2. The Rust kernel and `BrowserSidecar` service are not the active browser execution path.
3. Current "browser" tests use `NodeTestWorker`, so they validate protocol logic but not:
   - real `SharedArrayBuffer` behavior
   - real worker boot semantics
   - real cross-origin-isolation requirements
   - real browser module/asset resolution
   - real OPFS persistence behavior
4. The playground is too UI-heavy to be the primary runtime contract test surface.

If we keep adding browser behavior only inside `packages/browser`, we will end up maintaining a second architecture instead of finishing the one the spec already chose.

## Required End State

Browser support is only "done" when all of the following are true:

- the active browser runtime path uses `sidecar-browser` as the main-thread sidecar model
- the kernel used by browser execution is the shared Rust kernel, not a separate JavaScript kernel path
- JavaScript and WebAssembly guest execution both run through worker-based execution under the same browser sidecar
- parity-sensitive guest operations preserve the sync-looking guest ABI through a blocking bridge
- unsupported browser environments fail closed with a clear capability error
- the public browser package remains the JS-facing entrypoint, but it is a thin wrapper around the browser sidecar path
- a real browser E2E suite passes against built artifacts, not just source-loaded Node shims

## Recommended Architecture

### 1. Keep `crates/sidecar-browser` as the core browser-side runtime model

Do not move the browser orchestration model back into TypeScript.

`crates/sidecar-browser` should remain the owner of:

- VM lifecycle
- guest context lifecycle
- execution lifecycle
- worker ownership bookkeeping
- lifecycle and structured event emission
- deterministic cleanup semantics

The browser package should call into this layer, not reimplement it.

### 2. Add a wasm wrapper crate for `sidecar-browser`

Do not try to turn the pure Rust crate directly into a JS app layer.

Add a thin wrapper crate:

```text
crates/
  sidecar-browser/
  sidecar-browser-wasm/
```

Recommended responsibilities:

- `crates/sidecar-browser`
  - pure Rust domain logic
  - no browser-global assumptions
  - easy native and wasm-target unit testing
- `crates/sidecar-browser-wasm`
  - `wasm-bindgen` boundary
  - JS interop shims
  - handle serialization and event marshalling
  - no business logic beyond the boundary layer

This keeps the browser sidecar testable without forcing Rust service logic to live inside JS glue code.

### 3. Keep the worker bridge and sync bridge in TypeScript

The current TS worker path is the most reusable part of the browser implementation and should be preserved.

Keep and adapt:

- `packages/browser/src/worker.ts`
- `packages/browser/src/sync-bridge.ts`
- `packages/browser/src/worker-protocol.ts`
- `packages/browser/src/worker-adapter.ts`

Those files already encode:

- worker request/response framing
- blocking `SharedArrayBuffer` + `Atomics.wait` bridge behavior
- timing mitigation inside the worker
- control-token hardening
- module and filesystem sync-looking guest shims

The main change is ownership:

- today: `BrowserRuntimeDriver` owns worker orchestration directly
- target: JS bridge code owns worker mechanics, but `sidecar-browser` owns the execution lifecycle and state model

### 4. Make `packages/browser` a thin public host wrapper

The public package should remain `@rivet-dev/agent-os-browser`, but its role changes.

Target responsibilities:

- create the JS bridge adapter for browser-only capabilities
- load the wasm sidecar module
- expose `createBrowserDriver` and `createBrowserRuntimeDriverFactory`
- translate public JS options into:
  - browser sidecar config
  - worker bridge config
  - filesystem/network/permission bridge config

Non-goal:

- keeping `packages/browser` as the place where the real runtime state machine lives

### 5. Use a JS bridge adapter at the wasm boundary

The browser sidecar needs JS-only capabilities that Rust cannot do directly:

- create and terminate `Worker` instances
- interact with OPFS APIs
- access `fetch`
- serialize permission callbacks
- own `SharedArrayBuffer` objects for worker bridges

So the wasm wrapper should depend on a JS bridge adapter with explicit operations such as:

- `createWorker`
- `terminateWorker`
- `createSyncBridge`
- `readHostFile` / `writeHostFile` / `readDir` / `stat`
- `fetch`
- `emitLifecycle`
- `emitStructuredEvent`

This is the browser equivalent of a host bridge. Keep it method-oriented and explicit.

### 6. Treat filesystem in browser as two modes

Browser support should not pretend every filesystem mode is equivalent.

Supported first-party browser modes should be:

- `memory`
- `opfs`

Anything else should fail closed unless there is an explicitly supported browser bridge for it.

For browser parity:

- `memory` is the baseline contract environment
- `opfs` is the persistence environment

Do not block browser E2E delivery on trying to make remote mounts or native-only filesystem plugins behave identically in the browser.

## Implementation Plan

### Phase 0: Stabilize the contract surface

Before wiring in wasm:

- document that the original `sidecar-browser` design is the source of truth
- keep the public browser package names stable
- define the minimal browser-supported filesystem matrix:
  - memory: required
  - opfs: required
  - everything else: explicit fail-closed behavior
- add a browser-safe kernel helper surface so the playground no longer imports `dist/*` internals directly from `@rivet-dev/agent-os-kernel`

This is important because the playground currently has brittle direct `dist/` imports and should not become the runtime contract.

### Phase 1: Compile the browser sidecar to wasm

Add `crates/sidecar-browser-wasm` with:

- `cdylib` target
- `wasm-bindgen`
- exported sidecar handle/class
- exported methods for:
  - `createVm`
  - `disposeVm`
  - `createJavascriptContext`
  - `createWasmContext`
  - `startExecution`
  - `writeExecutionStdin`
  - `killExecution`
  - `pollExecutionEvent`

Output shape:

```text
packages/browser/dist/
  index.js
  worker.js
  sidecar-browser.wasm
  sidecar-browser.js
```

The build must produce relocatable browser assets with predictable URLs.

### Phase 2: Introduce the JS browser bridge adapter

Build a JS bridge layer inside `packages/browser` that:

- spawns workers
- creates the `SharedArrayBuffer` sync bridge
- exposes filesystem and network ops to the wasm sidecar
- serializes permission callbacks safely
- surfaces structured and lifecycle events to JS callers

At this stage, keep the existing worker code mostly intact. The rewrite target is ownership, not the worker ABI.

### Phase 3: Cut `BrowserRuntimeDriver` over to the sidecar-browser path

Refactor `BrowserRuntimeDriver` so it becomes:

- a public facade
- a request builder
- a result/event adapter

and not:

- the owner of execution state
- the owner of worker lifecycle bookkeeping

This should preserve the current user-facing API where practical:

- `createBrowserRuntimeDriverFactory`
- `createBrowserDriver`
- `runtime.exec(...)`
- timing mitigation options
- filesystem and permission options

### Phase 4: Add a dedicated runtime E2E harness page

Do not use the Monaco playground UI as the primary browser runtime test harness.

Add a minimal test app, for example:

```text
packages/browser/e2e/
  server.ts
  fixtures/
  public/
    index.html
    harness.js
```

This harness should:

- load the built `@rivet-dev/agent-os-browser` bundle
- load the wasm sidecar asset the same way users would
- expose a small `window.__agentOsHarness` API for the E2E runner
- support toggles for:
  - `memory` vs `opfs`
  - COOP/COEP on vs off
  - timing mitigation `freeze` vs `off`
  - JS guest vs Wasm guest

This keeps runtime tests deterministic and avoids making Monaco/editor behavior part of the runtime acceptance contract.

### Phase 5: Keep the playground as a smoke test only

After the runtime E2E harness is stable, keep `packages/playground` as:

- a packaging smoke test
- a UI smoke test
- a manual-debug environment

Do not make the playground the only browser validation path.

## E2E Test Strategy

## Principle

The browser runtime should be tested at four layers:

1. Rust sidecar logic
2. wasm boundary
3. runtime package integration
4. real browser E2E

The important correction is that layer 4 must be a real browser, not a Node worker shim.

### Layer 1: Rust unit and integration tests

Keep expanding:

- `crates/sidecar-browser/tests/service.rs`
- `crates/sidecar-browser/tests/bridge.rs`
- `crates/sidecar-browser/tests/smoke.rs`

Focus:

- VM/context/execution lifecycle
- worker bookkeeping
- invalid-state handling
- kill/dispose semantics
- structured/lifecycle event ordering
- JS and Wasm context symmetry

These tests should not depend on browser globals.

### Layer 2: wasm boundary tests

Add headless wasm tests for the new wrapper crate.

Recommended tools:

- `wasm-bindgen-test` for boundary correctness
- headless Chromium for browser-executed wasm tests

Focus:

- wasm asset loads correctly
- JS bridge callbacks are invoked correctly
- handle serialization works
- event polling works
- worker spawn/terminate bridge calls round-trip correctly

This layer catches wasm packaging and ABI breakage before full E2E.

### Layer 3: package-level integration tests

Keep the fast package-level tests in `packages/browser/tests`, but redefine what they are for.

They should remain useful for:

- worker protocol parsing
- sync-bridge framing
- payload size enforcement
- timing mitigation implementation details
- permission callback serialization

They should not be treated as proof that browser support works end to end.

Current Node-hosted tests are still valuable, just not sufficient.

### Layer 4: real browser E2E tests

Add Playwright-based browser E2E tests as the gating suite.

Recommended default:

- Chromium required in CI
- Firefox and WebKit optional smoke lanes later

Chromium should be the first-class target because:

- it gives stable `SharedArrayBuffer` behavior with the right headers
- it makes OPFS validation practical
- it is the fastest way to establish a real browser gate

## Required E2E Cases

### 1. Boot and capability detection

- page loads with COOP/COEP headers
- runtime initializes successfully
- worker boots successfully
- sidecar wasm asset resolves successfully

Negative:

- same page without COOP/COEP fails closed with a clear `SharedArrayBuffer` capability error

### 2. Sync filesystem parity in memory mode

Guest code should be able to:

- `mkdirSync`
- `writeFileSync`
- `readFileSync`
- `readdirSync`
- `statSync`
- relative-path module loading from the virtual filesystem

This validates the core blocking bridge contract in a real browser.

### 3. OPFS persistence across page reload

Real E2E must verify persistence, not just API calls.

Test flow:

1. create runtime in `opfs` mode
2. write a file
3. fully reload the page
4. recreate runtime
5. read the file back

This is one of the most important browser-only tests because Node shims cannot validate it honestly.

### 4. JavaScript guest execution

Validate:

- stdout/stderr capture
- `cwd`
- env passing
- relative module loading
- multiple sequential executions in the same runtime

### 5. WebAssembly guest execution

Add a tiny deterministic wasm fixture and validate:

- wasm context creation
- execution
- stdout or structured result
- parity with JS lifecycle events

Do not call browser support complete without a real browser wasm run.

### 6. Timing mitigation

Validate in a real browser worker:

- `freeze` mode freezes `Date.now()` and `performance.now()`
- `off` restores advancing clocks
- `SharedArrayBuffer` hiding/restoration behavior is correct across runs

This needs a real browser because worker global behavior and timer scheduling are part of the contract.

### 7. Control-channel hardening

Validate that guest code cannot:

- forge control messages
- reach raw control helpers
- break worker reuse
- bypass lifecycle handling

This should mirror the current positive tests, but in a real browser page.

### 8. Deterministic termination and cleanup

Validate:

- a hung execution can be killed
- worker is actually terminated
- sync-bridge state is reset
- a subsequent execution on the same page still works

### 9. Packaging and asset resolution

The E2E suite must load the runtime from built package artifacts, not source-only imports.

This should catch:

- wrong worker URL resolution
- missing wasm asset emission
- broken relative import paths
- broken package `exports`

### 10. Fail-closed unsupported paths

Explicitly verify failure for unsupported browser cases, for example:

- missing `SharedArrayBuffer`
- unsupported filesystem mode
- browser environment without required blocking bridge primitives

The spec requires fail-closed behavior. Test it directly.

## Test Harness Design

### Server requirements

The E2E server must:

- serve all assets same-origin
- set COOP/COEP headers on the happy-path route
- optionally omit those headers on a failure-path route
- serve the worker JS and wasm assets under stable URLs

The existing `packages/playground/backend/server.ts` is a good starting point and can either be reused or copied into a smaller runtime harness server.

### Page requirements

The harness page should expose a narrow JS API, for example:

```ts
type BrowserHarness = {
  init(options: {
    filesystem: "memory" | "opfs";
    timingMitigation?: "freeze" | "off";
  }): Promise<void>;
  exec(code: string, options?: {
    filePath?: string;
    cwd?: string;
    env?: Record<string, string>;
  }): Promise<{ code: number; stdout: string[]; stderr: string[] }>;
  terminate(): Promise<void>;
  reset(): Promise<void>;
};
```

The E2E runner should talk to this API through `page.evaluate(...)`, not through brittle DOM scraping.

### Fixture strategy

Keep fixtures small and deterministic:

- `fixtures/js/hello.js`
- `fixtures/js/relative-module.js`
- `fixtures/wasm/echo.wasm`
- `fixtures/wasm/add.wasm`

Avoid giant app fixtures. The goal is runtime validation, not UI realism.

## CI Plan

Recommended commands:

```bash
cargo test -p agent-os-sidecar-browser
cargo test -p agent-os-sidecar-browser-wasm
pnpm --dir packages/browser check-types
pnpm --dir packages/browser test
pnpm --dir packages/browser test:e2e
pnpm --dir packages/playground test
```

Recommended gating order:

1. Rust tests
2. package integration tests
3. browser E2E tests
4. playground smoke

If browser E2E is expensive, allow package integration tests to run on every change and full browser E2E on:

- PRs that touch `crates/sidecar-browser*`
- PRs that touch `packages/browser/*`
- PRs that touch browser worker or sync-bridge code

But before shipping browser runtime changes, the full browser E2E lane must pass.

## Risks And Open Questions

### 1. wasm bridge complexity

The Rust-to-JS boundary is the main implementation risk.

Mitigation:

- keep `crates/sidecar-browser` pure
- keep the wasm wrapper thin
- keep the JS bridge method-oriented

### 2. OPFS semantic gaps

OPFS is not POSIX.

Known limitations such as rename behavior should be:

- explicitly documented
- tested
- surfaced as clear unsupported behavior where needed

### 3. Browser compatibility surface

Not every browser environment can support the required blocking bridge semantics.

Mitigation:

- define Chromium as the first required target
- fail closed elsewhere if capability checks fail
- add extra browsers later only after Chromium is solid

### 4. UI coupling

If the runtime contract is only tested through the playground UI, runtime debugging becomes noisy and brittle.

Mitigation:

- keep a dedicated runtime harness page
- use the playground only as secondary smoke coverage

## Recommendation

Implement browser support by finishing the original `sidecar-browser` architecture, not by continuing to grow the current JavaScript-only browser runtime driver as if it were the final design.

Concretely:

1. Add a wasm wrapper around `crates/sidecar-browser`.
2. Reuse the current TS worker and sync-bridge code as the JS execution bridge layer.
3. Make `packages/browser` a thin public wrapper over that sidecar path.
4. Add a minimal real-browser E2E harness page.
5. Gate browser support on Chromium E2E that validates real worker boot, real `SharedArrayBuffer`, real OPFS persistence, JS guest execution, Wasm guest execution, timing mitigation, control-channel hardening, and deterministic cleanup.

That gets browser support onto the same architectural path the original spec intended and gives us a browser acceptance suite that can actually catch browser-specific failures.
