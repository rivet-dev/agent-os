/**
 * SQLite-backed FsMetadataStore for persistent local and cloud storage.
 *
 * All data is stored in four tables: inodes, dentries, symlinks, chunks.
 * Root inode (ino=1, type='directory') is created at initialization.
 * transaction() wraps in BEGIN/COMMIT, rolls back on error.
 * resolvePath uses iterative SELECT queries with ELOOP limit of 40.
 *
 * Usage:
 *   const store = new SqliteMetadataStore({ dbPath: ':memory:' });
 *   // or: new SqliteMetadataStore({ dbPath: '/tmp/metadata.db' });
 *
 * The store implements the FsMetadataStore interface and can be composed
 * with any FsBlockStore via createChunkedVfs() to form a full VirtualFileSystem.
 */

import Database from "better-sqlite3";
import type BetterSqlite3 from "better-sqlite3";
import { KernelError } from "../kernel/types.js";
import type {
	CreateInodeAttrs,
	DentryInfo,
	DentryStatInfo,
	FsMetadataStore,
	FsMetadataStoreVersioning,
	InodeMeta,
	InodeType,
	VersionMeta,
} from "./types.js";

const SYMLOOP_MAX = 40;

const S_IFREG = 0o100000;
const S_IFDIR = 0o040000;
const S_IFLNK = 0o120000;

export interface SqliteMetadataStoreOptions {
	/** Path to the SQLite database file. Use ':memory:' for in-memory. */
	dbPath: string;
	/** Enable file versioning support. Default: false. */
	versioning?: boolean;
}

export class SqliteMetadataStore implements FsMetadataStore, FsMetadataStoreVersioning {
	private db: BetterSqlite3.Database;
	private versioningEnabled: boolean;

	// Prepared statements for hot paths.
	private stmtGetInode: BetterSqlite3.Statement;
	private stmtUpdateInode: BetterSqlite3.Statement | null = null;
	private stmtDeleteInode: BetterSqlite3.Statement;
	private stmtDeleteSymlink: BetterSqlite3.Statement;
	private stmtDeleteChunks: BetterSqlite3.Statement;
	private stmtDeleteDentriesForParent: BetterSqlite3.Statement;
	private stmtLookup: BetterSqlite3.Statement;
	private stmtCreateDentry: BetterSqlite3.Statement;
	private stmtRemoveDentry: BetterSqlite3.Statement;
	private stmtListDir: BetterSqlite3.Statement;
	private stmtListDirWithStats: BetterSqlite3.Statement;
	private stmtGetSymlink: BetterSqlite3.Statement;
	private stmtGetChunkKey: BetterSqlite3.Statement;
	private stmtSetChunkKey: BetterSqlite3.Statement;
	private stmtLookupFull: BetterSqlite3.Statement;
	private stmtGetAllChunkKeys: BetterSqlite3.Statement;
	private stmtDeleteAllChunks: BetterSqlite3.Statement;
	private stmtDeleteChunksFrom: BetterSqlite3.Statement;
	private stmtDeleteChunksFromDel: BetterSqlite3.Statement;
	private stmtRenameDentry: BetterSqlite3.Statement;

	// Versioning prepared statements (only initialized if versioning is enabled).
	private stmtCreateVersion!: BetterSqlite3.Statement;
	private stmtGetVersion!: BetterSqlite3.Statement;
	private stmtListVersions!: BetterSqlite3.Statement;
	private stmtGetVersionChunkMap!: BetterSqlite3.Statement;
	private stmtDeleteVersion!: BetterSqlite3.Statement;
	private stmtMaxVersion!: BetterSqlite3.Statement;

	constructor(options: SqliteMetadataStoreOptions) {
		this.db = new Database(options.dbPath);
		this.db.pragma("journal_mode = WAL");
		this.db.pragma("foreign_keys = ON");
		this.versioningEnabled = options.versioning ?? false;

		this.initSchema();

		// Prepare statements.
		this.stmtGetInode = this.db.prepare(
			"SELECT ino, type, mode, uid, gid, size, nlink, atime_ms, mtime_ms, ctime_ms, birthtime_ms, storage_mode, inline_content FROM inodes WHERE ino = ?",
		);
		this.stmtDeleteInode = this.db.prepare("DELETE FROM inodes WHERE ino = ?");
		this.stmtDeleteSymlink = this.db.prepare(
			"DELETE FROM symlinks WHERE ino = ?",
		);
		this.stmtDeleteChunks = this.db.prepare(
			"DELETE FROM chunks WHERE ino = ?",
		);
		this.stmtDeleteDentriesForParent = this.db.prepare(
			"DELETE FROM dentries WHERE parent_ino = ?",
		);
		this.stmtLookup = this.db.prepare(
			"SELECT child_ino FROM dentries WHERE parent_ino = ? AND name = ?",
		);
		this.stmtLookupFull = this.db.prepare(
			"SELECT child_ino, child_type FROM dentries WHERE parent_ino = ? AND name = ?",
		);
		this.stmtCreateDentry = this.db.prepare(
			"INSERT INTO dentries (parent_ino, name, child_ino, child_type) VALUES (?, ?, ?, ?)",
		);
		this.stmtRemoveDentry = this.db.prepare(
			"DELETE FROM dentries WHERE parent_ino = ? AND name = ?",
		);
		this.stmtListDir = this.db.prepare(
			"SELECT name, child_ino, child_type FROM dentries WHERE parent_ino = ?",
		);
		this.stmtListDirWithStats = this.db.prepare(
			`SELECT d.name, d.child_ino, d.child_type,
				i.ino, i.type, i.mode, i.uid, i.gid, i.size, i.nlink,
				i.atime_ms, i.mtime_ms, i.ctime_ms, i.birthtime_ms,
				i.storage_mode, i.inline_content
			FROM dentries d
			JOIN inodes i ON d.child_ino = i.ino
			WHERE d.parent_ino = ?`,
		);
		this.stmtGetSymlink = this.db.prepare(
			"SELECT target FROM symlinks WHERE ino = ?",
		);
		this.stmtGetChunkKey = this.db.prepare(
			"SELECT block_key FROM chunks WHERE ino = ? AND chunk_index = ?",
		);
		this.stmtSetChunkKey = this.db.prepare(
			"INSERT OR REPLACE INTO chunks (ino, chunk_index, block_key) VALUES (?, ?, ?)",
		);
		this.stmtGetAllChunkKeys = this.db.prepare(
			"SELECT chunk_index, block_key FROM chunks WHERE ino = ? ORDER BY chunk_index",
		);
		this.stmtDeleteAllChunks = this.db.prepare(
			"SELECT block_key FROM chunks WHERE ino = ?",
		);
		this.stmtDeleteChunksFrom = this.db.prepare(
			"SELECT block_key FROM chunks WHERE ino = ? AND chunk_index >= ?",
		);
		this.stmtDeleteChunksFromDel = this.db.prepare(
			"DELETE FROM chunks WHERE ino = ? AND chunk_index >= ?",
		);
		this.stmtRenameDentry = this.db.prepare(
			"UPDATE dentries SET parent_ino = ?, name = ? WHERE parent_ino = ? AND name = ?",
		);

		// Versioning prepared statements.
		if (this.versioningEnabled) {
			this.stmtCreateVersion = this.db.prepare(
				"INSERT INTO versions (ino, version, size, created_at, storage_mode, inline_content, chunk_map) VALUES (?, ?, ?, ?, ?, ?, ?)",
			);
			this.stmtGetVersion = this.db.prepare(
				"SELECT version, size, created_at, storage_mode, inline_content FROM versions WHERE ino = ? AND version = ?",
			);
			this.stmtListVersions = this.db.prepare(
				"SELECT version, size, created_at, storage_mode, inline_content FROM versions WHERE ino = ? ORDER BY version DESC",
			);
			this.stmtGetVersionChunkMap = this.db.prepare(
				"SELECT chunk_map FROM versions WHERE ino = ? AND version = ?",
			);
			this.stmtDeleteVersion = this.db.prepare(
				"DELETE FROM versions WHERE ino = ? AND version = ?",
			);
			this.stmtMaxVersion = this.db.prepare(
				"SELECT MAX(version) as max_version FROM versions WHERE ino = ?",
			);
		}
	}

	private initSchema(): void {
		this.db.exec(`
			CREATE TABLE IF NOT EXISTS inodes (
				ino              INTEGER PRIMARY KEY AUTOINCREMENT,
				type             TEXT NOT NULL CHECK(type IN ('file', 'directory', 'symlink')),
				mode             INTEGER NOT NULL,
				uid              INTEGER NOT NULL DEFAULT 0,
				gid              INTEGER NOT NULL DEFAULT 0,
				size             INTEGER NOT NULL DEFAULT 0,
				nlink            INTEGER NOT NULL DEFAULT 0,
				atime_ms         INTEGER NOT NULL,
				mtime_ms         INTEGER NOT NULL,
				ctime_ms         INTEGER NOT NULL,
				birthtime_ms     INTEGER NOT NULL,
				storage_mode     TEXT NOT NULL DEFAULT 'inline' CHECK(storage_mode IN ('inline', 'chunked')),
				inline_content   BLOB
			);

			CREATE TABLE IF NOT EXISTS dentries (
				parent_ino  INTEGER NOT NULL,
				name        TEXT NOT NULL,
				child_ino   INTEGER NOT NULL,
				child_type  TEXT NOT NULL,
				PRIMARY KEY (parent_ino, name),
				FOREIGN KEY (parent_ino) REFERENCES inodes(ino),
				FOREIGN KEY (child_ino) REFERENCES inodes(ino)
			);
			CREATE INDEX IF NOT EXISTS idx_dentries_child ON dentries(child_ino);

			CREATE TABLE IF NOT EXISTS symlinks (
				ino     INTEGER PRIMARY KEY,
				target  TEXT NOT NULL,
				FOREIGN KEY (ino) REFERENCES inodes(ino)
			);

			CREATE TABLE IF NOT EXISTS chunks (
				ino          INTEGER NOT NULL,
				chunk_index  INTEGER NOT NULL,
				block_key    TEXT NOT NULL,
				PRIMARY KEY (ino, chunk_index),
				FOREIGN KEY (ino) REFERENCES inodes(ino)
			);
		`);

		if (this.versioningEnabled) {
			this.db.exec(`
				CREATE TABLE IF NOT EXISTS versions (
					ino            INTEGER NOT NULL,
					version        INTEGER NOT NULL,
					size           INTEGER NOT NULL,
					created_at     INTEGER NOT NULL,
					storage_mode   TEXT NOT NULL,
					inline_content BLOB,
					chunk_map      TEXT,
					PRIMARY KEY (ino, version),
					FOREIGN KEY (ino) REFERENCES inodes(ino)
				);
			`);
		}

		// Create root inode (ino=1) if it doesn't exist.
		const rootExists = this.db
			.prepare("SELECT ino FROM inodes WHERE ino = 1")
			.get();
		if (!rootExists) {
			const now = Date.now();
			this.db
				.prepare(
					`INSERT INTO inodes (ino, type, mode, uid, gid, size, nlink, atime_ms, mtime_ms, ctime_ms, birthtime_ms, storage_mode, inline_content)
				VALUES (1, 'directory', ?, 0, 0, 0, 2, ?, ?, ?, ?, 'inline', NULL)`,
				)
				.run(S_IFDIR | 0o755, now, now, now, now);
		}
	}

	private rowToInodeMeta(row: Record<string, unknown>): InodeMeta {
		const inlineContent = row.inline_content as Buffer | null;
		return {
			ino: row.ino as number,
			type: row.type as InodeType,
			mode: row.mode as number,
			uid: row.uid as number,
			gid: row.gid as number,
			size: row.size as number,
			nlink: row.nlink as number,
			atimeMs: row.atime_ms as number,
			mtimeMs: row.mtime_ms as number,
			ctimeMs: row.ctime_ms as number,
			birthtimeMs: row.birthtime_ms as number,
			storageMode: row.storage_mode as "inline" | "chunked",
			inlineContent: inlineContent
				? new Uint8Array(inlineContent)
				: null,
		};
	}

	// -- Transactions --

	private savepointCounter = 0;

	async transaction<T>(fn: () => Promise<T>): Promise<T> {
		const name = `sp_${this.savepointCounter++}`;
		this.db.exec(`SAVEPOINT ${name}`);
		try {
			const result = await fn();
			this.db.exec(`RELEASE ${name}`);
			return result;
		} catch (err) {
			this.db.exec(`ROLLBACK TO ${name}`);
			this.db.exec(`RELEASE ${name}`);
			throw err;
		}
	}

	// -- Inode lifecycle --

	async createInode(attrs: CreateInodeAttrs): Promise<number> {
		const now = Date.now();

		let mode = attrs.mode;
		if (attrs.type === "file") mode |= S_IFREG;
		else if (attrs.type === "directory") mode |= S_IFDIR;
		else if (attrs.type === "symlink") mode |= S_IFLNK;

		const result = this.db
			.prepare(
				`INSERT INTO inodes (type, mode, uid, gid, size, nlink, atime_ms, mtime_ms, ctime_ms, birthtime_ms, storage_mode, inline_content)
			VALUES (?, ?, ?, ?, 0, 0, ?, ?, ?, ?, 'inline', NULL)`,
			)
			.run(attrs.type, mode, attrs.uid, attrs.gid, now, now, now, now);

		const ino = Number(result.lastInsertRowid);

		if (attrs.type === "symlink" && attrs.symlinkTarget !== undefined) {
			this.db
				.prepare("INSERT INTO symlinks (ino, target) VALUES (?, ?)")
				.run(ino, attrs.symlinkTarget);
		}

		return ino;
	}

	async getInode(ino: number): Promise<InodeMeta | null> {
		const row = this.stmtGetInode.get(ino) as Record<string, unknown> | undefined;
		if (!row) return null;
		return this.rowToInodeMeta(row);
	}

	async updateInode(ino: number, updates: Partial<InodeMeta>): Promise<void> {
		const setClauses: string[] = [];
		const values: unknown[] = [];

		if (updates.type !== undefined) {
			setClauses.push("type = ?");
			values.push(updates.type);
		}
		if (updates.mode !== undefined) {
			setClauses.push("mode = ?");
			values.push(updates.mode);
		}
		if (updates.uid !== undefined) {
			setClauses.push("uid = ?");
			values.push(updates.uid);
		}
		if (updates.gid !== undefined) {
			setClauses.push("gid = ?");
			values.push(updates.gid);
		}
		if (updates.size !== undefined) {
			setClauses.push("size = ?");
			values.push(updates.size);
		}
		if (updates.nlink !== undefined) {
			setClauses.push("nlink = ?");
			values.push(updates.nlink);
		}
		if (updates.atimeMs !== undefined) {
			setClauses.push("atime_ms = ?");
			values.push(updates.atimeMs);
		}
		if (updates.mtimeMs !== undefined) {
			setClauses.push("mtime_ms = ?");
			values.push(updates.mtimeMs);
		}
		if (updates.ctimeMs !== undefined) {
			setClauses.push("ctime_ms = ?");
			values.push(updates.ctimeMs);
		}
		if (updates.birthtimeMs !== undefined) {
			setClauses.push("birthtime_ms = ?");
			values.push(updates.birthtimeMs);
		}
		if (updates.storageMode !== undefined) {
			setClauses.push("storage_mode = ?");
			values.push(updates.storageMode);
		}
		if (updates.inlineContent !== undefined) {
			setClauses.push("inline_content = ?");
			values.push(
				updates.inlineContent
					? Buffer.from(updates.inlineContent)
					: null,
			);
		}

		if (setClauses.length === 0) return;

		values.push(ino);
		this.db
			.prepare(`UPDATE inodes SET ${setClauses.join(", ")} WHERE ino = ?`)
			.run(...values);
	}

	async deleteInode(ino: number): Promise<void> {
		this.stmtDeleteChunks.run(ino);
		this.stmtDeleteSymlink.run(ino);
		this.stmtDeleteDentriesForParent.run(ino);
		this.stmtDeleteInode.run(ino);
	}

	// -- Directory entries --

	async lookup(parentIno: number, name: string): Promise<number | null> {
		const row = this.stmtLookup.get(parentIno, name) as
			| { child_ino: number }
			| undefined;
		return row ? row.child_ino : null;
	}

	async createDentry(
		parentIno: number,
		name: string,
		childIno: number,
		type: InodeType,
	): Promise<void> {
		const existing = this.stmtLookup.get(parentIno, name);
		if (existing) {
			throw new KernelError("EEXIST", `'${name}' already exists in directory`);
		}
		this.stmtCreateDentry.run(parentIno, name, childIno, type);
	}

	async removeDentry(parentIno: number, name: string): Promise<void> {
		this.stmtRemoveDentry.run(parentIno, name);
	}

	async listDir(parentIno: number): Promise<DentryInfo[]> {
		const rows = this.stmtListDir.all(parentIno) as Array<{
			name: string;
			child_ino: number;
			child_type: string;
		}>;
		return rows.map((row) => ({
			name: row.name,
			ino: row.child_ino,
			type: row.child_type as InodeType,
		}));
	}

	async listDirWithStats(parentIno: number): Promise<DentryStatInfo[]> {
		const rows = this.stmtListDirWithStats.all(parentIno) as Array<
			Record<string, unknown>
		>;
		return rows.map((row) => ({
			name: row.name as string,
			ino: row.child_ino as number,
			type: row.child_type as InodeType,
			stat: this.rowToInodeMeta(row),
		}));
	}

	async renameDentry(
		srcParentIno: number,
		srcName: string,
		dstParentIno: number,
		dstName: string,
	): Promise<void> {
		// Remove destination if it exists.
		this.stmtRemoveDentry.run(dstParentIno, dstName);

		// Move source to destination in a single UPDATE (no lookup needed).
		this.stmtRenameDentry.run(dstParentIno, dstName, srcParentIno, srcName);
	}

	// -- Path resolution --

	async resolvePath(path: string): Promise<number> {
		const components = splitPathComponents(path);
		return this.resolveComponents(components, 0);
	}

	async resolveParentPath(
		path: string,
	): Promise<{ parentIno: number; name: string }> {
		const components = splitPathComponents(path);
		if (components.length === 0) {
			throw new KernelError("ENOENT", "cannot resolve parent of root");
		}
		const name = components[components.length - 1]!;
		const parentComponents = components.slice(0, -1);
		const parentIno = await this.resolveComponents(parentComponents, 0);
		return { parentIno, name };
	}

	private resolveComponents(
		components: string[],
		symlinkDepth: number,
	): number {
		let currentIno = 1; // root

		for (let i = 0; i < components.length; i++) {
			const name = components[i]!;

			// Verify current inode is a directory.
			const meta = this.stmtGetInode.get(currentIno) as
				| Record<string, unknown>
				| undefined;
			if (!meta || meta.type !== "directory") {
				throw new KernelError(
					"ENOENT",
					`no such file or directory: component '${name}'`,
				);
			}

			// Look up child.
			const entry = this.stmtLookup.get(currentIno, name) as
				| { child_ino: number }
				| undefined;
			if (!entry) {
				throw new KernelError(
					"ENOENT",
					`no such file or directory: '${name}'`,
				);
			}

			currentIno = entry.child_ino;

			// Check if child is a symlink.
			const childMeta = this.stmtGetInode.get(currentIno) as
				| Record<string, unknown>
				| undefined;
			if (childMeta && childMeta.type === "symlink") {
				if (symlinkDepth >= SYMLOOP_MAX) {
					throw new KernelError("ELOOP", "too many levels of symbolic links");
				}

				const symlinkRow = this.stmtGetSymlink.get(currentIno) as
					| { target: string }
					| undefined;
				if (!symlinkRow) {
					throw new KernelError("ENOENT", "dangling symlink");
				}

				const target = symlinkRow.target;
				const targetComponents = splitPathComponents(target);
				const remaining = components.slice(i + 1);

				let fullComponents: string[];
				if (target.startsWith("/")) {
					fullComponents = [...targetComponents, ...remaining];
				} else {
					// Relative symlink: resolve relative to parent of the symlink.
					const parentComponents = components.slice(0, i);
					fullComponents = [
						...parentComponents,
						...targetComponents,
						...remaining,
					];
				}

				return this.resolveComponents(fullComponents, symlinkDepth + 1);
			}
		}

		return currentIno;
	}

	// -- Symlinks --

	async readSymlink(ino: number): Promise<string> {
		const row = this.stmtGetSymlink.get(ino) as
			| { target: string }
			| undefined;
		if (!row) {
			throw new KernelError("EINVAL", `inode ${ino} is not a symlink`);
		}
		return row.target;
	}

	// -- Chunk mapping --

	async getChunkKey(ino: number, chunkIndex: number): Promise<string | null> {
		const row = this.stmtGetChunkKey.get(ino, chunkIndex) as
			| { block_key: string }
			| undefined;
		return row ? row.block_key : null;
	}

	async setChunkKey(
		ino: number,
		chunkIndex: number,
		key: string,
	): Promise<void> {
		this.stmtSetChunkKey.run(ino, chunkIndex, key);
	}

	async getAllChunkKeys(
		ino: number,
	): Promise<{ chunkIndex: number; key: string }[]> {
		const rows = this.stmtGetAllChunkKeys.all(ino) as Array<{
			chunk_index: number;
			block_key: string;
		}>;
		return rows.map((row) => ({
			chunkIndex: row.chunk_index,
			key: row.block_key,
		}));
	}

	async deleteAllChunks(ino: number): Promise<string[]> {
		const rows = this.stmtDeleteAllChunks.all(ino) as Array<{
			block_key: string;
		}>;
		const keys = rows.map((row) => row.block_key);
		this.stmtDeleteChunks.run(ino);
		return keys;
	}

	async deleteChunksFrom(ino: number, startIndex: number): Promise<string[]> {
		const rows = this.stmtDeleteChunksFrom.all(ino, startIndex) as Array<{
			block_key: string;
		}>;
		const keys = rows.map((row) => row.block_key);
		this.stmtDeleteChunksFromDel.run(ino, startIndex);
		return keys;
	}

	// -- Versioning --

	async createVersion(ino: number): Promise<number> {
		if (!this.versioningEnabled) {
			throw new Error("versioning is not enabled");
		}

		const meta = this.stmtGetInode.get(ino) as Record<string, unknown> | undefined;
		if (!meta) {
			throw new KernelError("ENOENT", `inode ${ino} not found`);
		}

		// Get next version number.
		const maxRow = this.stmtMaxVersion.get(ino) as { max_version: number | null };
		const version = (maxRow.max_version ?? 0) + 1;

		const now = Date.now();
		const storageMode = meta.storage_mode as string;
		const inlineContent = meta.inline_content as Buffer | null;

		// Capture current chunk map as JSON.
		let chunkMapJson: string | null = null;
		if (storageMode === "chunked") {
			const chunkRows = this.stmtGetAllChunkKeys.all(ino) as Array<{
				chunk_index: number;
				block_key: string;
			}>;
			chunkMapJson = JSON.stringify(
				chunkRows.map((r) => ({
					chunkIndex: r.chunk_index,
					blockKey: r.block_key,
				})),
			);
		}

		this.stmtCreateVersion.run(
			ino,
			version,
			meta.size as number,
			now,
			storageMode,
			inlineContent,
			chunkMapJson,
		);

		return version;
	}

	async getVersion(ino: number, version: number): Promise<VersionMeta | null> {
		if (!this.versioningEnabled) {
			throw new Error("versioning is not enabled");
		}

		const row = this.stmtGetVersion.get(ino, version) as Record<string, unknown> | undefined;
		if (!row) return null;

		const inlineContent = row.inline_content as Buffer | null;
		return {
			version: row.version as number,
			size: row.size as number,
			createdAt: row.created_at as number,
			storageMode: row.storage_mode as "inline" | "chunked",
			inlineContent: inlineContent ? new Uint8Array(inlineContent) : null,
		};
	}

	async listVersions(ino: number): Promise<VersionMeta[]> {
		if (!this.versioningEnabled) {
			throw new Error("versioning is not enabled");
		}

		const rows = this.stmtListVersions.all(ino) as Array<Record<string, unknown>>;
		return rows.map((row) => {
			const inlineContent = row.inline_content as Buffer | null;
			return {
				version: row.version as number,
				size: row.size as number,
				createdAt: row.created_at as number,
				storageMode: row.storage_mode as "inline" | "chunked",
				inlineContent: inlineContent ? new Uint8Array(inlineContent) : null,
			};
		});
	}

	async getVersionChunkMap(
		ino: number,
		version: number,
	): Promise<{ chunkIndex: number; key: string }[]> {
		if (!this.versioningEnabled) {
			throw new Error("versioning is not enabled");
		}

		const row = this.stmtGetVersionChunkMap.get(ino, version) as
			| { chunk_map: string | null }
			| undefined;
		if (!row || !row.chunk_map) return [];

		const parsed = JSON.parse(row.chunk_map) as Array<{
			chunkIndex: number;
			blockKey: string;
		}>;
		return parsed.map((e) => ({
			chunkIndex: e.chunkIndex,
			key: e.blockKey,
		}));
	}

	async deleteVersions(ino: number, versions: number[]): Promise<string[]> {
		if (!this.versioningEnabled) {
			throw new Error("versioning is not enabled");
		}
		if (versions.length === 0) return [];

		// Collect all block keys referenced by the versions being deleted.
		const deletedBlockKeys = new Set<string>();
		for (const v of versions) {
			const chunkMap = await this.getVersionChunkMap(ino, v);
			for (const entry of chunkMap) {
				deletedBlockKeys.add(entry.key);
			}
			this.stmtDeleteVersion.run(ino, v);
		}

		if (deletedBlockKeys.size === 0) return [];

		// Find block keys still referenced by remaining versions.
		const remainingVersions = this.stmtListVersions.all(ino) as Array<Record<string, unknown>>;
		const referencedKeys = new Set<string>();

		for (const rv of remainingVersions) {
			const chunkMapStr = (
				this.stmtGetVersionChunkMap.get(ino, rv.version as number) as
					| { chunk_map: string | null }
					| undefined
			)?.chunk_map;
			if (chunkMapStr) {
				const parsed = JSON.parse(chunkMapStr) as Array<{
					chunkIndex: number;
					blockKey: string;
				}>;
				for (const e of parsed) {
					referencedKeys.add(e.blockKey);
				}
			}
		}

		// Also check the current chunk map.
		const currentChunks = this.stmtGetAllChunkKeys.all(ino) as Array<{
			chunk_index: number;
			block_key: string;
		}>;
		for (const c of currentChunks) {
			referencedKeys.add(c.block_key);
		}

		// Return orphaned keys (not referenced by any remaining version or current state).
		const orphanedKeys: string[] = [];
		for (const key of deletedBlockKeys) {
			if (!referencedKeys.has(key)) {
				orphanedKeys.push(key);
			}
		}

		return orphanedKeys;
	}

	async restoreVersion(ino: number, version: number): Promise<void> {
		if (!this.versioningEnabled) {
			throw new Error("versioning is not enabled");
		}

		const versionRow = this.stmtGetVersion.get(ino, version) as Record<string, unknown> | undefined;
		if (!versionRow) {
			throw new KernelError("ENOENT", `version ${version} not found for inode ${ino}`);
		}

		const versionChunkMapStr = (
			this.stmtGetVersionChunkMap.get(ino, version) as
				| { chunk_map: string | null }
				| undefined
		)?.chunk_map;

		// Clear current chunk map.
		this.stmtDeleteChunks.run(ino);

		// Restore chunk map from version.
		if (versionChunkMapStr) {
			const parsed = JSON.parse(versionChunkMapStr) as Array<{
				chunkIndex: number;
				blockKey: string;
			}>;
			for (const entry of parsed) {
				this.stmtSetChunkKey.run(ino, entry.chunkIndex, entry.blockKey);
			}
		}

		// Restore inode metadata from version.
		const inlineContent = versionRow.inline_content as Buffer | null;
		const updates: Partial<InodeMeta> = {
			size: versionRow.size as number,
			storageMode: versionRow.storage_mode as "inline" | "chunked",
			inlineContent: inlineContent ? new Uint8Array(inlineContent) : null,
			mtimeMs: Date.now(),
			ctimeMs: Date.now(),
		};
		await this.updateInode(ino, updates);
	}
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function splitPathComponents(path: string): string[] {
	if (!path || path === "/") return [];
	const normalized = path.startsWith("/") ? path.slice(1) : path;
	return normalized.split("/").filter((c) => c.length > 0);
}
