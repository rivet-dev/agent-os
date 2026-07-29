# agentOS Packages

- Client packages must stay same-version with the sidecar: assert the single protocol version integer, and do not add wire back-compat, runtime negotiation, or converters.
- Generated client layers return raw generated protocol types; the `AgentOs` facade in `@rivet-dev/agentos-core` is the only sanctioned ergonomic wrapper.
- Generic secure-exec clients must stay agent-agnostic and must not branch on the Agent OS ACP namespace.
- secure-exec packages must never depend on agent-os packages; dependency direction is strictly agent-os to secure-exec and must be CI-enforced after the split.
- The sidecar remains the source of truth for runtime behavior; TypeScript package code should forward generated requests instead of reimplementing sidecar state machines.
- Cron and agent configuration types are Rust-owned after the split; TypeScript packages may re-export or mirror them only in lockstep.

## React UI (dashboard inspector)

- Never `setState` from a `useEffect` to derive state from props, query data, or
  other state. Derive it during render (`useMemo`), or reset it during render by
  comparing against the value it is keyed on. Effects are for subscriptions,
  timers, and imperative cleanup only.
- Server mutations use `useMutation`; do not hand-roll `busy`/`error` state
  around an `async` handler.
- Coupled state that always changes together belongs in one `useReducer` (or one
  state object), not in a pile of independent `useState` calls updated in
  sequence.
- Imperative browser resources (object URLs, observers, listeners) belong in a
  self-contained hook that owns both creation and cleanup.
