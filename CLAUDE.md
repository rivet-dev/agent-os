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
