# agentOS

Always spell the product name `agentOS`, never `AgentOS`; do not alter type
identifiers such as `AgentOSActorConfig`.

agentOS owns the runtime, kernel, VFS, language execution, registry packages,
ACP/session layer, agentOS client APIs, docs, and publish machinery. agentOS
Exec is the JavaScript, TypeScript, and Python execution surface of agentOS.

## Boundaries

- Keep agentOS product versions pinned at `0.0.1` in committed files. Release
  workflows apply real versions transiently with `scripts/publish`; never commit
  release-version rewrites.
- agentOS-owned npm packages must use the `@rivet-dev/agentos-*` namespace.
  Registry software packages must use `@agentos-software/*`. Never introduce
  packages under `@agentos/*`.
- Call guest environments VMs, not sandboxes, except when referring to a package
  or public API that already uses the word.
- The protocol has no backward compatibility guarantee. Client, sidecar, and
  protocol crates ship in same-version lockstep; update both sides together.
- Generic runtime and language-execution work belongs here. Do not add a
  compatibility mirror or a second package namespace for AgentOS language execution.
- Keep root `package.json` scripts limited to Turbo orchestration; repo-specific
  commands belong in `justfile` recipes or scoped package scripts.
- agentOS targets native Linux/container execution. Browser support is not
  needed or supported here: browser sources may remain as dormant reference
  code, but their entrypoints must stay disabled and they must not enter default
  builds, CI, publication, or behavioral-parity requirements without a
  separately approved design.

## Security Model

Trust model:

- **Client**: trusted, except for code/payloads it submits for execution.
- **Sidecar/runtime**: trusted enforcement point. It owns the kernel, VFS,
  mounts/plugins, socket table, permissions, and resource policy.
- **Executor**: untrusted V8 isolate or WASM guest. Assume guest JS/Python/WASM
  and third-party packages are hostile.

The security boundary is sidecar/runtime to executor. Client-provided config is
trusted input; a guest bypassing an applied policy is in scope, while a client
choosing dangerous credentials, endpoints, mounts, or allowlists is not a
runtime escape.

Every limit, timeout, queue, buffer, and per-entity collection must be bounded
by default, warn near threshold, and fail with a typed error that names the
limit and how to raise it. Host-visible warnings/errors must reach stderr/log
or structured trace paths, not stay trapped in the VM.

Never swallow errors silently. Every failure must either propagate as a hard,
typed error to the caller (preferred) or be clearly logged at the failure site;
empty `catch`/`let _ =` on fallible operations and fire-and-forget promises
that drop rejections are bugs, not defensive coding. For guest-visible
surfaces, prefer matching Linux behavior — the correct POSIX errno delivered to
the guest — over inventing a softer fallback that hides the failure.

## SQLite Schema Ownership

The per-VM SQLite database is physically shared but has three independent schema
owners. Each owner must manage its own version table and append-only migration
ladder, operate only on its own table namespace, and never read, advance,
migrate, or delete another owner's schema:

- Filesystem storage owns `agentos_fs_*`, including
  `agentos_fs_schema_version`.
- Sidecar/core durable state owns `agentos_core_*`, including
  `agentos_core_schema_version`. This namespace is intentionally generic; do
  not name it after sessions, ACP, or another current consumer.
- The agentOS TypeScript actor layer owns `agentos_actor_*`, including
  `agentos_actor_schema_version`.

Do not use a shared schema-version table, a `component` discriminator, or a
global migration sequence across these owners. Each migration must update its
owner's version in the same SQLite transaction or savepoint as its schema
changes. agentOS-owned tables must be `STRICT`.

There is no compatibility requirement for the previous SQLite layout. Remove
the shared component-version mechanism and rename or replace legacy
`agentos_vfs_*`, `agentos_session*`, and `agent_os_*` tables directly; do not
add compatibility views, aliases, legacy adoption paths, or dual writes.

## Runtime And Registry

- The WASM guest is a Linux-in-WASM environment — a POSIX superset of WASI, not
  stock WASI. The kernel supplies a POSIX userspace via host imports: a process
  table with real `fork`/`exec`/`wait` and signals, fd/socket tables, the brush
  shell, and a uutils coreutils surface. A program written for Linux is expected
  to run unmodified, subject to the available execution runtime (Node.js, WASM,
  Python). Do NOT reason about guest capabilities from plain-WASI limits (e.g.
  "no shell", "no subprocess spawning", "no process model") — those hold for raw
  WASI Preview 1, not for agentOS. See
  `website/public/docs/docs/architecture/processes.md` and
  `posix-syscalls.md`, and `crates/kernel/CLAUDE.md`.
- The projected `/opt/agentos` filesystem is the source of truth for software
  and agent resolution. Read it live; do not cache package lists captured at VM
  configuration time.
- Packages are packed `.aospkg` files (`crates/vfs/package-format/v1.bare`:
  header + vbare manifest + mount index + mount tar) projected under
  `/opt/agentos/pkgs/<name>/<version>`; commands are linked under
  `/opt/agentos/bin/`. The vbare chunk1 manifest is the only runtime manifest —
  `agentos-package.json` is toolchain input, stripped at pack time and never
  shipped or materialized into the guest.
- Agent resolution and enumeration are sidecar-owned. Clients send agent names
  and forward a single package `path` (the `.aospkg`, or a transition dir);
  they do not scan `node_modules` or parse adapter manifests for discovery.
- TypeScript and Rust clients must stay behaviorally identical. Any public
  method or wire behavior change in one client must be mirrored in the other.
- Clients are thin transport adapters, not runtime policy owners. They may
  validate and serialize explicit caller input, forward requests, route host
  callbacks/events, and retain host-only state that the sidecar cannot access.
  VM defaults, base environment, filesystem/bootstrap policy, default software,
  permission policy, agent/session orchestration, prompt assembly, and other
  behavior shared across clients belong in the sidecar/runtime.
- Behavioral parity must come from one sidecar-owned implementation, not copied
  TypeScript/Rust/actor constants or parallel state machines. Prefer omitted
  wire fields meaning "use the sidecar default"; clients should send overrides
  only when the caller explicitly supplied them.
- Agent adapters must use real upstream SDKs. Do not replace SDK adapters with
  direct API-call stubs.
- `rivet-dev/pi-acp` is an agentOS-maintained fork. When Pi ACP behavior needs
  to change, fix and test the fork directly, push the fork commit, then update
  the pinned commit and verified source-archive checksum in
  `software/pi/scripts/build-pi-acp.mjs`; do not work around fork bugs in the
  agentOS package wrapper or resolve `pi-acp` from npm.
- WASM command binaries and every toolchain build output are generated
  artifacts. Never commit `packages/runtime-core/commands/`, `software/*/bin/`,
  `toolchain/vendor/`, `toolchain/c/{build,vendor,libs,sysroot,.cache}/`, or
  `toolchain/std-patches/wasi-libc-overrides/*.o`. A fresh checkout intentionally
  contains source and patches only. Rebuild and stage the complete default tool
  set from the repository root with:

  ```bash
  pnpm install --frozen-lockfile
  just tools-rebuild
  ```

  `just tools-rebuild` runs `just toolchain-build`, copies the canonical output
  from `toolchain/target/wasm32-wasip1/release/commands/` into runtime staging,
  and builds the `@agentos-software/*` packages. For focused development,
  `just toolchain-cmd <command>` rebuilds one command, but it is not sufficient
  for a release or complete package validation. Publish workflows must rebuild
  and stage the complete command set and fail when it is absent or incomplete.

## Software Build (WASM Toolchain)

Registry software is **real upstream Linux software** (GNU coreutils, grep, sed,
gawk, real curl/sqlite/duckdb/vim, …) compiled to `wasm32-wasip1` against a
**sysroot we fully own** — a patched Rust std + libc whose gaps are filled by
custom host-syscall imports. Treat that target as **native POSIX**;
`wasm32-wasip1` is an implementation detail, not a feature ceiling.

### `wasm32-wasip1` capability rule

- **Never use `target_os = "wasi"` as evidence that a POSIX capability is
  absent.** In agentOS it does not imply missing symlinks, executable modes,
  TCP/UDP/Unix sockets, processes, signals, polling, or other Linux behavior.
- When an upstream Rust crate hides a supported API behind `cfg(unix)`, expose
  the same behavior through the corresponding `std::os::wasi` or libc API. A
  crate or Codex patch may adapt target-specific API names and cfg routing; it
  must not replace working behavior with `Unsupported`, a no-op, a dummy
  capability report, or a Windows fallback.
- Fix a genuinely missing POSIX operation in this order: Rust std, agentOS
  libc/sysroot, then the host import and kernel implementation. Do not make an
  application compile by disabling the operation one layer above the gap.
- A WASI stub is allowed only for a genuinely host-native architectural
  boundary, not for a missing syscall. Its source must state which trusted
  agentOS component owns the behavior and why the guest implementation must not
  run. For example, guest HTTP uses the sidecar-brokered HTTP transport because
  the sidecar owns network policy; that does not mean guest TCP sockets are
  unsupported.
- Before accepting a new WASI patch or stub, audit every error path and cfg for
  capability loss. Compilation alone is not validation: add or run a guest
  behavior test for each POSIX surface the patch exposes.

- **We do not depend on stock WASI / wasi-libc.** The sysroot is ours. A missing
  libc/POSIX API (`getrlimit`/`RLIMIT_NOFILE`, `getgroups`, spawn, fd dup, …) is
  never a blocker — implement the real operation in the patched
  std/libc/host-import layer. "WASI doesn't have X" is not a reason to stop; X is
  ours to add. Do not substitute a success-returning stub, hard-coded value, or
  target-level omission for a syscall agentOS can implement.
- **Fix portability one layer down, in the sysroot** — a new std/libc patch or a
  new host import — not with `cfg(target_*)` branches or shims in the tool's own
  source. A WASM-specific branch in application code usually means the fix
  belongs in the libc layer.
- **Patch the real upstream tool only as a fallback**, when the fix genuinely
  cannot live in the sysroot. Patching the real tool is allowed; reimplementing
  it is not.
- **"NOT POSSIBLE" is reserved for genuine impossibility** after exhausting both
  sysroot patches and tool patches — never for a missing syscall we could
  implement. Document the specific wall if you claim it.
- **Working in `software/`, you may (and should) fix the layer underneath.** When
  a package behaves differently from real Linux, the root cause is usually not the
  package — it's the runtime. It is in-scope and expected to fix the underlying
  implementation: the Node-compat / bridge layer, the WASM execution runtime, the
  kernel/VFS syscalls, or the patched sysroot/libc. Do **not** paper over a
  Linux-deviating behavior in the package, its wrapper, or its test — chase it
  down into whichever runtime layer owns it and make that layer match Linux.

## JavaScript Networking Architecture

- The migration target is Node.js's evented networking invariants:
  sidecar-owned nonblocking I/O, readiness-driven bounded work, real `Duplex`
  backpressure, active-handle liveness, and fair scheduling. Do not reproduce
  Node's trust boundary by exposing descriptors to the guest.
- New or migrated TCP, Unix, UDP, listener, TLS, and HTTP/2 code must use the
  process's single Tokio runtime, shared by all VMs and subsystems with a fixed
  worker count. Do not create subsystem- or VM-owned Tokio runtimes,
  per-socket/per-session I/O threads, unbounded I/O queues, recurring I/O
  polling timers, or one event per packet/chunk. Existing instances are
  migration debt governed by the phase exit gates in the linked specification,
  not patterns to preserve.
- Guest V8/Node execution is not a Tokio task. Run synchronous, thread-affine,
  untrusted guest execution on a separate bounded executor so it cannot block a
  trusted sidecar runtime worker. Unavoidable blocking host work must use
  bounded admission and fixed workers, not another Tokio runtime.
- Keep V8's process-global platform topology explicit: one process-lifetime
  owner and a fixed four-worker background pool. Do not pass zero to V8's
  default-platform worker count, because that makes the thread census depend on
  host CPU count.
- New readiness paths must use coalesced level state: durable bounded sidecar
  state, at most one queued wake per execution session, and application reads
  stopped when `Readable.push()` returns false until `_read()` resumes them.
- The bridge migration must route responses directly to their registered call
  waiter and replace blocking session-command admission. Its completed state
  never makes a synchronous call scan, consume, or defer unrelated session
  events while waiting for its response.
- Native process transport uses three strict physical lanes: fd 0 for host
  `RequestFrame` ingress, stdout for non-heartbeat `EventFrame` egress, and the
  required inherited full-duplex fd 3 for responses, sidecar requests,
  heartbeats, callback results, and typed shutdown control. Never multiplex a
  registered response or termination behind ordinary frames.
- Signal delivery must use the bounded/coalesced session broker and must never
  spawn an OS thread per delivered signal. Embedded V8, standalone WASM, and
  Python must share sidecar reactor capabilities rather than own parallel
  networking implementations. Browser runtime sources remain in-tree only as
  dormant reference code; browser entrypoints and support remain disabled until
  a separate design is approved.
- The architecture and migration contract are specified in
  `docs/design/unified-sidecar-runtime.md`.

## Publishing

- `scripts/publish` is the source of truth for npm/crates discovery, version
  rewriting, npm publish, crates publish, release assets, and R2 upload.
- Publishable npm packages and Rust crates are agentOS-owned. agentOS language
  execution is exposed through `@rivet-dev/agentos`; do not publish separate
  language packages, compatibility artifacts, or language subpaths.
- The release workflow must build and stage the native sidecar binaries,
  runtime-sidecar binaries, registry WASM commands, and pyodide assets before
  publish.
- `scripts/verify-fixed-versions.mjs` must pass in the committed tree.

## Docs

- The agentOS website lives in `website/`. Its canonical public domain is
  `agentos-sdk.dev`. This is the only accepted AgentOS domain for URLs, schema
  IDs, email addresses, package metadata, and documentation.
- Keep docs current in the same change as user-facing behavior: public APIs,
  runtime options, env knobs, limits, architecture, and package names.
- Runnable docs code must come from real checked example files via the docs
  theme `<CodeSnippet>` mechanism. Inline code is fine only for shell commands,
  config fragments, or non-runnable examples.
- Validate docs changes with `pnpm --dir website build` when the site changes.
- Run `just docs-check-links` when changing documentation paths, routes,
  redirects, headings used as link anchors, or shared navigation links. It
  builds and crawls the rendered Astro site. Pass `true` to include external
  URLs (`just docs-check-links true`).

## Tests

- Required PR CI should target under 10 minutes of wall-clock time and must stay
  under 15 minutes. Budget the complete critical path, including checkout and
  dependency setup, cache restore/save, artifact compression, upload, download,
  extraction, job fan-in, and tests; moving work between jobs does not make that
  work free. Transfer only the smallest required outputs, never Cargo `target/`,
  `node_modules/`, duplicated software staging trees, or unstripped debug
  binaries. Balance total transferred bytes against critical-path latency:
  parallel consumers may repeat a small, measured setup or download tailored
  artifacts when that materially reduces wall time and runner capacity is real;
  do not consolidate them into a slower serial gate merely to minimize bytes. If
  CI misses the 10-minute target, warn the user promptly, identify the slow job,
  transfer, or setup step, and compare the workflow diff and recent baseline run
  times; do not silently normalize slow CI.
- Cheap gates for normal changes: `cargo check --workspace`, `pnpm build`,
  `pnpm check-types`, publish helper checks, changed script syntax checks, and
  workflow YAML parsing.
- Expensive runtime suites, cross-repo dispatches, real publish workflows,
  benchmarks, protocol fixture regeneration, and end-to-end sanity runs belong
  in the explicit expensive validation phase.
- Tests that prove absence of a bound by saturating CPU, heap, fd/process/socket
  limits, or watchdog timeouts must be ignored/skipped by default with a clear
  reason. Fast tests where the configured safeguard fires should stay in the
  default suite.

## Gigacode Performance Investigations

- For cold-start latency, run `gigacode` directly and use the plain
  `[gigacode]` phase lines and durations mirrored from `daemon.log` while the
  client waits for provider bootstrap. These startup lines are intentionally
  human-readable and separate from Pino session logs.
- Investigate Gigacode latency from its per-session Pino JSONL logs, not by
  inferring timing from the OpenCode screen or the aggregate `daemon.log`.
- Logs live at
  `~/.local/state/gigacode/session-logs/<open-code-session-id>.jsonl` by default,
  or under `$GIGACODE_STATE_DIR/session-logs/` when that override is set.
- Reproduce one turn in a fresh session, identify the newest log with
  `ls -lt ~/.local/state/gigacode/session-logs`, then inspect its ordered
  `event` and `durationMs` fields with `jq`.
- Compare `rivet.actor.resolved`, `agentos.session.created`,
  `agentos.prompt.completed`, `prompt.completed`, `session.idle`, and
  `agentos.connection.disposed` before optimizing. The actor event measures
  resolution of the shared per-cwd workspace actor; the ACP event measures the
  distinct harness session created inside it.
- Preserve the raw JSONL file when reporting a regression. Use
  `GIGACODE_LOG_LEVEL` to change the Pino level; performance phase records are
  emitted at `info`.

## Version Control

- Commit and PR titles are plain conventional commits with no coding-agent
  attribution.
- PR descriptions should be a short high-level bullet list. Avoid per-file
  narration and generated-by language.
