# Dev container

agentOS targets native Linux execution, so on macOS the engine, sidecar, and VMs
run in this container while the working tree stays on the host. The repo is
bind-mounted at `/build`; every build output lives in a named volume, so host
edits are visible immediately and no darwin-native binary is ever dragged into
Linux.

This is a dev environment. For release artifacts see `docker/build/`.

## First run

```bash
just dev-up          # build the image, start the container
just dev-bootstrap   # pnpm install + WASM command set + sidecar (slow, one time)
```

`dev-bootstrap` builds the full default WASM tool set from source, which is the
long pole. It only needs re-running when the toolchain or software sources
change.

## Daily use

```bash
just dev-terminal-example   # engine on :6420, Vite on :5173
just dev-shell              # interactive shell in the container
just dev-exec 'cargo check --workspace'
just dev-build-tabs         # rebuild the inspector custom-tab bundle
just dev-down               # stop
```

Open <http://localhost:5173> for the example UI. The hosted Rivet dashboard runs
in the host browser and points at <http://localhost:6420>, which serves the actor
gateway, `/inspector/*`, and custom-tab assets.

## Layout

| Path | Backing | Why |
| --- | --- | --- |
| `/build` | bind mount | host edits visible immediately |
| `/build/node_modules` | volume | host tree holds darwin binaries |
| `/build/target` | volume | Linux cargo output, survives recreation |
| `/build/toolchain/target`, `toolchain/c/*` | volumes | generated WASM/sysroot trees |
| cargo registry/git, pnpm store | volumes | warm caches across rebuilds |

## Caveats

- **Run `pnpm install` inside the container, not on the host.** Both write
  package-level `node_modules` symlinks into the bind mount, and their
  platform-specific optional dependencies differ. If you need host typechecks,
  re-run `pnpm install` on the host afterwards.
- DNS is pinned to 1.1.1.1/8.8.8.8. Docker Desktop's embedded resolver
  intermittently stops answering, which surfaces as an `EAI_AGAIN` storm during
  install.
- `AGENTOS_SIDECAR_BIN` points at `/build/target/debug/agentos-sidecar`. Rebuild
  it with `just dev-exec 'cargo build -p agentos-sidecar'`.
- The container publishes 6420 and 5173. Free them on the host first if another
  engine is already bound.
