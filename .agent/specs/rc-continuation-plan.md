# RC Continuation Plan

How to get from "Codex stopped mid-flight with a big uncommitted diff" to "clean focused PRD driving the release candidate." Companion to `.agent/specs/release-candidate-spec.md` — the spec defines *what* to ship; this document defines *how to pick up from the current state*.

## Current state (2026-04-12)

- Branch: `finish-ts-rust-migration`
- Last committed Ralph story: **US-201** (timeout shim busy-wait fix) at commit `1c39a55`, ~03:31 PDT.
- Codex was mid-flight on the old **US-088** ("make the full first-party workspace green"). That story is definitionally too big for a single Ralph iteration and produced a huge uncommitted diff before it stopped.
- Working tree: ~73 modified files + ~10 new probe/scratch files, all unstaged. Spans `crates/`, `packages/core/`, `registry/`, `pnpm-lock.yaml`.
- `scripts/ralph/todo/prd.json` still contains the old 134-story post-audit backlog (renamed from `scripts/ralph/prd.json` in this session).
- `packages/core/CLAUDE.md` explicitly warns that bare `pnpm test` hangs on this branch.
- `.agent/specs/release-candidate-spec.md` defines the 5-milestone RC plan (M1 quickstart tests → M2 permissions → M3 crashes → M4 isolation → M5 honest green).

## What changes from here

### Step 1 — Triage the in-flight working tree (human, ~30 min)

Do this first. Do not restructure the PRD or ask Ralph/Codex to do anything until the working tree is clean, because those 80+ uncommitted files will contaminate whatever comes next.

1. `git status --short` — classify every dirty file into one of three buckets:
   - **Keep**: change is cohesive, correct, and justifiable in 1–2 sentences. These land as focused conventional commits (`fix:`/`test:`/`feat:` — do not use `feat: US-088` since the old US-088 is being retired).
   - **Reset**: partial or speculative investigation artifact. `git checkout --` it.
   - **Delete**: scratch/probe file not meant to land. `git clean -f` it.
2. Land each `Keep` bucket as its own commit so the history stays bisectable.
3. After triage, `git status --short` should show only intentional new files (or nothing).
4. Append a one-line-per-commit summary to `scripts/ralph/progress.txt` so the next iteration has context.

Blocker to watch for: if inspection reveals that the diff was working toward a specific fix that matches one of the spec's M1–M4 stories (e.g., Codex was mid-way on US-243's http.request rerouting), keep those changes together, then open that specific story first in the new PRD so the next iteration finishes it.

### Step 2 — Replace the PRD with the release-candidate backlog (human or agent, ~20 min)

Rewrite `scripts/ralph/todo/prd.json` as a narrow 18-story release-candidate PRD driven by the five milestones in `.agent/specs/release-candidate-spec.md`. This replaces the 134-story audit backlog entirely. Use the existing schema: `project`, `branchName`, `description`, `testPolicy`, `userStories[]` with `id`, `title`, `description`, `acceptanceCriteria[]`, `priority`, `passes`, `notes`.

**Order (each is one Ralph iteration):**

1. **US-QS-GIT** — `packages/core/tests/git-quickstart.test.ts`, gated on registry-commands helper.
2. **US-QS-PI-EXT** — `packages/core/tests/pi-extensions.test.ts`, llmock-based.
3. **US-QS-S3** — `packages/core/tests/s3-backend.test.ts`, explicit `SKIP_S3=1` gate, no silent skipIf.
4. **US-QS-SANDBOX** — `packages/core/tests/sandbox-integration.test.ts`, explicit `SKIP_DOCKER=1` gate.
5. **US-217** — default-deny sidecar permissions. Migrate tests that rely on allow-all to explicit opt-in.
6. **US-218** — reject empty-operation/empty-path permission rules.
7. **US-219** — permission `*` glob must not cross `/`.
8. **US-190** — gate `FindListener` / `FindBoundUdp` / `GetProcessSnapshot` behind `network.inspect` / `process.inspect` permissions.
9. **US-173** — fallible ACP serialize; no more panic-on-send.
10. **US-184** — ACP inbound request waits for host response before falling back to `-32601`.
11. **US-188** — `register_toolkit` rejects duplicates; gate tool invocation behind permission.
12. **US-202** — no panic in `pump_process_events` when VM/process already reaped.
13. **US-243** — route guest `http.request`/`https.request` through undici + kernel socket table. Drop `_networkHttpRequestRaw`. Unblocks US-QS-NETWORK.
14. **US-250** — remove dev-shell host fallthrough; route through kernel command resolver only.
15. **US-251** — browser sidecar kernel calls go through `VmState`, not host shortcuts.
16. **US-QS-NETWORK** — fix `vm.fetch` guest-listener routing on top of US-243, add `packages/core/tests/network-vm-fetch.test.ts`, and delete the "vm.fetch does not currently translate arbitrary guest listener ports" warning from `packages/core/CLAUDE.md`.
17. **US-100** — diagnose and remove the bare `pnpm test` hang. Fix the hanging test(s) at the root. Delete the "Never run bare pnpm test without a filter" warning from `packages/core/CLAUDE.md`. Prerequisite for US-089.
18. **US-089** — final RC verification sweep. Lists every exact scoped command: `pnpm install --frozen-lockfile`, `pnpm check-types`, `pnpm test`, the five `cargo test -p agent-os-{kernel,bridge,execution,v8-runtime,sidecar} -- --test-threads=1`, the ignored snapshot test, and `pnpm test:migration-parity`. Zero first-party `#[ignore]`, zero product-debt `skip`/`skipIf` (only explicit external-credential gates like browserbase/duckdb remain).

**Each story's acceptance criteria must:**
- Name the exact scoped test command. Never list bare `pnpm test` except in US-100 and US-089.
- Be completable in one Ralph iteration (if you can't describe the change in 2–3 sentences, split it).
- Include `Typecheck passes: pnpm --dir packages/core check-types` (or workspace equivalent).

**Do not carry forward** the completed stories (US-201, US-295, US-297, US-307) or any of the post-audit backlog items that fall under "post-RC polish" in `.agent/specs/release-candidate-spec.md` §Out-of-scope (v8-bridge polyfill completeness, BARE codec expansion, cosmetic cron/EventEmitter/perf_hooks, filesystem semantic edge cases, POSIX shell builtin polish).

**Rewrite `description` and `testPolicy`** at the top of the new PRD to reflect the narrower RC scope. Drop all references to "US-001/US-002 restore reproducible workspace" and the "April 7–8, 2026 Ralph progress claims are stale" language from the old audit PRD — those are no longer relevant.

### Step 3 — Let Ralph/Codex continue (autonomous)

Once Step 1 (clean tree) and Step 2 (new PRD) are done, Ralph/Codex picks up at US-QS-GIT and works down the priority list. Each iteration is one story with a clear acceptance gate.

Monitoring:
- Progress appends to `scripts/ralph/progress.txt`.
- Each story should produce exactly one commit with the story ID in the message.
- If an iteration produces a >20-file diff without a clear single-story justification, stop and re-triage — it's another too-big-story situation.

## Things to decide before Step 3 starts

The release-candidate spec §Scope-decisions calls out five scope questions that will change the shape of several stories. Resolve these before starting the autonomous loop so stories don't get rewritten mid-run:

1. **`vm.fetch`**: fix guest-listener routing (recommended — handled by US-QS-NETWORK on top of US-243) or shrink the API?
2. **`workflow()` / `listPersistedSessions` / `createSignedPreviewUrl` / `mcpServers` / `GoogleDriveBlockStore` / `createS3BackendForAgent`**: implement in core, or move docs to RivetKit-only? Recommendation: move to RivetKit-only and delete from core docs. If you keep any of these in core, add a new story for each to cover its test.
3. **`@rivet-dev/agent-os-shell` (US-148)**: clean up or delete? If deleting, add a one-line story; if cleaning up, fold into US-250.
4. **Public `docs/` tree (US-147)**: resolve the uncommitted deletion before shipping so the doc source of truth is clear.
5. **Rivet repo path for actor-layer parity**: `~/r-aos`, `~/r16`, or something else. Needed for anything that changes an `AgentOs` method signature (US-QS-NETWORK and potentially US-217 if the permission option shape changes).

## What does not change

- `branchName: finish-ts-rust-migration` — stays the same.
- `project: agentOS` — stays the same.
- The Ralph script at `scripts/ralph/ralph.sh` — already points at `scripts/ralph/todo/prd.json` after the earlier rename.
- `scripts/ralph/progress.txt` — append-only, keep the existing Codebase Patterns section.

## References

- `.agent/specs/release-candidate-spec.md` — the what
- `.agent/notes/release-readiness-2026-04-12.md` — the findings behind the what
- `scripts/ralph/todo/prd.json` — the current stale PRD to replace
- `scripts/ralph/progress.txt` — recent Codex progress (check last 2–3 entries before starting)
- `packages/core/CLAUDE.md` — source of the `vm.fetch` and `pnpm test hangs` warnings that US-QS-NETWORK and US-100 must remove
