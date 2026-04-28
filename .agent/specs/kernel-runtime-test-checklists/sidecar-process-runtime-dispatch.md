# Native Sidecar Process Runtime Dispatch Test Checklist

Source files:
- `crates/sidecar/src/execution.rs`

Suggested test homes:
- `crates/sidecar/tests/service.rs`
- `crates/sidecar/tests/process_isolation.rs`
- `crates/sidecar/tests/kill_cleanup.rs`
- `crates/sidecar/tests/crash_isolation.rs`
- `crates/sidecar/tests/vm_lifecycle.rs`

## Checklist

### Runtime selection and startup

- [ ] Add tests that JavaScript, Python, WASM, and tool-backed process dispatch choose the intended launcher from the same command surface.
- [ ] Add tests that invalid runtime descriptors, missing entrypoints, and unsupported command types fail before any partial process registration leaks into state.
- [ ] Add tests that runtime env assembly merges defaults, user env, and filtered host env consistently across all runtime flavors.
- [ ] Add tests that launch-time failures after process registration but before guest start fully roll back the process entry and temp-root state.

### Process management

- [ ] Add tests that nested JS child-process RPC launches are accounted for as descendants of the initiating process.
- [ ] Add tests that process kill, crash, and normal exit each release runtime-specific state, shadow paths, and event subscriptions.
- [ ] Add tests that guest/host path mapping used during launch cannot reference paths outside the mounted VM tree.
- [ ] Add tests that process IDs, session IDs, and runtime-owned temp dirs are never reused in a way that could route late events to the wrong process.

### Isolation and cleanup

- [ ] Add tests that a crash in one runtime flavor does not poison process dispatch for the next launch of a different runtime flavor.
- [ ] Add tests that a guest process crash does not keep its shadow-root or runtime subscription entries alive after cleanup.
- [ ] Add tests that runtime-specific cleanup removes only the resources owned by the exiting process and leaves sibling launches intact.
