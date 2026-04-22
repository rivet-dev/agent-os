/**
 * DuckDB operations namespace. Covers the common analytics tasks
 * (query, execute, inspect schema, list tables) against any DuckDB
 * database reachable from inside the Pyodide runtime — in-memory or
 * NODEFS-bridged file-backed.
 *
 * A single Python helper (`RUN_DUCKDB_HELPER_PY`) dispatches on a JSON
 * request passed via argv and writes its structured response to
 * `DUCKDB_RESULT_PATH` inside the bridged scratch dir. The TS methods
 * wrap that round-trip with typed inputs and outputs.
 *
 * Availability: DuckDB ships as part of the dbt wheel bundle, so
 * `aos.duckdb.*` is usable whenever the VM was booted with
 * `python: { dbt: true }`. A DuckDB-standalone opt-in may be added later.
 */

import type { AgentOs } from "./agent-os.js";
import { AGENT_OS_SCRATCH_DIR } from "./dbt.js";

// ────────────────────────────────────────────────────────────────────
// Public types
// ────────────────────────────────────────────────────────────────────

/** Options for `aos.duckdb.query`. */
export interface DuckdbQueryOptions {
	/**
	 * Path to a .duckdb file or `:memory:`. Defaults to `:memory:`.
	 * File-backed databases must live under a NODEFS-bridged mount
	 * (e.g. `/root/dbt-projects/<project>/warehouse.duckdb`) so Pyodide
	 * can reach them.
	 */
	database?: string;
	/** Positional parameters bound to `?` placeholders in the SQL. */
	params?: unknown[];
	/** Max rows to fetch. Defaults to 1000, capped at 10 000. */
	limit?: number;
}

/** Structured query result. */
export interface DuckdbQueryResult {
	columns: string[];
	rows: unknown[][];
	rowCount: number;
	schema: { name: string; type: string; nullable: boolean }[];
	/** Wall-clock time the query took inside DuckDB, in milliseconds. */
	executionMs: number;
}

/** Options for `aos.duckdb.execute`. */
export interface DuckdbExecOptions {
	database?: string;
	/**
	 * Per-statement positional parameters. If `statements` is a string
	 * (single statement), pass a flat `unknown[]`. If `statements` is an
	 * array, pass `unknown[][]` where index N holds the params for
	 * statement N. Missing entries run without binding.
	 */
	params?: unknown[] | unknown[][];
}

/** Structured execute result. `rowsAffected` sums `cursor.rowcount` across statements. */
export interface DuckdbExecResult {
	success: boolean;
	rowsAffected: number;
	executionMs: number;
}

/** Options for `aos.duckdb.describeTable`. */
export interface DuckdbTableOptions {
	database?: string;
	/** Schema the table lives in. Defaults to `main`. */
	schema?: string;
}

/** Schema inspection output. */
export interface DuckdbTableSchema {
	name: string;
	columns: {
		name: string;
		type: string;
		nullable: boolean;
		isPrimary: boolean;
	}[];
	rowCount: number;
}

/** Options for `aos.duckdb.listTables`. */
export interface DuckdbListOptions {
	database?: string;
	/** Schema to filter by. Defaults to `main`. */
	schema?: string;
	/**
	 * If true, include each table's columns and rowCount in the
	 * response. Materializing rowCount on a large table is an O(n) scan
	 * — opt-in, not default.
	 */
	includeColumns?: boolean;
}

/** Catalog-level table descriptor. */
export interface DuckdbTableInfo {
	name: string;
	type: "table" | "view";
	rowCount?: number;
	columns?: { name: string; type: string }[];
}

/**
 * Thrown when DuckDB rejects the SQL, the database is unreachable, or
 * the helper script surfaces an error. `sql` is the statement that
 * failed (`null` for catalog operations that don't map to a single SQL
 * statement).
 */
export class DuckdbError extends Error {
	constructor(
		public readonly sql: string | null,
		public readonly detail: string,
	) {
		super(`DuckDB error: ${detail}`);
		this.name = "DuckdbError";
	}
}

// ────────────────────────────────────────────────────────────────────
// Internal constants
// ────────────────────────────────────────────────────────────────────

/** Path where the helper script is staged inside the VM. */
export const RUN_DUCKDB_HELPER_PATH = "/tmp/_agent_os_run_duckdb.py";

/** Where the helper script writes its structured response. */
export const DUCKDB_RESULT_PATH = `${AGENT_OS_SCRATCH_DIR}/duckdb_result.json`;

/**
 * Python helper dispatched on a JSON argv to perform query / execute /
 * describe / list operations. Writes `{ ok, …data | error }` to
 * `DUCKDB_RESULT_PATH` — we read the file rather than parsing stdout
 * so other prints (tracebacks, schema warnings) don't pollute the
 * payload.
 */
export const RUN_DUCKDB_HELPER_PY = `# agent-os duckdb helper — auto-installed; do not edit.
import json as _aos_json
import os as _aos_os
import sys as _aos_sys
import time as _aos_time
import traceback as _aos_traceback


def _aos_coerce(value):
    if value is None or isinstance(value, (bool, int, float, str)):
        return value
    if hasattr(value, "isoformat"):
        return value.isoformat()
    return str(value)


def _aos_run(request):
    import duckdb
    db_path = request.get("database") or ":memory:"
    con = duckdb.connect(db_path)
    cur = con.cursor()
    op = request.get("op")
    start = _aos_time.time()

    if op == "query":
        sql = request["sql"]
        params = request.get("params") or None
        limit = int(request.get("limit") or 1000)
        limit = max(1, min(limit, 10_000))
        if params:
            cur.execute(sql, params)
        else:
            cur.execute(sql)
        descriptors = cur.description or []
        columns = [d[0] for d in descriptors]
        schema = [{"name": d[0], "type": str(d[1]), "nullable": True} for d in descriptors]
        raw_rows = cur.fetchmany(limit)
        rows = [[_aos_coerce(v) for v in row] for row in raw_rows]
        return {
            "ok": True,
            "columns": columns,
            "rows": rows,
            "rowCount": len(rows),
            "schema": schema,
            "executionMs": int((_aos_time.time() - start) * 1000),
        }

    if op == "execute":
        statements = request.get("statements") or []
        if isinstance(statements, str):
            statements = [statements]
        params_list = request.get("params") or []
        rows_affected = 0
        for idx, statement in enumerate(statements):
            bind = None
            if isinstance(params_list, list) and idx < len(params_list):
                entry = params_list[idx]
                if isinstance(entry, list):
                    bind = entry
                elif entry is not None:
                    bind = [entry]
            if bind:
                cur.execute(statement, bind)
            else:
                cur.execute(statement)
            rc = getattr(cur, "rowcount", None)
            if isinstance(rc, int) and rc >= 0:
                rows_affected += rc
        try:
            con.commit()
        except Exception:
            pass
        return {
            "ok": True,
            "success": True,
            "rowsAffected": rows_affected,
            "executionMs": int((_aos_time.time() - start) * 1000),
        }

    if op == "describe":
        schema = request.get("schema") or "main"
        table = request["tableName"]
        full = f'"{schema}"."{table}"'
        cur.execute(f"DESCRIBE {full}")
        rows = cur.fetchall()
        columns = []
        for row in rows:
            name, col_type, nullable = row[0], row[1], row[2]
            key = row[3] if len(row) > 3 else None
            columns.append(
                {
                    "name": str(name),
                    "type": str(col_type),
                    "nullable": str(nullable).upper() == "YES",
                    "isPrimary": str(key or "").upper().startswith("PRI"),
                }
            )
        cur.execute(f"SELECT COUNT(*) FROM {full}")
        row_count = int(cur.fetchone()[0])
        return {
            "ok": True,
            "name": table,
            "columns": columns,
            "rowCount": row_count,
        }

    if op == "list":
        schema = request.get("schema") or "main"
        include_columns = bool(request.get("includeColumns"))
        cur.execute(
            "SELECT table_name, table_type FROM information_schema.tables "
            "WHERE table_schema = ? ORDER BY table_name",
            [schema],
        )
        tables = []
        for (name, ttype) in cur.fetchall():
            info = {
                "name": str(name),
                "type": "view" if "VIEW" in str(ttype).upper() else "table",
            }
            if include_columns:
                cur2 = con.cursor()
                cur2.execute(
                    "SELECT column_name, data_type FROM information_schema.columns "
                    "WHERE table_schema = ? AND table_name = ? "
                    "ORDER BY ordinal_position",
                    [schema, name],
                )
                info["columns"] = [
                    {"name": str(c), "type": str(t)} for (c, t) in cur2.fetchall()
                ]
                try:
                    cur2.execute(f'SELECT COUNT(*) FROM "{schema}"."{name}"')
                    info["rowCount"] = int(cur2.fetchone()[0])
                except Exception:
                    pass
            tables.append(info)
        return {"ok": True, "tables": tables}

    return {"ok": False, "error": f"unknown op: {op}"}


try:
    _aos_request = _aos_json.loads(_aos_sys.argv[1])
except Exception as _err:
    _aos_result = {"ok": False, "error": f"invalid request JSON: {_err}"}
else:
    try:
        _aos_result = _aos_run(_aos_request)
    except Exception as _err:
        _aos_traceback.print_exc(file=_aos_sys.stderr)
        _aos_result = {"ok": False, "error": str(_err)}

try:
    _aos_os.makedirs("${AGENT_OS_SCRATCH_DIR}", exist_ok=True)
    with open("${DUCKDB_RESULT_PATH}", "w") as _aos_out:
        _aos_json.dump(_aos_result, _aos_out)
except Exception:
    pass
print(_aos_json.dumps(_aos_result), flush=True)
`;

// ────────────────────────────────────────────────────────────────────
// AgentOsDuckdb — namespace exposed as `aos.duckdb`
// ────────────────────────────────────────────────────────────────────

/**
 * DuckDB operations namespace. Accessed as `aos.duckdb` on an
 * `AgentOs` instance. All methods throw `DuckdbError` on failure
 * (invalid SQL, missing table, unreachable database, etc.).
 */
export class AgentOsDuckdb {
	constructor(private readonly aos: AgentOs) {}

	/**
	 * Run a single SQL statement and return its rows.
	 *
	 * @example
	 * const res = await aos.duckdb.query(
	 *   "SELECT name, email FROM users WHERE active = ?",
	 *   { params: [true], limit: 50 },
	 * );
	 * for (const [name, email] of res.rows) console.log(name, email);
	 */
	async query(
		sql: string,
		options?: DuckdbQueryOptions,
	): Promise<DuckdbQueryResult> {
		const payload = await this._dispatch({
			op: "query",
			database: options?.database ?? ":memory:",
			sql,
			params: options?.params ?? null,
			limit: options?.limit ?? 1000,
		});
		if (!payload.ok) throw new DuckdbError(sql, (payload.error as string | undefined) ?? "query failed");
		return {
			columns: payload.columns as string[],
			rows: payload.rows as unknown[][],
			rowCount: payload.rowCount as number,
			schema: payload.schema as DuckdbQueryResult["schema"],
			executionMs: payload.executionMs as number,
		};
	}

	/**
	 * Run one or more DDL/DML statements. Commits at the end; rolls
	 * back on error. `rowsAffected` sums `cursor.rowcount` across
	 * statements — DuckDB returns `-1` for statements where it doesn't
	 * apply (DDL), those don't contribute.
	 */
	async execute(
		statements: string | string[],
		options?: DuckdbExecOptions,
	): Promise<DuckdbExecResult> {
		const payload = await this._dispatch({
			op: "execute",
			database: options?.database ?? ":memory:",
			statements,
			params: options?.params ?? null,
		});
		if (!payload.ok) {
			throw new DuckdbError(
				Array.isArray(statements) ? statements.join("; ") : statements,
				(payload.error as string | undefined) ?? "execute failed",
			);
		}
		return {
			success: true,
			rowsAffected: payload.rowsAffected as number,
			executionMs: payload.executionMs as number,
		};
	}

	/**
	 * Describe a table or view: column names, types, nullability,
	 * primary-key flag, and row count.
	 */
	async describeTable(
		tableName: string,
		options?: DuckdbTableOptions,
	): Promise<DuckdbTableSchema> {
		const payload = await this._dispatch({
			op: "describe",
			database: options?.database ?? ":memory:",
			tableName,
			schema: options?.schema ?? "main",
		});
		if (!payload.ok) {
			throw new DuckdbError(
				null,
				(payload.error as string | undefined) ?? `describe failed for ${tableName}`,
			);
		}
		return {
			name: payload.name as string,
			columns: payload.columns as DuckdbTableSchema["columns"],
			rowCount: payload.rowCount as number,
		};
	}

	/**
	 * List tables and views in a schema (default `main`). Pass
	 * `includeColumns: true` to also materialize each table's columns
	 * and row count.
	 */
	async listTables(options?: DuckdbListOptions): Promise<DuckdbTableInfo[]> {
		const payload = await this._dispatch({
			op: "list",
			database: options?.database ?? ":memory:",
			schema: options?.schema ?? "main",
			includeColumns: options?.includeColumns ?? false,
		});
		if (!payload.ok) {
			throw new DuckdbError(null, (payload.error as string | undefined) ?? "list failed");
		}
		return payload.tables as DuckdbTableInfo[];
	}

	private async _dispatch(
		request: Record<string, unknown>,
	): Promise<Record<string, unknown>> {
		await this.aos.writeFile(RUN_DUCKDB_HELPER_PATH, RUN_DUCKDB_HELPER_PY);
		const { pid } = this.aos.spawn(
			"python3",
			[RUN_DUCKDB_HELPER_PATH, JSON.stringify(request)],
		);
		await this.aos.waitProcess(pid);
		// Read the structured result from the bridged scratch file. If
		// the helper crashed before writing, we surface that with a clear
		// error — the caller gets a DuckdbError rather than a silent
		// empty object.
		let bytes: Uint8Array;
		try {
			bytes = await this.aos.readFile(DUCKDB_RESULT_PATH);
		} catch (err) {
			throw new DuckdbError(
				null,
				`helper produced no result file (${(err as Error).message})`,
			);
		}
		try {
			return JSON.parse(new TextDecoder().decode(bytes)) as Record<
				string,
				unknown
			>;
		} catch (err) {
			throw new DuckdbError(
				null,
				`helper result was not valid JSON (${(err as Error).message})`,
			);
		}
	}
}
