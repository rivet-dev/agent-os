# agentOS Registry

WASM command packages for agentOS, split by Debian/apt naming conventions.

## Architecture

Each package in `software/` corresponds to a Debian package name and contains:
- `src/index.ts` -- exports a descriptor object with command metadata
- `wasm/` -- WASM command binaries (gitignored, populated by `make copy-wasm`)
- `dist/` -- compiled TypeScript output

### Package Types

1. **Command packages** (`software/{name}/`): contain WASM binaries and a descriptor
2. **Meta-packages** (`software/common/`, `software/build-essential/`): aggregate other packages via dependencies, no wasm/ directory

### Naming Convention

All published packages follow `@rivet-dev/agent-os-{apt-name}` where `{apt-name}` matches the corresponding Debian/apt package name. For tools without an apt equivalent, use the common CLI name.

| apt Package | Our Package | Commands |
|---|---|---|
| coreutils | @rivet-dev/agent-os-coreutils | sh, cat, ls, cp, mv, rm, sort, etc. (~80 commands + stubs) |
| sed | @rivet-dev/agent-os-sed | sed |
| grep | @rivet-dev/agent-os-grep | grep, egrep, fgrep |
| gawk | @rivet-dev/agent-os-gawk | awk |
| findutils | @rivet-dev/agent-os-findutils | find, xargs |
| diffutils | @rivet-dev/agent-os-diffutils | diff |
| tar | @rivet-dev/agent-os-tar | tar |
| gzip | @rivet-dev/agent-os-gzip | gzip, gunzip, zcat |
| curl | @rivet-dev/agent-os-curl | curl |
| wget | @rivet-dev/agent-os-wget | wget |
| zip | @rivet-dev/agent-os-zip | zip |
| unzip | @rivet-dev/agent-os-unzip | unzip |
| jq | @rivet-dev/agent-os-jq | jq |
| ripgrep | @rivet-dev/agent-os-ripgrep | rg |
| fd-find | @rivet-dev/agent-os-fd | fd |
| tree | @rivet-dev/agent-os-tree | tree |
| file | @rivet-dev/agent-os-file | file |
| sqlite3 | @rivet-dev/agent-os-sqlite3 | sqlite3 |
| (none) | @rivet-dev/agent-os-yq | yq |
| (none) | @rivet-dev/agent-os-codex | codex, codex-exec |
| git | @rivet-dev/agent-os-git | git (planned) |
| make | @rivet-dev/agent-os-make | make (planned) |

### Disabled packages (WASM binaries not built)

The following packages exist but **cannot be compiled** until a patched wasi-libc sysroot is built (`make sysroot` in `native/c/`). The vanilla wasi-sdk sysroot lacks `<netdb.h>` and other POSIX networking headers these programs need. The `make publish` and `copy-wasm` targets automatically skip packages with empty `wasm/` directories.

| Package | Reason |
|---|---|
| @rivet-dev/agent-os-wget | Needs `<netdb.h>` (patched wasi-libc) |
| @rivet-dev/agent-os-sqlite3 | Needs patched wasi-libc |
| @rivet-dev/agent-os-git | WASM binary not yet built |

To unblock the remaining C packages: run `cd native && ./scripts/patch-wasi-libc.sh` to build the patched sysroot, then `cd .. && make build-wasm-c copy-wasm`.
When rerolling `native/patches/wasi-libc/*.patch`, validate the series with strict `git apply` semantics on the pinned temp worktree instead of relying on `patch` fuzz or reverse fallbacks; later patches such as `0012-posix-spawn-cwd.patch` intentionally depend on earlier series entries being applied in order.

The published `@rivet-dev/agent-os-curl` package is currently backed by the Rust `native/crates/commands/curl/` binary built on `crates/libs/wasi-http`. Keep curl CLI compatibility fixes there until the patched-sysroot C curl path is restored.
When patching the OpenCode ACP Node bundle in `registry/agent/opencode/scripts/build-opencode-acp.mjs`, run result-returning SQLite PRAGMAs through `db.$client.exec(...)` instead of drizzle `db.run(...)`. The VM `node:sqlite` shim treats `journal_mode`, `busy_timeout`, `foreign_keys`, and `wal_checkpoint` as queries with rows, so `db.run(...)` breaks `createSession("opencode")` during database bootstrap.
OpenCode ACP bundle patches that touch `packages/opencode/src/util/filesystem.ts` should resolve absolute guest paths through `AGENT_OS_GUEST_PATH_MAPPINGS` before calling `node:fs`, or tool writes can report success while landing outside the mounted project on the host. When patching streamed LLM or tool execution paths, keep the current `Instance` restored around the async work itself, not just the ACP entrypoint, or VM runs will fail with `No context found for instance`.
For vendored agent-bundle rewrite scripts under `registry/agent/*/scripts/`, add an explicit post-patch assertion in the build script and a `node:test` that reads the generated `dist/` artifact. Upstream minified bundle changes can otherwise leave stale kill-switches or missing guards hidden until runtime.

### Meta-packages

| Package | Includes |
|---|---|
| @rivet-dev/agent-os-common | coreutils + sed + grep + gawk + findutils + diffutils + tar + gzip |
| @rivet-dev/agent-os-build-essential | common + make + git |
| @rivet-dev/agent-os-everything | all available software packages in a single bundle |

### Permission Tiers

Commands declare a default permission tier that controls WASI host imports:

| Tier | Capabilities | Examples |
|------|-------------|---------|
| `full` | Spawn processes, network I/O, file read/write | sh, bash, curl, wget, git, make, env, timeout, xargs |
| `read-write` | File read/write, no network or process spawning | sqlite3, chmod, cp, mv, rm, mkdir, touch, ln |
| `read-only` | File read-only, no writes, no spawn, no network | grep, cat, sed, awk, jq, ls, find, sort, head, tail |
| `isolated` | Restricted to cwd subtree reads only | (reserved for future use) |

### WASM Binary Format

- Files in `wasm/` have **NO .wasm extension**. The WasmVM driver uses the filename as the command name.
- Aliases (bash->sh, egrep->grep) are **full copies** of the target binary, not symlinks. npm publish does not preserve symlinks.
- Rust command source lives in `native/crates/commands/` with shared libraries in `native/crates/libs/`.
- C command source lives in `native/c/programs/`.
- All WASM binaries are built in-repo via `make build-wasm`. No external dependencies except Rust toolchain and wasi-sdk.
- If you patch a vendored Rust dependency under `native/vendor/`, add the same patch under `native/patches/crates/<crate>/` so `native/scripts/patch-vendor.sh` reapplies it on future rebuilds instead of silently losing the fix.
- When you rebuild a Rust command locally, the fresh artifacts are the top-level `native/target/wasm32-wasip1/release/<command>.wasm` files. `release/commands/<command>` can lag until the packaging/copy step rewrites the published command directory.
- In Ralph shells, repo-root `RUSTC` / `RUSTDOC` are often pinned to the stable workspace toolchain. When running `registry/native` commands that rely on its local nightly toolchain (for example `make patch-std` or `cargo build -Z build-std=...`), unset those env vars first or the patch/build step will hit the wrong toolchain.
- For vendored `brush-core` on WASI, command lookup must require `is_file()` before treating a PATH candidate as executable, and once the shell resolves a guest binary path (for example `/bin/printf`) it should spawn that resolved path instead of falling back to the bare command name. Pi prepends `~/.pi/agent/bin` even when it does not exist, so bare-name WASI lookup can fail on the first PATH entry.
- For ACP adapters that proxy a line-based child process, keep child stdout event handling serialized and make the child-side request waiter buffer out-of-order responses by request id. Multiple permission/tool requests from one model turn can resolve out of order, and dropping unmatched lines turns into flaky multi-tool failures.

### Descriptor Format

Each package exports a default descriptor object:

```typescript
import type { WasmCommandPackage } from "@rivet-dev/agent-os-registry-types";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));

const pkg = {
  name: "grep",
  aptName: "grep",
  description: "GNU grep pattern matching (grep, egrep, fgrep)",
  source: "rust" as const,
  commands: [
    { name: "grep", permissionTier: "read-only" as const },
    { name: "egrep", permissionTier: "read-only" as const, aliasOf: "grep" },
    { name: "fgrep", permissionTier: "read-only" as const, aliasOf: "grep" },
  ],
  get commandDir() {
    return resolve(__dirname, "..", "wasm");
  },
} satisfies WasmCommandPackage;

export default pkg;
```

The `satisfies` keyword with `import type` ensures the published `.d.ts` has no reference to the internal types package. The types package is a devDependency only.

### Versioning

All packages use date-based versioning: `0.0.{YYMMDDHHmmss}` (e.g., `0.0.260329143500`). The version is generated at publish time. All packages in a release share the same version.

## Commands

```bash
make build-wasm    # Build all WASM commands from native source
make copy-wasm     # Copy built binaries into per-package wasm/ directories
make build         # pnpm install + build TypeScript for all packages
make test          # Run tests
make publish-dry   # Dry-run publish (verifies package contents)
make publish       # Publish changed packages to npm (skips unchanged via hash cache)
make publish-force # Publish all packages regardless of cache
make publish-clean # Clear publish cache
make clean         # Remove dist/ and wasm/ from all packages
```

## Testing

- The root `registry/` package does not keep its own `node_modules/` tree in this workspace. Keep `registry/package.json` test execution routed through `scripts/run-vitest.mjs`, and keep `registry/vitest.config.ts` as a plain exported object instead of importing `vitest/config`, so `pnpm --dir registry test` resolves the workspace-installed Vitest CLI cleanly.
- External-network registry tests should stay behind `AGENTOS_E2E_NETWORK=1`, probe host connectivity up front so CI can skip cleanly when the internet is unavailable, and retry the in-VM command itself for transient outbound failures instead of hard-failing on the first flaky request.
- First-party registry Vitest files under `registry/tests/` should avoid `describe.skipIf` / `it.skipIf`; use conditional registration helpers instead, and make gated suites leave behind a no-op placeholder test so the file does not fail with `No test found in suite` when prerequisites are absent.
- For intentionally partial Wasm command implementations such as `registry/native/crates/libs/git`, reject unsupported subcommands or transport/auth variants with a stable typed error that points at the package README. Generic "not a command" or downstream HTTP failures make guest debugging much harder than an explicit compatibility boundary.
- C-built Wasm command suites under `registry/tests/wasmvm/` should gate on `hasCWasmBinaries(...)` and mount `createWasmVmRuntime({ commandDirs: [C_BUILD_DIR, COMMANDS_DIR] })`; those binaries are not guaranteed to be copied into `native/target/wasm32-wasip1/release/commands`, so mounting only `COMMANDS_DIR` produces false `command not found` failures.
- Negative-path npm registry tests should disable npm fetch retries and shorten fetch timeouts explicitly; the default retry budget can outlive the Vitest timeout and turn a clear network-error assertion into a false hang.
- Cross-runtime kernel networking tests should prefer shipped first-party command artifacts from `native/target/wasm32-wasip1/release/commands` plus explicit `loopbackExemptPorts` host fixtures over optional `native/c` programs, unless the story is specifically about the patched-sysroot C command surface.
- Cross-runtime terminal tests should assert user-visible output and prompt behavior, not require incidental diagnostic `WARN could not retrieve pid` lines; that warning is runtime-dependent noise, while the stable contract is that real stdout still appears.
- `tests/wasmvm/dynamic-module-integration.test.ts` exercises the embedded sidecar’s real cold-start/compile path now. Keep the module-cache assertions on an explicit `30_000ms` budget instead of the default 5s timeout or the file will report false negatives even when repeated-command caching is working.
- Embedded registry kernel command suites that boot the full WasmVM/Node path can spend 8-12s inside a single healthy command. Keep per-command and per-test timeouts aligned with the real embedded cold-start path instead of reusing stale 5s budgets from the pre-embedded runtime.
- For registry kernel package-CLI behavior tests, prefer invoking the installed CLI entrypoint directly (for example `node /node_modules/<pkg>/dist/bin/<cli>.js`) instead of `npx`; npm exec flag parsing can swallow package flags and turn a package-behavior test into an npm-wrapper test.
- WASI command shims that poll child-process state must sleep through `wasi_ext::host_sleep_ms()` instead of `std::thread::sleep()`: the host import blocks inside the VM kernel, while `std::thread::sleep()` returns immediately on wasm32-wasip1 and turns retry loops like `timeout` into CPU-burning busy-waits.
- For builtin-only Rust command stories under `registry/native/crates/libs/builtins`, use `cargo test -p secureexec-builtins` from `registry/native/` as the focused truth suite when the PRD still points at the legacy `agent-os-kernel --test wasm_commands` target, then pair it with a narrow `registry/tests/kernel/*` guest-shell smoke if shell behavior is part of acceptance.
- For subprocess-streaming fixes in `registry/native/crates/libs/shims`, put the timing-sensitive assertions in the corresponding command crate integration tests (`registry/native/crates/commands/*/tests`) and spawn the real built binary via `CARGO_BIN_EXE_*`; library unit tests cannot observe inherited-stdio behavior faithfully.

## Native Source

All WASM command source code lives in `native/`:
- `native/crates/commands/` -- Rust command crates (105 commands)
- `native/crates/libs/` -- shared Rust libraries (grep engine, awk engine, etc.)
- `native/crates/wasi-ext/` -- WASI extension traits. Host-import wrappers here, matching wasi-libc patches, and uucore stubs should validate every guest buffer length crossing (`usize` -> `u32`) and reject host-returned lengths that exceed the supplied buffer; `poll()` wrappers should also enforce the exact 8-byte-per-`pollfd` layout.
- `native/c/programs/` -- C command source (curl, wget, sqlite3, zip, unzip)
- `native/patches/` -- Rust std patches for WASI
- `native/Makefile` -- Rust build system
- `native/c/Makefile` -- C build system (downloads wasi-sdk automatically)

## Dependencies

- **Rust nightly toolchain**: Specified in `native/rust-toolchain.toml`
- **wasi-sdk**: Downloaded automatically by the C Makefile
- **Registry types**: `@rivet-dev/agent-os-registry-types` from `packages/registry-types/` (linked via each package's devDependencies). This is the single source of truth for `WasmCommandPackage`, `WasmMetaPackage`, and `PermissionTier` types. If you need to change descriptor types, edit `packages/registry-types/src/index.ts`.

## Adding a New Package

1. Create `software/{apt-name}/` with `package.json`, `tsconfig.json`, `src/index.ts`
2. Add the package to the Makefile's `CMD_PACKAGES` list
3. Add copy rules to the `copy-wasm` target
4. Set the correct permission tier for each command
5. If it belongs in `common` or `build-essential`, add it as a dependency in the meta-package
6. Run `make copy-wasm && make build && make test`

## Git

- **Commit messages**: Single-line conventional commits (e.g., `feat: add ripgrep package`). No body, no co-author trailers.
