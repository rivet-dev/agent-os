# Executor conformance tests

This is a test-only crate. It is not a production execution layer and must
remain `publish = false`.

- Put cross-executor lifecycle, host-capability, safety, and behavioral-parity
  tests here.
- WebAssembly behavior that both engines support must run against V8-WASM and
  Wasmtime. Engine-specific tests belong in the concrete executor crate.
- Do not add production logic, sidecar dispatch, kernel semantics, or executor
  selection here.
- Benchmarks may share test helpers from this crate, but production crates must
  not depend on it.
- Keep runtime-specific implementation tests next to
  `agentos-executor-node-v8`, `agentos-executor-python-v8-pyodide`,
  `agentos-executor-wasm-v8`, or `agentos-executor-wasm-wasmtime`.

See `website/src/content/docs/docs/architecture/package-structure.mdx` for the
production dependency graph.
