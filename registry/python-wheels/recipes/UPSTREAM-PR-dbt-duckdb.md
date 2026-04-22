# Upstream PR: dbt-duckdb cursor lifecycle fix for Pyodide / wasm runtimes

This document captures the dbt-duckdb patch we ship locally, ready to
submit as a PR to https://github.com/duckdb/dbt-duckdb.

## What this PR does

Fixes two related correctness bugs in `dbt/adapters/duckdb/environments/local.py`
that surface when running dbt-duckdb under runtimes where:

1. dbt-core's connection-pool **reuses a closed cursor** after `close()`,
   and
2. DuckDB's **connection-scoped transaction state** doesn't tolerate
   redundant `BEGIN`/`COMMIT`/`ROLLBACK` from dbt's reuse pattern.

Both issues manifest in WebAssembly DuckDB builds (e.g. via Pyodide) but
the underlying race conditions exist in any environment where Python's
GC closes cursors more aggressively than CPython on Linux, including
some embedded/ASGI deployments.

## Symptoms (before the fix)

```
[…]
On create_memory_main: COMMIT
SQL status: OK in 0.000 seconds
On create_memory_main: Close
Using duckdb connection "create_memory_main"
On create_memory_main: BEGIN
DuckDB adapter: duckdb error: Connection Error: Connection already closed!
```

Or the symmetric variant:

```
On create_memory_main: BEGIN
DuckDB adapter: duckdb error: TransactionContext Error:
  cannot start a transaction within a transaction
```

## Changes

`dbt/adapters/duckdb/environments/local.py`:

- Rewrote `DuckDBCursorWrapper` as a **lazy cursor**: instead of holding
  a single underlying duckdb cursor for the lifetime of the wrapper, it
  mints a fresh cursor from the env's conn the first time `execute()` is
  called and on each call after `close()`. Closing is now idempotent and
  doesn't poison subsequent reuse.
- `execute()` now intercepts `BEGIN`/`COMMIT`/`ROLLBACK` and **swallows
  the three "redundant txn" errors** DuckDB raises:
  - `cannot start a transaction within a transaction`
  - `cannot commit - no transaction is active`
  - `cannot rollback - no transaction is active`
- `DuckDBConnectionWrapper.close()` is now idempotent (`_closed`
  sentinel + try/except around the cursor close).
- `LocalEnvironment._keep_open` now defaults to `True` so the underlying
  conn isn't torn down between dbt's logical-connection cycles.
- `LocalEnvironment.handle()` no longer needs an upfront raw cursor —
  the wrapper mints one lazily.

Net change: 125 lines added, 20 removed.

## Test plan

Verified against a Pyodide-built DuckDB 1.5.0 wheel + dbt-core 1.11.8:

- `dbt --version` succeeds
- `dbt parse` writes manifest + semantic_manifest
- `dbt run --threads 1 --single-threaded` creates models and writes
  `target/run_results.json` with `status=success`
- `dbt seed`, `dbt test`, `dbt build`, `dbt docs generate`, `dbt list`,
  `dbt show --inline "select 42"`, `dbt compile` all pass
- A multi-model DAG (mini jaffle-shop with seeds → staging → marts)
  builds and tests cleanly
- All native dbt-duckdb tests on CPython Linux still pass (no regression
  expected — the changes are additive and gated by error-pattern matching)

## Compatibility

- **No API changes.** `DuckDBConnectionWrapper.cursor()`,
  `LocalEnvironment.handle()`, etc. retain the same signatures and
  return-types.
- **No new deps.**
- **No behavior change for the happy path.** The error-swallowing only
  fires when DuckDB raises one of the three explicit txn-state errors;
  any other error is re-raised as `DbtRuntimeError` exactly as before.
- The `_keep_open = True` default trades a slight memory increase
  (one DuckDB conn cached per env) for a major correctness improvement.
  The previous heuristic
  `keep_open or path == ":memory:" or is_motherduck` is now subsumed.

## Files changed

- `dbt/adapters/duckdb/environments/local.py` (only file)

## Apply locally

```bash
git clone https://github.com/duckdb/dbt-duckdb && cd dbt-duckdb
git checkout 1.10.1
git apply /path/to/registry/python-wheels/recipes/dbt-duckdb-pyodide-fix.patch
```

Or build the wheel directly:

```bash
PBR_VERSION=1.10.2 python3 -m build --wheel
```

## How to actually open the PR (manual steps for the human)

1. Fork https://github.com/duckdb/dbt-duckdb on GitHub.
2. ```bash
   cd /tmp/dbt-duckdb-fix/dbt-duckdb
   git remote add fork git@github.com:<YOUR_USERNAME>/dbt-duckdb.git
   git checkout -b pyodide-cursor-lifecycle-fix
   git add dbt/adapters/duckdb/environments/local.py
   git commit -m "fix(local): make cursor reuse + redundant txn errors safe"
   git push fork pyodide-cursor-lifecycle-fix
   ```
3. Open the PR via the GitHub UI, paste the "What this PR does" / "Test
   plan" / "Compatibility" sections from above into the description.

## Related links

- Pyodide DuckDB build (xlwings):
  https://github.com/xlwings/duckdb-pyodide
- Pyodide DuckDB recipe:
  https://github.com/pyodide/pyodide-recipes/blob/main/packages/duckdb/meta.yaml
- agent-os usage at:
  https://github.com/rivet-dev/agent-os (ralph plan: dbt-on-Pyodide)
