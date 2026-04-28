"""fsspec-compatible HTTPS/S3 filesystem for DuckDB-in-Pyodide.

Bridges DuckDB's filesystem callbacks (Python side) to a host-installed
SharedArrayBuffer + side-worker fetch primitive. Sidesteps Pyodide's
inability to suspend wasm side modules on JS Promises by using
`Atomics.wait` for synchronous blocking instead.

See README.md for the SAB layout the host must implement.
"""
from __future__ import annotations

import json
from datetime import datetime, timezone
from typing import Any

from fsspec.spec import AbstractBufferedFile, AbstractFileSystem


__all__ = ["PyodideHTTPFileSystem", "PyodideHTTPFile", "register_with_duckdb"]


def _fetch_sync(url: str, *, method: str = "GET",
                 range_: tuple[int, int] | None = None,
                 extra_headers: dict[str, str] | None = None
                 ) -> tuple[int, dict[str, str], bytes]:
    """Synchronous fetch via the host's SAB-backed _sabFetch primitive.

    Returns (status_code, headers, body_bytes). Raises IOError if the
    host hasn't installed _sabFetch or if the fetch failed.
    """
    # Lazy-import js so this module is importable outside Pyodide too —
    # useful for testing and tooling. Real calls only work in Pyodide.
    import js  # type: ignore[import-not-found]

    if not hasattr(js, "_sabFetch"):
        raise IOError(
            "host has not installed globalThis._sabFetch — see "
            "pyodide_httpfs README for the required SAB protocol"
        )

    headers: dict[str, str] = dict(extra_headers or {})
    if range_ is not None:
        start, end = range_
        # HTTP Range is inclusive on both ends; fsspec end is exclusive.
        headers["Range"] = f"bytes={start}-{end - 1}"

    init = {"method": method, "headers": headers}
    res = js._sabFetch(url, js.JSON.parse(json.dumps(init)))

    err = res.error if hasattr(res, "error") else None
    if err:
        raise IOError(f"_sabFetch({url}): {err}")

    status = int(res.status) if hasattr(res, "status") else 0
    response_headers: dict[str, str] = {}
    if hasattr(res, "headers") and res.headers:
        response_headers = res.headers.to_py()
    body: bytes = b""
    if hasattr(res, "body") and res.body:
        body = bytes(res.body.to_py())
    return status, response_headers, body


class PyodideHTTPFile(AbstractBufferedFile):
    """fsspec buffered file that fetches byte ranges via _sabFetch."""

    DEFAULT_BLOCK_SIZE = 5 * 1024 * 1024  # 5 MB read buffer

    def _fetch_range(self, start: int, end: int) -> bytes:
        status, _, body = _fetch_sync(self.path, method="GET",
                                       range_=(start, end))
        if status not in (200, 206):
            raise IOError(f"fetch {self.path} returned status {status}")
        return body


class PyodideHTTPFileSystem(AbstractFileSystem):
    """fsspec filesystem mapping http(s):// and s3:// to host-side fetch.

    For S3 URLs the host's _sabFetch implementation is responsible for
    SigV4 signing — the host typically receives an `s3://bucket/key`
    URL, looks up the bucket's signing key, and rewrites to a signed
    https:// URL before fetching.
    """

    protocol = ("http", "https", "s3")

    def _strip_protocol(self, path: str) -> str:
        # Default fsspec behavior strips the protocol; we want to keep
        # the full URL because the host fetch needs it intact.
        return path

    @classmethod
    def _strip_protocol_cls(cls, path: str) -> str:  # noqa: D401
        return path

    def _open(self, path: str, mode: str = "rb",
              block_size: int | None = None,
              autocommit: bool = True,
              cache_options: dict[str, Any] | None = None,
              **kwargs: Any) -> PyodideHTTPFile:
        if mode != "rb":
            raise NotImplementedError(
                f"PyodideHTTPFileSystem only supports rb mode, got {mode}"
            )
        size = self._size(path)
        return PyodideHTTPFile(
            self, path, mode,
            block_size or PyodideHTTPFile.DEFAULT_BLOCK_SIZE,
            size=size,
        )

    def _size(self, path: str) -> int:
        # HEAD then parse Content-Length. Some servers don't return it
        # for HEAD; fall back to a 1-byte range request and read the
        # total from Content-Range.
        status, headers, _ = _fetch_sync(path, method="HEAD")
        cl = headers.get("content-length")
        if cl:
            return int(cl)
        status, headers, _ = _fetch_sync(path, method="GET", range_=(0, 1))
        cr = headers.get("content-range")  # e.g. "bytes 0-0/1234"
        if cr and "/" in cr:
            tail = cr.rsplit("/", 1)[-1]
            if tail.isdigit():
                return int(tail)
        return 0

    def info(self, path: str, **kwargs: Any) -> dict[str, Any]:
        return {"name": path, "size": self._size(path), "type": "file"}

    def modified(self, path: str) -> datetime:
        # fsspec.glob() / find() call this defensively even for single-
        # file paths. Returning current time is harmless for read queries.
        return datetime.now(timezone.utc)

    def created(self, path: str) -> datetime:
        return datetime.now(timezone.utc)

    def cat_file(self, path: str, start: int | None = None,
                 end: int | None = None, **kwargs: Any) -> bytes:
        if start is None:
            start = 0
        if end is None:
            end = self._size(path)
        status, _, body = _fetch_sync(path, method="GET", range_=(start, end))
        if status not in (200, 206):
            raise IOError(f"cat_file {path} returned status {status}")
        return body

    def exists(self, path: str, **kwargs: Any) -> bool:
        try:
            status, _, _ = _fetch_sync(path, method="HEAD")
            return 200 <= status < 400
        except IOError:
            return False

    def ls(self, path: str, detail: bool = True, **kwargs: Any
           ) -> list[Any]:
        # HTTP(S) URLs aren't directories; ls() is a no-op except for
        # the canonical "the path itself" entry that fsspec sometimes
        # expects.
        if detail:
            return [self.info(path)]
        return [path]


def register_with_duckdb(con: Any) -> PyodideHTTPFileSystem:
    """Register PyodideHTTPFileSystem with a DuckDBPyConnection.

    Idempotent — calling twice is a no-op. Returns the FS instance so
    callers can stash it for diagnostics.
    """
    fs = PyodideHTTPFileSystem()
    con.register_filesystem(fs)
    return fs
