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

The published `@rivet-dev/agent-os-curl` package is currently backed by the Rust `native/crates/commands/curl/` binary built on `crates/libs/wasi-http`. Keep curl CLI compatibility fixes there until the patched-sysroot C curl path is restored.
When patching the OpenCode ACP Node bundle in `registry/agent/opencode/scripts/build-opencode-acp.mjs`, run result-returning SQLite PRAGMAs through `db.$client.exec(...)` instead of drizzle `db.run(...)`. The VM `node:sqlite` shim treats `journal_mode`, `busy_timeout`, `foreign_keys`, and `wal_checkpoint` as queries with rows, so `db.run(...)` breaks `createSession("opencode")` during database bootstrap.
OpenCode ACP bundle patches that touch `packages/opencode/src/util/filesystem.ts` should resolve absolute guest paths through `AGENT_OS_GUEST_PATH_MAPPINGS` before calling `node:fs`, or tool writes can report success while landing outside the mounted project on the host. When patching streamed LLM or tool execution paths, keep the current `Instance` restored around the async work itself, not just the ACP entrypoint, or VM runs will fail with `No context found for instance`.

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
- For vendored `brush-core` on WASI, command lookup must require `is_file()` before treating a PATH candidate as executable, and once the shell resolves a guest binary path (for example `/bin/printf`) it should spawn that resolved path instead of falling back to the bare command name. Pi prepends `~/.pi/agent/bin` even when it does not exist, so bare-name WASI lookup can fail on the first PATH entry.

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

- External-network registry tests should stay behind `AGENTOS_E2E_NETWORK=1`, probe host connectivity up front so CI can skip cleanly when the internet is unavailable, and retry the in-VM command itself for transient outbound failures instead of hard-failing on the first flaky request.

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
