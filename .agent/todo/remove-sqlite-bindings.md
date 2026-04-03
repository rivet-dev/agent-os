# Remove sqlite-bindings.ts

The `packages/core/src/sqlite-bindings.ts` file provides SQLite database access inside the VM by proxying to host-side SQLite. The approach (temp files synced between host and VM) is fragile.

Consider replacing with a proper in-VM SQLite implementation or removing if no longer needed.

## Current state (2026-04-03)

The file was refactored in `fix/lazy-sqlite-bun-compat` to:
- **Lazy-load** the SQLite module on first `AgentOs.create()` instead of at import time (was crashing Bun).
- **Support both runtimes**: `node:sqlite` on Node.js, `bun:sqlite` on Bun via an adapter layer.
- **Promise-cached** module loading (no race conditions on concurrent calls).
- Pre-existing type errors were resolved by introducing internal `SqliteDatabase`/`SqliteStatement`/`SqliteModule` interfaces.

### Known adapter limitations (bun:sqlite)
- `setReadBigInts()` is a no-op — Bun uses `safeIntegers` at the database level, not per-statement.
- `setAllowBareNamedParameters()` / `setAllowUnknownNamedParameters()` are no-ops — Bun's `strict` mode covers similar ground at the database level.
- `columns()` returns `[{ name }]` only — Bun's `columnNames` string array doesn't include `column`, `table`, `database`, or `type` fields that node:sqlite provides.
- Constructor options aren't translated between runtimes (`readOnly` vs `readonly`).
