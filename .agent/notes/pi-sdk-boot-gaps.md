# Pi SDK Boot Gaps

Last probe: 2026-04-08
Command: `pnpm --dir packages/core exec vitest run tests/pi-sdk-boot-probe.test.ts`

## Scope

This probe runs focused guest `node` scripts inside fresh VMs on the V8 path. Each action gets its own VM so one wedged import does not poison later results.

## Current Results

- `@mariozechner/pi-coding-agent` is projected into `/root/node_modules` correctly.
- `import("@mariozechner/pi-coding-agent")` succeeds.
- `import("/root/node_modules/@mariozechner/pi-coding-agent/dist/index.js")` succeeds.
- `createCodingTools()` succeeds.
- `createAgentSession()` succeeds.
- `import("@mariozechner/jiti")` succeeds.
- `import("node:fs/promises")` succeeds.
- `import("@anthropic-ai/sdk")` succeeds.
- `import("zod")` succeeds.
- `import("node:child_process")` succeeds and exposes `spawn`/`spawnSync`.
- `import("node:module")` succeeds and exposes `createRequire`.

## Closed Gaps

1. Inline V8 builtin wrappers for `node:fs/promises` must not recurse through `_requireFrom("node:fs/promises")`; they need a direct `node:fs`-backed wrapper instead.
2. Changing generated builtin asset source requires a `NODE_IMPORT_CACHE_ASSET_VERSION` bump or stale materialized assets will keep serving the old code.
3. Guest CommonJS builtins need both `Module.builtinModules` registration and a `loadBuiltinModule()` implementation; `node:v8` was missing the latter, which blocked `@mariozechner/jiti` and therefore the Pi SDK boot chain.
