# JavaScript Runtime Host Path Test Checklist

Source files:
- `crates/execution/src/javascript.rs`
- `crates/execution/src/node_process.rs`

Suggested test homes:
- `crates/execution/tests/javascript_v8.rs`
- `crates/execution/tests/module_resolution.rs`
- `crates/execution/tests/permission_flags.rs`
- `crates/execution/src/javascript.rs`
- `crates/execution/src/node_process.rs`

## Checklist

### Execution lifecycle

- [ ] Add tests that JavaScript session startup failures at spawn, bootstrap, and V8 session registration each leave no leaked child process, control socket, temp dir, or exported FD.
- [ ] Add tests that event-stream ordering is stable across stdout, stderr, structured events, timer callbacks, and exit notifications.
- [ ] Add tests that timeout, cancellation, and normal exit each produce distinct terminal states and preserve the final stdout/stderr tail.
- [ ] Add tests that two concurrent prewarm callers share the same cache root and only one materialization path runs.

### Node process hardening

- [ ] Add tests that `apply_guest_env` strips reserved runtime keys and dangerous host keys such as `NODE_OPTIONS`, `LD_PRELOAD`, `LD_LIBRARY_PATH`, and `DYLD_INSERT_LIBRARIES`.
- [ ] Add tests that `harden_node_command` emits the exact permission flags for filesystem, network, worker, WASI, and child-process policy combinations.
- [ ] Add tests that `configure_node_control_channel` exports only the reserved control FD and that `ExportedChildFds` closes temporary duplicates on drop.
- [ ] Add tests that control-channel disconnects and malformed `NodeControlMessage` values fail closed rather than leaving a half-running session.

### Path and module behavior

- [ ] Add tests that `resolve_path_like_specifier` handles `file://`, `file:`, absolute, and relative specifiers and rejects bare package names.
- [ ] Add tests that `node_resolution_read_paths` walks parent directories and returns every visible `package.json` and `node_modules` directory exactly once per root.
- [ ] Add tests that `env_builtin_enabled` accepts a JSON array allowlist and rejects malformed JSON or non-array values.
- [ ] Add tests that guest-path to host-path translation stays bijective for mounted working files and rejects non-guest paths.
- [ ] Add tests that JS child-process RPC handling does not allow recursive or nested process launches to escape runtime limits.
