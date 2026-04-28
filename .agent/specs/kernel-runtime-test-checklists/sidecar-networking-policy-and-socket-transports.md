# Native Sidecar Networking Policy And Socket Transports Test Checklist

Source files:
- `crates/sidecar/src/execution.rs`
- `crates/sidecar/src/state.rs`

Suggested test homes:
- `crates/sidecar/tests/socket_state_queries.rs`
- `crates/sidecar/tests/security_hardening.rs`
- `crates/sidecar/tests/service.rs`

## Checklist

### Policy enforcement

- [ ] Add tests that DNS, outbound connect, bind, listen, loopback, and exempt-port rules are all enforced independently.
- [ ] Add tests that guest-port to host-port translation is deterministic and collision-safe across concurrent listeners.
- [ ] Add tests that denied network actions do not allocate socket IDs or leak partially initialized listener state.
- [ ] Add tests that per-VM DNS overrides and loopback exemptions cannot widen access to private or non-loopback targets.

### Transport state machines

- [ ] Add tests for TCP connect, listen, accept, half-close, reset, and close sequences as visible through guest APIs.
- [ ] Add tests for UDP bind, send, receive, connected UDP, and multi-peer datagram handling.
- [ ] Add tests for Unix socket bind/connect/listen flows, including stale path cleanup and namespace collisions if supported.
- [ ] Add tests that socket snapshots and listener discovery report a consistent view during rapid open/close churn.
- [ ] Add tests that guest-visible socket state remains coherent when the same logical listener is queried through both the service layer and the socket state query helpers.

### Resource accounting and failure behavior

- [ ] Add tests that socket resource counters are released after failed handshake, failed bind, and abrupt peer disconnect cases.
- [ ] Add tests that DNS resolver selection falls back correctly when the preferred resolver path fails.
- [ ] Add tests that loopback exemptions cannot be abused to reach non-loopback targets through translated addresses.
- [ ] Add tests that socket snapshots stop reporting closed listeners and stale UDP endpoints after teardown.
