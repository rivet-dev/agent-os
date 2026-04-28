# WASM Runtime Test Checklist

Source files:
- `crates/execution/src/wasm.rs`

Suggested test homes:
- `crates/execution/tests/wasm.rs`
- `crates/execution/src/wasm.rs`

## Checklist

### Module loading and lifecycle

- [ ] Add tests that malformed binaries, oversized custom sections, unsupported WASI imports, and recursion-heavy modules fail early with stable error categories.
- [ ] Add tests that warmup caches compiled modules correctly and invalidates stale cache entries when module bytes change.
- [ ] Add tests that timeout and cancellation cleanly stop guest execution without leaving background tasks or open host-call state.

### WASI and permission tiers

- [ ] Add tests that each `WasmPermissionTier` exposes exactly the intended WASI capabilities and no more.
- [ ] Add tests that filesystem-denied, network-denied, and stdin-denied operations fail with predictable guest-visible errors.
- [ ] Add tests that environment variables and argv passed into WASM honor the same filtering rules as JS and Python.
- [ ] Add tests that the runtime limit env knobs (`WASM_MAX_MEMORY_BYTES_ENV`, `WASM_MAX_FUEL_ENV`, and `WASM_PREWARM_TIMEOUT_MS_ENV`) enforce exact-boundary and over-limit cases.

### Host-call and signal behavior

- [ ] Add tests that sync RPC calls from WASM correctly cover partial reads/writes, large buffers, and failure propagation.
- [ ] Add tests that signal-registration mapping handles duplicate registrations, unsupported signals, and handler cleanup on exit.
- [ ] Add tests that host-call failures surface as guest-visible traps or exit statuses consistent with other runtimes.
