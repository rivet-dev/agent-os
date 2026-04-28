# ACP Agent Session Layer Test Checklist

Source files:
- `crates/sidecar/src/acp/client.rs`
- `crates/sidecar/src/acp/compat.rs`
- `crates/sidecar/src/acp/json_rpc.rs`
- `crates/sidecar/src/acp/session.rs`
- `crates/sidecar/src/service.rs`

Suggested test homes:
- `crates/sidecar/tests/acp/client.rs`
- `crates/sidecar/tests/acp/json_rpc.rs`
- `crates/sidecar/tests/acp_session.rs`
- `crates/sidecar/tests/acp_integration.rs`
- `crates/sidecar/tests/service.rs`

## Checklist

### JSON-RPC framing and transport

- [ ] Add tests that ACP JSON-RPC parsing rejects mixed request/response fields, wrong `jsonrpc` version strings, and invalid IDs.
- [ ] Add tests that line-delimited transport handles split frames, concatenated frames, and non-UTF8 bytes safely.
- [ ] Add tests that request dedupe and timeout behavior remain correct when responses arrive after local cancellation.
- [ ] Add tests that request/response correlation rejects duplicate IDs and stale responses without mutating session state.

### Session state and compatibility

- [ ] Add tests that event sequence numbers remain monotonic across reconnect-like transitions, terminal output, permission prompts, and close events.
- [ ] Add tests that terminal capture truncation logic handles multibyte UTF-8 boundaries without producing invalid strings.
- [ ] Add tests that compatibility shims for permission requests, cancel semantics, and agent quirks are applied only to the intended agent families.
- [ ] Add tests that config/mode derivation merges initialize-time and session-time options in the intended precedence order.
- [ ] Add tests that session-close cleanup resolves or cancels pending permission prompts and removes terminal capture state.

### Service-layer ACP orchestration

- [ ] Add tests that handshake, stdout prebuffering, terminal proxying, and close/kill wiring remain correct when the agent process exits during initialization.
- [ ] Add tests that inbound duplicate request IDs are ignored or rejected without corrupting session state.
- [ ] Add tests that pending permission requests are resolved, cancelled, or cleaned up correctly on session close and process crash.
- [ ] Add tests that agent-specific startup failures are surfaced as protocol-safe errors rather than uncaught transport exceptions.
