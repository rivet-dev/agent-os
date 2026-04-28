# VFS And Filesystem Substrate Test Checklist

Source files:
- `crates/kernel/src/vfs.rs`
- `crates/kernel/src/root_fs.rs`
- `packages/core/fixtures/base-filesystem.json`
- `crates/kernel/src/device_layer.rs`
- `crates/kernel/src/overlay_fs.rs`
- `crates/kernel/src/mount_table.rs`
- `crates/kernel/src/mount_plugin.rs`

Suggested test homes:
- `crates/kernel/tests/vfs.rs`
- `crates/kernel/tests/root_fs.rs`
- `crates/kernel/tests/mount_table.rs`
- `crates/kernel/tests/mount_plugin.rs`
- `crates/kernel/tests/device_layer.rs`
- `crates/kernel/src/overlay_fs.rs`

## Checklist

### Path handling and core VFS semantics

- [ ] Add tests for path normalization edge cases including repeated separators, trailing `/.`, `..` walks above root, and empty path segments.
- [ ] Add tests that symlink traversal enforces loop limits and cross-directory relative-link resolution correctly.
- [ ] Add tests that rename semantics match POSIX expectations for file-to-file, dir-to-empty-dir, and invalid cross-kind replacements.
- [ ] Add tests that snapshot/export paths needing stable entry order sort or otherwise normalize `readdir` results explicitly instead of relying on incidental VFS enumeration order.
- [ ] Add tests that inode metadata updates correctly after chmod-like metadata changes, truncation, and link-count transitions.

### Root filesystem descriptors and snapshots

- [ ] Add a test that `base-filesystem.json` deserializes into the exact root tree expected by the runtime with no silently ignored fields.
- [ ] Add snapshot round-trip tests for files, directories, symlinks, metadata, and empty trees.
- [ ] Add tests for importing malformed snapshot descriptors and invalid entry graphs without panic or partial mutation.
- [ ] Add tests that root filesystem load preserves executable bits, timestamps, and symlink targets where supported.

### Overlay behavior

- [ ] Add tests for nested whiteouts and opaque directories across more than one lower layer, not just single-layer merges.
- [ ] Add tests that deleting and recreating a path in the upper layer does not accidentally revive shadowed lower-layer metadata.
- [ ] Add tests that directory rename across whiteouted or opaque boundaries preserves the expected visible tree.
- [ ] Add tests for overlay `stat`, `readdir`, and `readlink` consistency when the same path exists in both upper and lower layers.
- [ ] Add tests for out-of-band overlay metadata persistence if overlays are snapshotted or reloaded.

### Mount routing and plugins

- [ ] Add tests that mount-point longest-prefix matching wins when nested mounts overlap.
- [ ] Add tests that cross-mount operations fail or route correctly for rename, link, and metadata-changing calls.
- [ ] Add tests that read-only mount wrappers reject all mutating calls, including less obvious operations like `truncate`, `utime`, and `chmod`-like metadata writes.
- [ ] Add plugin-factory tests that bad plugin names, invalid descriptors, and constructor failures surface clear errors without leaving partial registrations.
