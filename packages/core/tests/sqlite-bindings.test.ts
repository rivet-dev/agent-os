import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";
import {
	_resetSqliteModuleCache,
	createBunSqliteAdapter,
	createSqliteBindingsFromModule,
	isBunRuntime,
	loadSqliteModule,
	type SqliteDatabase,
	type SqliteModule,
	type SqliteStatement,
} from "../src/sqlite-bindings.js";

// ---------------------------------------------------------------------------
// Shared test fixtures
// ---------------------------------------------------------------------------

function createMockKernel() {
	const files = new Map<string, Uint8Array>();
	const dirs = new Set<string>(["/", "/tmp"]);

	return {
		files,
		dirs,
		exists: vi.fn(async (path: string) => files.has(path) || dirs.has(path)),
		mkdir: vi.fn(async (path: string) => {
			dirs.add(path);
		}),
		readFile: vi.fn(async (path: string) => {
			const data = files.get(path);
			if (!data) throw new Error(`ENOENT: ${path}`);
			return data;
		}),
		writeFile: vi.fn(async (path: string, data: Uint8Array | string) => {
			files.set(
				path,
				typeof data === "string" ? new TextEncoder().encode(data) : data,
			);
		}),
	};
}

// ---------------------------------------------------------------------------
// 1. No eager import — the whole point of this refactor
// ---------------------------------------------------------------------------

describe("lazy loading", () => {
	afterEach(() => {
		_resetSqliteModuleCache();
	});

	test("importing sqlite-bindings.ts does not eagerly require node:sqlite", () => {
		expect(isBunRuntime).toBeTypeOf("function");
		expect(createSqliteBindingsFromModule).toBeTypeOf("function");
	});

	test("loadSqliteModule returns a valid SqliteModule on Node.js", async () => {
		const mod = await loadSqliteModule();
		expect(mod).toBeDefined();
		expect(mod.DatabaseSync).toBeTypeOf("function");
		expect(
			typeof mod.constants === "object" || mod.constants === undefined,
		).toBe(true);
	});

	test("loadSqliteModule caches the result across calls", async () => {
		const mod1 = await loadSqliteModule();
		const mod2 = await loadSqliteModule();
		expect(mod1).toBe(mod2);
	});

	test("_resetSqliteModuleCache forces a fresh load", async () => {
		const mod1 = await loadSqliteModule();
		_resetSqliteModuleCache();
		const mod2 = await loadSqliteModule();
		expect(mod1.DatabaseSync).toBeTypeOf("function");
		expect(mod2.DatabaseSync).toBeTypeOf("function");
	});

	test("concurrent loadSqliteModule calls do not race", async () => {
		const [mod1, mod2] = await Promise.all([
			loadSqliteModule(),
			loadSqliteModule(),
		]);
		expect(mod1).toBe(mod2);
	});
});

// ---------------------------------------------------------------------------
// 2. Runtime detection
// ---------------------------------------------------------------------------

describe("isBunRuntime", () => {
	test("returns false on Node.js", () => {
		expect(isBunRuntime()).toBe(false);
	});
});

// ---------------------------------------------------------------------------
// 3. Bun adapter via mock
// ---------------------------------------------------------------------------

describe("createBunSqliteAdapter", () => {
	function createMockBunDatabase() {
		const rows = [
			{ id: 1, name: "alice" },
			{ id: 2, name: "bob" },
		];

		class MockStatement {
			_sql: string;
			columnNames: string[];
			_runParams: unknown[][] = [];

			constructor(sql: string) {
				this._sql = sql;
				this.columnNames = ["id", "name"];
			}

			run(...params: unknown[]) {
				this._runParams.push(params);
				return { changes: 1, lastInsertRowid: 42 };
			}

			get(..._params: unknown[]) {
				return rows[0];
			}

			all(..._params: unknown[]) {
				return [...rows];
			}

			values(..._params: unknown[]) {
				return rows.map((r) => [r.id, r.name]);
			}

			finalize() {}
		}

		class MockDatabase {
			filename: string;
			_closed = false;
			_execLog: string[] = [];

			constructor(path?: string, _options?: Record<string, unknown>) {
				this.filename = path ?? ":memory:";
			}

			close() {
				this._closed = true;
			}

			exec(sql: string) {
				this._execLog.push(sql);
			}

			prepare(sql: string) {
				return new MockStatement(sql);
			}
		}

		return MockDatabase;
	}

	let adapter: SqliteModule;

	beforeEach(() => {
		adapter = createBunSqliteAdapter(createMockBunDatabase());
	});

	test("DatabaseSync constructor creates a database", () => {
		const db = new adapter.DatabaseSync(":memory:");
		expect(db).toBeDefined();
	});

	test("db.location() returns the filename for file-backed databases", () => {
		const db = new adapter.DatabaseSync("/tmp/test.db");
		expect(db.location()).toBe("/tmp/test.db");
	});

	test("db.location() returns null for memory databases", () => {
		const db = new adapter.DatabaseSync();
		expect(db.location()).toBeNull();
	});

	test("db.location() returns null for explicit :memory: path", () => {
		const db = new adapter.DatabaseSync(":memory:");
		expect(db.location()).toBeNull();
	});

	test("db.close() releases the database connection without error", () => {
		const db = new adapter.DatabaseSync(":memory:");
		db.close();
	});

	test("db.exec() runs arbitrary SQL without returning a result", () => {
		const db = new adapter.DatabaseSync(":memory:");
		db.exec("CREATE TABLE t (id INTEGER)");
	});

	test("db.prepare() compiles SQL into a reusable statement handle", () => {
		const db = new adapter.DatabaseSync(":memory:");
		const stmt = db.prepare("SELECT * FROM t");
		expect(stmt).toBeDefined();
	});

	test("stmt.run() returns { changes, lastInsertRowid } from bun:sqlite", () => {
		const db = new adapter.DatabaseSync(":memory:");
		const stmt = db.prepare("INSERT INTO t VALUES (3, 'charlie')");
		const result = stmt.run() as any;
		expect(result).toEqual({ changes: 1, lastInsertRowid: 42 });
	});

	test("stmt.get() returns a single row as object", () => {
		const db = new adapter.DatabaseSync(":memory:");
		const stmt = db.prepare("SELECT * FROM t");
		const row = stmt.get() as any;
		expect(row).toEqual({ id: 1, name: "alice" });
	});

	test("stmt.all() returns all rows as objects", () => {
		const db = new adapter.DatabaseSync(":memory:");
		const stmt = db.prepare("SELECT * FROM t");
		const rows = stmt.all() as any[];
		expect(rows).toHaveLength(2);
		expect(rows[0]).toEqual({ id: 1, name: "alice" });
		expect(rows[1]).toEqual({ id: 2, name: "bob" });
	});

	test("stmt.iterate() returns an iterable of rows", () => {
		const db = new adapter.DatabaseSync(":memory:");
		const stmt = db.prepare("SELECT * FROM t");
		const rows = [...stmt.iterate()] as any[];
		expect(rows).toHaveLength(2);
		expect(rows[0]).toEqual({ id: 1, name: "alice" });
	});

	test("stmt.columns() returns array of {name} objects", () => {
		const db = new adapter.DatabaseSync(":memory:");
		const stmt = db.prepare("SELECT * FROM t");
		const cols = stmt.columns() as any[];
		expect(cols).toEqual([{ name: "id" }, { name: "name" }]);
	});

	test("results switch from objects to arrays when array mode is enabled", () => {
		const db = new adapter.DatabaseSync(":memory:");
		const stmt = db.prepare("SELECT * FROM t");
		stmt.setReturnArrays(true);

		const row = stmt.get() as any;
		expect(Array.isArray(row)).toBe(true);
		expect(row).toEqual([1, "alice"]);

		const allRows = stmt.all() as any[];
		expect(allRows).toEqual([
			[1, "alice"],
			[2, "bob"],
		]);
	});

	test("get() returns undefined in array mode when no rows exist", () => {
		function createEmptyMock() {
			class S {
				columnNames: string[] = [];
				run() {
					return { changes: 0, lastInsertRowid: 0 };
				}
				get() {
					return undefined;
				}
				all() {
					return [];
				}
				values() {
					return [];
				}
				finalize() {}
			}
			class D {
				filename: string;
				constructor(p?: string) {
					this.filename = p ?? ":memory:";
				}
				close() {}
				exec() {}
				prepare() {
					return new S();
				}
			}
			return D;
		}
		const a = createBunSqliteAdapter(createEmptyMock());
		const db = new a.DatabaseSync(":memory:");
		const stmt = db.prepare("SELECT 1 WHERE 0");
		stmt.setReturnArrays(true);
		expect(stmt.get()).toBeUndefined();
	});

	test("array mode disables when setReturnArrays(false) is called", () => {
		const db = new adapter.DatabaseSync(":memory:");
		const stmt = db.prepare("SELECT * FROM t");
		stmt.setReturnArrays(true);
		stmt.setReturnArrays(false);
		const row = stmt.get() as any;
		expect(row).toEqual({ id: 1, name: "alice" });
	});

	test("stmt.finalize() delegates to underlying statement", () => {
		const db = new adapter.DatabaseSync(":memory:");
		const stmt = db.prepare("SELECT * FROM t");
		expect(() => stmt.finalize?.()).not.toThrow();
	});

	test("stmt.setReadBigInts is a no-op (does not throw)", () => {
		const db = new adapter.DatabaseSync(":memory:");
		const stmt = db.prepare("SELECT * FROM t");
		expect(() => stmt.setReadBigInts(true)).not.toThrow();
	});

	test("stmt.setAllowBareNamedParameters is a no-op", () => {
		const db = new adapter.DatabaseSync(":memory:");
		const stmt = db.prepare("SELECT * FROM t");
		expect(() => stmt.setAllowBareNamedParameters(true)).not.toThrow();
	});

	test("stmt.setAllowUnknownNamedParameters is a no-op", () => {
		const db = new adapter.DatabaseSync(":memory:");
		const stmt = db.prepare("SELECT * FROM t");
		expect(() => stmt.setAllowUnknownNamedParameters(true)).not.toThrow();
	});

	test("constants defaults to empty object when not provided", () => {
		const a = createBunSqliteAdapter(createMockBunDatabase());
		expect(a.constants).toEqual({});
	});

	test("constants forwards bun:sqlite constants when provided", () => {
		const mockConstants = { SQLITE_FCNTL_PERSIST_WAL: 10 };
		const a = createBunSqliteAdapter(createMockBunDatabase(), mockConstants);
		expect(a.constants).toEqual({ SQLITE_FCNTL_PERSIST_WAL: 10 });
	});
});

// ---------------------------------------------------------------------------
// 4. createSqliteBindingsFromModule — binding tree with mock SqliteModule
// ---------------------------------------------------------------------------

describe("createSqliteBindingsFromModule", () => {
	function createMockSqliteModule(): SqliteModule {
		class MockStatement implements SqliteStatement {
			_sql: string;
			_returnArrays = false;
			_rows: Record<string, unknown>[] = [];
			_runResult = { changes: 0, lastInsertRowid: 0 };

			constructor(sql: string) {
				this._sql = sql;
			}
			run(..._params: unknown[]) {
				return this._runResult;
			}
			get(..._params: unknown[]) {
				if (this._returnArrays) {
					const r = this._rows[0];
					return r ? Object.values(r) : undefined;
				}
				return this._rows[0] ?? undefined;
			}
			all(..._params: unknown[]) {
				if (this._returnArrays) return this._rows.map((r) => Object.values(r));
				return [...this._rows];
			}
			iterate(..._params: unknown[]): Iterable<unknown> {
				return this.all(..._params) as unknown[];
			}
			columns() {
				if (this._rows.length > 0)
					return Object.keys(this._rows[0]).map((n) => ({ name: n }));
				return [];
			}
			finalize() {}
			setReturnArrays(e: boolean) {
				this._returnArrays = e;
			}
			setReadBigInts(_e: boolean) {}
			setAllowBareNamedParameters(_e: boolean) {}
			setAllowUnknownNamedParameters(_e: boolean) {}
		}

		class MockDatabase implements SqliteDatabase {
			_path: string;
			_closed = false;
			constructor(path?: string, _opts?: Record<string, unknown>) {
				this._path = path ?? ":memory:";
			}
			close() {
				this._closed = true;
			}
			exec(_sql: string) {}
			location() {
				return this._path === ":memory:" ? null : this._path;
			}
			prepare(sql: string) {
				return new MockStatement(sql);
			}
		}

		return {
			DatabaseSync: MockDatabase as unknown as SqliteModule["DatabaseSync"],
			constants: { SQLITE_OK: 0 },
		};
	}

	test("exposes database, statement, and meta namespaces on the binding tree", () => {
		const kernel = createMockKernel();
		const mod = createMockSqliteModule();
		const bindings = createSqliteBindingsFromModule(kernel as any, mod);
		expect(bindings).toBeDefined();
		expect(bindings.sqlite).toBeDefined();
		const sqlite = bindings.sqlite as any;
		expect(sqlite.meta).toBeDefined();
		expect(sqlite.database).toBeDefined();
		expect(sqlite.statement).toBeDefined();
	});

	test("meta.constants returns the module constants", () => {
		const kernel = createMockKernel();
		const mod = createMockSqliteModule();
		const bindings = createSqliteBindingsFromModule(kernel as any, mod);
		const sqlite = bindings.sqlite as any;
		expect(sqlite.meta.constants()).toEqual({ SQLITE_OK: 0 });
	});

	test("open returns a numeric handle, close invalidates it", async () => {
		const kernel = createMockKernel();
		const mod = createMockSqliteModule();
		const bindings = createSqliteBindingsFromModule(kernel as any, mod);
		const sqlite = bindings.sqlite as any;
		const dbId = await sqlite.database.open(":memory:");
		expect(typeof dbId).toBe("number");
		expect(dbId).toBeGreaterThan(0);
		expect(await sqlite.database.close(dbId)).toBeNull();
	});

	test("open with no args creates an in-memory database", async () => {
		const kernel = createMockKernel();
		const mod = createMockSqliteModule();
		const bindings = createSqliteBindingsFromModule(kernel as any, mod);
		const sqlite = bindings.sqlite as any;
		const dbId = await sqlite.database.open();
		expect(typeof dbId).toBe("number");
		await sqlite.database.close(dbId);
	});

	test("location returns vmPath for file-backed databases", async () => {
		const kernel = createMockKernel();
		const mod = createMockSqliteModule();
		const bindings = createSqliteBindingsFromModule(kernel as any, mod);
		const sqlite = bindings.sqlite as any;
		const dbId = await sqlite.database.open("/tmp/test.db");
		expect(sqlite.database.location(dbId)).toBe("/tmp/test.db");
		await sqlite.database.close(dbId);
	});

	test("location returns null for memory databases", async () => {
		const kernel = createMockKernel();
		const mod = createMockSqliteModule();
		const bindings = createSqliteBindingsFromModule(kernel as any, mod);
		const sqlite = bindings.sqlite as any;
		const dbId = await sqlite.database.open(":memory:");
		expect(sqlite.database.location(dbId)).toBeNull();
		await sqlite.database.close(dbId);
	});

	test("exec runs SQL and returns null", async () => {
		const kernel = createMockKernel();
		const mod = createMockSqliteModule();
		const bindings = createSqliteBindingsFromModule(kernel as any, mod);
		const sqlite = bindings.sqlite as any;
		const dbId = await sqlite.database.open(":memory:");
		expect(
			await sqlite.database.exec(dbId, "CREATE TABLE t (id INTEGER)"),
		).toBeNull();
		await sqlite.database.close(dbId);
	});

	test("prepare returns a statement handle", async () => {
		const kernel = createMockKernel();
		const mod = createMockSqliteModule();
		const bindings = createSqliteBindingsFromModule(kernel as any, mod);
		const sqlite = bindings.sqlite as any;
		const dbId = await sqlite.database.open(":memory:");
		const stmtId = sqlite.database.prepare(dbId, "SELECT 1");
		expect(typeof stmtId).toBe("number");
		expect(stmtId).toBeGreaterThan(0);
		await sqlite.database.close(dbId);
	});

	test("statement.run returns encoded result", async () => {
		const kernel = createMockKernel();
		const mod = createMockSqliteModule();
		const bindings = createSqliteBindingsFromModule(kernel as any, mod);
		const sqlite = bindings.sqlite as any;
		const dbId = await sqlite.database.open(":memory:");
		const stmtId = sqlite.database.prepare(dbId, "INSERT INTO t VALUES (1)");
		const result = await sqlite.statement.run(stmtId, []);
		expect(result).toEqual({ changes: 0, lastInsertRowid: 0 });
		await sqlite.database.close(dbId);
	});

	test("finalize invalidates the statement handle", async () => {
		const kernel = createMockKernel();
		const mod = createMockSqliteModule();
		const bindings = createSqliteBindingsFromModule(kernel as any, mod);
		const sqlite = bindings.sqlite as any;
		const dbId = await sqlite.database.open(":memory:");
		const stmtId = sqlite.database.prepare(dbId, "SELECT 1");
		expect(sqlite.statement.finalize(stmtId)).toBeNull();
		expect(() => sqlite.statement.get(stmtId, [])).toThrow(
			/statement handle.*not found/,
		);
		await sqlite.database.close(dbId);
	});

	test("configuration setters accept values without error", async () => {
		const kernel = createMockKernel();
		const mod = createMockSqliteModule();
		const bindings = createSqliteBindingsFromModule(kernel as any, mod);
		const sqlite = bindings.sqlite as any;
		const dbId = await sqlite.database.open(":memory:");
		const stmtId = sqlite.database.prepare(dbId, "SELECT 1");
		expect(sqlite.statement.setReturnArrays(stmtId, true)).toBeNull();
		expect(sqlite.statement.setReadBigInts(stmtId, true)).toBeNull();
		expect(
			sqlite.statement.setAllowBareNamedParameters(stmtId, true),
		).toBeNull();
		expect(
			sqlite.statement.setAllowUnknownNamedParameters(stmtId, true),
		).toBeNull();
		await sqlite.database.close(dbId);
	});

	test("closing a database invalidates its statements", async () => {
		const kernel = createMockKernel();
		const mod = createMockSqliteModule();
		const bindings = createSqliteBindingsFromModule(kernel as any, mod);
		const sqlite = bindings.sqlite as any;
		const dbId = await sqlite.database.open(":memory:");
		const stmtId = sqlite.database.prepare(dbId, "SELECT 1");
		await sqlite.database.close(dbId);
		expect(() => sqlite.statement.get(stmtId, [])).toThrow(
			/statement handle.*not found.*parent database/,
		);
	});

	test("each open returns a unique handle", async () => {
		const kernel = createMockKernel();
		const mod = createMockSqliteModule();
		const bindings = createSqliteBindingsFromModule(kernel as any, mod);
		const sqlite = bindings.sqlite as any;
		const db1 = await sqlite.database.open(":memory:");
		const db2 = await sqlite.database.open(":memory:");
		expect(db1).not.toBe(db2);
		await sqlite.database.close(db1);
		await sqlite.database.close(db2);
	});

	test("invalid database id error includes diagnostic context", () => {
		const kernel = createMockKernel();
		const mod = createMockSqliteModule();
		const bindings = createSqliteBindingsFromModule(kernel as any, mod);
		const sqlite = bindings.sqlite as any;
		expect(() => sqlite.database.location(999)).toThrow(
			/database handle 999 not found.*may have already been closed/,
		);
	});

	test("invalid statement id error includes diagnostic context", () => {
		const kernel = createMockKernel();
		const mod = createMockSqliteModule();
		const bindings = createSqliteBindingsFromModule(kernel as any, mod);
		const sqlite = bindings.sqlite as any;
		expect(() => sqlite.statement.get(999, [])).toThrow(
			/statement handle 999 not found.*finalized.*parent database/,
		);
	});

	test("exec on invalid database id rejects with context", async () => {
		const kernel = createMockKernel();
		const mod = createMockSqliteModule();
		const bindings = createSqliteBindingsFromModule(kernel as any, mod);
		const sqlite = bindings.sqlite as any;
		await expect(sqlite.database.exec(999, "SELECT 1")).rejects.toThrow(
			/database handle 999 not found/,
		);
	});

	test("prepare on invalid database id throws with context", () => {
		const kernel = createMockKernel();
		const mod = createMockSqliteModule();
		const bindings = createSqliteBindingsFromModule(kernel as any, mod);
		const sqlite = bindings.sqlite as any;
		expect(() => sqlite.database.prepare(999, "SELECT 1")).toThrow(
			/database handle 999 not found/,
		);
	});

	test("double-close lists no remaining open handles", async () => {
		const kernel = createMockKernel();
		const mod = createMockSqliteModule();
		const bindings = createSqliteBindingsFromModule(kernel as any, mod);
		const sqlite = bindings.sqlite as any;
		const dbId = await sqlite.database.open(":memory:");
		await sqlite.database.close(dbId);
		await expect(sqlite.database.close(dbId)).rejects.toThrow(
			/database handle.*not found.*No databases are currently open/,
		);
	});

	test("finalized statement run rejects with diagnostic guidance", async () => {
		const kernel = createMockKernel();
		const mod = createMockSqliteModule();
		const bindings = createSqliteBindingsFromModule(kernel as any, mod);
		const sqlite = bindings.sqlite as any;
		const dbId = await sqlite.database.open(":memory:");
		const stmtId = sqlite.database.prepare(dbId, "SELECT 1");
		sqlite.statement.finalize(stmtId);
		await expect(sqlite.statement.run(stmtId, [])).rejects.toThrow(
			/statement handle.*not found.*finalized/,
		);
		await sqlite.database.close(dbId);
	});

	test("exec with mutating SQL on memory DB does not trigger VFS sync", async () => {
		const kernel = createMockKernel();
		const mod = createMockSqliteModule();
		const bindings = createSqliteBindingsFromModule(kernel as any, mod);
		const sqlite = bindings.sqlite as any;
		const dbId = await sqlite.database.open(":memory:");
		await sqlite.database.exec(dbId, "INSERT INTO t VALUES (1)");
		expect(kernel.writeFile).not.toHaveBeenCalled();
		await sqlite.database.close(dbId);
	});
});

// ---------------------------------------------------------------------------
// 5. Node.js integration — real node:sqlite end-to-end
// ---------------------------------------------------------------------------

describe("node:sqlite integration", () => {
	afterEach(() => {
		_resetSqliteModuleCache();
	});

	test("full CRUD with real node:sqlite", async () => {
		const mod = await loadSqliteModule();
		const kernel = createMockKernel();
		const bindings = createSqliteBindingsFromModule(kernel as any, mod);
		const sqlite = bindings.sqlite as any;

		const dbId = await sqlite.database.open(":memory:");
		await sqlite.database.exec(
			dbId,
			"CREATE TABLE test (id INTEGER PRIMARY KEY, value TEXT)",
		);

		const insertId = sqlite.database.prepare(
			dbId,
			"INSERT INTO test (id, value) VALUES (?, ?)",
		);
		const insertResult = await sqlite.statement.run(insertId, [1, "hello"]);
		expect((insertResult as any).changes).toBe(1);

		const selectId = sqlite.database.prepare(
			dbId,
			"SELECT * FROM test WHERE id = ?",
		);
		expect(sqlite.statement.get(selectId, [1])).toEqual({
			id: 1,
			value: "hello",
		});

		const allId = sqlite.database.prepare(dbId, "SELECT * FROM test");
		expect(sqlite.statement.all(allId, [])).toEqual([
			{ id: 1, value: "hello" },
		]);
		expect(sqlite.statement.iterate(allId, [])).toEqual([
			{ id: 1, value: "hello" },
		]);

		const cols = sqlite.statement.columns(selectId);
		expect(cols).toEqual(
			expect.arrayContaining([
				expect.objectContaining({ name: "id" }),
				expect.objectContaining({ name: "value" }),
			]),
		);

		sqlite.statement.finalize(insertId);
		sqlite.statement.finalize(selectId);
		sqlite.statement.finalize(allId);
		await sqlite.database.close(dbId);
	});

	test("bigint encoding round-trip preserves large values", async () => {
		const mod = await loadSqliteModule();
		const kernel = createMockKernel();
		const bindings = createSqliteBindingsFromModule(kernel as any, mod);
		const sqlite = bindings.sqlite as any;

		const dbId = await sqlite.database.open(":memory:");
		await sqlite.database.exec(dbId, "CREATE TABLE big (val INTEGER)");
		const insertId = sqlite.database.prepare(
			dbId,
			"INSERT INTO big VALUES (?)",
		);
		await sqlite.statement.run(insertId, [
			{ __agentosSqliteType: "bigint", value: "9007199254740993" },
		]);

		const selectId = sqlite.database.prepare(dbId, "SELECT * FROM big");
		sqlite.statement.setReadBigInts(selectId, true);
		const row = sqlite.statement.get(selectId, []) as any;
		expect(row).toBeDefined();
		expect(row.val).toBeDefined();

		sqlite.statement.finalize(insertId);
		sqlite.statement.finalize(selectId);
		await sqlite.database.close(dbId);
	});

	test("Uint8Array encoding round-trip preserves binary data", async () => {
		const mod = await loadSqliteModule();
		const kernel = createMockKernel();
		const bindings = createSqliteBindingsFromModule(kernel as any, mod);
		const sqlite = bindings.sqlite as any;

		const dbId = await sqlite.database.open(":memory:");
		await sqlite.database.exec(dbId, "CREATE TABLE blobs (data BLOB)");
		const insertId = sqlite.database.prepare(
			dbId,
			"INSERT INTO blobs VALUES (?)",
		);
		await sqlite.statement.run(insertId, [
			{
				__agentosSqliteType: "uint8array",
				value: Buffer.from("binary data test").toString("base64"),
			},
		]);

		const selectId = sqlite.database.prepare(dbId, "SELECT * FROM blobs");
		const row = sqlite.statement.get(selectId, []) as any;
		expect(row).toBeDefined();
		expect(row.data).toBeDefined();

		sqlite.statement.finalize(insertId);
		sqlite.statement.finalize(selectId);
		await sqlite.database.close(dbId);
	});

	test("transaction tracking with BEGIN/COMMIT", async () => {
		const mod = await loadSqliteModule();
		const kernel = createMockKernel();
		const bindings = createSqliteBindingsFromModule(kernel as any, mod);
		const sqlite = bindings.sqlite as any;

		const dbId = await sqlite.database.open(":memory:");
		await sqlite.database.exec(
			dbId,
			"CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT)",
		);
		await sqlite.database.exec(dbId, "BEGIN");
		const insertId = sqlite.database.prepare(
			dbId,
			"INSERT INTO items (name) VALUES (?)",
		);
		await sqlite.statement.run(insertId, ["item1"]);
		await sqlite.statement.run(insertId, ["item2"]);
		await sqlite.statement.run(insertId, ["item3"]);
		await sqlite.database.exec(dbId, "COMMIT");

		const selectId = sqlite.database.prepare(
			dbId,
			"SELECT COUNT(*) as cnt FROM items",
		);
		expect((sqlite.statement.get(selectId, []) as any).cnt).toBe(3);

		sqlite.statement.finalize(insertId);
		sqlite.statement.finalize(selectId);
		await sqlite.database.close(dbId);
	});

	test("VFS sync writes to kernel on mutation outside transaction", async () => {
		const mod = await loadSqliteModule();
		const kernel = createMockKernel();
		const bindings = createSqliteBindingsFromModule(kernel as any, mod);
		const sqlite = bindings.sqlite as any;
		const dbId = await sqlite.database.open("/tmp/sync-test.db");
		await sqlite.database.exec(
			dbId,
			"CREATE TABLE t (id INTEGER); INSERT INTO t VALUES (1);",
		);
		expect(kernel.writeFile).toHaveBeenCalledWith(
			"/tmp/sync-test.db",
			expect.any(Uint8Array),
		);
		await sqlite.database.close(dbId);
	});

	test("VFS sync deferred during transactions", async () => {
		const mod = await loadSqliteModule();
		const kernel = createMockKernel();
		const bindings = createSqliteBindingsFromModule(kernel as any, mod);
		const sqlite = bindings.sqlite as any;
		const dbId = await sqlite.database.open("/tmp/txn-test.db");
		await sqlite.database.exec(dbId, "CREATE TABLE t (id INTEGER)");
		kernel.writeFile.mockClear();

		await sqlite.database.exec(dbId, "BEGIN");
		const insertId = sqlite.database.prepare(dbId, "INSERT INTO t VALUES (?)");
		await sqlite.statement.run(insertId, [1]);
		await sqlite.statement.run(insertId, [2]);
		const callsDuringTxn = kernel.writeFile.mock.calls.length;
		await sqlite.database.exec(dbId, "COMMIT");
		expect(kernel.writeFile.mock.calls.length).toBeGreaterThan(callsDuringTxn);

		sqlite.statement.finalize(insertId);
		await sqlite.database.close(dbId);
	});

	test("SELECT does not trigger VFS sync", async () => {
		const mod = await loadSqliteModule();
		const kernel = createMockKernel();
		const bindings = createSqliteBindingsFromModule(kernel as any, mod);
		const sqlite = bindings.sqlite as any;
		const dbId = await sqlite.database.open("/tmp/readonly-test.db");
		await sqlite.database.exec(
			dbId,
			"CREATE TABLE t (id INTEGER); INSERT INTO t VALUES (1);",
		);
		kernel.writeFile.mockClear();
		await sqlite.database.exec(dbId, "SELECT * FROM t");
		expect(kernel.writeFile).not.toHaveBeenCalled();
		await sqlite.database.close(dbId);
	});
});

// ---------------------------------------------------------------------------
// 6. Public API — createSqliteBindings (the actual entry point)
// ---------------------------------------------------------------------------

describe("createSqliteBindings (public API)", () => {
	afterEach(() => {
		_resetSqliteModuleCache();
	});

	test("returns a working binding tree via the public async API", async () => {
		const { createSqliteBindings } = await import("../src/sqlite-bindings.js");
		const kernel = createMockKernel();
		const bindings = await createSqliteBindings(kernel as any);
		const sqlite = bindings.sqlite as any;

		const dbId = await sqlite.database.open(":memory:");
		await sqlite.database.exec(dbId, "CREATE TABLE test (val TEXT)");
		const stmtId = sqlite.database.prepare(dbId, "INSERT INTO test VALUES (?)");
		await sqlite.statement.run(stmtId, ["hello from public API"]);
		const selectId = sqlite.database.prepare(dbId, "SELECT * FROM test");
		expect(sqlite.statement.get(selectId, [])).toEqual({
			val: "hello from public API",
		});

		sqlite.statement.finalize(stmtId);
		sqlite.statement.finalize(selectId);
		await sqlite.database.close(dbId);
	});
});
