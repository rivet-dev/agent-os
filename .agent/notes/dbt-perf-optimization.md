# dbt-on-Pyodide cold-start optimization opportunities

## Current state (April 2026)

Measured on darwin/Node 24, Pyodide 0.29.3, 27 wheels (incl. DuckDB):

| Metric | Median | Budget |
|---|---|---|
| Cold-start (`AgentOs.create` → `import dbt` ready) | 5.0 s | 30 s |
| Warm `import dbt` (subsequent call on same kernel) | 1.1 ms | 5 s |
| Trivial dbt run (1 model) | 1.4 s | 15 s |
| jaffle_shop dbt build (4 models + tests) | 7.7 s | 60 s |

**Cold-start is well within budget.** This document captures
optimizations to apply if the budget tightens or if we ever boot kernels
per request (e.g. one kernel per HTTP request in a server runtime).

## Where the 5 seconds goes

Approximate breakdown (instrumented manually):

1. **Pyodide + Python 3.13 + asm.wasm load**: ~1.5 s
   - V8 instantiates wasm, loads CPython
   - Driven by `loadPyodide(...)` in `wheel-preload.ts`
2. **`pyodide.loadPackage(["micropip", ...26 bundled deps])`**: ~1.5 s
   - Pyodide downloads + extracts each bundled wheel from the indexURL
   - Already batched (single call with array); can't easily speed up
3. **`micropip.install(27 wheels, deps=False)`**: ~1.5 s
   - Each wheel: read from NODEFS, parse metadata, extract to site-packages
4. **Bootstrap script**: ~0.5 s
   - Multiprocessing stub, ThreadPool patch, dbt pre-imports

## Opportunities (not yet implemented)

### 1. Pyodide lockfile for the bundled-deps step

Pyodide supports `loadPyodide({ lockFileURL: 'pyodide-lock.json' })` to
pre-resolve the package set. We could ship a custom lockfile that
includes our 26 bundled deps as already-resolved, eliminating the
loadPackage round-trips.

**Estimated savings:** 1.0–1.5 s on cold-start.

**Cost:** lockfile must be regenerated whenever Pyodide bumps its
bundled package set. Adds CI complexity.

### 2. Skip micropip; install wheels via `pyodide.unpackArchive`

`micropip` is a thin layer around Pyodide's filesystem. We could mount
the wheels directly and use `pyodide.unpackArchive('emfs:/wheels/...')`
to skip the metadata parsing and dependency-resolution overhead.

**Estimated savings:** 0.3–0.8 s.

**Cost:** loses `micropip.list()` introspection for our wheels;
breaks any dbt code that does `from importlib.metadata import version`.
Probably not worth it.

### 3. Persist Pyodide state across kernel instances

If the same agent-os process boots multiple kernels with the same
`python.dbt: true` config, the wheel install work is repeated. A
worker-pool that keeps initialized workers warm would amortize cold-start
across requests.

**Estimated savings:** 5.0 s per warm reuse (i.e. all of cold-start).

**Cost:** worker-pool implementation in agent-os/core; lifecycle/cleanup
logic; memory cost (each warm worker holds ~150 MB resident).

### 4. Parallelize loadPackage and wheel-mount preparation

NODEFS mount is synchronous and ~free. The loadPackage call waits for
network/disk fetch of the bundled deps. Could overlap with mount setup
but the win is negligible (<50 ms).

### 5. Pre-compile dbt's Jinja templates

dbt-core lazily compiles 475 Jinja macros on first run. A pre-compile
step run once per project (not per kernel) could shave ~200 ms off the
first `dbt run` after each fresh boot.

**Estimated savings:** 200 ms.

**Cost:** dbt-core has no public API for pre-compiled macro caches.

## Recommendation

**Do nothing for now.** 5 s cold-start is excellent for a
batteries-included Python data stack. If we adopt a worker-pool model
for serving (e.g. Rivet actor with multiple sessions per actor),
revisit option #3 first — it's the highest-leverage win.

If we ever need sub-1-second cold-start (e.g. for true serverless/
per-request kernels), option #1 (lockfile) + option #3 (worker pool)
together would get us close.
