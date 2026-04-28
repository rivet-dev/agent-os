# Native Sidecar TLS, HTTP, And HTTP/2 Planes Test Checklist

Source files:
- `crates/sidecar/src/execution.rs`
- `crates/sidecar/src/state.rs`

Suggested test homes:
- `crates/sidecar/tests/fetch_via_undici.rs`
- `crates/sidecar/tests/service.rs`
- `crates/sidecar/tests/socket_state_queries.rs`

## Checklist

### TLS

- [ ] Add tests that TLS client upgrades handle valid handshakes, invalid cert chains, hostname mismatch, ALPN negotiation, and abrupt handshake aborts.
- [ ] Add tests that TLS server state correctly tracks per-connection cert material and client-hello inspection results.
- [ ] Add tests that cert/key material loading failures do not leave half-upgraded sockets in state.
- [ ] Add tests that TLS verification failures preserve the socket's pre-upgrade state and do not leak upgraded-only metadata.

### HTTP/1

- [ ] Add tests that outbound HTTP bridging preserves method, header, body-stream, redirect, and keepalive behavior expected by guest clients.
- [ ] Add tests that loopback HTTP serving and outbound HTTP use distinct policy paths where required.
- [ ] Add tests for chunked transfer, trailers, early response close, oversized header failure behavior, and request cancellation.
- [ ] Add tests that HTTP request/response metadata is preserved when the same path is exercised through `fetch_via_undici.rs` and direct service-layer HTTP helpers.

### HTTP/2

- [ ] Add tests that server/session/stream state handles concurrent streams, reset frames, flow-control windows, and graceful shutdown.
- [ ] Add tests that HTTP/2 over TLS handoff preserves ALPN and does not regress plain TLS socket bookkeeping.
- [ ] Add tests that stream event queues remain ordered per stream and per connection under bursty traffic.
- [ ] Add tests that HTTP/2 session teardown clears pending response slots and stream bookkeeping even when the peer disconnects mid-flight.
