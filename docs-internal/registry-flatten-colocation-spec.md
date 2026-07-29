# Spec: Flatten `registry/` → `software/` + colocated `native/`

Status: proposal · Owner: registry · Last updated: 2026-07-07

## Summary

Eliminate the `registry/` wrapper. The package catalog becomes a top-level
`software/` directory where **every `@agentos-software/*` package — command
packages, agent adapters, and bundles — is one flat folder** that owns its
manifest, TS descriptor, e2e test, **and its own command source** under
`native/`. Shared WASM build infrastructure that no single package can own
(patched sysroot, shared runtime-shim crates, build orchestration, libc
conformance) moves to a top-level **`toolchain/`**.

In the same effort, push the generic package-authoring capabilities that live
only in this repo's `Makefile`/`justfile` today — **compiling** a command to WASM
and **testing** it in a VM — into the published `agentos-toolchain` CLI, so the
`justfile` is left with only repo-specific orchestration.

Result: `registry/` disappears; `software/<pkg>/` is self-describing; the
misleading "libc tests under registry" problem is gone; and authoring an agentOS
package is the same flow for us and for external users.

## Motivation

Today `registry/` mixes three unlike things: package manifests (`software/`,
`agent/`), the command **source + WASM toolchain** (`native/`), and libc/sysroot
**conformance tests** (`native/tests/`). "Registry" reads as "the catalog of
packages," so a C/Rust cross-compilation toolchain and libc conformance tests
living under it is misleading. Package source also lives far from the package
(`software/curl/` has the manifest; `curl.c` is in `registry/native/c/programs/`).

## Target file structure

```
repo-root/
├── software/                      ← the entire @agentos-software/* catalog (flat, no registry/)
│   │
│   ├── curl/                      # C-based command package
│   │   ├── package.json           #   @agentos-software/curl
│   │   ├── agentos-package.json   #   manifest + registry block (title, description, category, kind)
│   │   ├── src/index.ts           #   TS descriptor
│   │   ├── test/curl.test.ts      #   e2e integration test
│   │   ├── native/
│   │   │   └── c/
│   │   │       ├── curl.c         #   command implementation
│   │   │       └── overlay/       #   pkg-specific upstream overlay (was c/curl-upstream-overlay/)
│   │   └── bin/curl               #   staged build output (gitignored)
│   │
│   ├── git/                       # Rust-based command package
│   │   ├── package.json  agentos-package.json  src/  test/  bin/
│   │   └── native/crates/
│   │       ├── cmd-git/           #   command crate  (was native/crates/commands/git)
│   │       └── git/               #   1:1 lib crate  (was native/crates/libs/git)
│   │
│   ├── coreutils/                 # multi-command package (sh + ~80 utils)
│   │   ├── package.json  agentos-package.json  src/  test/  bin/
│   │   └── native/crates/
│   │       ├── sh/ cat/ ls/ cp/ mv/ sort/ …   #   ~80 command crates
│   │       └── (du/ expr/ column/ rev/ strings/  — 1:1 libs that belong to coreutils)
│   │
│   ├── sqlite3/ duckdb/ wget/ zip/ unzip/               # C-based, native/c/<name>.c
│   ├── grep/ sed/ gawk/ jq/ yq/ fd/ ripgrep/ tree/ file/ tar/ gzip/ diffutils/ findutils/  # Rust, native/crates/
│   │
│   ├── claude/ codex/ opencode/ pi/ pi-cli/    # agent adapters — @agentos-software/*, JS only, no native/
│   │   ├── package.json  agentos-package.json (kind: "agent")  src/adapter.ts  test/
│   │
│   ├── vim/                                     # editor (C in native/c/)
│   └── browserbase/ build-essential/ common/ everything/   # meta/external — manifest only, no native/
│
├── toolchain/                     ← shared WASM build infra (NOT a package; nothing here is @agentos-software/*)
│   ├── Cargo.toml                 #   WASM cargo workspace root — members glob into software/*/native/crates/*
│   ├── Cargo.lock
│   ├── rust-toolchain.toml        #   nightly + wasm32-wasip1 + build-std
│   ├── Makefile                   #   builds every software/*/native/ against the shared sysroot
│   ├── crates/                    #   SHARED rust crates used by ≥2 packages / infra
│   │   ├── shims/ builtins/ stubs/ wasi-ext/
│   │   ├── wasi-http/ wasi-pty/ wasi-spawn/
│   │   └── reqwest-shim/ portable-pty-wasi/ codex-network-proxy{,-wasi}/ codex-otel/ uucore/ ctrlc/ hostname/
│   ├── sysroot/                   #   patched wasi-sdk build (was native/c/{include,patches,cmake,scripts} + wasi-sdk)
│   │   ├── wasi-sdk/              #     fetched
│   │   ├── include/ patches/ cmake/ scripts/
│   │   └── libs-cache/           #     fetched upstream C sources (sqlite, zlib, curl, duckdb)
│   ├── std-patches/              #   rust std patches 0001–0009  (was native/patches/)
│   ├── scripts/                  #   patch-std.sh  patch-vendor.sh  patch-wasi-libc.sh
│   ├── test-programs/            #   C test-program fixtures (tcp_server, udp_echo, signal_handler, http_server, …)
│   │                             #     built here; runtime-core integration tests consume the built binaries
│   ├── conformance/              #   libc/os-test/c-parity — tests the sysroot, not a package (was native/tests/)
│   │   ├── c-parity.test.ts  libc-test-conformance.test.ts  os-test-conformance.test.ts  *-exclusions.json
│   └── target/                   #   shared cargo build output (gitignored)
│
├── test-harness/                 ← private workspace pkg; shared vitest helpers + WASM VM runtime + binary resolution
│   ├── package.json              #   @rivet-dev/agentos-test-harness (private, not published)
│   └── src/{helpers,terminal-harness}.ts
│
└── packages/
    └── runtime-core/
        └── tests/integration/    #   VM integration tests (net, npm-e2e, wasi, signal, cross-runtime)
```

All native **compilation stays in `toolchain/`** — command source lives with its
package for editing, but nothing compiles WASM outside the toolchain build. C
test-program fixtures stay in `toolchain/test-programs/` (not scattered into
`packages/`) so there is exactly one place that invokes the C compiler + sysroot.

## Boundary rules

1. **`software/<pkg>/` owns**: `package.json`, `agentos-package.json`, `src/`
   (TS descriptor), `test/` (e2e), `native/` (this package's command source),
   `bin/` (staged output).
2. **`toolchain/` owns**: the patched sysroot, shared runtime-shim crates, std
   patches, build orchestration, and libc/sysroot conformance tests. Nothing in
   `toolchain/` is an `@agentos-software/*` package.
3. **Command source colocates; shared infra does not.** A crate used by exactly
   one package moves into that package; a crate used by ≥2 packages (or that is
   pure infra) stays in `toolchain/crates/`. This is the honest limit of
   colocation — see [Shared vs per-package](#shared-vs-per-package-split).
4. **Agents are packages too.** `claude`/`codex`/`opencode`/`pi`/`pi-cli` live
   under `software/` because they publish as `@agentos-software/*`. They carry
   `kind: "agent"` in the manifest and have no `native/`. Command packages carry
   `kind: "software"` (or omit; default). This `kind` field is what distinguishes
   them — not folder location.
5. **`native/` holds the REAL upstream tool, patched — never a reimplementation.**
   A command package's `native/` must build the genuine upstream program (GNU
   coreutils, real `curl`/`git`/`jq`, GNU grep/sed/gawk/tar/gzip/diffutils, …)
   fetched + pinned and patched for WASI — not a from-scratch Rust/C rewrite, a
   stub, or a hand-rolled CLI over a library. The only exception is a tool whose
   canonical upstream *is* the Rust project (`ripgrep`, `fd`). Several current
   commands violate this (coreutils=uutils, grep, curl driver, and the
   `agentos-*` rewrites) — tracked in `registry-parity-worklist.md`
   Cross-cutting #0; new packages must not add more.

## What moves where

| Current | Destination |
|---|---|
| `software/<pkg>/` | `software/<pkg>/` |
| `software/<pkg>/` | `software/<pkg>/` (+ `kind: "agent"`) |
| `registry/native/crates/commands/<cmd>` | `software/<owner>/native/crates/cmd-<cmd>/` |
| `registry/native/crates/libs/<x>` (1:1) | `software/<owner>/native/crates/<x>/` |
| `registry/native/crates/libs/<x>` (shared ≥2) | `toolchain/crates/<x>/` |
| `registry/native/crates/{wasi-ext,libs/{shims,builtins,stubs,wasi-http,wasi-pty,wasi-spawn}}` | `toolchain/crates/` |
| `registry/native/stubs/*` | `toolchain/crates/` |
| `registry/native/c/programs/<cmd>.c` (a package command) | `software/<owner>/native/c/<cmd>.c` |
| `registry/native/c/programs/<test-prog>.c` (tcp_server, udp_echo, signal_handler, …) | `toolchain/test-programs/` (built by toolchain; consumed by runtime-core integration tests via the binary path) |
| `registry/native/c/{include,patches,cmake,scripts,vim overlay}` + wasi-sdk | `toolchain/sysroot/` |
| `registry/native/patches/` (std) | `toolchain/std-patches/` |
| `registry/native/scripts/` | `toolchain/scripts/` |
| `registry/native/tests/` (conformance) | `toolchain/conformance/` |
| `registry/native/{Cargo.toml,Cargo.lock,rust-toolchain.toml,Makefile}` | `toolchain/` |
| `registry/tests/` (empty leftover) | delete |

The command→package owner is derived mechanically from each
`agentos-package.json` `commands` array — the migration script computes it, no
hand-mapping.

## Shared vs per-package split

Verified against the current tree:

- **Shared → `toolchain/crates/`** (used by ≥2 commands or pure infra):
  `shims` (7 users), `builtins` (3), `wasi-http` (4), `wasi-spawn` (2),
  `stubs`, `wasi-ext`, `wasi-pty`, and all `stubs/*` shim crates
  (`reqwest-shim`, `portable-pty-wasi`, `codex-network-proxy{,-wasi}`,
  `codex-otel`, `uucore`, `ctrlc`, `hostname`).
- **Per-package → `software/<owner>/native/crates/`** (1:1): `grep`→grep,
  `awk`→gawk, `jq`→jq, `yq`→yq, `git`→git, `fd`→fd, `tar`→tar, `tree`→tree,
  `gzip`→gzip, `diff`→diffutils, `find`→findutils, `file-cmd`→file, and
  `du`/`expr`/`column`/`rev`/`strings-cmd`→coreutils.

## Test execution & harness

The old `agentos-registry` package and its `run-vitest.mjs` shim (which resolved
vitest out of the outer store) **dissolve**. Replacement:

- A private workspace package **`test-harness/`** (`@rivet-dev/agentos-test-harness`, not
  published) owns the shared helpers, terminal harness, and `createWasmVmRuntime`
  — the surface every test imports today from `registry/tests/helpers.ts`.
- Each `software/<pkg>` gets a `test` script that runs **`agentos-toolchain test`**
  (see [Toolchain CLI boundary](#toolchain-cli-vs-justfile-boundary)), which drives
  the `test/` suite through the harness in a VM — the same runner external authors
  use. `turbo test` discovers and runs them — this is what re-attaches the suites
  to CI (fixing the current orphan + `@xterm/headless`-resolution bugs, since every
  runner is now a workspace member). The `@rivet-dev/agentos-test-harness` package supplies
  `createWasmVmRuntime` + helpers that both the CLI runner and the raw `*.test.ts`
  files import.
- Command binaries are resolved from `toolchain/target` (Rust) and the toolchain
  C build dir via `AGENTOS_WASM_COMMANDS_DIR` / `AGENTOS_C_WASM_COMMANDS_DIR`,
  set once in the harness. No relative-path coupling to the build tree.
- `runtime-core/tests/integration/` and `toolchain/conformance/` import the same
  `@rivet-dev/agentos-test-harness`.

## Leftover `registry/` files

When `registry/` is deleted, its non-package contents relocate:

| Current | Destination |
|---|---|
| `software/package.json` (`agentos-registry`) + `registry/scripts/run-vitest.mjs` | removed — superseded by per-package `test` scripts + `test-harness/` |
| `registry/tsconfig.base.json` | `software/tsconfig.base.json` (shared by package `tsconfig`s) |
| `registry/CONTRIBUTING.md`, `registry/README.md` | `software/` (or `docs-internal/`), updated for the new layout |

## Orphan commands (no owning package)

`envsubst` ships a command + a passing test but **no `software/*` package
declares it**. Before executing, it must get a home — one of: (a) fold into an
existing package's `commands` (e.g. `coreutils`), (b) create `software/envsubst/`,
or (c) reclassify its `.c` + test as a `toolchain/test-programs` fixture. Same
check applies to any other command with a binary/test but no manifest owner —
the migration script lists them; each needs an explicit decision, none are
silently dropped.

## Cargo workspace strategy (the critical detail)

There are two Rust workspaces today: the **main** repo workspace (repo-root
`Cargo.toml`, explicit members, stable toolchain) and the **wasm** workspace
(`registry/native/Cargo.toml`, nightly + build-std). They must stay separate —
different toolchains.

After the move, colocated crates live at `software/*/native/crates/*`, which is
inside the **main** workspace's directory tree. Cargo resolves a crate's
workspace by walking up to the nearest `[workspace]`, so those crates would be
captured by the repo-root workspace — wrong toolchain. Fix with two edits:

1. **Repo-root `Cargo.toml`**: add `exclude = ["software"]` so the main
   workspace never claims package `native/` crates.
2. **`toolchain/Cargo.toml`**: `[workspace]` with
   `members = ["crates/*", "../software/*/native/crates/*"]`. Cargo permits
   members outside the workspace root via relative path; this private,
   never-published workspace is a valid use of it.

All wasm builds run against the toolchain workspace explicitly
(`cargo build --manifest-path toolchain/Cargo.toml …`, driven by
`toolchain/Makefile`). Cargo's shared `target/` lives at `toolchain/target/`.

**Fallback if cross-dir members prove fragile in practice:** colocate only the
**C sources + tests + manifest**, and keep the **Rust command crates** together
in `toolchain/crates/commands/`. Less pure, but sidesteps the workspace-spanning
entirely (C has no cargo-workspace concern). This is the one decision to lock
before executing — see [Risks](#risks--decision).

## Reference updates (~40 sites, mechanical)

- `software/*/package.json` build scripts:
  `../../native/target/...` → `../../../toolchain/target/...`
  (and the C build-output dir similarly).
- `justfile`: repoint **and rename** every `registry-*` recipe (the `registry`
  prefix is dead once the folder is gone). Rename by the dir the recipe acts on —
  `software-*` for package builds, `toolchain-*` for the WASM build/commands:

  | Current recipe | New recipe | Body change |
  |---|---|---|
  | `registry-build` | `software-build` | `pnpm --filter '@agentos-software/*' build` (unchanged filter) |
  | `registry-native` | `toolchain-build` | `make -C registry/native commands` → `make -C toolchain commands` |
  | `registry-native-cmd <name>` | `toolchain-cmd <name>` | `make -C registry/native cmd/<name>` → `make -C toolchain cmd/<name>` |
  | `registry-native-preflight` | `toolchain-preflight` | `cd registry/native/c` → `cd toolchain/sysroot` |
  | `registry-copy-commands` | `toolchain-copy-commands` | repoint copy-wasm-commands SRC to `toolchain/target/...` |

  Update any callers of the old recipe names (CI `just` invocations, other
  recipes, docs) in the same pass. If you prefer a single flat prefix over the
  dir-aligned split, use `software-*` for all — but `toolchain-*` reads truer for
  the recipes that build the sysroot/commands rather than the packages.
- `packages/runtime-core/scripts/copy-wasm-commands.mjs`: SRC path
  `registry/native/target/...` → `toolchain/target/...`.
- CI (`ci.yml`, `ci-nightly.yml`, `bench.yml`): `make -C registry/native`,
  the rust-cache `workspaces:` mapping, and the
  `hashFiles('registry/native/Cargo.lock')` cache key → `toolchain/`.
- Repo-root `Cargo.toml`: add `exclude = ["software"]`.
- `pnpm-workspace.yaml`: `software/*`, `software/*` →
  `software/*` (single glob covers commands + agents) plus `test-harness`.
- `toolchain/Makefile`: replace the fixed `c/programs/*.c` source list with a
  glob over `../software/*/native/c/*.c`, and cargo members over the workspace.

## Toolchain CLI vs justfile boundary

**Principle:** `agentos-toolchain` (the published CLI) owns everything a
third-party author of *any* `@scope/*` package needs — author → compile → test →
package → publish. The `justfile` owns only what is specific to building and
releasing *this* repo's registry. Nothing generic to package authoring should
live only in our `justfile`/`Makefile`.

Today the CLI stops at packaging (`pack`, `pack-aospkg`, `stage`, `build`,
`publish`); `stage` consumes an *already-compiled* `--commands-dir`. The two
hardest capabilities — **compiling** a command to WASM and **testing** it in a VM
— live only in the repo Makefile/harness, so external authors can't do them.
This refactor closes that gap.

### New published toolchain verbs

1. **`agentos-toolchain compile [<packageDir>]`** — compile the package's
   `native/` (Rust crate(s) or C source) to a WASM command binary against the
   **pinned agentOS sysroot**, emitting into the commands dir that `stage`
   consumes. Wraps sysroot + build-std + wasm-opt. That pipeline is heavy, so the
   CLI fetches a **pinned toolchain bundle** (the way it already fetches wasi-sdk)
   or runs a container image; the pin is versioned for reproducible external
   builds. This is the capability that today only `registry/native/Makefile` has.
2. **`agentos-toolchain test [<packageDir>]`** — boot a throwaway VM, register the
   package, run its `test/` suite (and/or a command smoke-run) via the public
   `@rivet-dev/agentos-runtime-core/test-runtime`. This is what our
   `software/<pkg>/test/` scripts *and* external authors call — one runner.
3. **`agentos-toolchain validate [<packageDir>]`** — lint `agentos-package.json`:
   declared `commands` map to staged binaries; `registry` block + `category` +
   `kind` present; limits bounded. Turns our repo-only coverage/layout checks into
   a per-package check every author gets.
4. **`agentos-toolchain init [<name>]`** — scaffold a package
   (`agentos-package.json` + `src/index.ts` + `test/`) instead of copying an
   existing package by hand.

### Shim crates must be publishable

The shared Rust crates in `toolchain/crates/` (`wasi-ext`, `wasi-http`,
`wasi-spawn`, `builtins`, `shims`) are **path-deps today, unpublished** — so an
external Rust command cannot build against them. For `compile` to work outside
this repo, either publish them (crates.io) or have `compile` vendor them. Add
them to the publish set (`scripts/publish` discovery).

### justfile = thin loop over the CLI

Repo recipes orchestrate; they must not reimplement what the CLI does:

| Recipe | Body |
|---|---|
| `software-build` | for each `software/*`: `agentos-toolchain compile && stage && build` |
| `toolchain-build` | build/pin the shared sysroot bundle the CLI consumes |
| per-package `test` script + external authors | `agentos-toolchain test` |

**Stays repo-specific** (not CLI, not overfit): `copy-wasm-commands` (vendor into
runtime-core), `verify-fixed-versions` (the 0.0.1 pin), `generate-agentos-mirror`,
registry-wide release orchestration, cross-repo dispatch, and the status
reporter / coverage gate scoped to *our* registry.

## Migration phases

1. **Scaffold `toolchain/`** — move `registry/native/{Cargo.*, rust-toolchain,
   Makefile, patches→std-patches, scripts, tests→conformance}` and the shared
   crates + sysroot. Repoint the workspace + Makefile + CI + copy-commands, and
   **rename the `registry-*` justfile recipes** (`software-*` / `toolchain-*` per
   the table above) plus their callers. Build green with crates still under
   `toolchain/crates/commands/` (pre-split).
2. **Flatten catalog + harness** — `git mv software/* software/`,
   `git mv software/* software/`; create `test-harness/` from the old
   `registry/tests/helpers.ts`+`terminal-harness.ts`; give each package a `test`
   script; update `pnpm-workspace.yaml`; add `kind` to agent manifests; relocate
   leftover `registry/` files; delete empty `registry/`.
3. **Colocate command source** — move each `cmd-<x>` + its 1:1 lib into
   `software/<owner>/native/`; move C command sources into
   `software/<owner>/native/c/`; C test-programs stay in `toolchain/test-programs/`;
   apply the `Cargo.toml` exclude + cross-dir members; resolve orphan commands.
4. **Add `category`** to every `registry` manifest block (grouping metadata).
5. **Productize the toolchain** — add `compile` / `test` / `validate` / `init`
   verbs to `agentos-toolchain`; make the shim crates publishable; rewrite the
   `software-build` / `toolchain-build` recipes as thin loops over the CLI; point
   each package `test` script at `agentos-toolchain test`. (Can land incrementally
   after the move; the move doesn't block it, but the CLI is the long-term home.)
6. **Enforce** — structural test (below) + coverage gate; delete dead paths.

## Enforcement (`scripts/check-layout.mjs`, CI)

Fail the build if:
- any `*.test.ts` sits outside an allowed home (`software/<pkg>/test/`,
  `toolchain/conformance/`, `packages/*/tests/**`);
- any `software/*` command package lacks `test/` (allowlist meta bundles +
  external wrappers);
- `registry/` exists;
- a crate under `software/*/native/crates/` is claimed by the main workspace
  (guards the Cargo split).

## Risks & decision

1. **Cargo cross-dir members (biggest).** `toolchain/Cargo.toml` owning
   `../software/*/native/crates/*` plus repo-root `exclude = ["software"]` is
   valid but unusual; some tooling assumes members live under the root. Two known
   facets: `cargo metadata` on cross-dir members, and **rust-analyzer/`rust-toolchain`
   resolution** — a colocated crate opened directly walks up to the repo-root
   (stable) toolchain, not `toolchain/`'s nightly, so IDE builds may use the wrong
   channel unless the harness always drives builds from `toolchain/`. **Decide:
   full colocation (this spec) vs. the C-only fallback** (Rust crates stay in
   `toolchain/crates/commands/`). Recommendation: attempt full colocation in a
   throwaway branch first; fall back if `cargo metadata`/rust-analyzer misbehave.
2. **Makefile source discovery.** Building must glob C sources and cargo members
   across `software/*` instead of a fixed list — a one-time Makefile rewrite.
3. **No build isolation.** Building one package still needs all of `toolchain/`
   (sysroot + shared crates). Colocation improves navigation, not build scope.
4. **Shared crates aren't self-contained.** `software/coreutils/native/crates/sh`
   still depends on `toolchain/crates/builtins`; a package folder is not fully
   standalone. Accepted, documented in rule #3.
5. **Sysroot distribution for `agentos-toolchain compile`.** The compile verb
   needs the patched sysroot + nightly + wasm-opt, which are large and
   platform-specific. **Decide** how the published CLI ships them: a pinned
   downloadable bundle (like wasi-sdk) vs. a container image. Until decided,
   `compile` works in-repo only and external authors still can't build — so this
   gates the "same flow for external users" goal, not the internal move.
