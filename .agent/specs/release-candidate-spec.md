# Release Candidate Spec

**Branch:** `finish-ts-rust-migration`
**Date:** 2026-04-12
**Owner:** TBD
**Status:** proposed — pre-RC

## Goal

Ship the first honest release candidate of `@rivet-dev/agent-os-core` and its companion packages (`-common`, `-git`, `-pi`, `-claude`, `-codex-agent`, `-opencode`, `-s3`, `-sandbox`). "Honest" means:

1. Every example in `examples/quickstart/` runs to completion without modification on a clean clone.
2. Every feature in `~/r10/docs/docs/agent-os/` is either (a) exercised by a passing scoped test, or (b) deleted from the public docs.
3. `pnpm test` and the five gate cargo suites complete without `skip`/`ignore` markers on first-party tests.
4. The default security posture is safe: no allow-all permissions, no host fallthroughs, no guest-reachable panics.

Everything else in `scripts/ralph/todo/prd.json` moves to post-RC point releases.

## Non-goals

- Closing every pending PRD story. ~120 of the 130 items are hardening, polyfill completeness, or edge-case correctness. They can ship in 0.1.x.
- Shipping `workflow()`, `listPersistedSessions`, `createSignedPreviewUrl`, `GoogleDriveBlockStore`, or `createS3BackendForAgent` in core. See §6 for the scope decision.
- Re-running the full RivetKit driver test suite — tracked separately.

## Milestones

### M1 — Quickstart honesty (1–2 days)

Every script under `examples/quickstart/src/` has a matching test that proves the same code path works end-to-end. Today, 5 of 14 have zero coverage.

| Quickstart | New test to land | Notes |
|---|---|---|
| `network.ts` | `packages/core/tests/network-vm-fetch.test.ts` | **Blocks on a real fix, not just a test.** `packages/core/CLAUDE.md` states "vm.fetch() does not currently translate arbitrary guest listener ports back to the host." The quickstart uses `vm.fetch(port, req)` against a guest `http.createServer()` and is definitionally broken. Either fix the sidecar port-translation path (ideally by routing through the kernel socket table the way US-243 prescribes), or shrink the API surface and update the quickstart + docs. A passing test is only meaningful after the underlying path works. |
| `git.ts` | `packages/core/tests/git-quickstart.test.ts` | Exercises `git init`/`add`/`commit`/`clone`/`checkout` with `software: [common, git]`. Gate on WASM git artifact presence via `tests/helpers/registry-commands.ts` (existing helper). No network — uses the local-path remote from the quickstart. |
| `pi-extensions.ts` | `packages/core/tests/pi-extensions.test.ts` | Writes a `~/.pi/agent/extensions/*.js` that hooks `before_agent_start` and appends an instruction, then runs a session against llmock and asserts the instruction landed in the outgoing request. Reuses the llmock setup from `tests/pi-headless.test.ts` and `tests/claude-session.test.ts`. |
| `s3-filesystem.ts` | `packages/core/tests/s3-backend.test.ts` | Stand up MinIO in a background process (or mock the S3 HTTP surface) and round-trip `writeFile`/`readFile`/`readdir` through `createS3Backend(...)`. If MinIO is not available, keep the test gated with an explicit env skip (`SKIP_MINIO=1` style), not a silent skipIf. |
| `sandbox.ts` | `packages/core/tests/sandbox-integration.test.ts` | Guarded on Docker availability. Mounts `createSandboxFs`, calls the `sandbox` toolkit via the same RPC port pattern the quickstart uses, asserts `run-command` and `list-processes` round-trip. |

**Acceptance:** Each new test runs in isolation via `pnpm --dir packages/core exec vitest run tests/<file>`, matches quickstart behavior exactly, and has been run alongside the corresponding `node --import tsx src/<file>.ts` from `examples/quickstart/`.

### M2 — Default-safe permissions (1 day)

These three story IDs are the minimum permission-model fix. Shipping with allow-all is a security footgun the moment someone publishes a package against the public API.

- **US-217** — sidecar default permissions must be deny, not allow-all. Fix in `crates/sidecar/src/state.rs` (permission descriptor construction) and `crates/kernel/src/device_layer.rs` (default policy). Audit every test that relies on the current allow-all default and migrate them to explicit `permissions: allowAll` opt-in, so test changes stay localized.
- **US-218** — reject empty-operation and empty-path permission rules. Fix in `crates/sidecar/src/state.rs` permission parsing.
- **US-219** — permission glob `*` must not cross path separators. Fix in the Rust glob matcher (look in `crates/kernel/` for the current implementation).
- **US-190** — gate `FindListener` / `FindBoundUdp` / `GetProcessSnapshot` behind `network.inspect` / `process.inspect` permissions. These are currently reachable without any permission.

**Acceptance:** A new `tests/security_hardening.rs` (or extension to the existing one) case creates a default-config VM and proves (a) `spawn("echo")` is denied, (b) empty permission rules are rejected, (c) `network/*` does not grant `network/foo/bar`, (d) find-listener RPCs refuse without the new permissions.

### M3 — Crash/safety fixes that trip on real workloads (1 day)

These four items are the highest-impact stability items that any real agent session will trip within minutes.

- **US-173** — replace panic-on-serialize with a fallible ACP notification path (`packages/core/src/sidecar/rpc-client.ts` or the equivalent Rust notify path). A single un-serializable field currently crashes the session.
- **US-184** — ACP inbound request must wait for host response before falling back to `-32601`. Today the adapter can return Method Not Found while the host is still answering. Fix in the adapter round-trip code.
- **US-188** — `register_toolkit` must reject duplicate toolkit names and gate tool invocation behind permission checks. Fix in `crates/sidecar/src/tools.rs`.
- **US-202** — stop panicking in `pump_process_events` when the VM/process has already been reaped. Fix in `crates/kernel/src/kernel.rs` (or wherever `pump_process_events` lives).

**Acceptance:** Add targeted tests for each case. US-173 gets a negative test in `packages/core/tests/sidecar-*.test.ts`. US-202 gets a `crates/kernel/tests/` or `crates/sidecar/tests/` reap-race test.

### M4 — Isolation invariant closure (2–3 days)

Three remaining host-escape / host-fallthrough items. These violate the core virtualization invariant (from the top-level `CLAUDE.md`: "ALL guest code MUST execute inside the kernel with ZERO host escapes"). They must land before RC.

- **US-243** — route guest `http.request` / `https.request` through undici + the kernel socket table, not the `_networkHttpRequestRaw` bridge shortcut. This also unlocks the real fix for M1's `vm.fetch` story. Touches `crates/execution/assets/v8-bridge.js` (or `v8-bridge.source.js`) and whatever handles `_networkHttpRequestRaw` on the Rust side.
- **US-250** — replace dev-shell's hybrid-VFS host fallthrough with a real kernel path. Find and remove the host-fallthrough in the dev-shell implementation (`@rivet-dev/agent-os-shell` package, and/or wherever dev-shell resolves command paths).
- **US-251** — browser sidecar kernel must be `allow_all` and actually routed through `VmState`, not a host shortcut. Fix in the browser sidecar plumbing.

**Acceptance:** Extend `tests/security_hardening.rs` with adversarial cases for each path — a guest that tries to reach a real host HTTP endpoint, a guest shell that tries to read a host file via dev-shell, and a browser-sidecar flow that tries to bypass VmState.

### M5 — Test suite honest green (1–2 days)

US-088 and US-089 are the release gate. They cannot be closed until M1–M4 are in.

- **US-088** — full first-party workspace green. `pnpm test` from repo root and the five scoped `cargo test -p agent-os-{kernel,bridge,execution,v8-runtime,sidecar} -- --test-threads=1` commands pass, with zero first-party `#[ignore]` and zero product-debt `skip`/`skipIf`.
- **US-089** — final verification sweep: `pnpm install --frozen-lockfile`, `pnpm check-types`, `pnpm test`, the five scoped cargo commands, plus `cargo test -p agent-os-v8-runtime snapshot::tests::snapshot_consolidated_tests -- --exact --ignored`.

**Caveat about `pnpm test`:** `packages/core/CLAUDE.md` currently says "Never run bare `pnpm test` without a filter — integration tests can hang indefinitely." That's a symptom of a real test-isolation bug, not a test-policy choice. Fixing this is part of M5: identify the hanging test(s) (add per-test timeouts, run the suite under `--bail=1` with a watchdog, bisect), fix them, and then remove the warning from the CLAUDE.md.

**Acceptance:** One CI-quality run on a clean clone passes in under 30 minutes without manual intervention. The `.last-publish-hash` markers are clean, no orphaned vitest workers remain after the suite exits.

## Out of scope for RC (defer to 0.1.x point releases)

All of priority-19-and-up in `scripts/ralph/todo/prd.json`:
- v8-bridge polyfill completeness (US-156, US-157, US-227, US-237, US-261, US-266, US-280, US-292, US-299, US-303, US-305)
- BARE codec coverage expansion (US-313, US-314, US-315)
- Cosmetic cron / EventEmitter / perf_hooks items (US-199, US-276, US-286, US-287, US-288, US-289)
- Filesystem semantic edge cases (US-234, US-235, US-236, US-247, US-267, US-274, US-283, US-284)
- POSIX shell builtin polish (US-178, US-179, US-180, US-181)
- Everything else not referenced above

Triage these after RC by the same severity bar: does a real npm package hit it on its golden path? If yes, file it as an 0.1.x bug. Otherwise, leave it in the backlog.

## Scope decisions the user must make

1. **`vm.fetch` surface.** Fix the listener→host translation (harder, right answer) or shrink the API (faster). Recommendation: fix it via US-243.
2. **`workflow()` + persisted sessions + preview URLs in core docs.** These are RivetKit-only today but appear in core-surface docs. Pick: (a) scope them out of core docs and keep them in RivetKit, or (b) implement them in core. Recommendation: (a).
3. **`@rivet-dev/agent-os-shell` package fate (US-148).** Either clean it up to build cleanly or delete it before 0.1 ships.
4. **Public `docs/` tree (US-147).** Resolve the uncommitted deletion before RC so the doc source of truth is clear.
5. **Which Rivet repo path to mirror to.** `~/r-aos`, `~/r16`, or something else — needed for M7.

## Open questions

- Are we shipping 0.1 under `@rivet-dev/*` scope, or is this also where the `@secure-exec/*` public surface branches? Relevant because the top-level `CLAUDE.md` has specific rules about `@secure-exec/typescript` that may or may not apply to this RC.
- Do we want a CI matrix (WASM-only / WASM+C / full-infra) before RC, or after? (US-240 is the tracking story.)
- Is the `tests/migration-parity.test.ts` suite the single source of truth for "core API works end-to-end on the native sidecar path"? If yes, it should be listed in the M5 acceptance command list alongside `pnpm test`.

## References

- Findings report: `.agent/notes/release-readiness-2026-04-12.md`
- PRD backlog: `scripts/ralph/todo/prd.json`
- Quickstart source: `examples/quickstart/src/`
- Core tests: `packages/core/tests/`
- Core invariants: `packages/core/CLAUDE.md`, `crates/CLAUDE.md`, `crates/execution/CLAUDE.md`, `crates/kernel/CLAUDE.md`
