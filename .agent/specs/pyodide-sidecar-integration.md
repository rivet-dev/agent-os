# Pyodide Sidecar Integration Spec

Run Python code inside the Rust kernel sidecar by hosting Pyodide (CPython compiled to WASM via Emscripten) within the existing Node.js execution infrastructure.

## Design Decision

Pyodide's WASM module requires a JavaScript host — its Emscripten glue imports ~2,579 JS functions, including a ~147-function FFI bridge that operates on live JS heap objects. Reimplementing this in Rust is not viable. Instead, we run Pyodide inside the same sandboxed Node.js subprocesses the sidecar already spawns for JavaScript and WASM execution.

This reuses 100% of the existing execution infrastructure: Node.js sandbox hardening (`--permission`), compile caching, frozen time, stdio event streaming, and process lifecycle management.

## Architecture

```
Rust sidecar
  │
  ├── JavascriptExecutionEngine  →  spawns Node.js  →  runs user JS code
  ├── WasmExecutionEngine        →  spawns Node.js  →  runs wasm-runner.mjs  →  node:wasi  →  .wasm binary
  └── PythonExecutionEngine      →  spawns Node.js  →  runs python-runner.mjs →  loadPyodide()  →  user Python code
```

All three engines follow the same pattern: spawn a hardened Node.js child process, stream stdout/stderr/exit back over pipes. The sidecar owns the process lifecycle.

Pyodide handles its own WASM loading internally (`WebAssembly.compile` + `WebAssembly.instantiate` with Emscripten imports). The sidecar never touches `pyodide.asm.wasm` directly.

## Key Constraints

- **No CDN fetches.** Pyodide and all Python packages are pre-bundled in the filesystem image. `indexURL` and `packageBaseUrl` point to local paths. Network access for package loading is disabled.
- **No `node:vm` dependency.** If Pyodide requires `node:vm` (currently blocked in the sandbox), either add it to the allowed builtins for Python contexts or patch Pyodide's usage.
- **No `node:child_process`.** Pyodide must not spawn host processes. This is already blocked by the sandbox.
- **Frozen time applies.** Python code sees frozen time like all other guest runtimes. Pyodide's internal timers will reflect the frozen timestamp.
- **VFS integration is scoped.** Phase 1 gives Python access to the workspace directory (same as WASM execution today). Full kernel VFS integration (reading/writing arbitrary VM paths) comes in Phase 3 via a stdin/stdout RPC bridge.

## Node.js API Requirements

| Module | Required By | Sandbox Status | Action |
|---|---|---|---|
| `node:fs` / `node:fs/promises` | Pyodide (load .wasm + packages from disk) | Allowed | None |
| `node:path` | Pyodide (path resolution) | Allowed | None |
| `node:url` | Pyodide (file URL conversion) | Allowed | None |
| `node:crypto` | Pyodide (crypto ops) | Allowed | None |
| `WebAssembly.*` | Pyodide (WASM loading) | Always available (V8 built-in) | None |
| `node:vm` | Pyodide (eval — needs testing) | Blocked | Test; allow for Python contexts if needed |
| `node:child_process` | Pyodide (optional) | Blocked | Keep blocked; verify Pyodide degrades gracefully |

---

## Phase 1: Minimal Python Execution

Run a Python string inside the sidecar and get stdout/stderr/exit code back.

### Rust Changes

**`crates/sidecar/src/protocol.rs`** — Add `Python` variant to `GuestRuntimeKind`:

```rust
pub enum GuestRuntimeKind {
    JavaScript,
    WebAssembly,
    Python,
}
```

**`crates/execution/src/python.rs`** — New `PythonExecutionEngine`, structurally identical to `WasmExecutionEngine`:
- `CreatePythonContextRequest` / `PythonContext` — tracks bundled Pyodide path
- `StartPythonExecutionRequest` — includes Python code string, env, cwd
- `PythonExecution` — wraps child process handle + event receiver
- Spawns Node.js with `python-runner.mjs` as entrypoint
- Passes Python code via `AGENT_OS_PYTHON_CODE` env var
- Passes bundled Pyodide path via `AGENT_OS_PYODIDE_INDEX_URL` env var
- Reuses `harden_node_command` with Pyodide distribution path added to read paths
- Reuses `spawn_stream_reader` / `spawn_waiter` for stdio streaming

**`crates/sidecar/src/service.rs`** — Add third dispatch arm:

```rust
GuestRuntimeKind::Python => {
    let context = self.python_engine.create_context(...);
    let execution = self.python_engine.start_execution(...);
    ActiveExecution::Python(execution)
}
```

### JS Changes

**`crates/execution/src/node_import_cache.rs`** — Add `NODE_PYTHON_RUNNER_SOURCE`:

```javascript
import { loadPyodide } from "pyodide";

const code = process.env.AGENT_OS_PYTHON_CODE;
const indexURL = process.env.AGENT_OS_PYODIDE_INDEX_URL;

const py = await loadPyodide({
    indexURL,
    stdout: (msg) => process.stdout.write(msg + "\n"),
    stderr: (msg) => process.stderr.write(msg + "\n"),
});

try {
    await py.runPythonAsync(code);
} catch (err) {
    process.stderr.write(err.message + "\n");
    process.exit(1);
}
```

### Pyodide Bundling

- Bundle Pyodide distribution (pyodide.mjs, pyodide.asm.wasm, pyodide-lock.json, stdlib packages) into a known path within the sidecar's cache or asset directory
- The `NodeImportCache` manages the Pyodide bundle alongside the existing JS/WASM assets
- Set `lockFileContents` in `loadPyodide()` to avoid any network fetch for the lock file

### Acceptance Criteria

- [ ] `GuestRuntimeKind::Python` is accepted by the protocol
- [ ] Sidecar can execute `print("hello world")` and return stdout `"hello world\n"` with exit code 0
- [ ] Syntax errors in Python code produce stderr output and exit code 1
- [ ] Python execution respects frozen time (`import time; print(time.time())` returns the frozen value)
- [ ] `node:child_process` and `node:vm` are not accessible from within the Pyodide runtime
- [ ] No network requests are made during Pyodide initialization or code execution
- [ ] Process lifecycle works: stdin write, stdin close, kill (SIGTERM), exit code propagation
- [ ] Multiple concurrent Python executions in different VMs work independently

---

## Phase 2: Pre-bundled Python Packages

Ship numpy, pandas, and other common packages as pre-compiled Emscripten `.whl` files bundled into the Pyodide distribution.

### Changes

- Extend the Pyodide bundle to include pre-built wheels for target packages
- Add `AGENT_OS_PYTHON_PRELOAD_PACKAGES` env var (JSON array of package names to load at init)
- `python-runner.mjs` calls `await py.loadPackage(packages)` before running user code
- Packages load from local disk via `packageBaseUrl` — no CDN fetch
- Add a registry software package `@rivet-dev/agent-os-python-packages` that bundles the wheels

### Acceptance Criteria

- [ ] `import numpy; print(numpy.__version__)` works with pre-bundled numpy
- [ ] `import pandas; print(pandas.__version__)` works with pre-bundled pandas
- [ ] Package loading does not make network requests
- [ ] Packages are loaded from the local bundle path, not from CDN
- [ ] Unknown package imports fail with a clear error (no silent CDN fallback)
- [ ] Total bundle size is documented (Pyodide base ~25MB + packages)

---

## Phase 3: Kernel VFS Integration

Python code reads/writes the kernel's virtual filesystem, not just the host-mapped workspace directory.

### Changes

**Sidecar-side RPC bridge** — The `python-runner.mjs` communicates with the sidecar over a dedicated channel (either additional file descriptors or a structured protocol over stdin) for filesystem operations:

- `fsRead(path)` → sidecar reads from kernel VFS → returns content
- `fsWrite(path, content)` → sidecar writes to kernel VFS
- `fsStat(path)` → sidecar stats in kernel VFS
- `fsReaddir(path)` → sidecar lists kernel VFS directory
- `fsMkdir(path)` → sidecar creates directory in kernel VFS

**Pyodide filesystem backend** — Register a custom Emscripten filesystem mount in Pyodide that proxies all operations through the RPC bridge:

```javascript
py.FS.mount(py.FS.filesystems.PROXYFS, {
    root: "/",
    createdNode: (parent, name) => { /* proxy to sidecar */ },
}, "/workspace");
```

Alternative: use Pyodide's `secure_exec` JS module pattern (already exists in `packages/python/dist/driver.js:145-149`) where Python code calls `import secure_exec; secure_exec.read_text_file(path)` and the JS host proxies to the sidecar.

### Acceptance Criteria

- [ ] Python code can read files written by the kernel VFS (`open("/workspace/file.txt").read()`)
- [ ] Python code can write files visible to other kernel runtimes
- [ ] Python `os.listdir("/workspace")` reflects the kernel VFS state
- [ ] File operations respect kernel permissions
- [ ] Cross-runtime file visibility: JS writes a file → Python reads it (and vice versa)

---

## Phase 4: Stdin / Interactive Python

Support interactive Python execution with stdin streaming, matching the JS and WASM execution models.

### Changes

- `python-runner.mjs` reads stdin and feeds it to Pyodide's stdin handler via `py.setStdin()`
- Sidecar's `WriteStdin` request routes to the Python child process stdin pipe
- `CloseStdin` triggers EOF in Python's `input()` / `sys.stdin.read()`
- Support `AGENT_OS_PYTHON_FILE` env var as alternative to `AGENT_OS_PYTHON_CODE` for running `.py` files from the VFS

### Acceptance Criteria

- [ ] `input("prompt: ")` blocks until stdin data arrives from the sidecar
- [ ] Multiple `input()` calls work with streaming stdin writes
- [ ] `CloseStdin` causes `input()` to raise `EOFError`
- [ ] `sys.stdin.read()` collects all stdin until close
- [ ] Running a `.py` file by path works (`AGENT_OS_PYTHON_FILE=/workspace/script.py`)

---

## Phase 5: Prewarm and Performance

Optimize Python startup time using the same prewarm pattern as WASM execution.

### Changes

- Add `AGENT_OS_PYTHON_PREWARM_ONLY` env var — runner loads Pyodide then exits immediately
- `PythonExecutionEngine` runs a prewarm step before first real execution (same as `prewarm_wasm_path`)
- Stamp file tracks Pyodide version + Node.js compile cache state
- Node.js `--compile-cache` covers Pyodide's JS glue compilation
- Measure and document cold start vs warm start times

### Acceptance Criteria

- [ ] Second Python execution in the same sidecar session starts faster than the first
- [ ] Prewarm stamp is invalidated when Pyodide bundle version changes
- [ ] `AGENT_OS_WASM_WARMUP_DEBUG=1` equivalent produces timing metrics for Python startup
- [ ] Cold start time documented (target: <3s on commodity hardware)
- [ ] Warm start time documented (target: <500ms)

---

## Out of Scope

- **Python package installation at runtime** (`pip install`, `micropip.install`). All packages are pre-bundled. Runtime installation is blocked.
- **Python-JS FFI from user code** (`from js import ...`). The `js` and `pyodide_js` modules remain blocked (sandbox escape prevention, already implemented in existing driver).
- **Multi-threaded Python** (threading, multiprocessing). Pyodide is single-threaded. No SharedArrayBuffer or Worker threads.
- **Python REPL / notebook mode**. Interactive REPL is deferred. Phase 4 covers stdin for `input()` but not a persistent REPL session.
