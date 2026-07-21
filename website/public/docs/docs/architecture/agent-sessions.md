# Agent Sessions

How durable sessions, ACP adapters, prompts, permissions, and history flow through AgentOS.

An AgentOS session is a durable SQLite record with an optional live ACP adapter. The stable public session ID and the adapter's private ACP session ID are deliberately separate.

## Ownership

- **Actor:** lifecycle and `keepAwake` for active turns; one actor owns one VM.
- **Core SDK:** thin TypeScript or Rust transport and native ACP data types.
- **Sidecar:** session policy, SQLite transactions, adapter lifecycle, restore selection, and event delivery.
- **ACP adapter:** agent-specific private context and native protocol behavior.

```text
client -> actor/core -> sidecar -> ACP adapter
                    ↘ VM SQLite event log
```

The sidecar—not the actor or SDK—owns defaults and orchestration. TypeScript and Rust send omitted fields as omissions and expose the same methods.

## Turn lifecycle

1. `openSession` creates the SQLite record and negotiates an adapter, or reuses the compatible existing record. It resolves without returning metadata.
2. `prompt` restores an unloaded adapter if needed.
3. AgentOS atomically marks the session running, records the prompt, and appends complete user-message ACP updates.
4. The prompt is dispatched exactly once. Live message/thought deltas are ephemeral; committed completed updates receive durable sequence numbers.
5. The prompt result and terminal idle/failed state commit atomically. Only then does the actor release `keepAwake`.

One prompt may run per session. Cancellation races are first-writer-wins. AgentOS does not automatically replay interrupted prompts because tool side effects may already have occurred.

The ACP runtime and AgentOS client transports impose no wall-clock deadline on prompts or human permission waits. After 30 seconds without ACP activity, AgentOS logs an inactivity warning with the total elapsed time and last observed activity, repeating every 30 seconds until activity resumes. Streaming output resets that interval. Bootstrap, teardown, filesystem, terminal, and other machine-to-machine operations retain bounded failure deadlines. Actor-hosted calls currently inherit RivetKit's maximum timer-safe action bound of about 24.8 days because RivetKit does not yet support an unbounded action.

## Reads versus adapter operations

`getSession`, `listSessions`, `readHistory`, and cached negotiation getters read SQLite without starting an adapter. `prompt` and configuration setters may restore one. `unloadSession` preserves SQLite while stopping the adapter; `deleteSession` removes both runtime and durable state.

## Native ACP data

Prompt content, stop reasons, session updates, configuration options, agent information, capabilities, and permission requests use upstream ACP shapes. AgentOS adds persistence envelopes and load/save semantics; it does not invent a parallel event vocabulary.

ACP has no portable durable history-read method. Native restore is also inconsistent across real adapters, so adapter storage cannot be AgentOS's public history source. SQLite remains authoritative even if an adapter emitted output that failed to commit.

## Adapter exits

An unexpected adapter exit evicts the live route and fails the active turn. AgentOS does not respawn the adapter or replay work implicitly. The next explicit prompt can restore the session through native resume/load or the bounded continuation fallback.

## Next

- [Sessions](/docs/sessions) for the public API.
- [Sessions & Persistence](/docs/architecture/sessions-persistence) for SQLite and restoration.
- [Approvals](/docs/approvals) for ACP permission options.