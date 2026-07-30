# V8 WebAssembly executor

This crate is the maintained standalone-WASM compatibility executor hosted by
V8.

- Shared ABI definitions, validation profiles, permission tiers, and stable
  errors belong in `agentos-executor-wasm-abi`.
- Reusable isolate/session mechanics belong in `agentos-executor-v8-runtime`.
- Linux/POSIX behavior belongs in the kernel and sidecar host-capability
  implementation, not in an engine-specific fork.
- Behavior shared with Wasmtime must have cross-engine conformance coverage.
