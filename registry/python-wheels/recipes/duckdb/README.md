# DuckDB Pyodide wheel — from-source build with httpfs

Replaces the xlwings/duckdb-pyodide prebuilt wheel with a local
from-source build that adds the `httpfs` extension (statically
linked, with TLS via the already-vendored mbedtls).

## Why

Pyodide's DuckDB statically links its bundled extensions; you can't
`LOAD '/path/httpfs.duckdb_extension'` at runtime against a wheel that
wasn't built with httpfs. The xlwings wheel has the four core
extensions (core_functions, json, parquet, icu) and **no httpfs**.

For dbt-duckdb running inside Pyodide to read S3 sources directly
over HTTPS during `dbt build`, the wheel itself must include httpfs.
This recipe rebuilds DuckDB from the `duckdb-python` v1.5.0 source
with `BUILD_HTTPFS_EXTENSION=ON`.

## Build flow (mirrors xlwings's known-working setup)

1. **Clone** `duckdb/duckdb-python.git` at `v1.5.0` with submodules
   (the duckdb C++ source is a submodule at `duckdb/`).
2. **Patch CMakeLists.txt** — wasm-ld doesn't support GNU ld's
   `--export-dynamic-symbol`; pyodide-build handles symbol exports via
   `--exports=whole_archive`. (Replicated from xlwings/duckdb-pyodide
   `scripts/patch_cmake.py`.)
3. **Patch httpfs+CMake for emscripten** — wire mbedtls (already
   vendored in `duckdb/third_party/mbedtls/`) and use httplib over
   emscripten's BSD socket emulation, which proxies sockets through
   Node's `net` module when running under Pyodide-on-Node.
4. **Run `pyodide build --exports=whole_archive`** with the
   environment xlwings uses (`DUCKDB_CUSTOM_PLATFORM=wasm_eh_pyodide`,
   `CFLAGS=-fwasm-exceptions`, etc.) plus our `BUILD_HTTPFS_EXTENSION=ON`
   addition.

The build takes ~20 min on a fast machine. Expect the first
iteration to fail at the link step — emscripten's libc differs from
WASI in subtle ways (no `select.h` shim, different errno mapping,
etc.). Diagnostics in `.build-cache/duckdb/build-out.log`.

## Patches in this recipe

| Patch | Purpose |
|---|---|
| `0001-cmake-wasm-ld-export-dynamic-symbol.patch` | Skip GNU ld export flags under emscripten. From xlwings/duckdb-pyodide. |
| `0002-cmake-enable-httpfs-with-mbedtls.patch` | Force-enable httpfs cmake target + link mbedtls. New for this recipe. |
| `0003-httpfs-emscripten-socket-emulation.patch` | Adjust the WASI httpfs httplib client to work over emscripten's BSD socket emulation. Adapted from agent-os WASI httpfs patch. |

## Critical finding from research (2026-04-28)

Upstream `duckdb-httpfs` ships an **emscripten stub** at
`src/httpfs_client_wasm.cpp` (16 lines, throws `InternalException` on
every call). DuckDB-WASM (the npm package) avoids this by intercepting
network reads in the JavaScript runtime layer that ships alongside it.

Pyodide has no equivalent JS interception layer — so simply enabling
`-DBUILD_HTTPFS_EXTENSION=ON` against an unmodified upstream produces a
wheel where `LOAD httpfs` succeeds but the first `SELECT ... FROM 's3://...'`
throws.

**Implication:** Patch 0003 must replace the upstream emscripten stub
with a real httplib client analogous to our WASI patch
(`agent-os/registry/native/c/patches/httpfs/0001-wasi-real-httplib-client.patch`),
adapted for emscripten's BSD socket emulation. emscripten exposes POSIX
sockets via `net.Socket` proxying when running under Pyodide-on-Node,
which is our deploy target.

The WASI patch is the template; emscripten differences:
- Different headers/sysroot
- Different errno mappings
- TLS via mbedtls works (mbedtls already vendored in duckdb's third_party/)

## Caveats

- **DuckDB extension installation** at runtime (`INSTALL httpfs`)
  remains a no-op — Pyodide's wheel is statically linked. `LOAD httpfs`
  succeeds because httpfs is already compiled in.
- **HTTPS handshake under emscripten** is the highest-risk gate.
  Phase 17b verifies this end-to-end. If mbedtls's entropy source
  doesn't work under emscripten, fallback is fetching via Python
  urllib (slower per-query but unblocks 100 GB).
- **Bumping the DuckDB version** requires also bumping pyodide / Python
  tags — see `Makefile:DUCKDB_VERSION` for the version compatibility
  matrix xlwings publishes.

## Verification

```bash
# After build completes:
node scripts/verify_duckdb.mjs ./wheels-dist
# Smoke tests: import, SELECT, parquet read, LOAD httpfs, MinIO HTTP read.
```
