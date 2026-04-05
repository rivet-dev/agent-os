# Contributing to agentOS Registry

Thank you for your interest in contributing to the agentOS Registry.

## What belongs here vs. the main repo

This registry is for **software that runs inside agentOS VMs**: WASM command binaries and JavaScript agent/tool packages.

**Non-software packages** like filesystem drivers (S3, Google Drive), sandbox providers, and other host-side integrations live in the main repo at [`rivet-dev/agent-os`](https://github.com/rivet-dev/agent-os).

## License

By contributing, you agree that your contributions will be licensed under the Apache License 2.0.

## Adding a New Package

1. Create `software/{apt-name}/` with:
   - `package.json` (name: `@rivet-dev/agent-os-{apt-name}`)
   - `tsconfig.json` (extends `../../tsconfig.base.json`)
   - `src/index.ts` (exports a descriptor satisfying `WasmCommandPackage`)
2. Add the package to the Makefile `CMD_PACKAGES` list.
3. Add copy rules to the `copy-wasm` target mapping command names to `software/{name}/wasm/`.
4. Set the correct permission tier for each command (see CLAUDE.md for tier reference).
5. If it belongs in `standard` or `build-essential`, add it as a dependency in the meta-package.
6. Run `make copy-wasm && make build && make test` to verify.

## Package Naming

All packages follow `@rivet-dev/agent-os-{name}` where `{name}` matches the Debian/apt package name. For tools without an apt equivalent, use the common CLI name (e.g., `jq`, `ripgrep`, `yq`).

## Development

```bash
# Install dependencies
pnpm install

# Build all packages
make build

# Build WASM from the local legacy source checkout configured in the Makefile
make build-wasm

# Copy WASM binaries to packages
make copy-wasm

# Run tests
make test
```

## Publishing

Publishing is done from a local machine:

```bash
# Dry run
make publish-dry

# Publish (generates date-based version, skips unchanged packages)
make publish
```
