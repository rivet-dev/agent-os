# Loader, Materialization, And Builtin Interception Test Checklist

Source files:
- `crates/execution/src/node_import_cache.rs`
- `crates/execution/src/runtime_support.rs`
- `crates/execution/src/node_process.rs`
- `crates/execution/assets/runners/python-runner.mjs`

Suggested test homes:
- `crates/execution/src/node_import_cache.rs`
- `crates/execution/tests/module_resolution.rs`
- `crates/execution/tests/cjs_esm_interop.rs`
- `crates/execution/tests/python_prewarm.rs`

## Checklist

### Loader asset generation

- [ ] Add tests that `ensure_materialized` writes the loader template, register template, Python runner, and bundled asset registry into the import-cache root with the intended filenames and contents.
- [ ] Add tests that repeated materialization reuses an already-valid cache and does not rewrite loader assets unnecessarily.
- [ ] Add tests that `NODE_IMPORT_CACHE_SCHEMA_VERSION`, loader version, or asset version bumps invalidate stale cache state independently.
- [ ] Add tests that concurrent materialization into the same root yields one coherent cache-state file and no torn asset writes.
- [ ] Add tests that interrupted materialization leaves no partially written loader state behind on the next run.
- [ ] Add tests that builtin deny/allow rules generate different loader outputs in the expected cases.

### Builtin interception and module interop

- [ ] Add tests that `resolveBuiltinAsset`, `resolveDeniedBuiltin`, and `resolveAgentOsAsset` intercept both bare and `node:` specifiers before Node resolves host builtins.
- [ ] Add tests that CommonJS named-export extraction covers `exports.X`, `Object.defineProperty`, `Object.assign`, spread patterns, and runtime fallback extraction.
- [ ] Add tests that `require()` of ESM throws `ERR_REQUIRE_ESM` immediately rather than hanging or recursively retrying.
- [ ] Add tests that `import()` of CommonJS yields stable default and named export behavior across circular dependency graphs.
- [ ] Add tests for `A -> B -> A` and `A -> B -> C -> A` cycles through the generated loader path.

### Path scrubbing and staging

- [ ] Add tests that host-path to guest-path mapping refuses paths outside the mounted import roots.
- [ ] Add tests that guest-path scrubbing handles Unicode, spaces, Windows-like separators, and URL-style specifiers consistently.
- [ ] Add tests that `AGENT_OS_PYTHON_PRELOAD_PACKAGES` parsing rejects invalid JSON, non-array input, non-string entries, and duplicate package names.
- [ ] Add tests that `python-runner.mjs` emitted by materialization matches the checked-in template and is regenerated when the template or bundled Pyodide assets change.
- [ ] Add tests that the embedded WASM host runner asset is regenerated when its source changes and not silently reused from stale cache state.
