# pyodide-httpfs

`fsspec`-compatible HTTPS/S3 filesystem for DuckDB running inside Pyodide.

## Why this exists

DuckDB compiled for Pyodide statically links the upstream emscripten httpfs
stub at `httpfs_client_wasm.cpp` — a 16-line file that throws `"InitializeClient
is not expected to be called"` on every network call. The official
`@duckdb/duckdb-wasm` npm package works around this with ~700 lines of
JS-side patches that intercept extension loading. None of that is available
to Pyodide-loaded DuckDB.

We can't fix the C++ side because **Pyodide's runtime doesn't expose Asyncify
or JSPI to side modules** — the .so dlopen'd into Pyodide can't suspend on a
JS Promise. So `httpfs` (which is wasm code in a side module) is fundamentally
unable to do synchronous network I/O in Pyodide.

But Python in Pyodide's main module CAN do it, via a `SharedArrayBuffer +
side-worker` bridge installed by the host JS environment before Pyodide
loads. `Atomics.wait` blocks the wasm thread synchronously without any
runtime suspension — pure JS-level blocking.

This package bridges DuckDB → Python → SAB → side-worker → real `fetch()`,
so DuckDB sees `https://...` and `s3://...` URLs as if they were files
with full range-request support. Parquet column projection pushdown works.

## Usage

```python
import duckdb
from pyodide_httpfs import PyodideHTTPFileSystem

con = duckdb.connect()
con.register_filesystem(PyodideHTTPFileSystem())

# Now any read_csv / read_parquet / etc. against http(s):// URLs
# routes through our FS:
con.execute("SELECT * FROM read_parquet('https://bucket.s3.amazonaws.com/file.parquet') LIMIT 10")
```

## Required host setup

The host JS environment (where Pyodide runs) MUST register a synthetic
Python module named `_pyodide_httpfs_host` exposing a synchronous
`fetch(url, initJson)` function before any user Python code runs:

```typescript
pyodide.registerJsModule("_pyodide_httpfs_host", {
  fetch: (url: string, initJson: string) => {
    const init = JSON.parse(initJson);
    return sabFetch(url, init);  // your SAB-backed sync fetch
  },
});
```

`sabFetch(url, init)` returns synchronously with shape:

```typescript
{
  status: number,
  headers: Record<string, string>,
  body: Uint8Array,
  error: string | null,
}
```

This indirection (registered module instead of `import js`) lets sandboxed
Python environments — like agent-os's Pi VM, which blocks `import js` and
`import pyodide_js` — still load the package.

Implementation: spawn a side worker that exposes `fetch()`. Main thread
writes request to a `SharedArrayBuffer`, calls `Atomics.wait`, side worker
fetches and writes response back to the SAB, calls `Atomics.notify`.

Reference implementation for agent-os is in
`packages/python/src/sab-fetch-bootstrap.ts`. For other hosts, follow the
same SAB layout:

| offset | type     | meaning                                |
|-------:|----------|----------------------------------------|
|      0 | `i32`    | state: 0=pending, 1=success, 2=error   |
|      4 | `i32`    | response status code                   |
|      8 | `i32`    | response body length in bytes          |
|     12 | `i32`    | response headers utf-8 length in bytes |
|     16 | bytes    | response body                          |
|  16+bl | bytes    | response headers (utf-8, "key: val\\n") |

The SAB must be at least `16 + max_body_size + max_headers_size` bytes.
Our default in agent-os is 64 MB, sufficient for parquet footers + a few
row-group column chunks.

## Limitations

- **Single-threaded fetch**: only one in-flight request at a time per
  filesystem instance. DuckDB issues range reads serially anyway, so this
  isn't a bottleneck for typical analytical queries.
- **No streaming**: each `_fetch_range` returns the full byte range as a
  single `bytes` object. For 100 MB+ ranges memory pressure adds up.
  DuckDB's parquet reader chunks reads naturally; CSV reads can be larger.
- **Auth via host fetch**: any auth (S3 SigV4, bearer tokens, etc.) must
  be applied by the host's `_sabFetch` implementation, not this package.

## Implementation notes

`PyodideHTTPFileSystem` is a thin `fsspec.AbstractFileSystem` subclass that
delegates all I/O to `_fetch_sync`, which calls `globalThis._sabFetch` via
Pyodide's `js` module. The fsspec `AbstractBufferedFile` superclass handles
buffering / seeking / partial reads — we only implement `_fetch_range`.

`modified()` and `created()` return current time because most HTTP servers
don't expose these for arbitrary URLs and DuckDB doesn't actually use them
for read queries. fsspec calls them defensively from `glob()` / `find()`
even for single-file paths, which is why we implement them at all.
