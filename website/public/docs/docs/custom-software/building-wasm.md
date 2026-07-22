# Building Binaries

Compile WASM command binaries for agentOS from source.

WASM command packages ship **compiled `.wasm` binaries** in their `bin/` that run inside the VM as guest commands. The binaries are build artifacts and are not checked into git, so to add or change a command you build it from source in the AgentOS repo.

You only need this to author new commands. To use existing ones, install the published package (e.g. `@agentos-software/ripgrep`) and pass it to `software`. See [using the registry](#using-the-registry) below.

## Where it lives

Command packages live under top-level `software/`, while shared build infrastructure lives under `toolchain/`:

- **`software/<pkg>/native/crates/cmd-<name>/`**: the Rust source for each command — a cargo package named `cmd-<name>` that emits a `<name>` binary.
- **`software/<pkg>/native/c/`**: the C source for C-built package commands.
- **`software/<name>/`**: the npm package for each command set (`@agentos-software/<name>`). It exports a `{ packagePath }` descriptor pointing at the packed `dist/package.aospkg`, and declares which binaries it ships in its `agentos-package.json` (`commands`, plus optional `aliases` and `stubs`).

## Build

Everything runs through `just` recipes at the AgentOS repo root:

```bash
just toolchain-build           # compile ALL native wasm binaries (slow; once per checkout)
just toolchain-cmd sh    # recompile ONE command (cargo package cmd-sh)
just software-build            # stage + assemble every software package
just software-build ripgrep    # ... or just one
```

The native build compiles each command for `wasm32-wasip1` with the pinned **nightly** toolchain from `rust-toolchain.toml` (the build vendors and patches `std` for WASI), optimizes with `wasm-opt`, and drops the binaries in `toolchain/target/wasm32-wasip1/release/commands/`. C-based commands (e.g. `sqlite3`, `unzip`, `wget`, `zip`) compile with a **wasi-sdk** clang toolchain via `make -C toolchain/c`.

Each package's build then runs the **agentos-toolchain** lifecycle: `agentos-toolchain stage` copies the binaries listed in the package's `agentos-package.json` into its `bin/`, and `agentos-toolchain build` assembles the clean `dist/package/` dir with a `bin` map in its `package.json` and packs it into `dist/package.aospkg` (the `{ packagePath }` target).

## Add a new command package

1. Add the command source as `software/<pkg>/native/crates/cmd-<name>/` (cargo package `cmd-<name>`; Rust) or under `software/<pkg>/native/c/` (C).
2. Create `software/<name>/` as an `@agentos-software/<name>` npm package that exports a `{ packagePath }` descriptor pointing at `dist/package.aospkg`.
3. Declare the shipped binaries in its `agentos-package.json`: `{ "commands": ["<name>"] }` (plus `aliases`/`stubs` if needed).
4. If it belongs in a meta-package (e.g. `common` or `build-essential`), add it there.
5. Verify with `just toolchain-cmd <name> && just software-build <name>` and `pnpm --filter './software/*' test`.

## Let an agent build it

This is a mechanical, well-scoped task, so you can hand it to a coding agent. A prompt like:

```text
Add a WASM command package for `<command>` to AgentOS:
- put the Rust source at software/<command>/native/crates/cmd-<command>/ as a cargo
  package named cmd-<command>,
- create software/<command>/ as an @agentos-software/<command> npm
  package that exports a { packagePath } descriptor and declares the command in
  its agentos-package.json,
then run `just toolchain-cmd <command> && just software-build <command>`
and `pnpm --filter './software/*' test`, and fix any failures.
```

## Using the registry

Install a published package and pass it to `software`. Registry WASM packages are `{ packagePath }` descriptors — import and pass them directly:

Meta-packages bundle a full set, e.g. `@agentos-software/common` (coreutils, sed, grep, gawk, findutils, diffutils, tar, gzip). Run the commands from the client; see [Processes & Shell](/docs/processes). Browse the full catalog on the [Registry](/registry), and see the package descriptor in [Software Definition](/docs/custom-software/definition). To ship your package to npm or use a local build, see [Publishing Packages](/docs/custom-software/publishing).