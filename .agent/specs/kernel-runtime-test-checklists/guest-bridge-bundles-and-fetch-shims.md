# Guest Bridge Bundles And Fetch Compatibility Shims Test Checklist

Source files:
- `crates/execution/assets/v8-bridge.source.js`
- `crates/execution/assets/v8-bridge.js`
- `crates/execution/assets/v8-bridge-zlib.js`
- `crates/execution/assets/polyfill-registry.json`
- `crates/execution/assets/undici-shims/*`

Suggested test homes:
- `crates/sidecar/tests/fetch_via_undici.rs`
- `crates/sidecar/tests/builtin_conformance.rs`
- `crates/sidecar/tests/builtin_completeness.rs`
- `crates/sidecar/tests/security_hardening.rs`

## Checklist

### Bundle integrity

- [ ] Add a golden test that `v8-bridge.source.js`, `v8-bridge.js`, and `polyfill-registry.json` stay in lockstep for exported globals and builtin names.
- [ ] Add a test that `polyfill-registry.json` references only bundle entries that actually exist and can be loaded.
- [ ] Add a test that `v8-bridge-zlib.js` exposes only the zlib surface and does not duplicate core bridge registrations.
- [ ] Add a test that bundle regeneration detects source drift instead of silently reusing stale checked-in artifacts.

### Fetch and undici behavior

- [ ] Add tests that guest `fetch()` routes through the kernel-backed `net.connect` path rather than any bypass helper.
- [ ] Add tests for redirect handling, streamed request bodies, response streaming, backpressure, abort signals, and trailer handling through the undici shims.
- [ ] Add tests that HTTP and HTTPS shims preserve header normalization and connection reuse behavior expected by real Node consumers.
- [ ] Add tests that TLS errors, DNS failures, and refused connections surface in guest-visible shapes matching real Node closely enough for SDK compatibility.

### Builtin conformance

- [ ] Add tests that `stream`, `web-stream`, `fetch`, `http`, `https`, `tls`, and zlib shims interoperate correctly with the bridge polyfills in mixed usage patterns.
- [ ] Add tests that unsupported builtin APIs throw explicit `ERR_NOT_IMPLEMENTED`-style failures instead of silent stubs or `undefined`.
- [ ] Add tests that the runtime-loadable builtin registry fails cleanly when a requested bundle entry is missing, duplicated, or malformed.
