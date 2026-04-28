# Execution Runtime Common Layer Test Checklist

Source files:
- `crates/execution/src/lib.rs`
- `crates/execution/src/common.rs`
- `crates/execution/src/runtime_support.rs`

Suggested test homes:
- `crates/execution/src/common.rs`
- `crates/execution/src/runtime_support.rs`
- `crates/execution/tests/smoke.rs`

## Checklist

### Shared helpers and invariants

- [ ] Add tests that `stable_hash64` returns the same value for repeated calls over empty input, ASCII input, and mixed-byte input.
- [ ] Add tests that `encode_json_string`, `encode_json_string_array`, and `encode_json_string_map` escape quotes, backslashes, control characters, and non-BMP code points exactly once.
- [ ] Add tests that `encode_json_string_map` preserves `BTreeMap` ordering and never reorders keys during serialization.
- [ ] Add tests that `resolve_execution_path` leaves absolute paths untouched and joins relative paths against the supplied cwd.
- [ ] Add tests that `sandbox_root` honors `AGENT_OS_SANDBOX_ROOT` when set and falls back to the cwd when unset.
- [ ] Add tests that `env_flag_enabled` only accepts the exact string `"1"` and rejects other truthy-looking values.
- [ ] Add tests that `lib.rs` keeps the runtime export surface aligned when feature flags enable or disable JS, Python, or WASM support.

### Cache and warmup scaffolding

- [ ] Add tests that `import_cache_root` returns the parent of the `NodeImportCache` path and uses the fallback path when the cache path has no parent.
- [ ] Add tests that `configure_compile_cache` creates the target directory, sets `NODE_COMPILE_CACHE`, and removes `NODE_DISABLE_COMPILE_CACHE`.
- [ ] Add tests that `compile_cache_ready` distinguishes empty, populated, and missing directories.
- [ ] Add tests that `warmup_marker_path` changes when the prefix, version, or content hash changes and stays stable for repeated calls with the same inputs.
- [ ] Add tests that `file_fingerprint` reports `missing` for absent files and changes when an existing file is rewritten in place.

### Regression coverage

- [ ] Add a cross-runtime smoke test that JS, Python, and WASM receive the same sandbox-root and cache-root shape when launched from the same cwd and environment.
- [ ] Add tests for empty, oversized, and Unicode-heavy identifiers used in cache-key derivation.
- [ ] Add tests that cache helpers never emit parent-directory escapes or duplicate separators in generated marker and cache paths.
