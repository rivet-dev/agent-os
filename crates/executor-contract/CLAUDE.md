# Executor contract

This crate owns engine-neutral lifecycle, host-capability, reply, wake, signal,
identity, bounded-value, and typed-error contracts.

- It must not depend on the kernel, Tokio, V8, Wasmtime, or a sidecar.
- Keep requests owned and bounded; guest-memory borrows and engine handles
  cannot cross this boundary.
- Contract invariants such as generation binding and exactly-once completion
  belong here. Linux semantics and policy do not.
