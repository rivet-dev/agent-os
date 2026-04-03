import { Buffer } from "node:buffer";
import {
	existsSync,
	mkdirSync,
	mkdtempSync,
	readFileSync,
	rmSync,
	writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname as hostDirname, join, posix as posixPath } from "node:path";
import type { Kernel } from "@secure-exec/core";
import type { BindingTree } from "@secure-exec/nodejs";

// ---------------------------------------------------------------------------
// Internal SQLite abstraction – matches the node:sqlite shape consumed below.
// Both node:sqlite and bun:sqlite are adapted to this common interface so the
// binding tree logic is runtime-agnostic.
// ---------------------------------------------------------------------------

/** Unified database handle (mirrors node:sqlite DatabaseSync). */
export interface SqliteDatabase {
	close(): void;
	exec(sql: string): void;
	location(): string | null;
	prepare(sql: string): SqliteStatement;
}

/** Unified prepared-statement handle (mirrors node:sqlite StatementSync). */
export interface SqliteStatement {
	run(...params: unknown[]): unknown;
	get(...params: unknown[]): unknown;
	all(...params: unknown[]): unknown;
	iterate(...params: unknown[]): Iterable<unknown>;
	columns(): unknown;
	finalize?(): void;
	setReturnArrays(enabled: boolean): void;
	setReadBigInts(enabled: boolean): void;
	setAllowBareNamedParameters(enabled: boolean): void;
	setAllowUnknownNamedParameters(enabled: boolean): void;
}

/** Module-level SQLite provider. */
export interface SqliteModule {
	DatabaseSync: new (
		path?: string,
		options?: Record<string, unknown>,
	) => SqliteDatabase;
	constants?: Record<string, unknown>;
}

// ---------------------------------------------------------------------------
// Runtime detection
// ---------------------------------------------------------------------------

/** Returns `true` when running under Bun (detected via `process.versions.bun`). */
export function isBunRuntime(): boolean {
	return typeof process !== "undefined" && !!process.versions?.bun;
}

function isNodeRuntime(): boolean {
	return typeof process !== "undefined" && !!process.versions?.node;
}

function describeRuntime(): string {
	if (isBunRuntime()) return `Bun ${process.versions.bun}`;
	if (isNodeRuntime()) return `Node.js ${process.versions.node}`;
	return "unknown runtime";
}

// ---------------------------------------------------------------------------
// bun:sqlite adapter
//
// Wraps bun:sqlite's Database/Statement to match the node:sqlite
// DatabaseSync/StatementSync shape expected by the binding tree.
// ---------------------------------------------------------------------------

/** @internal — exported for testing only */
export function createBunSqliteAdapter(
	BunDatabase: any,
	bunConstants?: Record<string, unknown>,
): SqliteModule {
	class BunStatementAdapter implements SqliteStatement {
		private _stmt: any;
		private _returnArrays = false;

		constructor(stmt: any) {
			this._stmt = stmt;
		}

		run(...params: unknown[]): unknown {
			// bun:sqlite Statement.run() returns { changes, lastInsertRowid }
			// which matches the node:sqlite shape — use it directly.
			return this._stmt.run(...params);
		}

		get(...params: unknown[]): unknown {
			if (this._returnArrays) {
				const rows = this._stmt.values(...params);
				return rows[0] ?? undefined;
			}
			// bun:sqlite may return null for "no row" while node:sqlite returns
			// undefined. Normalize to undefined so the wire format is consistent.
			return this._stmt.get(...params) ?? undefined;
		}

		all(...params: unknown[]): unknown {
			if (this._returnArrays) {
				return this._stmt.values(...params);
			}
			return this._stmt.all(...params);
		}

		iterate(...params: unknown[]): Iterable<unknown> {
			// Prefer native iterate() if available (bun:sqlite supports it via
			// the @@iterator protocol), otherwise fall back to collecting all rows.
			if (typeof this._stmt.iterate === "function") {
				return this._stmt.iterate(...params);
			}
			const rows = this.all(...params);
			return Array.isArray(rows) ? rows : [];
		}

		columns(): unknown {
			// node:sqlite returns [{ name, column, table, ... }]; bun:sqlite
			// only exposes an array of column name strings.
			const names: string[] = this._stmt.columnNames ?? [];
			return names.map((name: string) => ({ name }));
		}

		setReturnArrays(enabled: boolean): void {
			this._returnArrays = enabled;
		}

		finalize(): void {
			if (typeof this._stmt.finalize === "function") {
				this._stmt.finalize();
			}
		}

		// No per-statement equivalents in bun:sqlite — safe no-ops.
		setReadBigInts(_enabled: boolean): void {}
		setAllowBareNamedParameters(_enabled: boolean): void {}
		setAllowUnknownNamedParameters(_enabled: boolean): void {}
	}

	class BunDatabaseAdapter implements SqliteDatabase {
		private _db: any;

		constructor(path?: string, options?: Record<string, unknown>) {
			this._db = new BunDatabase(path ?? ":memory:", options);
		}

		close(): void {
			this._db.close();
		}

		exec(sql: string): void {
			this._db.exec(sql);
		}

		location(): string | null {
			const f = this._db.filename;
			// Normalize to null for in-memory databases to match node:sqlite behavior.
			return f === ":memory:" || f === "" || f == null ? null : f;
		}

		prepare(sql: string): SqliteStatement {
			return new BunStatementAdapter(this._db.prepare(sql));
		}
	}

	return {
		DatabaseSync: BunDatabaseAdapter as unknown as SqliteModule["DatabaseSync"],
		constants: bunConstants ?? {},
	};
}

async function loadBunSqliteModule(): Promise<SqliteModule> {
	// Dynamic import keeps bun:sqlite out of the Node.js module graph.
	// Use a variable so TypeScript does not attempt to resolve the module.
	const bunSqliteId = "bun:sqlite";
	const bunSqlite: any = await import(bunSqliteId);
	const BunDatabase = bunSqlite.Database ?? bunSqlite.default?.Database;

	if (!BunDatabase) {
		const bunVersion = process.versions?.bun ?? "unknown";
		throw new Error(
			`bun:sqlite loaded but the Database class was not found (Bun ${bunVersion}). ` +
				`agent-os requires Bun >= 1.0.0 with bun:sqlite support. ` +
				`Upgrade with: curl -fsSL https://bun.sh/install | bash`,
		);
	}

	// Forward bun:sqlite constants (e.g. SQLITE_FCNTL_PERSIST_WAL) so
	// VM-side code can use them via the binding tree's meta.constants().
	const bunConstants =
		bunSqlite.constants ?? bunSqlite.default?.constants ?? {};

	return createBunSqliteAdapter(BunDatabase, bunConstants);
}

// ---------------------------------------------------------------------------
// node:sqlite loader
// ---------------------------------------------------------------------------

async function loadNodeSqliteModule(): Promise<SqliteModule> {
	try {
		const { createRequire } = await import("node:module");
		const esmRequire = createRequire(import.meta.url);
		return esmRequire("node:sqlite") as SqliteModule;
	} catch (error) {
		const nodeVersion = process.versions?.node ?? "unknown";
		throw new Error(
			`node:sqlite is not available (Node.js ${nodeVersion} detected). ` +
				`agent-os requires Node.js >= 22.5.0 with the --experimental-sqlite flag, ` +
				`or Node.js >= 23.4.0 where it is stable. ` +
				`Alternatively, use Bun (>= 1.0.0) which provides bun:sqlite natively.`,
			{ cause: error },
		);
	}
}

// ---------------------------------------------------------------------------
// Lazy loader (cached per process)
// ---------------------------------------------------------------------------

let _cachedModulePromise: Promise<SqliteModule> | null = null;

/** @internal — exported for testing only */
export function loadSqliteModule(): Promise<SqliteModule> {
	if (!_cachedModulePromise) {
		if (isBunRuntime()) {
			_cachedModulePromise = loadBunSqliteModule();
		} else if (isNodeRuntime()) {
			_cachedModulePromise = loadNodeSqliteModule();
		} else {
			_cachedModulePromise = Promise.reject(
				new Error(
					`SQLite bindings are not available on ${describeRuntime()}. ` +
						`agent-os requires Node.js >= 22.5.0 (with --experimental-sqlite) or Bun >= 1.0.0.`,
				),
			);
		}
	}
	return _cachedModulePromise;
}

/** @internal — reset the cached module (for testing only) */
export function _resetSqliteModuleCache(): void {
	_cachedModulePromise = null;
}

// ---------------------------------------------------------------------------
// Value encoding/decoding
// ---------------------------------------------------------------------------

type EncodedSqliteValue =
	| null
	| boolean
	| number
	| string
	| EncodedSqliteValue[]
	| { [key: string]: EncodedSqliteValue }
	| {
			__agentosSqliteType: "bigint" | "uint8array";
			value: string;
	  };

function encodeSqliteValue(value: unknown): EncodedSqliteValue {
	if (
		value === null ||
		typeof value === "boolean" ||
		typeof value === "number" ||
		typeof value === "string"
	) {
		return value;
	}

	if (typeof value === "bigint") {
		return {
			__agentosSqliteType: "bigint",
			value: value.toString(),
		};
	}

	if (Buffer.isBuffer(value) || value instanceof Uint8Array) {
		return {
			__agentosSqliteType: "uint8array",
			value: Buffer.from(value).toString("base64"),
		};
	}

	if (Array.isArray(value)) {
		return value.map((entry) => encodeSqliteValue(entry));
	}

	if (value && typeof value === "object") {
		return Object.fromEntries(
			Object.entries(value).map(([key, entry]) => [
				key,
				encodeSqliteValue(entry),
			]),
		);
	}

	return null;
}

function decodeSqliteValue<T = unknown>(value: unknown): T {
	if (value === null) {
		return value as T;
	}

	if (Array.isArray(value)) {
		return value.map((entry) => decodeSqliteValue(entry)) as T;
	}

	if (value && typeof value === "object") {
		const tagged = value as {
			__agentosSqliteType?: string;
			value?: string;
		};
		if (
			tagged.__agentosSqliteType === "bigint" &&
			typeof tagged.value === "string"
		) {
			return BigInt(tagged.value) as T;
		}

		if (
			tagged.__agentosSqliteType === "uint8array" &&
			typeof tagged.value === "string"
		) {
			return Buffer.from(tagged.value, "base64") as T;
		}

		return Object.fromEntries(
			Object.entries(value).map(([key, entry]) => [
				key,
				decodeSqliteValue(entry),
			]),
		) as T;
	}

	return value as T;
}

// ---------------------------------------------------------------------------
// SQL classification helpers
// ---------------------------------------------------------------------------

function isTransactionalSql(sql: string): boolean {
	return /^\s*(begin|commit|rollback|savepoint|release\s+savepoint)\b/i.test(
		sql,
	);
}

function isMutatingSql(sql: string): boolean {
	if (isTransactionalSql(sql)) {
		return true;
	}
	return /^\s*(insert|update|delete|replace|create|alter|drop|vacuum|reindex|analyze|attach|detach|pragma)\b/i.test(
		sql,
	);
}

// ---------------------------------------------------------------------------
// Binding tree factory — accepts a pre-loaded module for testability
// ---------------------------------------------------------------------------

/**
 * Create the SQLite binding tree from an already-loaded SQLite module.
 *
 * This is the injectable/testable variant of {@link createSqliteBindings}. It
 * accepts a pre-resolved `SqliteModule` so tests can inject mocks and custom
 * backends can supply alternative SQLite implementations (e.g. `better-sqlite3`
 * wrapped to match the interface).
 *
 * @param kernel - The secure-exec kernel (used for VFS sync of file-backed databases).
 * @param sqliteModule - A resolved SQLite module conforming to the `SqliteModule` interface.
 * @returns A `BindingTree` with the `sqlite` namespace (database, statement, meta).
 */
export function createSqliteBindingsFromModule(
	kernel: Kernel,
	sqliteModule: SqliteModule,
): BindingTree {
	let nextDatabaseId = 1;
	let nextStatementId = 1;
	const tempRoot = mkdtempSync(join(tmpdir(), "agentos-sqlite-"));

	const databases = new Map<
		number,
		{
			db: SqliteDatabase;
			statementIds: Set<number>;
			hostPath: string | null;
			vmPath: string | null;
			dirty: boolean;
			transactionDepth: number;
		}
	>();
	const statements = new Map<
		number,
		{
			dbId: number;
			sql: string;
			stmt: SqliteStatement;
		}
	>();

	function getDatabase(id: number) {
		const record = databases.get(id);
		if (!record) {
			const openIds = [...databases.keys()];
			throw new Error(
				`sqlite database handle ${id} not found. ` +
					`The database may have already been closed. ` +
					(openIds.length > 0
						? `Open handles: [${openIds.join(", ")}].`
						: `No databases are currently open.`),
			);
		}
		return record;
	}

	function getStatement(id: number) {
		const record = statements.get(id);
		if (!record) {
			const activeIds = [...statements.keys()];
			throw new Error(
				`sqlite statement handle ${id} not found. ` +
					`The statement may have been finalized, or its parent database was closed ` +
					`(closing a database invalidates all its prepared statements). ` +
					(activeIds.length > 0
						? `Active handles: [${activeIds.join(", ")}].`
						: `No statements are currently active.`),
			);
		}
		return record;
	}

	async function ensureVmParentDir(path: string): Promise<void> {
		const parent = posixPath.dirname(path);
		if (parent === "/" || parent === ".") {
			return;
		}
		let current = "";
		for (const part of parent.split("/").filter(Boolean)) {
			current += `/${part}`;
			if (!(await kernel.exists(current))) {
				await kernel.mkdir(current);
			}
		}
	}

	function markMutation(
		record: {
			dirty: boolean;
			transactionDepth: number;
		},
		sql: string,
	): void {
		if (!isMutatingSql(sql)) {
			return;
		}

		record.dirty = true;

		if (/^\s*(begin|savepoint)\b/i.test(sql)) {
			record.transactionDepth += 1;
			return;
		}

		if (/^\s*(commit|release\s+savepoint)\b/i.test(sql)) {
			record.transactionDepth = Math.max(0, record.transactionDepth - 1);
			return;
		}

		if (/^\s*rollback\b/i.test(sql) && !/^\s*rollback\s+to\b/i.test(sql)) {
			record.transactionDepth = Math.max(0, record.transactionDepth - 1);
		}
	}

	async function syncDatabase(record: {
		db: SqliteDatabase;
		hostPath: string | null;
		vmPath: string | null;
		dirty: boolean;
		transactionDepth: number;
	}): Promise<void> {
		if (
			!record.dirty ||
			record.transactionDepth > 0 ||
			!record.hostPath ||
			!record.vmPath
		) {
			return;
		}

		try {
			record.db.exec("PRAGMA wal_checkpoint(TRUNCATE)");
		} catch {
			// Best-effort only.
		}

		if (!existsSync(record.hostPath)) {
			return;
		}

		await ensureVmParentDir(record.vmPath);
		await kernel.writeFile(record.vmPath, readFileSync(record.hostPath));
		record.dirty = false;
	}

	async function closeDatabase(id: number) {
		const record = getDatabase(id);
		for (const statementId of record.statementIds) {
			statements.delete(statementId);
		}
		record.statementIds.clear();
		record.db.close();
		if (record.hostPath && record.vmPath && existsSync(record.hostPath)) {
			await ensureVmParentDir(record.vmPath);
			await kernel.writeFile(record.vmPath, readFileSync(record.hostPath));
			rmSync(record.hostPath, { force: true });
			rmSync(`${record.hostPath}-shm`, { force: true });
			rmSync(`${record.hostPath}-wal`, { force: true });
		}
		databases.delete(id);
	}

	function decodeParams(params: unknown): unknown[] {
		if (!Array.isArray(params)) {
			return [];
		}
		return params.map((entry) => decodeSqliteValue(entry));
	}

	return {
		sqlite: {
			meta: {
				constants(..._args: unknown[]) {
					return encodeSqliteValue(sqliteModule.constants ?? {});
				},
			},
			database: {
				open(...args: unknown[]) {
					return (async () => {
						const [pathArg, optionsArg] = args;
						const path = typeof pathArg === "string" ? pathArg : undefined;
						const normalizedOptions =
							optionsArg == null
								? undefined
								: (decodeSqliteValue(optionsArg) as Record<string, unknown>);
						let db: SqliteDatabase;
						const id = nextDatabaseId++;
						const vmPath = path && path !== ":memory:" ? path : null;
						const hostPath =
							vmPath !== null ? join(tempRoot, `${id}.sqlite`) : null;
						try {
							if (hostPath && vmPath) {
								if (await kernel.exists(vmPath)) {
									mkdirSync(hostDirname(hostPath), { recursive: true });
									writeFileSync(
										hostPath,
										Buffer.from(await kernel.readFile(vmPath)),
									);
								}
							}
							db =
								normalizedOptions === undefined
									? new sqliteModule.DatabaseSync(
											hostPath ?? path ?? ":memory:",
										)
									: new sqliteModule.DatabaseSync(
											hostPath ?? path ?? ":memory:",
											normalizedOptions,
										);
						} catch (error) {
							const details =
								error instanceof Error
									? (error.stack ?? error.message)
									: JSON.stringify(error);
							const vmDisplay = path ?? ":memory:";
							let hint = "";
							if (details.includes("ENOENT")) {
								hint = " The parent directory may not exist.";
							} else if (
								details.includes("EACCES") ||
								details.includes("permission")
							) {
								hint = " Permission denied — check that the path is writable.";
							} else if (details.includes("not a database")) {
								hint = " The file exists but is not a valid SQLite database.";
							}
							throw new Error(
								`Failed to open SQLite database "${vmDisplay}": ${details}${hint}`,
							);
						}
						databases.set(id, {
							db,
							statementIds: new Set(),
							hostPath,
							vmPath,
							dirty: false,
							transactionDepth: 0,
						});
						return id;
					})();
				},
				close(...args: unknown[]) {
					return (async () => {
						const [idArg] = args;
						const id = Number(idArg);
						await closeDatabase(id);
						return null;
					})();
				},
				exec(...args: unknown[]) {
					return (async () => {
						const [idArg, sqlArg] = args;
						const id = Number(idArg);
						const sql = String(sqlArg ?? "");
						const record = getDatabase(id);
						record.db.exec(sql);
						markMutation(record, sql);
						await syncDatabase(record);
						return null;
					})();
				},
				prepare(...args: unknown[]) {
					const [idArg, sqlArg] = args;
					const id = Number(idArg);
					const sql = String(sqlArg ?? "");
					const db = getDatabase(id);
					const statementId = nextStatementId++;
					const stmt = db.db.prepare(sql);
					db.statementIds.add(statementId);
					statements.set(statementId, {
						dbId: id,
						sql,
						stmt,
					});
					return statementId;
				},
				location(...args: unknown[]) {
					const [idArg] = args;
					const id = Number(idArg);
					const record = getDatabase(id);
					return record.vmPath ?? record.db.location();
				},
			},
			statement: {
				run(...args: unknown[]) {
					return (async () => {
						const [idArg, params] = args;
						const id = Number(idArg);
						const record = getStatement(id);
						const result = record.stmt.run(...decodeParams(params));
						const db = getDatabase(record.dbId);
						markMutation(db, record.sql);
						await syncDatabase(db);
						return encodeSqliteValue(result);
					})();
				},
				get(...args: unknown[]) {
					const [idArg, params] = args;
					const id = Number(idArg);
					return encodeSqliteValue(
						getStatement(id).stmt.get(...decodeParams(params)),
					);
				},
				all(...args: unknown[]) {
					const [idArg, params] = args;
					const id = Number(idArg);
					return encodeSqliteValue(
						getStatement(id).stmt.all(...decodeParams(params)),
					);
				},
				iterate(...args: unknown[]) {
					const [idArg, params] = args;
					const id = Number(idArg);
					return encodeSqliteValue([
						...getStatement(id).stmt.iterate(...decodeParams(params)),
					]);
				},
				columns(...args: unknown[]) {
					const [idArg] = args;
					const id = Number(idArg);
					return encodeSqliteValue(getStatement(id).stmt.columns());
				},
				setReturnArrays(...args: unknown[]) {
					const [idArg, enabled] = args;
					const id = Number(idArg);
					getStatement(id).stmt.setReturnArrays(Boolean(enabled));
					return null;
				},
				setReadBigInts(...args: unknown[]) {
					const [idArg, enabled] = args;
					const id = Number(idArg);
					getStatement(id).stmt.setReadBigInts(Boolean(enabled));
					return null;
				},
				setAllowBareNamedParameters(...args: unknown[]) {
					const [idArg, enabled] = args;
					const id = Number(idArg);
					getStatement(id).stmt.setAllowBareNamedParameters(Boolean(enabled));
					return null;
				},
				setAllowUnknownNamedParameters(...args: unknown[]) {
					const [idArg, enabled] = args;
					const id = Number(idArg);
					getStatement(id).stmt.setAllowUnknownNamedParameters(
						Boolean(enabled),
					);
					return null;
				},
				finalize(...args: unknown[]) {
					const [idArg] = args;
					const id = Number(idArg);
					const record = getStatement(id);
					record.stmt.finalize?.();
					const db = databases.get(record.dbId);
					db?.statementIds.delete(id);
					statements.delete(id);
					return null;
				},
			},
		},
	};
}

// ---------------------------------------------------------------------------
// Public API — lazily loads the SQLite module on first call
// ---------------------------------------------------------------------------

/**
 * Create the SQLite binding tree for the secure-exec Node runtime.
 *
 * Lazily loads the host SQLite module (`node:sqlite` on Node.js, `bun:sqlite`
 * on Bun) on first call, then caches the module for subsequent invocations.
 * The returned binding tree is passed to `createNodeRuntime({ bindings })`.
 *
 * @param kernel - The secure-exec kernel (used for VFS sync of file-backed databases).
 * @returns A `BindingTree` with the `sqlite` namespace (database, statement, meta).
 */
export async function createSqliteBindings(
	kernel: Kernel,
): Promise<BindingTree> {
	const sqliteModule = await loadSqliteModule();
	return createSqliteBindingsFromModule(kernel, sqliteModule);
}
