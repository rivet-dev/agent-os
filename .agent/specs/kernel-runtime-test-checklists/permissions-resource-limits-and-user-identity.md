# Permissions, Resource Limits, And User Identity Test Checklist

Source files:
- `crates/kernel/src/permissions.rs`
- `crates/kernel/src/resource_accounting.rs`
- `crates/kernel/src/user.rs`

Suggested test homes:
- `crates/kernel/tests/permissions.rs`
- `crates/kernel/tests/resource_accounting.rs`
- `crates/kernel/tests/user.rs`
- `crates/kernel/tests/api_surface.rs`
- `crates/kernel/tests/kernel_integration.rs`

## Checklist

### Permission decisions

- [ ] Add tests that filesystem permissions distinguish read, write, create, delete, rename, and metadata mutations on the same path.
- [ ] Add tests that path-based permission checks behave correctly across symlinks, mount boundaries, and normalized-vs-non-normalized inputs.
- [ ] Add tests that command permissions cover direct paths, shebang interpreters, and registry-backed commands separately.
- [ ] Add tests that environment-variable filtering handles allowlist, denylist, and inherited-default cases without host leakage.
- [ ] Add tests that network permissions distinguish DNS, outbound connect, listen, loopback, and exempt-port behavior.

### Resource accounting

- [ ] Add tests for simultaneous exhaustion of multiple limits so the first failing resource is reported deterministically.
- [ ] Add tests that failed allocations roll back counters for FDs, pipes, PTYs, sockets, and filesystem usage.
- [ ] Add tests that process exit and mount teardown release all tracked resource counters.
- [ ] Add tests for size-limit enforcement on large reads, large writes, and large directory listings at exact boundary values.
- [ ] Add tests that WASM-specific limits are enforced independently from global VM limits when both are configured.

### User model

- [ ] Add tests that passwd rendering stays stable for the default VM user and any supported override cases.
- [ ] Add tests that home directory, shell path, UID, and GID changes propagate to process spawn defaults and procfs identity views.
- [ ] Add tests that user identity cannot be used to bypass permissions that are meant to be policy-driven rather than Unix-mode-driven.
