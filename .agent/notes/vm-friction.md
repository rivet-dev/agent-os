# VM friction log

Things in the agent-os runtime that diverge from a standard
POSIX/Node/Python system. Each entry: the deviation, root cause, and
whether a fix exists.

## Python (Pyodide)

### dbt-core requires multiprocessing.get_context monkey-patch

**Deviation.** `import dbt.mp_context` runs `multiprocessing.get_context("spawn")`
at module import time. Pyodide's `multiprocessing` module is importable
but `get_context("spawn")` raises because spawn semantics require fork
and `_posixsubprocess`, neither of which exist under Emscripten.

**Root cause.** Pyodide is single-threaded by design (no real pthreads,
no fork). dbt-core was written for a desktop CPython that supports
multiprocessing.

**Fix.** `packages/python/src/dbt-bootstrap.ts` exports `DBT_BOOTSTRAP_SCRIPT`
which monkey-patches `multiprocessing.get_context` to return a
`SimpleNamespace` shim exposing the small surface dbt-adapters consumes
(Lock/RLock/Semaphore/Queue, all backed by `threading`). Applied at
worker init when `python.dbt: true` is set on AgentOs.

### dbt-extractor needs Pyodide wheel build

**Deviation.** dbt-core's `parser/models.py` imports `dbt_extractor` at
module top. The upstream Rust crate has a `rayon` dependency that does
not link cleanly under `wasm32-unknown-emscripten` without pthreads.

**Root cause.** Pyodide ABI does not allow `-pthread` link flags.

**Fix.** Two-track approach:

1. Build a real wheel via `pyodide-build` with a patch that
   feature-gates rayon behind `cfg(not(target_os = "emscripten"))`. See
   `registry/python-wheels/recipes/dbt-extractor/`.
2. Pure-Python shim package at
   `registry/python-wheels/recipes/dbt-extractor-shim/` that exposes
   `ExtractionError` and a `py_extract_from_source` that always raises.
   dbt-core catches the error and falls back to full Jinja rendering.
   Used when the Rust wheel can't be built.

### DuckDB inside Pyodide has limited extensions

**Deviation.** The xlwings-built DuckDB wheel only statically links
`core_functions`, `json`, `parquet`, `icu`. Runtime `INSTALL` /
`LOAD` of any extension fails. `httpfs` in particular is absent —
no remote Parquet, no S3.

**Root cause.** The Pyodide build pipeline (CMake under emcc) statically
links a fixed extension set. Loading additional `.duckdb_extension.wasm`
files would require porting duckdb-wasm's `WASM_LOADABLE_EXTENSIONS`
patch, which is ~700 LOC and out of scope.

**Fix.** Document the constraint; for remote files, fetch via Python
(`pyodide.http.pyfetch`) and pass to DuckDB locally.

### DuckDB single-threaded under Pyodide

**Deviation.** `PRAGMA threads=N` is ignored; DuckDB's TaskScheduler runs
serial. Aggregations over very large datasets are 2–8x slower than
native.

**Root cause.** No Emscripten pthreads in the Pyodide ABI.

**Fix.** None planned. Workaround: keep dataset sizes modest and use
columnar queries.

### dbt cold-start is ~10 seconds

**Deviation.** First `aos.exec("python -c 'import dbt'")` takes 7–15
seconds on a cold kernel because the DuckDB wasm has to be loaded via
`pyodide.loadDynamicLibrary` and the dbt stack has to be
micropip-installed.

**Root cause.** Pyodide instantiates V8 + the wasm runtime + linker on
every fresh worker.

**Fix.** Reuse warm kernels across invocations. The Python worker stays
alive per-driver instance, so subsequent calls are fast.

### micropip is allowlisted, not freely accessible

**Deviation.** `import micropip` from agent code raises
`ERR_PYTHON_PACKAGE_INSTALL_UNSUPPORTED` even when wheels are mounted.

**Root cause.** Deliberate sandbox boundary. Allowing arbitrary
`micropip.install` from agent code would let agents pull the entire PyPI
catalog. The wheel preload runs once at worker init, before the install
block re-engages.

**Fix.** None planned. Use `wheelPreload` (or `python.dbt`) to pre-vendor
wheels at the host level.

## Pyodide ABI mismatch

**Deviation.** A wheel built for `pyodide_2025_0_wasm32` will not load
in a runtime running on a different ABI tag (e.g.
`pyemscripten_2026_0_wasm32` from Pyodide 0.30 alpha).

**Root cause.** Pyodide bumps its ABI on every Python version change.

**Fix.** Pin `PYODIDE_VERSION` and `PYODIDE_ABI_TAG` together in
`registry/python-wheels/Makefile`. Bump both when upgrading.
