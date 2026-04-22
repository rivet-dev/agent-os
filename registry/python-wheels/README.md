# registry/python-wheels — wheel build infrastructure

Builds the Pyodide-compatible wheels that ship inside
[`@rivet-dev/agent-os-python-wheels`](../software/python-wheels/).

This directory is **build-only** — it produces `.whl` files into
`../software/python-wheels/wheels/` and exits. The published package never
references anything in this directory.

## Layout

```
registry/python-wheels/
├── Makefile                  # build orchestration
├── README.md                 # this file
├── .gitignore                # toolchain caches
├── recipes/
│   ├── dbt-extractor/
│   │   ├── meta.yaml         # pyodide-build recipe (Rust + PyO3)
│   │   └── patches/          # rayon-emscripten gate, tree-sitter-jinja2 vendor
│   └── dbt-extractor-shim/   # pure-Python fallback (same import name)
│       ├── pyproject.toml
│       └── src/dbt_extractor/__init__.py
├── scripts/
│   ├── build_extractor.sh
│   ├── build_shim.sh
│   ├── fetch_duckdb.sh
│   ├── build_pure_index.py
│   ├── verify_extractor.mjs
│   ├── verify_duckdb.mjs
│   └── verify_pure_index.mjs
└── .toolchain/               # gitignored: emsdk + Python venv
```

## Quick start

```bash
# One-time
make setup-toolchain   # ~5 min: installs Python venv, pyodide-build, emsdk

# Build everything
make build-all         # ~20-30 min: builds all wheels into ../software/python-wheels/wheels/

# Verify
make verify-all        # spawns Node + Pyodide, imports each wheel
```

## Pinned versions

| Component | Version | Notes |
|---|---|---|
| Pyodide runtime | 0.29.3 | ABI tag `pyodide_2025_0_wasm32` |
| Python | 3.13 | tag `cp313` |
| pyodide-build | 0.34.1 | PyPI |
| Emscripten | (queried from Pyodide) | typically 4.0.9 for Pyodide 0.29 |
| dbt-core | 1.11.x (latest compat) | resolved by `uv pip compile` |
| dbt-duckdb | 1.x (latest compat) | resolved by `uv pip compile` |
| DuckDB | 1.5.1 | xlwings prebuilt wheel |
| dbt-extractor | 0.6.x | built from sdist |

To update, edit the `PYODIDE_VERSION` / `PYTHON_TAG` / `PYODIDE_ABI_TAG`
variables at the top of the `Makefile`. The pure-Python closure is
re-resolved by `scripts/build_pure_index.py` on every build.

## CI

GitHub Actions workflow at
[`.github/workflows/python-wheels.yml`](../../.github/workflows/python-wheels.yml)
runs the same Makefile targets on `ubuntu-24.04` and uploads the resulting
wheels as a release asset.
