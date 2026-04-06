# Test Structure Recommendation

## Current State

99 TypeScript test files + ~310 Rust tests across 5 crates. The main problem is `packages/core/tests/` — 57 files in a flat directory with no grouping. The Rust side is better but has a few monoliths and no fast/slow distinction.

## TypeScript: Target Structure

```
packages/core/tests/
├── unit/                          # No VM, no sidecar — pure logic tests
│   ├── host-tools-argv.test.ts
│   ├── host-tools-prompt.test.ts
│   ├── host-tools-shims.test.ts
│   ├── mount-descriptors.test.ts
│   ├── root-filesystem-descriptors.test.ts
│   ├── sidecar-permission-descriptors.test.ts
│   ├── sidecar-placement.test.ts
│   ├── os-instructions.test.ts
│   ├── cron-manager.test.ts
│   ├── cron-timer-driver.test.ts
│   ├── allowed-node-builtins.test.ts
│   ├── list-agents.test.ts
│   └── software-projection.test.ts
│
├── filesystem/                    # VM filesystem operations
│   ├── crud.test.ts               # (was filesystem.test.ts)
│   ├── move-delete.test.ts
│   ├── batch-ops.test.ts
│   ├── readdir-recursive.test.ts
│   ├── overlay.test.ts            # (was overlay-backend.test.ts)
│   ├── layers.test.ts
│   ├── mount.test.ts
│   ├── host-dir.test.ts
│   └── base-filesystem.test.ts
│
├── process/                       # Process execution, signals, trees
│   ├── execute.test.ts
│   ├── management.test.ts
│   ├── tree.test.ts
│   ├── all-processes.test.ts
│   ├── spawn-flat-api.test.ts
│   └── shell-flat-api.test.ts
│
├── session/                       # ACP session lifecycle and protocol
│   ├── lifecycle.test.ts
│   ├── events.test.ts
│   ├── capabilities.test.ts
│   ├── mcp.test.ts
│   ├── cancel.test.ts
│   ├── protocol.test.ts          # (was acp-protocol.test.ts)
│   └── e2e.test.ts               # (merge session.test.ts + session-comprehensive + session-mock-e2e)
│
├── agents/                        # Per-agent adapter tests
│   ├── pi/
│   │   ├── headless.test.ts
│   │   ├── acp-adapter.test.ts
│   │   ├── sdk-adapter.test.ts
│   │   └── tool-llmock.test.ts
│   ├── claude/
│   │   ├── investigate.test.ts
│   │   ├── sdk-adapter.test.ts
│   │   └── session.test.ts
│   ├── opencode/
│   │   ├── acp.test.ts
│   │   ├── headless.test.ts
│   │   └── session.test.ts
│   └── codex/
│       └── session.test.ts
│
├── wasm/                          # WASM command and permission tests
│   ├── commands.test.ts
│   └── permission-tiers.test.ts
│
├── network/
│   ├── network.test.ts
│   └── host-tools-server.test.ts
│
├── sidecar/
│   ├── client.test.ts
│   └── native-process.test.ts
│
├── cron/
│   └── integration.test.ts
│
└── helpers/                       # Shared test utilities (stays as-is)
```

### Registry tests

```
registry/tests/
├── e2e/                           # Rename kernel/ → e2e/ for clarity
│   ├── npm/                       # Group the 9 npm e2e tests
│   │   ├── install.test.ts
│   │   ├── scripts.test.ts
│   │   ├── suite.test.ts
│   │   ├── lifecycle.test.ts
│   │   ├── version-init.test.ts
│   │   ├── npx-and-pipes.test.ts
│   │   ├── concurrently.test.ts
│   │   ├── nextjs-build.test.ts
│   │   └── project-matrix.test.ts
│   ├── cross-runtime/             # Group the 3 cross-runtime tests
│   │   ├── network.test.ts
│   │   ├── pipes.test.ts
│   │   └── terminal.test.ts
│   ├── bridge-child-process.test.ts
│   ├── ctrl-c-shell-behavior.test.ts
│   ├── dispose-behavior.test.ts
│   ├── error-propagation.test.ts
│   ├── exec-integration.test.ts
│   ├── fd-inheritance.test.ts
│   ├── module-resolution.test.ts
│   ├── node-binary-behavior.test.ts
│   ├── signal-forwarding.test.ts
│   ├── tree-test.test.ts
│   └── vfs-consistency.test.ts
├── wasmvm/                        # Already well organized — keep as-is
├── projects/                      # Fixtures — keep as-is
└── smoke.test.ts
```

## Rust: Target Structure

The per-crate layout is already good. The changes are surgical:

### Split `execution/tests/javascript.rs` (46 tests)

```
crates/execution/tests/
├── javascript/
│   ├── mod.rs                     # common setup
│   ├── builtin_interception.rs    # require('fs') → polyfill routing
│   ├── module_resolution.rs       # ESM/CJS loading, import paths
│   ├── env_hardening.rs           # env stripping, process proxy, guest env
│   └── sync_rpc.rs                # sync RPC bridge, timeouts
├── python.rs                      # (15 tests — fine as-is)
├── python_prewarm.rs              # (2 tests — fine as-is)
├── wasm.rs                        # (20 tests — fine as-is)
├── permission_flags.rs            # (6 tests — fine as-is)
├── benchmark.rs
└── smoke.rs
```

### Mark slow sidecar integration tests

Tests that spawn real sidecar processes (`crash_isolation`, `session_isolation`, `vm_lifecycle`, `process_isolation`) should use `#[ignore]`:

```rust
#[test]
#[ignore] // spawns sidecar process — run with: cargo test -- --ignored
fn crash_isolation() { ... }
```

This lets `cargo test` stay fast; CI runs `cargo test -- --include-ignored`.

### Keep kernel/tests/ as-is

The 1-file-per-subsystem pattern (vfs, fd_table, process_table, pipe_manager, etc.) already maps cleanly to kernel modules. No changes needed.

### Summary

| Crate | Status | Action |
|-------|--------|--------|
| `kernel/tests/` (19 files, 161 tests) | Good — 1:1 with subsystems | Keep as-is |
| `execution/tests/` (8 files, 95 tests) | `javascript.rs` is a monolith | Split into submodule |
| `sidecar/tests/` (14 files, 49 tests) | Mixes fast/slow | `#[ignore]` on integration tests |
| `bridge/tests/` (2 files, 1 test) | Fine | Keep as-is |
| `sidecar-browser/tests/` (3 files, 5 tests) | Fine | Keep as-is |

## Migration Approach

This should be done incrementally, one directory at a time:

1. Create subdirectories and move files (git mv preserves history)
2. Update vitest config globs / Cargo test paths after each move
3. Verify CI passes after each batch
4. Do not combine restructuring with functional changes in the same PR
