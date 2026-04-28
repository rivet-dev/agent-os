# US-090 — Fix the WASM warmup shebang root cause blocking US-088

Handoff spec. Hand this to another agent (Claude, Codex, or a fresh subagent) to unblock Ralph.

## Background — what Ralph is stuck on

Ralph has spent **19+ hours and 9 incremental commits on US-088** (the P4 meta release-gate "make the full first-party workspace green") without flipping it to passing. Analysis of `scripts/ralph/codex-streams/step-82.log` shows ~27 vitest files failing in `pnpm test`, and **one root-cause error cascades through ~303 test cases**:

```
ERR_AGENT_OS_NODE_SYNC_RPC: WebAssembly warmup exited with status 1:
  CompileError: WebAssembly.Module(): expected magic word 00 61 73 6d,
                                       found 23 21 2f 62 @+0
```

The found bytes `23 21 2f 62` decode as **`#!/b`** — a shebang header. Something in the WASM prewarm path is handing a `#!/bin/sh` (or `#!/usr/bin/env node`) shell-shim file to `WebAssembly.compile()` instead of real `\0asm` bytes. **Fix that one bug and ~200+ test cases flip green in a single shot.**

## Evidence

- Full stream with 303 occurrences: `scripts/ralph/codex-streams/step-82.log` (30+ MB, grep for `WebAssembly warmup exited`).
- Top failing test files (iter 82): `tests/os-instructions.test.ts` (34 fails), `tests/mount.test.ts` (20), `tests/agent-os-base-filesystem.test.ts` (20), `tests/process-management.test.ts` (18), `tests/cron-integration.test.ts` (17), `tests/claude-session.test.ts` (16), `tests/filesystem.test.ts` (14), `tests/session-cleanup.test.ts` (12), 19 more.
- Codex has been symptom-swatting `bridge-child-process.test.ts > execFileSync on node_modules/.bin shell shims unwraps to the node entrypoint` for 4+ hours — same failure family, wrong level.

## Where to look

Most likely locations (inspect in this order):

1. **`crates/execution/src/node_import_cache.rs`** — contains `prewarm_wasm_path()`. From `scripts/ralph/progress.txt`: *"WASM prewarm still runs the embedded V8 runner, so `prewarm_wasm_path()` must service the runner's internal `node:fs` sync-RPC traffic just like normal execution"*. This is where prewarm resolves and loads WASM bytes.
2. **`crates/execution/src/wasm.rs`** — WASM module instantiation; the actual `WebAssembly.compile()` call happens here or downstream.
3. **`packages/core/src/sidecar/rpc-client.ts`** — commit `c5470ca` (one of today's US-088 commits) touched 150 lines here, likely related to command resolution / shim unwrap.
4. **Secure-exec reference**: `/home/nathan/secure-exec-1/` (tagged `v0.2.1`). The old JS kernel handled the exact same bug — search for how it detects `#!` prefixes and routes to the shell path. Key files per `CLAUDE.md`: `nodejs/src/bridge/network.ts`, `nodejs/src/bridge-handlers.ts`.

## Likely bug shape

During prewarm, `prewarm_wasm_path()` resolves a command name (e.g., `vitest`, `tsx`, `astro`) via `node_modules/.bin/<cmd>`. Those bin entries are shell-shim scripts that start with `#!/bin/sh` and internally `exec node ../some-real-js`. The current code treats them as WASM and hands the raw bytes to `WebAssembly.compile()` which rejects with the magic-word error.

**The fix is one of**:

- (a) Detect `#!` at byte 0 during prewarm and either follow the shim's real target (parse the `exec node …` line) or fall back to the Node dispatch path, or
- (b) Restrict prewarm candidates to actual `.wasm` files (reject anything not starting with `\0asm` up front with a clear error), and let the caller route non-WASM commands to the shell/Node path.

Prefer (a) — it's what real Node does via `npm`'s bin shim unwrap logic, and secure-exec's reference implementation handled it that way.

## What to do

### Step 1 — Find and fix the bug

1. Search for `prewarm_wasm_path` across the repo. Read the call sites.
2. Add a magic-byte sniff: if the resolved path's first 4 bytes aren't `[0x00, 0x61, 0x73, 0x6d]`, check if bytes 0..2 are `#!`. If yes, parse the shim's first 20 lines for a `exec "$basedir/node" "$basedir/../some/path" …` pattern and follow it to the real target. If the real target is a `.js` file, route through the Node dispatch path instead of WASM. If it's still `#!`, error loudly with the actual resolved path so the next iteration can see what's happening.
3. Write a focused test in `crates/execution/tests/wasm.rs` (or a new file) that reproduces the bug: create a `node_modules/.bin/fake-shim` with `#!/bin/sh\nexec "$basedir/node" "$basedir/../fake/dist/cli.js" "$@"`, call the prewarm path, assert it doesn't throw `WebAssembly.Module()` and instead either succeeds via Node dispatch or returns a typed "not-wasm" error.
4. Run the verification command: `pnpm --dir packages/core exec vitest run tests/os-instructions.test.ts --reporter=verbose`. It should pass without any `ERR_AGENT_OS_NODE_SYNC_RPC: WebAssembly warmup exited with status 1` messages. If you can't run it locally because cargo isn't in your PATH, see "Environment caveats" below.

### Step 2 — Update `scripts/ralph/prd.json`

Add **two new stories** to `userStories` and keep priority ordering clean:

**Story 1** — insert at the top of the array (before US-088):

```json
{
  "id": "US-090",
  "title": "Reject shebang shell-shim bytes in WASM prewarm with a typed error and route through Node dispatch",
  "description": "As a maintainer, I want `prewarm_wasm_path()` to detect when a resolved command is a `#!`-prefixed shell shim instead of a real `\\0asm` WASM binary, so ~200 failing first-party test cases that all cascade from a single `ERR_AGENT_OS_NODE_SYNC_RPC: WebAssembly warmup exited with status 1: CompileError: WebAssembly.Module(): expected magic word 00 61 73 6d, found 23 21 2f 62 @+0` stop masking the next real blocker for US-088.",
  "acceptanceCriteria": [
    "`prewarm_wasm_path()` (or equivalent WASM prewarm entry point) sniffs the first 4 bytes of the resolved candidate and refuses to hand non-`\\0asm` content to `WebAssembly.compile`",
    "When the first 2 bytes are `#!`, the prewarm path either follows the shim to its real `node_modules/<pkg>/...` target and routes through Node dispatch, or fails with a typed error naming the resolved path (never `CompileError`)",
    "Add a focused test in `crates/execution/tests/wasm.rs` that synthesizes a `node_modules/.bin/<shim>` pointing to a JS entry via `#!/bin/sh` + `exec node ...`, exercises the prewarm path, and asserts no `CompileError: WebAssembly.Module()` is raised",
    "Run `pnpm --dir packages/core exec vitest run tests/os-instructions.test.ts --reporter=verbose` and confirm zero `ERR_AGENT_OS_NODE_SYNC_RPC: WebAssembly warmup exited with status 1` lines in the output",
    "Typecheck passes: `pnpm --dir packages/core check-types`"
  ],
  "priority": 3,
  "passes": false,
  "notes": "Root cause blocking US-088. Secure-exec reference implementation handled shell-shim unwrap — see /home/nathan/secure-exec-1/ (tagged v0.2.1), specifically nodejs/src/bridge-handlers.ts and nodejs/src/bridge/network.ts. Evidence: scripts/ralph/codex-streams/step-82.log has 303 occurrences of the error."
}
```

**Story 2** — insert immediately after the US-088 story:

```json
{
  "id": "US-091",
  "title": "Finish the US-088 release-gate sweep after the WASM-warmup root cause lands",
  "description": "As a maintainer, once US-090 has unblocked the ~27 vitest files that currently fail via a single `#!/b` WASM-warmup cascade, I want Ralph to resume the US-088 green sweep against whatever real failures remain, so the release gate can actually flip to passing instead of symptom-swatting forever.",
  "acceptanceCriteria": [
    "US-090 is marked `passes: true` before this story is started",
    "Re-run `pnpm test` from the repo root and record the remaining failing test files and root-cause groups in `scripts/ralph/progress.txt`",
    "Land focused fixes or follow-up stories for each remaining root-cause group, one bounded iteration each",
    "Final acceptance: `pnpm test` runs to completion and every remaining failure is a documented external-dependency gate (Docker, S3, network) with an explicit `SKIP_*=1` escape hatch, no silent skips, no unclassified failures",
    "Typecheck passes: `pnpm --dir packages/core check-types`"
  ],
  "priority": 5,
  "passes": false,
  "notes": "Follow-up to US-088 that Ralph was stuck on for 19+ hours before the US-090 root cause was identified. US-088 itself stays in the backlog as the meta gate; this story tracks the non-cascade cleanup work."
}
```

**Validation after the edit**:

- `python3 -c "import json; d = json.load(open('scripts/ralph/prd.json')); print(len(d['userStories']))"` should print `124` (was 122).
- Both new stories must be parseable JSON.
- Priorities 3 and 5 should not collide with any existing story — search `"priority": 3` and `"priority": 5` first and bump the new story priorities if needed to avoid collisions.

### Step 3 — Don't touch existing files beyond the fix

- Do NOT modify `crates/sidecar/`, `packages/core/src/agent-os.ts`, or any unrelated files.
- Do NOT edit `scripts/ralph/ralph.sh`, `scripts/ralph/ralph-docker.sh`, or `scripts/ralph/ralph-docker-per-iter.sh`.
- Do NOT flip any other `passes: true` flags.
- Commit the code fix and the prd.json edit as **two separate commits**:
  1. `feat: US-090 - Reject shebang shell-shim bytes in WASM prewarm with a typed error and route through Node dispatch` (with the real code + test)
  2. `chore: add US-090 and US-091 to prd.json` (just the prd.json edit)

### Environment caveats (if you can't run cargo/pnpm locally)

- If `cargo` is not in PATH, install rustup under `$HOME/.cargo` with `curl https://sh.rustup.rs -sSf | sh -s -- -y --profile minimal` and source `$HOME/.cargo/env`. The ralph container does this automatically on first failure.
- If `pnpm exec vitest` fails with `EACCES`, use `node_modules/.pnpm/vitest@*/node_modules/vitest/vitest.mjs run tests/os-instructions.test.ts --reporter=verbose` directly.
- If neither works, at minimum: add the Rust-side unit test in `crates/execution/tests/wasm.rs`, verify it compiles with `bash -n` / `cargo check`, and note in the commit message that runtime verification is pending.

## Success criteria for this task

- [x] Code fix lands with a focused test reproducing the `#!/b` → `CompileError` bug.
- [x] `scripts/ralph/prd.json` gains two new stories (US-090 at priority 3, US-091 at priority 5) with valid JSON.
- [x] Two commits, no unrelated file edits.
- [x] `pnpm --dir packages/core exec vitest run tests/os-instructions.test.ts --reporter=verbose` passes (or at least no longer hits the WASM warmup error — other unrelated failures are OK).

Start with step 1, report back when the root cause is located, then land the fix.
