# Browser-Side Sidecar Variant Test Checklist

Source files:
- `crates/sidecar-browser/src/lib.rs`
- `crates/sidecar-browser/src/service.rs`

Suggested test homes:
- `crates/sidecar-browser/tests/bridge.rs`
- `crates/sidecar-browser/tests/service.rs`
- `crates/sidecar-browser/tests/smoke.rs`

## Checklist

### Browser worker lifecycle

- [ ] Add tests that worker creation, termination, and crash handling keep VM/context state coherent on the main thread.
- [ ] Add tests that repeated worker-backed executions do not leak worker handles or message listeners.
- [ ] Add tests that worker startup failures surface protocol-safe errors rather than browser-specific exceptions.
- [ ] Add tests that worker teardown after a crash still clears any pending VM/session bookkeeping on the main thread.

### Service behavior

- [ ] Add tests that the browser-side service enforces the same request ordering and ownership rules as the native sidecar where the API overlaps.
- [ ] Add tests that browser bridge traits reject unsupported native-only features explicitly instead of silently no-oping.
- [ ] Add tests that message ordering between main thread and worker remains stable under rapid stdout/event bursts.
- [ ] Add tests that browser-side service failures are serialized with the same protocol shape as native-side failures.

### Compatibility

- [ ] Add tests that browser-hosted execution reports feature gaps clearly for filesystem, networking, or process features not supported in that environment.
- [ ] Add tests that browser and native sidecars serialize shared protocol payloads identically for overlapping message types.
- [ ] Add tests that the browser-side bridge compiles against the composed host bridge and preserves the same method surface as the native sidecar where features overlap.
