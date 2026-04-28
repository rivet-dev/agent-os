# First-Party Mount Plugins Test Checklist

Source files:
- `crates/sidecar/src/plugins/mod.rs`
- `crates/sidecar/src/plugins/host_dir.rs`
- `crates/sidecar/src/plugins/module_access.rs`
- `crates/sidecar/src/plugins/js_bridge.rs`
- `crates/sidecar/src/plugins/sandbox_agent.rs`
- `crates/sidecar/src/plugins/s3.rs`
- `crates/sidecar/src/plugins/google_drive.rs`
- `registry/file-system/s3/src/index.ts`
- `registry/file-system/google-drive/src/index.ts`

Suggested test homes:
- `crates/sidecar/tests/host_dir.rs`
- `crates/sidecar/tests/sandbox_agent.rs`
- `crates/sidecar/tests/s3.rs`
- `crates/sidecar/tests/google_drive.rs`
- `registry/file-system/s3/tests/s3.test.ts`
- `registry/file-system/google-drive/tests/google-drive.test.ts`
- `crates/sidecar/tests/service.rs`

## Checklist

### Registry and descriptor helpers

- [ ] Add tests that the first-party registrations in `plugins/mod.rs` stay deterministic and that user-facing descriptor names resolve to the intended concrete plugin implementation.
- [ ] Add tests that the TypeScript descriptor helpers emit payloads accepted by the native plugin parser without shape drift.
- [ ] Add tests that invalid descriptor combinations fail early in both TypeScript helper validation and native sidecar plugin loading.
- [ ] Add tests that registry helper output remains stable enough to snapshot emitted descriptor JSON and native parser inputs across refactors.

### Host-backed plugins

- [ ] Add tests that `host_dir` mirrors host mutations, symlinks, permissions, and rename behavior correctly through the mounted view.
- [ ] Add tests that `module_access` enforces read-only behavior for every mutating filesystem verb, not just writes.
- [ ] Add tests that `module_access` exposes projected `node_modules` resolution correctly for nested package trees and scoped packages.
- [ ] Add tests that host-backed plugins preserve realpath and stat behavior when the host mutates the mounted tree underneath an open handle.

### Callback-driven and remote-backed plugins

- [ ] Add tests that `js_bridge` handles callback ordering, reentrant callback failures, and connection loss without corrupting the mount state.
- [ ] Add tests that `sandbox_agent` remote-process-backed filesystem operations clean up subprocesses and IPC resources on unmount or crash.
- [ ] Add tests that callback-driven plugins cannot deadlock the sidecar if the remote peer stalls mid-operation.
- [ ] Add tests that callback-driven plugins propagate the same error codes for missing callbacks and stalled peers across repeated operations.

### Persisted object-store plugins

- [ ] Add tests that S3 and Google Drive working-tree materialization handles manifest corruption, missing chunks, and partial upload/download failures.
- [ ] Add tests that concurrent edits inside the memory working tree and remote sync flows resolve deterministically.
- [ ] Add tests that object-store-backed rename, delete, and directory emulation behavior matches guest filesystem expectations.
- [ ] Add tests that credential failures, permission denials, and transient remote errors surface stable guest-visible errors without local tree corruption.
- [ ] Add tests that object-store plugins preserve links, metadata-only flushes, and truncate cleanup across reopen cycles.
