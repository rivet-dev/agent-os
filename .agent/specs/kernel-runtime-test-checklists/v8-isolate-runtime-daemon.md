# V8 Isolate Runtime Daemon Test Checklist

Source files:
- `crates/v8-runtime/src/main.rs`
- `crates/v8-runtime/build.rs`
- `crates/v8-runtime/src/isolate.rs`
- `crates/v8-runtime/src/session.rs`
- `crates/v8-runtime/src/execution.rs`
- `crates/v8-runtime/src/bridge.rs`
- `crates/v8-runtime/src/host_call.rs`
- `crates/v8-runtime/src/ipc_binary.rs`
- `crates/v8-runtime/src/ipc.rs`
- `crates/v8-runtime/src/snapshot.rs`
- `crates/v8-runtime/src/stream.rs`
- `crates/v8-runtime/src/timeout.rs`

Suggested test homes:
- `crates/v8-runtime/src/main.rs`
- `crates/v8-runtime/src/session.rs`
- `crates/v8-runtime/src/execution.rs`
- `crates/v8-runtime/src/host_call.rs`
- `crates/v8-runtime/src/ipc_binary.rs`
- `crates/v8-runtime/src/ipc.rs`
- `crates/v8-runtime/src/snapshot.rs`
- `crates/v8-runtime/src/stream.rs`
- `crates/v8-runtime/src/timeout.rs`

## Checklist

### Daemon and session ownership

- [ ] Add tests that the listener rejects unauthenticated connections and tears them down without affecting existing sessions.
- [ ] Add tests that concurrency slot limits are enforced under rapid connect/disconnect churn.
- [ ] Add tests that session teardown during active execution releases isolate-owned resources and background tasks promptly.

### JS execution semantics

- [ ] Add tests that script compilation, module execution, dynamic import, top-level await, and CJS/ESM interop inside the daemon match the execution crate’s expectations.
- [ ] Add tests that global injection and builtin registration order are deterministic across fresh isolates and snapshot-restored isolates.
- [ ] Add tests that promise rejection tracking reports unhandled rejections once and suppresses duplicates after handled transitions.
- [ ] Add tests that guest-thrown errors preserve stack, cause, and module-origin information through daemon serialization.

### Bridge and host-call behavior

- [ ] Add tests that bridge value serialization covers typed arrays, ArrayBuffer slices, `BigInt` values, nested errors, external refs, and unsupported host objects.
- [ ] Add tests that `call_id` routing survives out-of-order host-call replies and cancellation races.
- [ ] Add tests that blocked sync host calls are interrupted correctly by timeout-triggered termination.

### Snapshot and protocol coverage

- [ ] Add tests that snapshot creation fails loudly when unsupported global state is introduced into the snapshot image.
- [ ] Add tests that snapshot cache invalidation tracks changes in bundled bridge assets and bootstrap code, not just Rust source changes.
- [ ] Add tests that both binary IPC and legacy MessagePack IPC reject malformed or cross-version frames predictably.
- [ ] Add tests that async stream events delivered back into V8 preserve order relative to microtask checkpoints and execution completion.
