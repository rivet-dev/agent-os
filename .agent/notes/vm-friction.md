# VM Friction Log

Tracks behaviors in the agent-os VM that differ from a standard POSIX/Node.js system.

---

## `node:sqlite` not available on Bun

**Deviation**: `node:sqlite` is a Node.js-only experimental built-in (requires Node >= 22.5.0). Bun provides `bun:sqlite` instead with a different API surface.

**Root cause**: The VM's SQLite bindings proxy host-side SQLite into the VM. The original implementation hard-coded `require("node:sqlite")` at the module top level, which crashed on any runtime without it.

**Fix**: `sqlite-bindings.ts` now lazy-loads the SQLite module and auto-selects `node:sqlite` or `bun:sqlite` based on runtime detection (`process.versions.bun`). A `BunStatementAdapter`/`BunDatabaseAdapter` layer normalizes the bun:sqlite API to match the node:sqlite shape.

**Remaining differences on Bun**:
- `setReadBigInts()` is a no-op (Bun uses `safeIntegers` at DB level).
- `columns()` returns `[{ name }]` only (no `table`/`type`/`database` fields).
- Database constructor options aren't translated (`readOnly` vs `readonly`).
- `get()` return value normalized: Bun returns `null` for no rows, node:sqlite returns `undefined`. Adapter normalizes to `undefined`.

---

## SQLite bindings use temp-file sync

**Deviation**: When VM code opens a file-backed SQLite database, the kernel VFS file is copied to a host temp directory, opened with host SQLite, and synced back on mutations. This means the database is not truly "in the VM" -- it lives on the host filesystem temporarily.

**Root cause**: The secure-exec kernel's VFS doesn't support SQLite's file locking and mmap requirements natively. The bindings work around this by proxying through host SQLite.

**Fix exists**: No. This is a fundamental architecture limitation. A proper fix would be an in-VM SQLite compiled to WASM (the `registry/software/sqlite3` package exists but is for the CLI tool, not the library).
