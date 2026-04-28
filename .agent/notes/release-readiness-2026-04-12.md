# Release Readiness Report — 2026-04-12

Branch: `finish-ts-rust-migration`. Goal: ship a release candidate where every quickstart + documented-feature path is exercised by a working test.

## TL;DR

**Not shippable yet.** Four of fourteen quickstart examples have zero end-to-end test coverage, one is explicitly broken per `packages/core/CLAUDE.md` (`vm.fetch` does not route guest listener ports back to the host), and the full-suite `pnpm test` run hangs on this branch (per CLAUDE.md, bare `pnpm test` must be avoided). The PRD has **130 pending stories**; only a small subset actually blocks a first release.

## 1. Quickstart Coverage (14 scripts)

| Quickstart | Test coverage | Status |
|---|---|---|
| `hello-world.ts` | `filesystem.test.ts`, `agent-os-base-filesystem.test.ts` | OK |
| `filesystem.ts` | `filesystem.test.ts`, `batch-file-ops.test.ts`, `filesystem-move-delete.test.ts`, `readdir-recursive.test.ts` | OK |
| `bash.ts` | `execute.test.ts`, `shell-flat-api.test.ts` | OK (needs WASM binaries built) |
| `processes.ts` | `spawn-flat-api.test.ts`, `execute.test.ts`, `all-processes.test.ts`, `process-tree.test.ts` | OK |
| `cron.ts` | `cron-integration.test.ts` (exec action at lines 68/85), `cron-manager.test.ts` | OK |
| `tools.ts` | `host-tools.test.ts`, `host-tools-zod.test.ts`, `sidecar-tool-dispatch.test.ts` | OK |
| `nodejs.ts` | `execute.test.ts`, `spawn-flat-api.test.ts` | OK |
| `agent-session.ts` | `claude-session.test.ts`, `codex-session.test.ts`, `opencode-session.test.ts`, `pi-headless.test.ts` | OK (uses llmock) |
| **`network.ts`** | none | **BROKEN** — `vm.fetch()` does not translate guest listener ports to host (CLAUDE.md: "Network tests on the native sidecar path should stick to listener bind/state assertions... `vm.fetch()` does not currently translate arbitrary guest listener ports back to the host"). Quickstart relies on this path. |
| **`git.ts`** | none | Gap — no git-specific test; depends on `@rivet-dev/agent-os-git` + `exec()`. Needs WASM git build. |
| **`pi-extensions.ts`** | none | Gap — `before_agent_start` hook and extension discovery path have no test. |
| **`s3-filesystem.ts`** | none | Gap — `createS3Backend` / S3 plugin descriptor path not exercised by any test. |
| **`sandbox.ts`** | none | Gap — `createSandboxFs` / `createSandboxToolkit` / Docker sandbox-agent integration untested. |

**Required new tests to call the release candidate honest:**
1. `network-vm-fetch.test.ts` — spawn a Node HTTP server inside a VM and assert `vm.fetch(port, req)` round-trips (and fix the underlying listener→host translation or document the exact limitation and land a smaller, working API).
2. `git-quickstart.test.ts` — exercise `init`/`add`/`commit`/`clone`/`checkout` with `@rivet-dev/agent-os-git` package, gated on WASM git artifact.
3. `pi-extensions.test.ts` — write extension to `~/.pi/agent/extensions/` inside VM, assert `before_agent_start` hook fires and modifies system prompt (llmock).
4. `s3-backend.test.ts` — stand up MinIO in-process (or mock S3 HTTP), mount through `createS3Backend`, assert read/write/readdir round-trip.
5. `sandbox-integration.test.ts` — guarded on Docker availability; mount `createSandboxFs` and call `sandbox` toolkit via the RPC port pattern the quickstart uses.

## 2. Documentation Coverage (`~/r10/docs/`)

### Covered
- Filesystem basic + batch (`readFiles` / `writeFiles` — `batch-file-ops.test.ts`)
- Process mgmt, `writeProcessStdin` / `closeProcessStdin`, `getProcess`, `processTree`, `allProcesses`
- Interactive PTY (`openShell`, `writeShell`, `resizeShell`, `closeShell`)
- Permission hooks (`onPermissionRequest`)
- `additionalInstructions` / `skipOsInstructions` (`os-instructions.test.ts`)
- Session lifecycle (`resumeSession` / `destroySession`) via session tests
- `setModel` / `setMode` (partial)

### Missing / Partial
- **`vmFetch` / `vm.fetch`** — zero tests; partially broken (see above).
- **`createSignedPreviewUrl` / `expireSignedPreviewUrl`** — RivetKit-layer only. Either delete from core docs or add a RivetKit driver-test-suite coverage item upstream.
- **`listPersistedSessions`** — documented but untested.
- **`mcpServers`** in session config — documented but no test.
- **`workflow()` / `c.step()` / `c.queue.iter()`** — documented extensively; not found anywhere in core package. Either RivetKit-only (then scope out of core docs) or unimplemented → hard release blocker.
- **`GoogleDriveBlockStore`** — untested.
- **`createS3BackendForAgent`** — untested.
- **`setThoughtLevel` / `getModes` / `getConfigOptions`** — not covered in tests.
- **Pi `before_agent_start` extensions** — documented, untested.

## 3. PRD Triage — Actual Release Blockers

Of the 130 pending stories, only a small set actually gates "core functionality working + quickstart passes."

### True release blockers (must land before RC)
| Priority | ID | Why it blocks |
|---|---|---|
| 4 | **US-088** | Gate story — "full first-party workspace green with no product-debt skips or ignored Rust tests" is the definition of ready. |
| 5 | **US-089** | Final verification sweep, same gate. |
| — | (new) | Add and fix the five missing quickstart tests listed in §1. |
| 13 | **US-217** | Default sidecar permissions must be **deny**, not allow-all. Shipping with allow-all default = security footgun. |
| 14 | **US-218** | Reject empty-op / empty-path permission rules. Complements US-217. |
| 15 | **US-219** | Permission glob `*` must not cross path separators — classic permission escape. |
| 9 | **US-190** | Permission-gate `FindListener` / `FindBoundUdp` / `GetProcessSnapshot` — listed for the same reason. |
| 16 | **US-243** | Route guest `http/https.request` through kernel socket table. Any real-world agent hits this immediately. Also unblocks fixing `network.ts`. |
| 17 | **US-250** | Dev-shell hybrid-VFS host fallthrough = isolation violation. Hardcore invariant per repo CLAUDE.md. |
| 18 | **US-251** | Browser sidecar kernel routing — same class of host-escape. |
| 6 | **US-173** | Panic-on-serialize → fallible path. Any serialization edge case crashes the session. |
| 7 | **US-184** | ACP inbound request must wait for host response. Incorrectly returning -32601 breaks permission round-trips. |
| 8 | **US-188** | `register_toolkit` duplicate detection + permission gating. Tool dispatch safety. |
| 11 | **US-202** | Panic in `pump_process_events` on reaped VM → sidecar crash. |

### Release-notes or follow-up (safe to ship without)
Everything in priority ≥ 19 in the PRD is hardening, compatibility polish, or edge-case correctness. Ship them in point releases. Specifically:
- Most v8-bridge polyfill gaps (US-156, US-157, US-227, US-237, US-261, US-266, US-280, US-292, US-299, US-303, US-305) — needed for broader npm compat but not for the quickstart set.
- All BARE codec expansion (US-313/314/315) — codec is green for the surface you use today.
- Cosmetic cron / EventEmitter / perf_hooks items (US-199, US-276, US-286, US-287, US-288, US-289).
- Private filesystem semantics that current tests don't care about (US-234, US-235, US-236, US-247, US-267, US-274, US-283, US-284).

### Scope-decision items (ask the user)
- **US-147** — "Resolve uncommitted deletion of public docs/ tree." This directly touches what we're measuring coverage against. Decide whether the `~/r10/docs/` surface you're targeting includes the deleted paths.
- **US-148** — `@rivet-dev/agent-os-shell` package cleanup. If it's not part of the release, delete it.
- **US-146** — Remove `minimal_root_snapshot` fallback. Decides whether the base-filesystem story is locked in.

## 4. Test-suite Execution Facts
- Bare `pnpm --filter @rivet-dev/agent-os-core test` hangs (the integration tests do not terminate on this branch). Per `packages/core/CLAUDE.md` this is expected — tests must always be run scoped.
- `native-sidecar-process.test.ts` (11 tests) passes cleanly in isolation (~4.5 s total).
- WASM binaries are present under `registry/native/target/wasm32-wasip1/release/commands/` and C artifacts under `registry/native/c/build/`, so the 70+ "skipIf WASM missing" blocks should actually run — but that needs to be confirmed on a clean CI pass (US-088 / US-089).
- Only three credentialed skipIfs remain by design: `duckdb-package`, `browserbase-e2e`, `browserbase-ws`.

## 5. Recommended Release Order of Attack

1. **Land the 5 missing quickstart tests** (§1). These define "quickstart is honest."
2. **Fix `vm.fetch` guest-port translation** or shrink the API to what actually works and update the quickstart + docs to match.
3. **US-217 / US-218 / US-219 / US-190** — lock down the permission model before anyone publishes a package under the current allow-all default.
4. **US-173 / US-184 / US-188 / US-202** — ACP + toolkit crash/safety items.
5. **US-243 / US-250 / US-251** — the remaining host-escape / host-fallthrough items that violate the core virtualization invariant.
6. Get `pnpm test` and the five `cargo test -p agent-os-{kernel,bridge,execution,v8-runtime,sidecar}` suites green end-to-end on a clean clone (US-088 then US-089). That's the RC gate.
7. **Decide `workflow()` / MCP / preview-URL scope**: either mark core-docs-only for RivetKit and move them, or implement.
8. Everything else in the PRD moves to point releases.
