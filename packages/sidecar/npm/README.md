# Agent OS sidecar platform packages

Each subdirectory is a platform-specific npm package that ships the compiled
`agentos-sidecar` binary for one target. They are published by the release
workflow, which builds the binary for each platform and copies it into the
matching directory before `npm publish`. They are not pnpm workspace members and
are not built by Turborepo.

The meta package `@rivet-dev/agentos-sidecar` (one directory up) resolves the
correct platform package at runtime via npm `os`/`cpu`/`libc` selection.

Supported platforms: `linux-x64-gnu`, `linux-arm64-gnu`, `darwin-arm64`, `darwin-x64`.
