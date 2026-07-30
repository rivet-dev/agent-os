# AgentOS native runtime fixes extracted from browser shell work

This document records the native AgentOS fixes extracted from `bb-demo`
revision `f48396c6` (PR #1745) onto `main` revision `f63c6ef2`. The extraction
contains native kernel, sidecar, Rust client, actor transport, V8 bridge,
software source/build, and agent packaging changes. It excludes
Browserbase, `runtime-browser`, browser terminal UI, browser test bundles, and
browser-only Pi module injection.

## Ordered PTY output

**Problem:** the Rust client exposed stdout and stderr through independently
scheduled streams. Terminal renderers could therefore display prompts and
control sequences out of sidecar wire order.

**Code:** the existing shell-data stream is the terminal rendering surface in
both the Rust and TypeScript APIs. Both shell execution paths publish every
wire chunk to it before optionally routing stderr to the diagnostic tap:

```rust
let _ = data_tx.send(output.chunk.clone());
if output.channel == StreamChannel::Stderr {
    let _ = stderr_tx.send(output.chunk);
}
```

Rust `AgentOs::on_shell_data` and TypeScript `AgentOs.onShellData` expose the
same ordered stream. The actor broadcasts it as `shellData` while retaining
`shellStderr` as the optional channel-specific diagnostic tap. A terminal
client renders only `shellData`; rendering `shellStderr` too would duplicate
the same stderr bytes.

## Native PTY state and signal behavior

**Problems:** raw mode did not disable carriage-return translation, embedded V8
processes did not observe terminal resize, and a full-screen child could leave
its parent terminal in raw mode.

**Code:**

- `crates/vm-kernel/src/pty.rs` adds `icrnl` to `LineDisciplineConfig` and merges it
  into the live PTY state.
- `service_javascript_pty_set_raw_mode_sync_rpc` in
  `crates/vm/src/execution.rs` acquires/releases the kernel PTY's
  raw-mode lease, which changes ICRNL, canonical mode, echo, signal processing,
  and output post-processing together.
- `resize_pty` reads the PTY foreground process group after resizing and mirrors
  `SIGWINCH` into every tracked JavaScript/WASM execution in that group,
  including nested full-screen and readline children:

  ```rust
  vm.kernel.pty_resize(
      EXECUTION_DRIVER_NAME,
      process.kernel_pid,
      writer_fd,
      payload.cols,
      payload.rows,
  )?;
  let foreground_pgid = vm.kernel.tcgetpgrp(
      EXECUTION_DRIVER_NAME,
      process.kernel_pid,
      writer_fd,
  )?;
  for active in vm.active_processes.values() {
      dispatch_v8_signal_to_tracked_processes(active, &foreground_pids, libc::SIGWINCH)?;
  }
  ```

- `PtyState` owns a bounded-by-process-count raw-mode lease stack. Only members
  of the current foreground process group receive recovery leases. Each lease
  has a monotonic generation and the exact prior `Termios`; nested leases unwind
  correctly even when owners exit out of order, while a newer `tcsetattr` or
  line-discipline mutation invalidates stale recovery. `ActiveProcess` retains
  only its lease generation, and child cleanup releases that specific lease.
  An ordinary or background child that never acquired raw mode cannot alter the
  terminal during reap.

`SIGWINCH` forwarding is limited to embedded JavaScript and WASM V8 executions;
Python and binding executions retain the kernel resize without receiving an invalid
JavaScript stream event.

The kernel PTY tests cover nested/out-of-order owners, stale-generation
protection, and background processes without recovery ownership. The isolated
sidecar service test asserts that raw mode disables `icrnl` and cooked
mode restores it; the signal suite verifies both root and nested foreground V8
executions observe a live resize.

## Native child execution

### Executable shebang scripts

**Problem:** JavaScript `child_process` resolution could identify an executable
guest file as WASM and fail before honoring its shebang. This prevented normal
executable shell scripts and `/usr/bin/env` entrypoints from working through the
sidecar.

**Code:** `resolve_javascript_child_process_with_shebang` resolves the initial
entrypoint, verifies execute permission, reads a bounded shebang from the guest
VFS, rewrites the command and arguments, and resolves again. It preserves
Linux's single optional interpreter string; `/usr/bin/env -S ...` is parsed as
shell words with quoted and backslash-escaped arguments before the real
interpreter is resolved. Plain `/usr/bin/env command` is also supported, while
multiple env arguments without `-S` fail explicitly. The resolver caps the
line at 256 bytes and allows at most four redirects. Guest VFS and metadata
failures propagate rather than being silently treated as non-shebang files.
Invalid input returns explicit `EACCES`, `ENOEXEC`, `ENOENT`, or `ELOOP` errors.

```rust
interpreter_args.push(script_path);
interpreter_args.extend(resolved.execution_args.iter().cloned());
request.command = command;
request.args = interpreter_args;
request.options.shell = false;
```

### Shared-PTY write ordering

**Problem:** a child sharing its shell's PTY could have its write acknowledged
before the owner queued the drained terminal bytes. The parent could then render
its next prompt before the child output.

**Code:** `service_shared_tty_stdio_write` now drains the PTY master and queues
the owner's `ActiveExecutionEvent::Stdout` before acknowledging the write. A
missing owner is an explicit `InvalidState` error rather than silently dropping
output.

## Native V8 bridge compatibility

These files are the source for the native bridge generated by
`packages/build-tools/scripts/build-v8-bridge.mjs`; they are not browser-runtime
copies.

- `builtin-modules.ts` makes `path.posix` reference the selected AgentOS Linux
  path module rather than a stale uncloned namespace.
- `console.ts` installs `util.formatWithOptions` before formatting console
  arguments, preserving Node-style placeholders and inspection behavior.
- `tty-config.ts` no longer permanently caches the 80x24 non-TTY bootstrap
  fallback before the kernel synchronous bridge has attached. It caches stable
  TTY identity but reads the current kernel window size for every dimensions
  query, so `process.stdout.columns` and `.rows` change after `SIGWINCH`. The
  added `tty-config.test.mjs` verifies both bootstrap recovery and live resize.

The browser-only `__agentOSBuiltinWasiModule` injection from the source PR is
intentionally not included.

The browser-driven `wasi-module.js` timestamp hunk is also excluded. Native
command execution already supplies `path_filestat_set_times` in
`wasm-runner.mjs`; porting the alternate Node `wasi` implementation without a
separate native contract test would broaden this fix PR rather than repair the
observed native path. Direct `process.uid`/`gid` properties are excluded because
host Node exposes identity through getter methods instead.

## Native software staging and builds

### On-demand coreutils artifacts

**Problem:** `registry/software/coreutils/bin/` contained 113 committed WASM
artifacts even though their patched native sources are authoritative. A fresh
source build was not sufficient to replace them: the bulk Rust build excluded
the `_stubs` command required by the package manifest, the C dependency graph
referenced archive members before their fetch targets existed, and the normal
source-only package build could not distinguish itself from a runtime build.

**Code:** the committed artifacts are removed and the coreutils `bin/` directory
is gitignored. The native Makefile maps the `_stubs` binary to its actual
`cmd-stubs` Cargo package and includes it in the default build. The C Makefile
declares zlib and minizip archive members as outputs of their representative
fetch targets, making a clean dependency graph buildable. Coreutils adds a
strict `build:runtime` script that fails unless all 113 manifest commands,
aliases, and stubs were built; the ordinary `build` script retains placeholder
behavior for repository-wide source checks. `CLAUDE.md` and
the root `justfile` document the complete runtime sequence:

```bash
pnpm install --frozen-lockfile
just tools-rebuild
```

### Brush child PID

**Problem:** Brush's WASI process shim returned `None` from `Child::id`, producing
`WARN could not retrieve pid for child process` for successfully spawned
commands.

**Code:** `toolchain/std-patches/crates/brush-core/0004-wasi-process-id.patch`
returns the PID supplied by the patched process implementation:

```rust
pub fn id(&self) -> Option<u32> {
    Some(self.inner.id())
}
```

Coreutils command artifacts are not committed. `just toolchain-build` applies
this patch while compiling the complete command set, and `just tools-rebuild`
stages that output into the software packages. The runtime build is strict so
an absent or incomplete native build cannot produce a misleading placeholder
package.

### Vim WASI cross-build

**Problem:** Vim configuration could detect host Wayland headers while targeting
WASI and then fail or produce a host-contaminated build.

**Code:** the Vim configure invocation in `registry/native/c/Makefile` includes
`--without-wayland` alongside the existing GUI and X exclusions.

### Pi package dependency

**Problem:** native Pi snapshot and fallback code import
`@mariozechner/pi-agent-core` directly, but the package relied on it arriving
transitively through `pi-coding-agent`.

**Code:** `registry/agent/pi/package.json` now declares the matching `0.60.0`
dependency directly, with the corresponding importer entry in `pnpm-lock.yaml`.
The browser-only `__piSdkModules` adapter fallback remains excluded.

## Extraction boundary

No files under `packages/runtime-browser`, `packages/browser`,
`examples/browser-terminal`, or `examples/experiments/browser-base-shell` are
part of this port. The browser WASI polyfill mirrors the native runner but is
left in the browser PR. Pi's `__piSdkModules` fallback is also excluded because
native Pi resolves its projected package graph normally.

## Validation

The extraction was validated from a fresh JJ workspace based on `main`:

- `cargo fmt --check`
- targeted `cargo check` for kernel, client, execution, sidecar, and
  actor plugin crates
- kernel PTY suite: 24 passed
- native raw-mode service regression: passed
- native path, identity, console, and executable-shebang integration suite:
  passed
- Rust client shell E2E: passed
- actor plugin unit tests: 5 passed; targeted event contract test: passed
- build-tools TTY regression: passed
- clean native registry build: all 113 coreutils manifest entries present
- strict coreutils staging and `.aospkg` assembly: passed
- native Pi package build: passed
- Brush interactive PTY suite: 3 passed
- V8 PTY line-discipline matrix: 20 passed, including live `SIGWINCH` resize
- repository-wide `pnpm check-types`: 147 tasks passed
- fixed-version verification: passed

The complete actor plugin test command also reaches an unrelated existing
failure: `ts_dto_field_names_match_rust_contract_fixture` expects a
`VirtualStat` interface shape that already differs in unchanged `main` files.
Neither the fixture nor `packages/core/src/runtime.ts` is touched by this port;
the actor unit and event-contract tests that cover the changed shell transport
pass.
