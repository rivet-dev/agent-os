"""dbt-duckdb plugin that auto-registers PyodideHTTPFileSystem.

Reference in dbt's profiles.yml:

```yaml
warehouse:
  target: dev
  outputs:
    dev:
      type: duckdb
      path: '/tmp/warehouse.duckdb'
      plugins:
        - module: pyodide_httpfs.dbt_plugin
```

After this, models can reference `s3://...` and `https://...` URIs in
sources.yml directly. dbt-duckdb calls `Plugin.configure_connection(conn)`
on every connection it creates, which registers our fsspec FS so DuckDB
sees those URLs as files.

Requires the host to have installed the `_pyodide_httpfs_host` JS module
before any dbt code runs (agent-os does this in driver.ts via the SAB
fetch bootstrap; see pyodide_httpfs README for the protocol).
"""
from __future__ import annotations

from typing import Any

from dbt.adapters.duckdb.plugins import BasePlugin

from . import register_with_duckdb


class Plugin(BasePlugin):
    """dbt-duckdb plugin entry point. Idempotent across cursor copies."""

    def configure_connection(self, conn: Any) -> None:  # type: ignore[override]
        register_with_duckdb(conn)

    def configure_cursor(self, cursor: Any) -> None:  # type: ignore[override]
        # Cursors are copies of the parent connection — the registered
        # filesystem propagates automatically. No-op kept for clarity.
        pass
