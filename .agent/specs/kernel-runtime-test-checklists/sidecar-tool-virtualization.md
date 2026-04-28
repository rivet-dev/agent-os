# Native Sidecar Tool Virtualization Test Checklist

Source files:
- `crates/sidecar/src/tools.rs`
- `crates/sidecar/src/execution.rs`
- `crates/sidecar/src/protocol.rs`

Suggested test homes:
- `crates/sidecar/tests/service.rs`
- `crates/sidecar/src/tools.rs`
- `crates/sidecar/tests/protocol.rs`

## Checklist

### Toolkit registration and discovery

- [ ] Add tests that toolkit registration order is deterministic and name collisions fail explicitly.
- [ ] Add tests that invalid toolkit schemas or incomplete command metadata are rejected before registration.
- [ ] Add tests that tool discovery output is stable enough for downstream prompt/reference generation snapshots.
- [ ] Add tests that toolkit registration produces the same command ordering after repeated VM setup and teardown cycles.

### CLI synthesis

- [ ] Add tests that JSON Schema to CLI flag parsing covers booleans, enums, arrays, defaults, nested objects, and repeated flags.
- [ ] Add tests that unknown flags, missing required args, and type mismatches surface user-facing errors without launching the tool runtime.
- [ ] Add tests that generated markdown help/reference output is stable and correctly escaped for unusual schema text.
- [ ] Add tests that schema defaults and repeated flags preserve the same values that a real toolkit invocation would see in `argv`.

### Virtual process execution

- [ ] Add tests that `agentos`, toolkit commands, and direct tool invocations resolve to sidecar-virtual processes rather than host binaries.
- [ ] Add tests that tool process stdout/stderr/exit handling matches the generic process model.
- [ ] Add tests that permission and ownership policy is enforced identically for tool-backed virtual processes and language runtime processes.
- [ ] Add tests that stalled or crashed remote tool peers release their process registrations and IPC resources cleanly.
