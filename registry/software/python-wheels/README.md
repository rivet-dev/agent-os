# @rivet-dev/agent-os-python-wheels

Pyodide wheels for the **dbt-core + dbt-duckdb + DuckDB** stack, vendored for
offline install via `micropip` inside the agent-os Python runtime.

This package is opt-in: agent-os only mounts these wheels when an `AgentOs`
instance is created with `python.dbt: true`.

## Contents

After running `make -C registry/python-wheels build-all`, the `wheels/`
directory contains:

- One ABI-tagged native wheel per native package (`dbt_extractor`, `duckdb`).
- ~22 pure-Python `*-py3-none-any.whl` wheels for the dbt closure.
- `wheels/index/<package>.json` files in the [warehouse JSON shape](https://warehouse.pypa.io/api-reference/json/) that `micropip.set_index_urls` can resolve.
- `wheels/lockfile.json` pinning exact versions, filenames, and sha256s.

## Build

The wheels are built by a separate pipeline that lives at
[`registry/python-wheels/`](../../python-wheels/) (alongside `registry/native/`).
That directory is build-only — it produces wheels into this package's
`wheels/` directory.

See [`registry/python-wheels/README.md`](../../python-wheels/README.md) for
toolchain setup and per-wheel build commands.

## Constraints

The vendored wheels target a single Pyodide ABI (currently
`pyodide_2025_0_wasm32`, Python 3.13). Mismatched runtimes will fail
`micropip.install`.

DuckDB inside Pyodide does not support:

- Runtime extension installation (only `core_functions`, `json`, `parquet`,
  `icu` are statically linked).
- The `httpfs` extension. Fetch remote files via Python and pass to DuckDB
  locally.
- Multi-threaded query execution.

dbt inside Pyodide:

- Forces `--threads 1` and `DBT_SINGLE_THREADED=True`.
- Disables the static parser (`DBT_STATIC_PARSER=False`), falling back to
  full Jinja rendering.
- Disables anonymous telemetry (`DBT_SEND_ANONYMOUS_USAGE_STATS=False`).
- Disables `dbt deps` (no git/HTTP at runtime); pre-vendor `dbt_packages/`.

See `docs/python-compatibility.mdx` and `.agent/notes/vm-friction.md` for
the full friction log.
