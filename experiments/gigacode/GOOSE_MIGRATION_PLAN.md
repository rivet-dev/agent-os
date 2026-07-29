# Gigacode Goose Migration Specification

## Status

This is the implementation specification. The product decisions below are
locked. Implementation has not started.

## Product Decisions

1. **Full Goose TUI over ACP.** Gigacode exists to put Goose's rich Ink TUI on
   top of AgentOS. The Rust interactive CLI is not an acceptable substitute.
2. **Dynamic AgentOS agent discovery.** Gigacode never hard-codes agent IDs.
   Every selectable agent comes from the live AgentOS instance's
   `listAgents()` response.
3. **Turn cancellation is not session cancellation.** Cancelling interrupts
   only the active prompt. The same session and actor remain usable.
4. **Persistent, resumable sessions.** There is exactly one Rivet actor per
   Goose session. Exiting or disconnecting Goose detaches without destroying
   it. Sessions can be listed, loaded, resumed, and explicitly deleted.
5. **Testing backends.** Automated integration and E2E tests use deterministic
   LLMock fixtures. The mandatory manual tmux test uses a real authenticated
   agent session.
6. **Platform scope.** Packaging and manual acceptance target this host only:
   Linux x64 (`linux-x64`, currently Linux 6.1 x86_64).
7. **Remove all OpenCode functionality.** Remove OpenCode from the entire
   repository and from every installed/transitive Gigacode dependency—not only
   from `experiments/gigacode`.

## Target Architecture

```text
Pinned Goose Ink TUI
    │  @aaif/goose-sdk / ACP
    ▼
Custom pinned `goose acp` server
    │  custom Goose ProviderDef: gigacode-acp
    ▼
`gigacode acp` provider process
    │  ACP ↔ Gigacode session/actor bridge
    ▼
Gigacode daemon + local Rivet engine
    │
    ├── bounded durable session catalog (listing/index only)
    │
    ├── Goose session A ──► AgentOS actor A ──► one inner ACP agent session
    ├── Goose session B ──► AgentOS actor B ──► one inner ACP agent session
    └── Goose session C ──► AgentOS actor C ──► one inner ACP agent session
                                      │
                                      ▼
                         live `/opt/agentos` agent registry
```

There are two deliberate ACP layers:

- The Ink TUI is an ACP client of the custom Goose core.
- Goose's `gigacode-acp` provider is an ACP client of `gigacode acp`.

This preserves the full Goose interface while making AgentOS—not Goose—the
owner of execution, tools, permissions, agent discovery, and durable agent
sessions.

## Source-of-Truth Invariants

- The projected `/opt/agentos` registry is the only agent catalog.
- `AgentOs.listAgents()` is called live; Gigacode does not parse package
  manifests or infer agents from npm dependencies.
- One outer Goose session maps to exactly one Rivet actor.
- An actor owns at most one current inner AgentOS ACP session.
- The actor's durable state owns execution state and the outer-to-inner session
  mapping. Process memory and Goose's local database are never authoritative.
- The daemon maintains a bounded durable catalog containing only list metadata
  and actor IDs. This prevents `session/list` from waking every sleeping actor.
  It is an index, not an execution coordinator.
- The catalog is reconciled with Rivet actor enumeration at daemon startup and
  before paginated listings. Actor existence and actor-owned mappings win every
  conflict; missing index rows are lazily rebuilt with bounded concurrency.
- Cancelling a prompt calls AgentOS turn cancellation and preserves the actor,
  mapping, transcript, and ability to send the next prompt.
- Closing the TUI, closing an ACP transport, or stopping the bridge detaches;
  none of those actions deletes an actor.
- Only an explicit user delete destroys the actor and its durable session
  state.
- Host-side Goose profiles, built-in tools, and extensions are disabled. Tools
  run through the selected AgentOS agent inside the VM; Goose must not provide
  a host-side bypass around AgentOS.

## Persistent Session Model

Each actor stores one bounded canonical record equivalent to:

```ts
type GigacodeSessionRecord = {
  sessionId: string;        // Stable outer Goose/ACP session ID and actor key
  actorId: string;
  agentType?: string;       // Exact ID returned by AgentOS listAgents()
  innerSessionId?: string;  // AgentOS ACP session ID
  cwd: string;
  title: string;
  createdAt: number;
  updatedAt: number;
  status: "new" | "idle" | "prompting" | "interrupted" | "unavailable";
};
```

The record and transcript/event history must be durable across bridge exit,
Goose exit, daemon restart, and actor sleep/wake. Metadata and event collections
remain bounded and expose typed limit errors.

The daemon catalog denormalizes only `sessionId`, `actorId`, `agentType`, `cwd`,
`title`, timestamps, and availability for fast pagination. Every create,
metadata update, unavailable transition, and delete updates the actor first and
then the catalog with an idempotent reconciliation marker. A catalog loss must
be recoverable from Rivet actors without losing a session.

### New session

1. Goose calls `session/new` with a workspace.
2. Gigacode creates one actor whose stable key is the outer session ID.
3. The actor calls live `listAgents()` and returns those installed agents as an
   ACP `Model`-category selection option.
4. `GIGACODE_DEFAULT_AGENT`, when set and present in the reported list, selects
   the default. Otherwise the first stable-sorted reported ID is the default.
5. No inner session is created until the user selects an agent and sends the
   first prompt.
6. On the first prompt, the actor creates one AgentOS session with the selected
   reported ID and persists the returned inner session ID.

If the AgentOS instance reports zero agents, session creation succeeds so the
TUI can explain the empty catalog, but prompting returns a typed
`no_agents_available` error.

### List and restore

1. Goose requests `session/list`.
2. Gigacode reads a bounded, paginated catalog page and reconciles its actor IDs
   against non-destroyed Rivet actors.
3. It wakes actors only to rebuild missing/stale rows, with bounded concurrency,
   and returns stable IDs, title, cwd, agent ID, timestamps, and availability.
4. Goose calls `session/load` or `session/resume` for the selected ID.
5. Gigacode resolves the existing actor by ID; it never creates a replacement
   actor for a known session.
6. If the inner session is already live, the bridge attaches to it. After actor
   sleep/wake or daemon restart, the actor calls AgentOS `resumeSession()` with
   the persisted inner ID, reported agent ID, cwd, environment, and transcript
   continuation path.
7. Native adapter resume is preferred; AgentOS transcript fallback is accepted
   and the external session ID remains stable even if AgentOS returns a new live
   inner ID.
8. If the persisted agent is no longer reported by `listAgents()`, listing marks
   the session `unavailable`; restore returns a typed `agent_unavailable` error
   without changing or deleting its state.

### Cancel, detach, and delete

- `session/cancel` forwards to AgentOS `cancelPrompt()` for the current inner
  session. The in-flight prompt returns ACP `cancelled`; the actor returns to
  `interrupted`/`idle`, and the next prompt reuses the same actor and session.
- ACP EOF, Goose exit, and TUI exit detach listeners and release process-local
  resources. The actor remains resumable.
- `session/close` means detach, not delete, for the Gigacode provider.
- The Goose TUI delete action must reach an explicit Gigacode delete operation.
  Delete closes the inner session if live, destroys the Rivet actor, removes it
  from listings, and is idempotent.

## Implementation Workstreams

### 1. Remove OpenCode repository-wide

Delete OpenCode product functionality and references, including:

- `registry/agent/opencode` and its upstream patch/build machinery.
- `examples/opencode` and custom/process example references.
- OpenCode session/headless helpers and tests under `packages/core/tests`.
- OpenCode-specific branches in core runtime/session behavior.
- `@agentos-software/opencode`, `opencode-ai`, and `@opencode-ai/sdk`
  dependencies and lockfile edges.
- Root/package READMEs, website agent docs, generated registry/routes, marketing
  copy, logos, and architecture/session-persistence examples.
- Workspace lifecycle allowlists and publish/registry discovery references.
- Gigacode compatibility routes, state projections, imports, launcher logic,
  installer repair, environment variables, and documentation.

Regenerate derived website and registry output when affected public surfaces
require it. Do not remove unrelated words
that merely contain the character sequence `opencode` unless they refer to the
OpenCode product.

### 2. Extend the AgentOS Rivet actor action surface

AgentOS core already implements live discovery, turn cancellation, and resume,
but the native Rivet actor currently exposes only create/prompt/close and
persisted-event reads. Add actor actions for:

- `listAgents()` returning the live sidecar registry entries.
- `cancelPrompt({ sessionId: innerSessionId })` preserving session usability.
- `resumeSession(innerSessionId, agentType, options)` returning native/fallback
  mode and the current live inner ID.
- Read/write of the single actor-owned `GigacodeSessionRecord` or a generic
  bounded metadata facility suitable for it.
- Explicit actor/session deletion and any transcript export path required by
  resume fallback.

Update the native actor plugin, generated TypeScript action declarations,
inspector surfaces, error mapping, and tests together. Preserve TypeScript/Rust
client behavioral parity for any core public API changes.

### 3. Replace Gigacode's OpenCode HTTP API with ACP

Implement `gigacode acp` using the ACP SDK compatibility version pinned with
Goose:

- `initialize`, `session/new`, `session/list`, `session/load`,
  `session/resume`, `session/prompt`, `session/cancel`, and detach/close.
- A Gigacode extension/custom request for explicit deletion if the pinned ACP
  version lacks a standard delete operation.
- Live AgentOS agent selection surfaced as ACP `Model` config values without
  hard-coded IDs.
- Streaming text, thought, plan, tool-call, and tool-result notifications rich
  enough for the Goose TUI.
- Permission request/reply forwarding with Goose in `approve` mode.
- Stable outer IDs and remapping of resumed inner session IDs.
- A bounded SQLite session catalog under Gigacode state with pagination,
  idempotent actor-first updates, startup reconciliation against Rivet actors,
  and lazy recovery from actor-owned metadata.
- Bounded concurrent listing, sessions, prompts, event buffers, pending
  permissions, input, stderr, and shutdown.
- JSON-RPC-only stdout and bounded diagnostics on stderr.
- Typed errors for unavailable agents, unknown sessions, limits, invalid state,
  unsupported content, and failed native/fallback resume.

Keep only the daemon health/admin endpoints needed for startup, debugger,
session observation, and explicit deletion. Do not create a Goose-compatible
REST API.

### 4. Build the full Goose ACP/TUI integration

Pin one exact Goose source commit/release and matching:

- `@aaif/goose-sdk`.
- Ink TUI package from `@aaif/goose`/`ui/text`.
- Goose Rust binary.
- `@agentclientprotocol/sdk` compatibility version.
- Gigacode Goose patch checksum.

The custom Goose patch must:

- Register `gigacode-acp` as a real Rust `ProviderDef` and inventory/catalog
  entry whose command is `gigacode acp`.
- Delegate dynamic agent/model config, list, load, resume, cancel, and delete to
  the Gigacode ACP provider.
- Preserve the rich TUI's session browser, transcript, streaming tool UI,
  permission prompts, interruption controls, and session restoration.
- Avoid provider assumptions that ACP sessions cannot be listed or resumed.
- Disable host-side Goose tools/extensions for Gigacode sessions.

`gigacode` launches the pinned Ink TUI connected to the explicit custom
`goose acp` binary. It must not fall back to a global Goose, optional upstream
binary, or `npx @aaif/goose@latest`. `gigacode run` uses the same custom Goose
core in headless mode.

### 5. Package, document, and operate

- Package the custom Goose binary, Ink TUI, SDK runtime, Gigacode ACP bridge,
  AgentOS sidecar/plugin, source lock, checksums, licenses, and modification
  notice for this Linux x64 host.
- Keep installation self-contained and recoverable without checkout-local
  `node_modules` or post-install network fallback.
- Isolate Goose configuration with a Gigacode-owned `GOOSE_PATH_ROOT` while
  keeping Gigacode's session list actor-backed.
- Mount real host credentials only for the explicit live/manual path. Automated
  tests use synthetic credentials and isolated HOME/XDG paths.
- Document the full-host trust model, dynamic agent catalog, persistence,
  cancellation, resume modes, deletion, and recovery behavior.

## Acceptance Gate 1: Goose SDK Full-Stack E2E

```bash
pnpm --dir experiments/gigacode test:e2e
```

Install into temporary deployment directories and use only installed artifacts.
The test explicitly spawns the installed custom `goose acp`, wraps its
stdin/stdout with `ndJsonStream`, and constructs `GooseClient` from the pinned
`@aaif/goose-sdk`.

Required chain:

```text
GooseClient
  -> custom goose acp
    -> gigacode-acp provider
      -> installed gigacode acp
        -> daemon/Rivet
          -> persistent AgentOS actor/session
            -> LLMock
```

The gate passes only if it proves:

1. Exact Goose/SDK/TUI/ACP versions, patch identity, and binary hashes.
2. Isolated HOME, XDG, Goose, Gigacode, Rivet, ports, and synthetic credentials;
   no user-global Goose profile, tool, extension, or binary is loaded.
3. SDK initialization negotiates the pinned ACP version and exposes
   `gigacode-acp`.
4. A new session creates exactly one actor. Its selectable agents exactly equal
   that actor's live `listAgents()` response—no missing or extra IDs.
5. Selecting an LLMock-configured reported agent creates one inner session.
   Two ordered streaming chunks and rich tool notifications arrive before the
   exact successful prompt stop reason.
6. A second prompt reuses the same outer ID, actor ID, agent ID, and inner ID.
7. A held third prompt is cancelled only after a recorded request/first chunk.
   It resolves `cancelled`; actor and inner session IDs remain unchanged; a
   sentinel fourth prompt succeeds in the same session.
8. Goose and its bridge exit without deleting the actor. A fresh custom Goose
   process lists the session, loads it, restores its transcript, resolves the
   same actor, resumes the inner session through native or documented fallback,
   and completes another deterministic prompt.
9. A second separately created Goose session produces a different actor, proving
   the one-to-one mapping. Listing returns both exactly once.
10. Explicit deletion removes only the selected actor/session. The remaining
    session still prompts successfully; deleting it returns actor count to the
    baseline.
11. Owned processes exit, ports can be rebound, no unhandled rejection appears,
    and successful temporary test state is removed only after explicit deletes.
12. Runtime recovery restores removed deployed dependencies without consulting
    the checkout or network and repeats the session-list smoke check.

LLMock must use deterministic multi-chunk, cancellation, tool-call, and resume
fixtures. Failure artifacts include bounded stdout/stderr, daemon logs, LLMock
requests, session/actor/inner IDs, resume mode, revisions, hashes, PIDs, ports,
and the named timeout that fired.

## Acceptance Gate 2: Direct ACP and Actor Integration

```bash
pnpm --dir experiments/gigacode test:integration
```

Drive `gigacode acp` directly with `@agentclientprotocol/sdk`, without Goose.
Use independent temporary state for each group.

### Dynamic discovery

- Test AgentOS instances reporting zero, one, and multiple synthetic agents.
- Assert the ACP selection values exactly match each live report and update on a
  new session after the projected registry changes.
- Assert default selection uses a valid `GIGACODE_DEFAULT_AGENT` or the stable
  sorted fallback, never a hard-coded ID.
- Assert a removed persisted agent makes an existing session unavailable but
  preserves it for later recovery.

### Actor/session lifecycle

- Assert new, prompt, detach, list, load, resume, explicit delete, and idempotent
  delete transitions against stable outer/actor IDs.
- Force actor sleep/wake, daemon restart, bridge EOF, bridge `SIGTERM`, and
  bridge force-kill. None may destroy or duplicate the actor.
- Delete/corrupt the daemon catalog, restart it, and assert bounded
  reconciliation rebuilds listings from the existing actors without creating
  actors or losing outer-to-inner mappings.
- Assert native resume preserves the inner ID; fallback resume atomically stores
  the replacement live inner ID while preserving the outer ID.
- Saturate the session/list limit and assert warning, typed error/pagination
  behavior, bounded concurrency, and no unbounded actor fan-out.

### Turn cancellation

- Synchronize on an LLMock request or first chunk, then call `session/cancel`.
- Assert AgentOS receives `cancelPrompt()`, the prompt resolves `cancelled`, no
  post-cancel chunks leak into the next turn, and the same inner session answers
  a sentinel prompt.
- Test idle/unknown cancel, repeated cancel, cancellation during a permission
  request, and cancellation racing prompt completion.

### Mounts and permissions

- Use disposable workspaces and synthetic Claude/Codex-style credential
  directories; never automated access to real home credentials.
- Verify workspace/full-host projection and both read-write/read-only credential
  policies with exact host-visible effects and POSIX errors.
- Script deterministic reject and approve tool calls. Rejection has no side
  effect; approval completes; permission and tool event IDs remain correlated
  through Goose-compatible ACP notifications.

### Protocol hygiene and errors

- Test invalid version, unknown method/session/agent, invalid params, concurrent
  prompts, unsupported content/MCP servers, each configured limit plus one,
  child exit, broken pipe, invalid JSON, and stdout pollution.
- Recoverable cases return exact JSON-RPC code plus `data.type`, followed by a
  successful valid request. Unparseable framing may close with bounded stderr.
- Every operation and poll has a named timeout; no promise, listener, permission,
  child, actor, or buffer leaks.

## Acceptance Gate 3: Live Goose TUI in tmux

This mandatory test uses the installed rich Ink TUI, an agent reported by the
real AgentOS instance, and the tester's real authenticated session. It must run
in a disposable workspace. Evidence must redact credentials, prompts containing
secrets, and provider response metadata that could identify the account.

Start a persistent shell and continuous capture before Goose:

```bash
artifact_dir=$(mktemp -d /tmp/gigacode-goose-live.XXXXXX)
session=gigacode-goose-live
tmux new-session -d -s "$session" "$SHELL"
tmux set-option -t "$session" remain-on-exit on
tmux pipe-pane -o -t "$session" "cat >> '$artifact_dir/tmux.log'"
tmux send-keys -t "$session" \
  "GIGACODE_STATE_DIR='$artifact_dir/state' GOOSE_MODE=approve gigacode" Enter
tmux attach-session -t "$session"
```

The tester records pass/fail for each item:

1. The full Goose TUI renders without onboarding and shows `gigacode-acp` plus
   exactly the agents reported by the real AgentOS actor.
2. Selecting one reported agent and sending a real prompt creates one actor and
   streams text/tool progress through the rich TUI.
3. Workspace read and write prompts affect only known disposable marker files;
   a second pane verifies exact contents.
4. A real permission request can be rejected with no side effect, then approved
   with the expected side effect.
5. Interrupt a visibly active turn. The turn reports cancellation, then a new
   prompt succeeds in the same TUI session and debugger-visible actor/inner IDs
   remain unchanged.
6. Exit Goose to the persistent shell without deleting the actor. Relaunch
   `gigacode`, open the session browser, find the prior session, restore it, see
   prior transcript context, and complete another real prompt using the same
   actor.
7. Create a second session and verify it owns a different actor. List both,
   switch between them, and confirm isolation.
8. Delete one session in the TUI and verify only its actor disappears. Delete the
   second, stop the daemon, and verify no actors, owned PIDs, ports, or marker
   files remain.

Continuous `pipe-pane` output is primary evidence; `capture-pane` is
supplemental. The redacted bundle records commands/exit codes, host OS/arch,
Goose/Gigacode revisions and hashes, reported agent IDs, actor/inner/outer IDs,
resume mode, and checklist results.

```bash
pnpm --dir experiments/gigacode manual:verify --artifact-dir "$artifact_dir"
```

## Final Validation

All commands exit zero:

```bash
pnpm --dir experiments/gigacode check-types
pnpm --dir experiments/gigacode test:integration
pnpm --dir experiments/gigacode test:e2e
bash -n experiments/gigacode/install-global.sh
node scripts/verify-fixed-versions.mjs
```

Repository-wide removal criteria:

- `registry/agent/opencode`, `examples/opencode`, OpenCode-specific tests,
  helpers, docs, images, and generated registry entries do not exist.
- No committed package manifest or lockfile edge references
  `@agentos-software/opencode`, `opencode-ai`, or `@opencode-ai/sdk`.
- No runtime, client, website, example, installer, publish, or documentation
  surface references the OpenCode product. The migration specification is the
  only allowed historical reference.
- Generated website and registry artifacts are current.

Goose/Gigacode criteria:

- The TUI agent catalog equals live AgentOS reports and contains no hard-coded
  agent allowlist.
- One actor per session holds across create, cancel, detach, list, restore,
  daemon restart, actor sleep/wake, and delete.
- Automated E2E, direct integration, and verified live tmux evidence all refer
  to the same Gigacode revision and custom Goose build.
- The installed Linux x64 runtime is self-contained, version/checksum verified,
  license-complete, and has no checkout or network fallback.
- AgentOS product versions remain `0.0.1` in committed files.
- Unrelated working-copy changes remain untouched.
