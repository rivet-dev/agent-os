# Native Sidecar Shadow-Root Reconciliation Test Checklist

Source files:
- `crates/sidecar/src/filesystem.rs`
- `crates/sidecar/src/execution.rs`
- `crates/sidecar/src/service.rs`
- `crates/sidecar/src/vm.rs`

Suggested test homes:
- `crates/sidecar/tests/service.rs`
- `crates/sidecar/tests/fs_watch_and_streams.rs`
- `crates/sidecar/tests/process_isolation.rs`
- `crates/sidecar/tests/vm_lifecycle.rs`

## Checklist

### Write mirroring and sync-back

- [ ] Add tests that guest writes to regular files, directories, symlinks, and deletes are mirrored into the shadow tree correctly.
- [ ] Add tests that host reads force sync-back from active shadow paths before serving content and before metadata-only reads.
- [ ] Add tests that sync-back handles partial failures without leaving kernel and shadow state permanently divergent.

### Host reconciliation

- [ ] Add tests that host-created directories, files, and symlinks are projected back into the kernel tree with the expected metadata.
- [ ] Add tests that host-side deletions and renames are reflected correctly when a guest still has open handles.
- [ ] Add tests that reconciliation does not follow host symlinks in ways that escape the intended mounted subtree.
- [ ] Add tests that host-created changes do not overwrite newer kernel-side mutations when both sides touch the same path.

### Lifecycle edges

- [ ] Add tests that process-exit writeback captures final buffered writes even when the process terminates abruptly.
- [ ] Add tests that shadow-root bootstrap state does not overwrite newer kernel-side mutations during VM startup.
- [ ] Add tests that concurrent guest and host modifications resolve deterministically or produce explicit conflict behavior.
- [ ] Add tests that shadow-root cleanup removes stale state after VM teardown without disturbing unrelated mounted paths.
