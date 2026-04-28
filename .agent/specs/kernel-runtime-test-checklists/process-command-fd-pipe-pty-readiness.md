# Process, Command, FD, Pipe, PTY, And Readiness Model Test Checklist

Source files:
- `crates/kernel/src/process_table.rs`
- `crates/kernel/src/fd_table.rs`
- `crates/kernel/src/pipe_manager.rs`
- `crates/kernel/src/pty.rs`
- `crates/kernel/src/poll.rs`
- `crates/kernel/src/command_registry.rs`
- `crates/kernel/src/kernel.rs`

Suggested test homes:
- `crates/kernel/tests/process_table.rs`
- `crates/kernel/tests/fd_table.rs`
- `crates/kernel/tests/pipe_manager.rs`
- `crates/kernel/tests/pty.rs`
- `crates/kernel/tests/poll.rs`
- `crates/kernel/tests/command_registry.rs`
- `crates/kernel/tests/kernel_integration.rs`

## Checklist

### Process table and wait semantics

- [ ] Add tests for process-group and session leadership transitions when the leader exits before children.
- [ ] Add tests that zombie reaping releases all process-table resources only after the final waiter collects status.
- [ ] Add tests for wait semantics with `WNOHANG`-style behavior, orphaned children, and multiple concurrent waiters.
- [ ] Add tests for signal-state bookkeeping when stop/continue/terminate-style transitions happen in quick succession.

### FD table and locks

- [ ] Add tests that `dup`/`dup2` preserve open-file-description sharing for offsets, flags, and lock state.
- [ ] Add tests that close-on-exec style flags are applied correctly during `exec` or equivalent process replacement paths.
- [ ] Add tests for advisory `flock` interactions across duplicated FDs, separate opens of the same inode, and process exit cleanup.
- [ ] Add tests that descriptor allocation reuses freed numbers safely without leaking readiness subscriptions or `/dev/fd` aliases.

### Pipes and PTYs

- [ ] Add tests for pipe EOF behavior when the last writer or last reader disappears while another task is blocked.
- [ ] Add tests for non-blocking pipe writes at capacity, including partial-write vs full-error behavior.
- [ ] Add tests for PTY canonical-mode line editing, backspace handling, and signal-generating control characters together in one flow.
- [ ] Add tests for PTY raw-mode transitions while data is buffered and while multiple readers or writers are attached.
- [ ] Add tests that PTY resize events propagate to both master and slave observable state.

### Poll and command resolution

- [ ] Add tests that `poll()` generation counters do not miss wakeups during rapid ready-unready-ready transitions.
- [ ] Add tests that mixed FD sets containing pipes, PTYs, files, and invalid descriptors return stable readiness/error masks.
- [ ] Add tests that command-registry shadowing rules are deterministic when two providers claim the same command name.
- [ ] Add tests that direct-path execution bypasses registry stubs only when the path actually resolves to a guest-visible executable.
