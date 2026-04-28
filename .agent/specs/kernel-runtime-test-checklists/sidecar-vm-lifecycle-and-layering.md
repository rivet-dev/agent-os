# Native Sidecar VM Lifecycle And Layering Test Checklist

Source files:
- `crates/sidecar/src/vm.rs`
- `crates/sidecar/src/bootstrap.rs`

Suggested test homes:
- `crates/sidecar/tests/vm_lifecycle.rs`
- `crates/sidecar/tests/layer_management.rs`
- `crates/sidecar/tests/service.rs`

## Checklist

### VM creation and teardown

- [ ] Add tests that VM creation from empty rootfs descriptors, imported snapshots, and layered rootfs descriptors succeeds only when the descriptor combination is valid.
- [ ] Add tests that VM creation failure at each stage rolls back partially created rootfs, shadow-root, mounts, and state entries.
- [ ] Add tests that VM disposal tears down running processes, listeners, sockets, callbacks, and temporary dirs without leaks or orphaned state, even when cleanup happens after partial startup failure.
- [ ] Add tests that repeated create-destroy-create cycles do not reuse stale filesystem or mount state.

### Rootfs and overlays

- [ ] Add tests that root filesystem descriptors and imported snapshots follow one documented precedence rule for overlapping paths, metadata, and mount state.
- [ ] Add tests that writable upper layers seal correctly when a snapshot is exported and later re-imported.
- [ ] Add tests that bootstrap directories and shadow-root scaffolding are recreated faithfully after VM restore.
- [ ] Add tests that snapshot import/export preserves device nodes, symlinks, metadata, and mount bookkeeping needed for later reconciliation.

### Mount reconciliation

- [ ] Add tests that module-access mounts and user-declared mounts follow one documented precedence rule when they target overlapping paths.
- [ ] Add tests that command-path refresh happens after mount changes and never advertises commands from removed mounts.
- [ ] Add tests that restored mount bookkeeping is replayed before reconciliation-sensitive guest operations such as path resolution and command discovery run.
- [ ] Add tests that mount reconciliation remains stable when a VM is restored with existing active mounts and new user-declared mounts are added afterward.
