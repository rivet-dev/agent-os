# Python Pyodide Runtime Test Checklist

Source files:
- `crates/execution/src/python.rs`
- `crates/execution/assets/runners/python-runner.mjs`
- `crates/execution/assets/pyodide/*`

Suggested test homes:
- `crates/execution/tests/python.rs`
- `crates/execution/tests/python_prewarm.rs`
- `crates/sidecar/tests/python.rs`
- `crates/execution/src/python.rs`

## Checklist

### Execution lifecycle

- [ ] Add tests that Python startup failures in interpreter boot, package staging, and runner bootstrap surface stable guest-visible errors and do not poison later runs.
- [ ] Add tests that timeout, cancellation, and normal completion flush stdout/stderr buffers exactly once without duplicate tail output.
- [ ] Add tests that prewarm and warmup flows are safe under concurrent callers and after a failed prior warmup.

### VFS and service bridging

- [ ] Add tests that Python VFS RPCs cover open/read/write/rename/stat/readdir/symlink and return Python-facing exceptions with stable shapes.
- [ ] Add tests that Python HTTP, DNS, and subprocess bridge calls honor the same permission and resource policies as JS and WASM.
- [ ] Add tests that binary payloads and non-UTF8 file contents round-trip through the Python bridge unchanged.
- [ ] Add tests that guest-side exceptions raised from bridge failures preserve traceback context rather than collapsing into generic runtime errors.

### Asset staging and package behavior

- [ ] Add tests that `pyodide.mjs`, `pyodide-lock.json`, the stdlib ZIP, and bundled wheels are present before execution starts.
- [ ] Add tests that repeated package staging reuses caches when valid and invalidates them when bundle versions change.
- [ ] Add tests that large wheel extraction, import-heavy workloads, and package import failures do not corrupt the runtime cache for later sessions.
- [ ] Add tests that `AGENT_OS_PYTHON_PRELOAD_PACKAGES` parsing rejects invalid JSON, non-array payloads, and non-string entries.
