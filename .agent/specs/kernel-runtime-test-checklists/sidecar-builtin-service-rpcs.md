# Native Sidecar Builtin Service RPCs Test Checklist

Source files:
- `crates/sidecar/src/execution.rs`

Suggested test homes:
- `crates/sidecar/tests/builtin_conformance.rs`
- `crates/sidecar/tests/builtin_completeness.rs`
- `crates/sidecar/tests/service.rs`
- `crates/sidecar/tests/python.rs`

## Checklist

### Crypto and SQLite service surfaces

- [ ] Add tests that guest crypto helper RPCs cover success, permission denial, malformed input, and deterministic error shaping.
- [ ] Add tests that large crypto payloads and streaming-style crypto operations respect RPC size and timeout limits.
- [ ] Add tests that SQLite bridge state is isolated per VM and cleaned up after process exit, crash, or VM disposal.
- [ ] Add tests that invalid SQLite handles, double-close, and use-after-close attempts fail cleanly.
- [ ] Add tests that crypto and SQLite RPC failures preserve guest-visible error codes and do not mutate sibling runtime state.

### Stdin and PTY/raw-mode services

- [ ] Add tests that kernel stdin service RPCs handle no-reader, late-writer, and EOF conditions correctly.
- [ ] Add tests that PTY raw-mode transitions via service RPC remain coherent with the kernel PTY model seen by guest code.
- [ ] Add tests that service RPCs affecting PTY or stdin state are rejected for processes that do not own those handles.
- [ ] Add tests that PTY/stdin RPC state is cleared after kill, close, and process exit paths.

### Cross-runtime behavior

- [ ] Add tests that JS, Python, and WASM guests observe the same semantics for shared builtin service RPCs where the API is intentionally common.
- [ ] Add tests that malformed builtin RPC requests from guest runtimes cannot crash the sidecar dispatch path.
- [ ] Add tests that shared builtin RPCs continue to work after an unrelated runtime crash or VM teardown.
