# Pi SDK Boot Gaps

Last probe: 2026-04-07
Command: `pnpm --dir packages/core exec vitest run tests/pi-sdk-boot-probe.test.ts`

## Scope

This probe runs focused guest `node` scripts inside fresh VMs on the V8 path. Each action gets its own VM so one wedged import does not poison later results.

## Current Results

- `@mariozechner/pi-coding-agent` is projected into `/root/node_modules` correctly.
- `import("@mariozechner/pi-coding-agent")` times out after 5s.
- `import("/root/node_modules/@mariozechner/pi-coding-agent/dist/index.js")` times out after 5s.
- `createCodingTools()` times out after 5s.
- `createAgentSession()` times out after 8s.
- `import("@mariozechner/jiti")` times out after 5s.
- `import("node:fs/promises")` times out after 5s.
- `import("@anthropic-ai/sdk")` succeeds.
- `import("zod")` succeeds.
- `import("node:child_process")` succeeds and exposes `spawn`/`spawnSync`.
- `import("node:module")` succeeds and exposes `createRequire`.

## Interpreted Gaps

1. The first reproducible blocker is a loader/runtime hang, not an immediate thrown exception.
2. The hang reproduces both on the Pi SDK entrypoint and on `@mariozechner/jiti`, which points to an unresolved module-loader or runtime compatibility issue in the same dependency chain.
3. `node:fs/promises` also hangs in isolation, so the V8 bridge still has a concrete filesystem-promises startup gap even before the full Pi SDK boots.
4. Basic builtins needed by Pi are not universally broken: `node:child_process`, `node:module`, `@anthropic-ai/sdk`, and `zod` all load in fresh VMs.

## Likely Next Debug Targets

- Trace why `node:fs/promises` import never resolves in V8.
- Trace `@mariozechner/jiti` module evaluation with extra loader logging.
- Compare the `node:fs/promises` and module-loader path against the recovered secure-exec implementation before changing the Pi adapter.
