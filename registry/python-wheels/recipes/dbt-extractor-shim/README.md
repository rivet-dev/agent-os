# dbt-extractor (shim)

Pure-Python fallback for `dbt-extractor`.

When dbt-core imports `dbt_extractor.py_extract_from_source`, this shim
raises `ExtractionError`, which dbt-core catches and falls back to the
full Jinja rendering path (the same path that runs when
`DBT_STATIC_PARSER=False`).

This package exists because the upstream `dbt-extractor` is a Rust crate
with a `rayon` dependency that does not link cleanly under
`wasm32-unknown-emscripten` without pthreads. Use this shim in Pyodide
deployments where the real wheel cannot be built.
