# Execution-Side V8 Client Transport And IPC Test Checklist

Source files:
- `crates/execution/src/v8_host.rs`
- `crates/execution/src/v8_ipc.rs`
- `crates/execution/src/v8_runtime.rs`

Suggested test homes:
- `crates/execution/src/v8_host.rs`
- `crates/execution/src/v8_ipc.rs`
- `crates/execution/src/v8_runtime.rs`
- `crates/execution/tests/javascript_v8.rs`

## Checklist

### Daemon spawn and authentication

- [ ] Add tests that spawning `agent-os-v8` fails cleanly when the binary is missing, stale, or exits before handshake.
- [ ] Add tests that authentication rejects wrong tokens, truncated handshakes, and out-of-order startup frames.
- [ ] Add tests that reconnect logic after daemon crash either rebuilds session state safely or fails with a hard, explicit error.

### IPC framing

- [ ] Add tests that all `BinaryFrame` variants round-trip through the execution-side binary IPC schema mirror.
- [ ] Add tests for truncated frames, oversized payloads, unknown tags, and invalid length prefixes.
- [ ] Add tests that multiplexed sessions cannot consume one another’s responses even when `call_id` or session close timing races occur.

### Session routing

- [ ] Add tests that concurrent session registration, execution, stream events, and teardown preserve ordering per session.
- [ ] Add tests that late events after session teardown are discarded without panicking or corrupting the client router.
- [ ] Add tests that daemon stdout/stderr noise or stray bytes do not desynchronize the IPC stream if that path is reachable.
