# @rivet-dev/agentos-toolchain

Build toolchain for **agentOS packages** — the only sanctioned way to turn an npm
package or a local script into a valid, self-contained agentOS package.

```bash
npx @rivet-dev/agentos-toolchain pack <npm-pkg | ./local-dir> [options]
```

## Why this lives in agentos

**Packaging is part of agentos's core behavior, so the tool that produces
packages lives here, next to the things it packages.** agentos owns the VM
runtime that *runs* packages — the kernel, the VFS, the `/opt/agentos` mount, the
`$PATH` command resolver, and the header/`binfmt` dispatch — and it owns the
package **definitions** themselves: the generic registry software (`software/*`)
and the agent adapters (`software/*`). This toolchain is what builds those
definitions into the on-disk package format that the runtime resolves. Its
`header.ts` is the same `binfmt` table the sidecar enforces (`crates/sidecar`), so
keeping it in this repo keeps the producer and the consumer of the format in one
place.

agent-os (the product layer) only *consumes* finished packages via
`defineSoftware({ name, dir })`; it does not need to own the builder. The package
name stays `@rivet-dev/agentos-toolchain` so the documented `npx` entrypoint is
unchanged.

## `pack`

Produces `<out>/<name>/<version>/` — a package in the agentOS
[package format](https://agentos-sdk.dev/docs/architecture/packages-and-command-resolution):

The output is a **flat, self-contained package directory** — a plain npm dependency, no
agentOS-specific manifest and no symlinks:

```
<out>/                       # the package dir itself (default ./<input-name>-package)
├── package.json             # name, version, and the "bin" command map
└── node_modules/            # flat, self-contained dependency closure
```

Commands are declared in `package.json` `"bin"` (command → a **real entry file**), so the
package ships cleanly via npm. The runtime builds the `/opt/agentos/bin` symlinks itself when
it mounts the package — they are never part of the shipped artifact.

### Steps

1. **Isolate** — a clean temp dir (no host pnpm/workspace bleed-through).
2. **Install flat** — `npm install <pkg> --omit=dev` (full closure, hoisted, no scripts).
3. **Ensure `package.json`** — `name`/`version` from the package plus a `"bin"` map (each entry
   gets a `#!` shebang). No `agentos-package.json` — `package.json` is the only metadata.
4. **Lay out flat** — `package.json` + `node_modules` written directly into `--out`.
5. **Verify** — every `bin` entry has a recognized header (`#!` shebang or `\0asm`); **reject**
   native `.node` addons (they can't run in V8) — the error names `--prune-native` as the escape.

### Options

| Flag | Meaning |
|---|---|
| `--agent <command>` | mark a `bin` command as the package's ACP entrypoint |
| `--out <dir>` | output dir for the package itself (flat; default `./<input-name>-package`) |
| `--prune-native` | delete unreachable native `.node` addons from the flat closure instead of failing |

### Examples

```bash
# package a local CLI → ./my-tool-package/
npx @rivet-dev/agentos-toolchain pack ./my-tool

# an agent whose SDK closure carries unreachable native addons → ./pi-package/
npx @rivet-dev/agentos-toolchain pack @agentos-software/pi --agent pi-sdk-acp --prune-native
```

The package is consumed by an agentOS host as `defineSoftware({ name, dir })` — add
`agent: { acpEntrypoint: "<bin>" }` for an agent. The host projects it under `/opt/agentos`,
and `openSession({ agent: name })` launches or restores the default session adapter through
`/opt/agentos/bin/<bin>`.
