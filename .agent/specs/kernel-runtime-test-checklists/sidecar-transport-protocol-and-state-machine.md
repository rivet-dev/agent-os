# Native Sidecar Transport, Protocol, And State Machine Test Checklist

Source files:
- `crates/sidecar/src/lib.rs`
- `crates/sidecar/src/protocol.rs`
- `crates/sidecar/src/state.rs`
- `crates/sidecar/src/stdio.rs`
- `crates/sidecar/src/main.rs`

Suggested test homes:
- `crates/sidecar/tests/protocol.rs`
- `crates/sidecar/tests/stdio_binary.rs`
- `crates/sidecar/tests/bidirectional_frames.rs`
- `crates/sidecar/tests/connection_auth.rs`
- `crates/sidecar/tests/service.rs`

## Checklist

### Wire protocol

- [ ] Add golden tests that `ProtocolSchema::current()`, `PROTOCOL_VERSION`, `PROTOCOL_NAME`, `DEFAULT_MAX_FRAME_BYTES`, and `SidecarScaffold` stay aligned with the serialized protocol surface.
- [ ] Add round-trip tests for `RequestFrame`, `ResponseFrame`, `SidecarRequestFrame`, `SidecarResponseFrame`, and `EventFrame`, including ownership scopes and request IDs.
- [ ] Add tests that ownership-scope payloads reject invalid combinations of VM, context, session, and process identifiers.
- [ ] Add tests that protocol decoding rejects truncated callback frames, oversized payloads, unknown discriminants, and malformed `frame_type` tags cleanly.
- [ ] Add tests that root-filesystem, permission-policy, and tool payloads preserve optional fields and defaults exactly.

### Transport and framing

- [ ] Add tests that framed stdio transport survives partial reads, short writes, split frames, concatenated frames, and back-to-back large frames without corruption.
- [ ] Add tests that auth failure, duplicate hello, and premature EOF each produce deterministic connection teardown paths.
- [ ] Add tests that callbacks cannot be double-resolved, left orphaned after a connection drop, or resolved after the connection has been torn down.

### State machine integrity

- [ ] Add tests that long-lived state maps for VMs, contexts, processes, listeners, sockets, callbacks, and tool executions stay internally consistent after failures and request retries.
- [ ] Add tests that duplicate request IDs, mismatched response ownership, and late replies after timeout are rejected without mutating state.
- [ ] Add tests that sidecar restart or crash during active callbacks does not leave resumable state in an impossible half-owned condition.
- [ ] Add tests that state cleanup occurs when top-level owners are removed out of order and that dependent resources are removed before their parents.
