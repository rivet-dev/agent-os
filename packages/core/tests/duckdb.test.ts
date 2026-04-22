/**
 * SDK-level tests for the `aos.duckdb` namespace against a mix of
 * in-memory and NODEFS-bridged file-backed databases.
 *
 * Skip-gated on the vendored DuckDB wheel being present.
 */
import { existsSync, readdirSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { allowAll } from "@secure-exec/core";
import { describe, expect, it } from "vitest";
import { AgentOs, DuckdbError } from "../src/index.js";

const __dirname = resolve(fileURLToPath(import.meta.url), "..");
const wheelsHostDir = resolve(
	__dirname,
	"../../../registry/software/python-wheels/wheels",
);

function hasWheelSet(): boolean {
	if (!existsSync(wheelsHostDir)) return false;
	const wheels = readdirSync(wheelsHostDir).filter((f) => f.endsWith(".whl"));
	return wheels.some((w) => w.startsWith("duckdb-"));
}

const READY = hasWheelSet();

describe.skipIf(!READY)("aos.duckdb.query", () => {
	it(
		"returns typed rows, columns, schema, and executionMs",
		{ timeout: 60_000 },
		async () => {
			const aos = await AgentOs.create({
				permissions: allowAll,
				python: { dbt: true },
			});
			try {
				const res = await aos.duckdb.query(
					"SELECT 1 AS id, 'hello' AS name UNION ALL SELECT 2, 'world' ORDER BY id",
				);
				expect(res.columns).toEqual(["id", "name"]);
				expect(res.rows).toEqual([
					[1, "hello"],
					[2, "world"],
				]);
				expect(res.rowCount).toBe(2);
				expect(res.schema).toHaveLength(2);
				expect(res.executionMs).toBeGreaterThanOrEqual(0);
			} finally {
				await aos.dispose();
			}
		},
	);

	it(
		"honors limit and positional params",
		{ timeout: 60_000 },
		async () => {
			const aos = await AgentOs.create({
				permissions: allowAll,
				python: { dbt: true },
			});
			try {
				const res = await aos.duckdb.query(
					"SELECT value FROM range(?, ?) AS t(value)",
					{ params: [1, 100], limit: 5 },
				);
				expect(res.rows).toHaveLength(5);
				expect(res.rows[0]).toEqual([1]);
			} finally {
				await aos.dispose();
			}
		},
	);

	it(
		"throws DuckdbError on invalid SQL",
		{ timeout: 60_000 },
		async () => {
			const aos = await AgentOs.create({
				permissions: allowAll,
				python: { dbt: true },
			});
			try {
				await expect(
					aos.duckdb.query("SELECT * FROM definitely_not_a_table"),
				).rejects.toBeInstanceOf(DuckdbError);
			} finally {
				await aos.dispose();
			}
		},
	);
});

describe.skipIf(!READY)("aos.duckdb.execute + describeTable + listTables", () => {
	it(
		"execute creates tables; describeTable + listTables reflect them",
		{ timeout: 120_000 },
		async () => {
			const aos = await AgentOs.create({
				permissions: allowAll,
				python: { dbt: true },
			});
			const dbPath = "/root/.dbt/.aos/duckdb_catalog.duckdb";
			try {
				const exec = await aos.duckdb.execute(
					[
						"CREATE TABLE widgets (id INTEGER PRIMARY KEY, name TEXT NOT NULL, notes TEXT)",
						"INSERT INTO widgets VALUES (1, 'sprocket', NULL)",
						"INSERT INTO widgets VALUES (2, 'gear', 'spinning')",
						"CREATE VIEW widgets_v AS SELECT id, name FROM widgets",
					],
					{ database: dbPath },
				);
				expect(exec.success).toBe(true);
				expect(exec.executionMs).toBeGreaterThanOrEqual(0);

				const schema = await aos.duckdb.describeTable("widgets", {
					database: dbPath,
				});
				expect(schema.name).toBe("widgets");
				expect(schema.rowCount).toBe(2);
				const colNames = schema.columns.map((c) => c.name).sort();
				expect(colNames).toEqual(["id", "name", "notes"]);
				const nameCol = schema.columns.find((c) => c.name === "name");
				expect(nameCol!.nullable).toBe(false);

				const tables = await aos.duckdb.listTables({
					database: dbPath,
					includeColumns: true,
				});
				const tableMap = new Map(tables.map((t) => [t.name, t]));
				expect(tableMap.get("widgets")?.type).toBe("table");
				expect(tableMap.get("widgets")?.columns).toBeDefined();
				expect(tableMap.get("widgets_v")?.type).toBe("view");
			} finally {
				await aos.dispose();
			}
		},
	);

	it(
		"describeTable throws DuckdbError for missing tables",
		{ timeout: 60_000 },
		async () => {
			const aos = await AgentOs.create({
				permissions: allowAll,
				python: { dbt: true },
			});
			try {
				await expect(
					aos.duckdb.describeTable("does_not_exist"),
				).rejects.toBeInstanceOf(DuckdbError);
			} finally {
				await aos.dispose();
			}
		},
	);
});
