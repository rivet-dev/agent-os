# Native-only simplification TODO

This list tracks the architecture cleanup that precedes the durable AgentOS
session API. The canonical session design is maintained separately at
`~/.agents/specs/agentos-session-api.md`; session persistence, history, adapter
restoration, and the shared SQLite layer belong to that follow-up revision.

## 1. Use one native ACP orchestration path

**Status:** complete.

`crates/agentos-sidecar/src/acp_extension.rs` is the only product ACP
orchestrator. The older `agentos-sidecar-core` implementation is retained only
as dormant browser reference source: it is excluded from the workspace,
default builds, and publication. Browser entrypoints remain disabled.

The native path keeps the existing explicit restore state machine and is async.
An adapter exit is terminal: AgentOS evicts the dead live route, emits a typed
exit event, and requires the caller to restore explicitly. It never respawns an
adapter or replays a prompt automatically. The durable session revision will
replace this transient orchestration with the specified session store and
adapter pump without reintroducing a second product path.

- [x] Exclude the old core from native builds and publication.
- [x] Keep create, restore, prompt, cancel, and close on the native extension.
- [x] Remove implicit adapter restart and request replay.
- [x] Preserve native resume and unknown-session fallback behavior for the
      upcoming durable-session migration.
- [x] Cover terminal adapter exit and explicit retry behavior in
      `acp_adapter_stderr.rs`.
- [x] Guard the single native product path in `architecture_guards.rs`.

## 2. Remove adapter-specific policy from shared ACP code

**Status:** complete.

The shared sidecar no longer branches on Claude, Codex, Pi, Pi CLI, or OpenCode
names. It assembles the AgentOS system prompt once and forwards it through the
adapter-neutral `ACP_APPEND_SYSTEM_PROMPT` launch contract. AgentOS-owned
package launchers translate that value into each upstream adapter's required
flag or context file while continuing to invoke the real upstream SDK adapter.

- [x] Remove adapter-name branches for prompt injection and config defaults.
- [x] Move Pi, Pi CLI, Claude, and OpenCode launch compatibility into their
      AgentOS-owned launchers.
- [x] Keep ACP capabilities and payload handling adapter-neutral.
- [x] Add a source guard preventing adapter-name policy from returning.
- [x] Add integration coverage proving the generic launch contract reaches the
      adapter and preserves assembled base/additional/binding instructions.

## 3. Disable browser support

**Status:** complete.

AgentOS targets native Linux/container behavior. Browser sources stay in the
repository for reference, but their Rust and TypeScript public entrypoints are
disabled. Browser crates and packages remain outside default builds, CI,
publication, and compatibility-mirror publication.

- [x] Disable browser Rust and TypeScript public entrypoints without deleting
      reference source.
- [x] Document the native-only boundary in `CLAUDE.md`.
- [x] Guard source retention plus build/publication exclusion.

## 4. Remove duplicate TypeScript in-memory filesystems

**Status:** complete.

The Rust sidecar VFS is the production AgentOS filesystem. The duplicate
in-memory filesystem, overlay filesystem, and in-memory layer store were
removed from `@rivet-dev/agentos-core` and its public exports. The low-level
runtime compatibility API now requires a caller-owned filesystem rather than
creating a default.

One implementation remains at the explicit
`@rivet-dev/agentos-runtime-core/test-runtime` test surface for repository test
fixtures and benchmarks that exercise host VFS callbacks. Disabled browser
sources remain dormant reference code, not a supported production VFS.

- [x] Remove the core in-memory VFS and overlay/layer-store implementations.
- [x] Remove their root and agentos compatibility exports.
- [x] Require a caller-owned filesystem in the low-level compatibility runtime.
- [x] Move remaining repository fixtures to the explicit test-only surface.
- [x] Remove obsolete duplicate semantic tests.
- [x] Add a source guard preventing a production AgentOS TypeScript VFS default
      or implementation from returning.

## 5. Remove the client transport event log

**Status:** complete.

The Rust sidecar transport correlates pending responses and fans out live events
through a bounded broadcast channel. It retains no `WireEventLog`, replay
history, global/route sequence state, or provisional process ownership. Durable
ACP history belongs to the sidecar-owned SQLite session store in the follow-up
session revision, not in a client transport.

- [x] Remove client-side retained event history and route sequencing.
- [x] Keep bounded live event delivery with typed broadcast lag.
- [x] Prove an event arriving before its response remains live-delivered.
- [x] Prove overflow reports lag instead of retaining/replaying history.
- [x] Add a source guard preventing transport history from returning.

## 6. Remove filesystem and terminal operations from ACP

**Status:** pending; do separately from the session API.

ACP should carry agent session input/output, permissions, and lifecycle rather
than acting as a second filesystem or process-execution capability plane.
Before removal, audit real upstream adapters to identify any use of ACP
`fs/*`/`terminal/*` requests and ensure necessary behavior exists through normal
AgentOS runtime tools.

- [ ] Inventory advertised capabilities, inbound handlers, callback routes, and
      retained terminal state.
- [ ] Characterize which upstream adapters depend on these requests.
- [ ] Stop advertising filesystem and terminal client capabilities.
- [ ] Reject adapter-initiated `fs/*` and `terminal/*` methods with a typed
      method-not-supported response.
- [ ] Remove obsolete handlers, state, callbacks, and success-path tests.
- [ ] Add a guard preventing the alternate capability plane from returning.

## 7. Remove cron from AgentOS completely

**Status:** pending; do separately from the session API.

Scheduling is an application, actor, container, or infrastructure concern.
Remove the AgentOS scheduler and its TypeScript/Rust APIs, actor actions/events,
protocol messages, configuration, state, examples, docs, tests, and generated
compatibility surfaces. External schedulers should wake the relevant actor/VM
and submit an ordinary AgentOS request.

- [ ] Inventory the complete cron surface and characterize current behavior.
- [ ] Remove scheduler implementation, protocol, client, actor, and state
      ownership.
- [ ] Remove examples, website pages, compatibility exports, and cron-only
      tests.
- [ ] Document the external scheduler boundary.
- [ ] Add a guard preventing AgentOS cron ownership from returning.

## Non-goals

- Do not move client policy into another duplicate sidecar subsystem when Linux
  or ACP already supplies the behavior.
- Do not edit or replace third-party adapter implementations.
- Do not restore browser support implicitly as part of native ACP work.
- Do not weaken bounds, ownership enforcement, typed failures, or error
  propagation while simplifying the implementation.
