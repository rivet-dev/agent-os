# agentOS Packages

- Client packages must stay same-version with the sidecar: assert the single protocol version integer, and do not add wire back-compat, runtime negotiation, or converters.
- Generated client layers return raw generated protocol types; the `AgentOs`
  facade is implemented in `@rivet-dev/agentos-core` and publicly exported from
  `@rivet-dev/agentos`. User-facing docs and examples must import the public
  package, not the internal core package.
- Generic agentos clients must stay agent-agnostic and must not branch on the Agent OS ACP namespace.
- agentos packages must never depend on agent-os packages; dependency direction is strictly agent-os to agentos and must be CI-enforced after the split.
- The sidecar remains the source of truth for runtime behavior; TypeScript package code should forward generated requests instead of reimplementing sidecar state machines.
- Language modules own their ecosystem's common end-to-end workflows: source
  and file execution, value evaluation, dependency installation, project entry
  points, and standard module/script workflows must not require users to invoke
  `node`, `python`, `python -m`, `npm`, or `pip`. Add typed, injection-safe
  helpers for stable intents, not one method per CLI flag; keep `exec`,
  `execArgv`, and `spawn` as the uncommon-command escape hatch.
- Cron and agent configuration types are Rust-owned after the split; TypeScript packages may re-export or mirror them only in lockstep.
