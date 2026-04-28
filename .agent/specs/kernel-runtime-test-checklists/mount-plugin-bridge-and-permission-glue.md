# Mount Plugin Bridge And Permission Glue Test Checklist

Source files:
- `crates/sidecar/src/bridge.rs`
- `crates/sidecar/src/plugins/mod.rs`

Suggested test homes:
- `crates/sidecar/tests/bridge.rs`
- `crates/sidecar/tests/host_dir.rs`
- `crates/sidecar/tests/service.rs`
- `crates/sidecar/tests/sandbox_agent.rs`

## Checklist

### Bridge-mounted filesystem wrapper

- [ ] Add tests that bridge-backed filesystem wrappers preserve file kind, inode, link-count, and timestamp metadata consistently across repeated stats.
- [ ] Add tests that host inode/link tracking remains stable when the host mutates a mounted tree underneath the wrapper.
- [ ] Add tests that synthetic metadata overlay cannot be used to forge unsupported file kinds or inconsistent stat results.
- [ ] Add tests that host-side rename, symlink, and delete operations still produce coherent stat and readdir results through the wrapper.

### Permission translation

- [ ] Add tests that sidecar policy objects translate into kernel `Permissions` with no silent widening of access.
- [ ] Add tests that read-only, read-write, and callback-driven mounts each receive the intended permission envelope.
- [ ] Add tests that permission decisions on mount paths remain correct across nested mount boundaries and symlink traversal.
- [ ] Add tests that permission translation rejects attempts to escalate a mount from read-only to writable via nested path access.

### Registry and helpers

- [ ] Add tests that plugin registry construction fails cleanly on duplicate plugin keys or invalid helper registrations.
- [ ] Add tests that memory-mount helpers preserve the same semantics as regular mounted filesystems for mutation and stat paths.
- [ ] Add tests that helper registration order stays deterministic so the same config always resolves to the same plugin implementation.
