# Wasmtime WebAssembly executor

This crate owns Wasmtime engine configuration, Stores, linker adaptation,
checked guest-memory access, compilation caching, interruption, and optional
thread-group mechanics.

- Do not use `wasmtime-wasi` or ambient host capabilities. Link the agentOS
  Preview1 plus POSIX ABI to bounded executor-contract capabilities.
- Never retain guest-memory references across an async wait; copy and validate
  input first, then reacquire and revalidate before writing output.
- Shared ABI/profile/error types belong in `agentos-executor-wasm-abi`.
- Linux/POSIX semantics belong in the kernel, not in Wasmtime-specific code.
- Serialized/AOT artifacts remain out of scope unless separately designed.
