import { Buffer } from "node:buffer";
import {
	existsSync,
	mkdtempSync,
	mkdirSync,
	readFileSync,
	rmSync,
	writeFileSync,
} from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import {
	dirname as hostDirname,
	join,
	posix as posixPath,
} from "node:path";
import type { BindingTree, Kernel } from "./runtime-compat.js";

const require = createRequire(import.meta.url);
const sqliteBuiltin = require("node:sqlite") as {
	DatabaseSync: new (
		path?: string,
		options?: Record<string, unknown>,
	) => {
		close(): void;
		exec(sql: string): void;
		location(): string | null;
		prepare(sql: string): {
			run(...params: unknown[]): unknown;
			get(...params: unknown[]): unknown;
			all(...params: unknown[]): unknown;
			iterate(...params: unknown[]): Iterable<unknown>;
			columns(): unknown;
			setReturnArrays(enabled: boolean): void;
			setReadBigInts(enabled: boolean): void;
			setAllowBareNamedParameters(enabled: boolean): void;
			setAllowUnknownNamedParameters(enabled: boolean): void;
		};
	};
	constants?: Record<string, unknown>;
};

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

export function createSqliteBindings(kernel: Kernel): BindingTree {
	let nextDatabaseId = 1;
	let nextStatementId = 1;
	const tempRoot = mkdtempSync(join(tmpdir(), "agentos-sqlite-"));

	const databases = new Map<
		number,
		{
			db: InstanceType<typeof sqliteBuiltin.DatabaseSync>;
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
			stmt: ReturnType<InstanceType<typeof sqliteBuiltin.DatabaseSync>["prepare"]>;
		}
	>();

	function getDatabase(id: number) {
		const record = databases.get(id);
		if (!record) {
			throw new Error(`sqlite database handle not found: ${id}`);
		}
		return record;
	}

	function getStatement(id: number) {
		const record = statements.get(id);
		if (!record) {
			throw new Error(`sqlite statement handle not found: ${id}`);
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
		db: InstanceType<typeof sqliteBuiltin.DatabaseSync>;
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
					return encodeSqliteValue(sqliteBuiltin.constants ?? {});
				},
			},
			database: {
				open(...args: unknown[]) {
					return (async () => {
					const [pathArg, optionsArg] = args;
					const path =
						typeof pathArg === "string" ? pathArg : undefined;
					const normalizedOptions =
						optionsArg == null
							? undefined
							: (decodeSqliteValue(optionsArg) as Record<
									string,
									unknown
								>);
					let db: InstanceType<typeof sqliteBuiltin.DatabaseSync>;
					const id = nextDatabaseId++;
					const vmPath =
						path && path !== ":memory:" ? path : null;
					const hostPath =
						vmPath !== null
							? join(tempRoot, `${id}.sqlite`)
							: null;
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
						db = normalizedOptions === undefined
							? new sqliteBuiltin.DatabaseSync(hostPath ?? path ?? ":memory:")
							: new sqliteBuiltin.DatabaseSync(
									hostPath ?? path ?? ":memory:",
									normalizedOptions,
								);
					} catch (error) {
						const details =
							error instanceof Error
								? error.stack ?? error.message
								: JSON.stringify(error);
						throw new Error(
							`sqlite database open failed for ${path ?? ":memory:"}: ${details}`,
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
					const db = databases.get(record.dbId);
					db?.statementIds.delete(id);
					statements.delete(id);
					return null;
				},
			},
		},
	};
}
