# agentOS

A high-level wrapper around the Agent OS runtime that provides a clean API for running coding agents inside isolated VMs via the Agent Communication Protocol (ACP).

## Agent OS Runtime

Agent OS is a **fully virtualized operating system**. The kernel, written as a Rust sidecar, provides a complete POSIX-like environment -- virtual filesystem, process table, socket table, pipe/PTY management, and permission system. Guest code sees a self-contained OS and must never interact with the host directly. Every system call (file I/O, networking, process spawning, DNS resolution) must be mediated by the kernel. No guest operation may fall through to a real host syscall.

**⚠️ CRITICAL: ALL guest code MUST execute inside the kernel with ZERO host escapes.** The three execution environments (WASM, Node.js/V8 isolates, Python/Pyodide) must all run within the kernel's isolation boundary. No runtime may spawn unsandboxed host processes, touch real host filesystems, open real network sockets, or call real Node.js builtins. **NEVER use `Command::new("node")` for guest execution — not even temporarily, not behind a flag.** Guest JS runs in V8 isolates (`crates/v8-runtime/`). If tests fail because they assume the old host-process model, fix or delete the tests. See `crates/execution/CLAUDE.md` for details.

- **Virtualization invariants, key subsystems, and Rust architecture rules** -- see `crates/CLAUDE.md`
- **Node.js isolation model, polyfill rules, Python execution** -- see `crates/execution/CLAUDE.md`
- **Linux compatibility, VFS design, filesystem conventions** -- see `crates/kernel/CLAUDE.md`
- **Agent sessions (ACP), testing, debugging policy** -- see `packages/core/CLAUDE.md`
- **Registry packages (software, agents, file-systems, tools)** -- see `registry/CLAUDE.md`

## Project Structure

- **Monorepo**: pnpm workspaces + Turborepo + TypeScript + Biome
- **Core package**: `@rivet-dev/agent-os-core` in `packages/core/` -- contains everything (VM ops, ACP client, session management)
- **Use the renamed core package everywhere**: workspace dependencies and TypeScript subpath imports must reference `@rivet-dev/agent-os-core` (including `@rivet-dev/agent-os-core/internal/runtime-compat` and `@rivet-dev/agent-os-core/test/*`). The legacy `@rivet-dev/agent-os` name is stale and breaks pnpm workspace resolution.
- **Registry types**: `@rivet-dev/agent-os-registry-types` in `packages/registry-types/` -- shared type definitions for WASM command package descriptors. The registry software packages link to this package. When changing descriptor types, update here and rebuild the registry.
- **npm scope**: `@rivet-dev/agent-os-*`
- **Actor integration** lives in the Rivet repo at `rivetkit-typescript/packages/rivetkit/src/agent-os/`, not as a separate package
- **The actor layer must maintain 1:1 feature parity with AgentOs.** Every public method on the `AgentOs` class (`packages/core/src/agent-os.ts`) must have a corresponding actor action in the Rivet repo's `rivetkit-typescript/packages/rivetkit/src/agent-os/`. Subscription methods are wired through actor events. Lifecycle methods are handled by the actor's onSleep/onDestroy hooks. This includes changes to method signatures, option types, return types, and configuration interfaces. **Always ask the user which Rivet repo/path to update** (e.g., `~/r-aos`, `~/r16`, etc.) before making changes there.
- **The RivetKit driver test suite must have full feature coverage of all agent-os actor actions.** Tests live in the Rivet repo's `rivetkit-typescript/packages/rivetkit/src/driver-test-suite/tests/`. When adding a new actor action, add a corresponding driver test in the same change.
- **The core quickstart (`examples/quickstart/`) and the RivetKit example (in the Rivet repo at `examples/agent-os/`) must stay in sync.** Both cover the same set of features with identical behavior, just different APIs.

## Terminology

- Call instances of the OS **"VMs"**, never "sandboxes"

## Architecture

- **The VM base filesystem artifact is derived from Alpine Linux, but runtime source should stay generic.** `packages/core/src/` must not hardcode Alpine-specific defaults. The runtime consumes `packages/core/fixtures/base-filesystem.json` as the default root layer.
- **Base filesystem rebuild flow:** `pnpm --dir packages/core snapshot:alpine-defaults` writes `alpine-defaults.json`, then `pnpm --dir packages/core build:base-filesystem` rewrites AgentOs-specific values and emits `base-filesystem.json`.
- **The default VM filesystem model should be Docker-like.** Layered overlay view with one writable upper layer on top of one or more immutable lower snapshot layers.
- **Everything runs inside the VM.** Agent processes, servers, network requests -- all spawned inside the Agent OS kernel, never on the host. This is a hard rule with no exceptions.

## Secure-Exec Reference Implementation

The Rust sidecar kernel was migrated from a working JavaScript kernel (`@secure-exec/core` + `@secure-exec/nodejs` + `@secure-exec/v8`). The original source is at `/home/nathan/secure-exec-1/` (tagged `v0.2.1`), and recovered polyfill/bridge code lives at `.agent/recovery/secure-exec/`. **When something doesn't work in the Rust V8 isolate runtime, check how secure-exec handled it first** — the answer is almost always already there. Key reference files:
- `nodejs/src/bridge-handlers.ts` (6,405 lines) -- host-side handlers for all kernel syscalls
- `nodejs/src/bridge/fs.ts` (3,974 lines) -- full kernel-backed `fs` polyfill
- `nodejs/src/bridge/network.ts` (11,149 lines) -- full `net`/`dgram`/`dns` polyfill
- `nodejs/src/bridge/process.ts` (2,251 lines) -- virtualized `process` global
- `nodejs/src/execution-driver.ts` (1,693 lines) -- V8 isolate session lifecycle

## V8 Polyfill and Module System Rules

- **Use `node-stdlib-browser` for pure-JS builtins, NOT hand-written stubs.** The package is already in `packages/core/package.json`. Bundle it into `v8-bridge.js` for modules like `path`, `assert`, `util`, `events`, `stream`, `buffer`, `url`, `querystring`, `string_decoder`, `punycode`, `constants`, `zlib`. Only write custom bridge-backed polyfills for kernel-backed modules (`fs`, `net`, `child_process`, `dns`, `http`, `os`, `crypto`). This is how secure-exec did it. Hand-written stubs are incomplete and break real packages.
- **Use undici for fetch(), not a high-level bridge call.** Guest `fetch()` must use undici running inside the V8 isolate, making TCP connections through the kernel socket table (`net.connect` bridge). Do NOT use `_networkFetchRaw` which bypasses the kernel network stack, permissions, and DNS. The fetch path must be: `undici → net.connect → kernel socket table → host network adapter`. This matches how real Node.js works.
- **Every Node.js builtin module must be a COMPLETE implementation, not a stub.** If `require('path')` is supported, it must have ALL standard methods (normalize, resolve, relative, join, dirname, basename, extname, isAbsolute, sep, delimiter, parse, format). A module that only implements `join` and `resolve` is a stub — stubs cause silent failures in real packages. If you can't implement a method fully, throw `ERR_NOT_IMPLEMENTED` — never return undefined or silently skip.
- **CJS export extraction must handle dynamic patterns.** The ESM wrapper for CJS modules extracts named exports via `extract_cjs_export_names()`. This MUST handle: `exports.X = ...`, `Object.defineProperty(exports, ...)`, `Object.assign(module.exports, ...)`, and spread syntax. If static extraction fails, fall back to runtime extraction (evaluate module, enumerate `Object.keys(module.exports)`). Incomplete extraction causes missing named imports that silently break downstream packages.
- **CJS/ESM interop must never hang.** If `require()` is called on an ESM-only package, throw `ERR_REQUIRE_ESM` immediately — never recurse infinitely or hang. If `import()` is called on a CJS package, wrap it in an ESM shim. Test both directions.
- **Circular dependencies must terminate.** The module cache must prevent re-evaluation. Test with A→B→A and A→B→C→A chains.
- **Every polyfill addition needs a conformance test.** When adding a new builtin method or module, add a test that verifies the return value matches real Node.js behavior. Tests go in `crates/execution/tests/` or `crates/sidecar/tests/`.

## npm Package Compatibility

- **npm packages must work UNMODIFIED inside the VM.** The V8 module resolver must load published npm packages from `node_modules/` as-is — no esbuild, no bundling, no transpilation, no preprocessing. If `require('some-package')` or `import 'some-package'` doesn't work, fix the module resolver or polyfills, don't add a build step to transform the package. The goal is: `npm install` a package on the host, mount `node_modules/` into the VM, and it just works.
- **Agent SDKs must run unmodified.** Pi SDK (`@mariozechner/pi-coding-agent`), Anthropic SDK (`@anthropic-ai/sdk`), and any other agent SDK must load and execute inside V8 without modification. Our custom ACP adapters (`registry/agent/*/`) are thin wrappers that import the SDK — the SDK itself is never patched or bundled.

## Agent Adapters

- **Agent adapters MUST use the real agent SDK.** Each agent adapter (`registry/agent/*/src/adapter.ts`) must call the agent's SDK directly (e.g., `createAgentSession()` from `@mariozechner/pi-coding-agent`). **NEVER replace an SDK adapter with a minimal/stub adapter that makes direct API calls** (e.g., direct `fetch` to `/v1/messages`). If the SDK doesn't work in V8, fix the V8 compatibility — don't bypass the SDK.
- **No host agent exceptions.** Host-native wrappers and host binary launch paths are not allowed.
- **Claude patched SDK/CLI artifacts are discovered via dist manifests.** `registry/agent/claude/scripts/build-patched-cli.mjs` writes `dist/claude-cli-patched.json` and `dist/claude-sdk-patched.json`; the adapter resolves those manifests first and only falls back to the upstream SDK files when they are missing. Update the build script/manifests rather than hardcoding hashed artifact paths in the adapter.

## VM System Tools

- **The VM has a full POSIX toolchain.** WASM-compiled coreutils, `sh`, `grep`, `sed`, `awk`, `find`, `tar`, `git`, and 100+ other commands are available via registry software packages (`registry/software/`, compiled from `registry/native/crates/commands/`). Agent code running inside the VM can spawn these tools via `child_process`. **Do not assume system tools are missing** — if a command isn't resolving, debug the command resolution path in the sidecar, don't work around it.

## Dependencies

- **Rivet repo** -- A modifiable copy lives at `~/r-aos`. Use this when you need to make changes to the Rivet codebase.
- Mount host `node_modules` read-only for agent packages (pi-acp, etc.)

## Documentation

- **Keep docs in `~/r-aos/docs/docs/agent-os/` up to date** when public API methods or types are added, removed, or changed on AgentOs or Session classes.
- **Keep the standalone `secure-exec` docs repo up to date** when exported API methods, types, or package-level behavior change for public `secure-exec` compatibility packages. The source of truth is the repo that contains `docs/docs.json`.
- **The active public `secure-exec` package scope is currently `secure-exec` and `@secure-exec/typescript`.** Do not assume other legacy `@secure-exec/*` packages are still part of the maintained public surface unless the user explicitly says so.
- **If a user asks for a `secure-exec` change without naming the package, prompt them to choose the target public package when it is ambiguous.**
- **Keep `website/src/data/registry.ts` up to date.** When adding, removing, or renaming a package, update this file so the website reflects the current set of available apps.
- **No implementation details in user-facing docs.** Never mention WebAssembly, WASM, V8 isolates, Pyodide, or SQLite VFS in documentation outside of `architecture.mdx`. Use user-facing language instead.

## Agent Working Directory

All agent working files live in `.agent/` at the repo root.

- **Specs**: `.agent/specs/` -- design specs and interface definitions for planned work.
- **Research**: `.agent/research/` -- research documents on external systems, prior art, and design analysis.
- **Todo**: `.agent/todo/*.md` -- deferred work items with context on what needs to be done and why.
- **Notes**: `.agent/notes/` -- general notes and tracking.

When the user asks to track something in a note, store it in `.agent/notes/` by default. When something is identified as "do later", add it to `.agent/todo/`. Design documents and interface specs go in `.agent/specs/`.

## CLAUDE.md Convention

- Every directory that has a `CLAUDE.md` must also have an `AGENTS.md` symlink pointing to it (`ln -s CLAUDE.md AGENTS.md`). This ensures other AI agents that look for `AGENTS.md` find the same instructions.

## Git

- **Commit messages**: Single-line conventional commits (e.g., `feat: add host tools RPC server`). No body, no co-author trailers.

## Build & Dev

```bash
pnpm install
pnpm build        # turbo run build
pnpm test         # turbo run test
pnpm check-types  # turbo run check-types
pnpm lint         # biome check
```

- When changing V8 bridge registration or snapshot bootstrap code under `crates/v8-runtime/`, rebuild `agent-os-v8` before rerunning sidecar V8 integration tests. `cargo test -p agent-os-sidecar` can reuse an older `target/debug/agent-os-v8` binary.
- The `crates/v8-runtime` snapshot test (`snapshot::tests::snapshot_consolidated_tests`) currently has to run in isolation: use `cargo test -p agent-os-v8-runtime -- --test-threads=1` for the main suite and `cargo test -p agent-os-v8-runtime snapshot::tests::snapshot_consolidated_tests -- --exact --ignored` separately until the shared test binary teardown SIGSEGV is fixed.
