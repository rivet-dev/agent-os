# Pseudo-Filesystems Dev And Proc Test Checklist

Source files:
- `crates/kernel/src/device_layer.rs`
- `crates/kernel/src/kernel.rs`

Suggested test homes:
- `crates/kernel/tests/device_layer.rs`
- `crates/kernel/tests/kernel_integration.rs`
- `crates/kernel/tests/api_surface.rs`

## Checklist

### `/dev` semantics

- [ ] Add tests that `/dev/null`, `/dev/zero`, and `/dev/urandom` match expected read, write, truncate, and `pread` semantics across repeated reads and large buffers.
- [ ] Add tests that `/dev/stdin`, `/dev/stdout`, and `/dev/stderr` track the caller process FD table rather than a global singleton.
- [ ] Add tests that `/dev/fd/N` resolution reflects dup/close races and fails once the underlying descriptor is gone.
- [ ] Add tests that `/dev/pts/*` nodes appear and disappear with PTY allocation and teardown, and that their stat metadata matches a character device.

### `/proc` visibility

- [ ] Add tests that `/proc/self` resolves per calling process and not per VM.
- [ ] Add tests that `/proc/[pid]` entries disappear after reaping and do not leak zombie-internal state once fully collected.
- [ ] Add tests for `/proc/[pid]/cmdline`, `/proc/[pid]/environ`, `/proc/[pid]/stat`, and `/proc/[pid]/fd` content stability across running, stopped, and exited process states.
- [ ] Add tests that forbidden or unsupported proc paths fail consistently instead of synthesizing partial data.

### Integration and security edges

- [ ] Add tests that pseudo-filesystem nodes respect top-level permissions where applicable and bypass only the intentional kernel-owned paths.
- [ ] Add tests that pseudo-filesystem paths cannot be shadowed by overlay writes or mounted filesystems.
- [ ] Add tests that path traversal through symlinks into `/dev` or `/proc` does not escape the intended pseudo-filesystem behavior.
