# Shared WebAssembly support

This crate owns the engine-neutral agentOS WebAssembly ABI, pinned Preview1
source, generated import registry, validation profiles, request/result types,
permission tiers, and stable errors.

- It must not depend on Tokio, V8, Wasmtime, the kernel, or a sidecar.
- Changes to checked-in WITX or the ABI manifest must regenerate and verify
  `assets/agentos-wasm-abi.json` and `src/abi/generated.rs`.
- Engine-specific compilation, memory, lifecycle, and cache behavior does not
  belong here.
