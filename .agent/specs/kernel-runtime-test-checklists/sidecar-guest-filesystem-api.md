# Native Sidecar Guest Filesystem API Test Checklist

Source files:
- `crates/sidecar/src/filesystem.rs`

Suggested test homes:
- `crates/sidecar/tests/service.rs`
- `crates/sidecar/tests/posix_compliance.rs`
- `crates/sidecar/tests/posix_path_repro.rs`
- `crates/sidecar/tests/fs_watch_and_streams.rs`

## Checklist

### Operation coverage

- [ ] Add direct API tests for every guest filesystem verb, including `read_file`, `write_file`, `create_file`, `create_dir`, `mkdir`, `remove_file`, `remove_dir`, `rename`, `symlink`, `link`, `chmod`, `chown`, `truncate`, `stat`, `lstat`, `readlink`, `readdir`, and `realpath`.
- [ ] Add tests that byte, text, and empty content encodings map correctly between protocol payloads and kernel-side data.
- [ ] Add tests that large reads, large writes, and batched readdir calls respect protocol and resource limits precisely at boundary values.

### Error and path behavior

- [ ] Add tests that guest filesystem API errors preserve path context and stable error codes for missing paths, wrong kinds, and permission failures.
- [ ] Add tests that normalized and non-normalized path spellings, including redundant separators and `.`/`..`, are treated consistently by the direct filesystem API.
- [ ] Add tests that writes through the API update metadata visible to subsequent stat/readdir/read calls immediately.
- [ ] Add tests that `lstat` and `readlink` preserve symlink semantics rather than dereferencing through mounted or overlay-backed paths.

### Integration

- [ ] Add tests that direct filesystem API calls stay coherent with process-visible filesystem state when a guest process is mutating the same paths concurrently.
- [ ] Add tests that mounted filesystems and overlay-backed paths both work through the same API surface without surprising kind-specific gaps.
- [ ] Add tests that the guest filesystem API reports the same behavior for host-mounted paths and kernel-root paths when metadata-only operations are repeated.
