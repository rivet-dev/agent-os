# Kernel VM And Syscall Surface Test Checklist

Source files:
- `crates/kernel/src/kernel.rs`

Suggested test homes:
- `crates/kernel/tests/api_surface.rs`
- `crates/kernel/tests/kernel_integration.rs`
- `crates/kernel/tests/virtual_process.rs`
- `crates/kernel/src/kernel.rs`

## Checklist

### VM lifecycle and process entry

- [ ] Add a test that `KernelVm` initialization wires the expected default mounts, device layer, `/proc`, `/dev`, and initial user-visible paths.
- [ ] Add a test that `spawn`, `exec`, and `open_shell` produce distinct process metadata and correctly seed `argv`, `cwd`, `env`, and stdio.
- [ ] Add a test that failed process startup leaves no leaked PID, FD, pipe, or wait-queue state behind.
- [ ] Add a test that concurrent process creation preserves PID uniqueness and parent-child accounting.

### Syscall plumbing

- [ ] Add end-to-end tests that exercise the full `read`/`write`/`open`/`stat`/`close`/`dup`/`waitpid`/`poll` path through `KernelVm`, not just the underlying managers.
- [ ] Add a test that syscall wrappers return correct errno-style failures for bad FDs, missing paths, permission denials, and unsupported operations.
- [ ] Add a test that syscall dispatch never bypasses the permission wrapper for filesystem, command, and network-sensitive operations.
- [ ] Add a test that kernel-level `poll`/`waitpid` interplay behaves correctly when a child exits while FDs are also becoming ready.

### Command resolution and shebangs

- [ ] Add a test that command lookup prefers absolute and relative direct paths correctly before registry-backed command stubs.
- [ ] Add a test for shebang parsing with spaces, quoted interpreter args, missing interpreter paths, and CRLF endings.
- [ ] Add a test that executing a non-executable regular file through the shebang path fails predictably rather than silently falling through.

### Mounts and pseudo-filesystems

- [ ] Add a test that mount attachment and removal update the visible VM namespace without corrupting open handles.
- [ ] Add a test that `/proc/self` and `/proc/[pid]` views change correctly across spawn, exit, and reap boundaries.
- [ ] Add a test that `/dev/std*` and `/dev/fd/*` resolve through the top-level syscall surface exactly like direct FD operations, including `/dev/fd/<n>` reopening semantics.
