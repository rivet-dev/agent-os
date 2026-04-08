/**
 * Pure JS Map-based FsMetadataStore for ephemeral VMs and tests.
 *
 * All data lives in memory. Root inode (ino=1, type='directory') is created
 * at construction time. transaction() just calls the callback directly since
 * single-threaded JS has no interleaving risk within synchronous sections.
 *
 * Optionally supports versioning when `{ versioning: true }` is passed to
 * the constructor. Version retention (automatic cleanup of old versions)
 * defaults to false. There is intentionally no background cleanup task;
 * callers are expected to prune versions explicitly via ChunkedVFS.
 */

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

export interface InMemoryMetadataStoreOptions {
	/** Enable file versioning support. Default: false. */
	versioning?: boolean;
}

const SYMLOOP_MAX = 40;

const S_IFREG = 0o100000;
const S_IFDIR = 0o040000;
const S_IFLNK = 0o120000;

interface DentryEntry {
	childIno: number;
	type: InodeType;
}

interface VersionRecord {
	version: number;
	size: number;
	createdAt: number;
	storageMode: "inline" | "chunked";
	inlineContent: Uint8Array | null;
	chunkMap: { chunkIndex: number; key: string }[];
}

export class InMemoryMetadataStore implements FsMetadataStore, FsMetadataStoreVersioning {
	private nextIno = 2;
	private inodes = new Map<number, InodeMeta>();
	private dentries = new Map<number, Map<string, DentryEntry>>();
	private symlinkTargets = new Map<number, string>();
	private chunks = new Map<number, Map<number, string>>();

	private versioningEnabled: boolean;
	private versions = new Map<number, VersionRecord[]>();

	constructor(options?: InMemoryMetadataStoreOptions) {
		this.versioningEnabled = options?.versioning ?? false;
		const now = Date.now();
		const rootInode: InodeMeta = {
			ino: 1,
			type: "directory",
			mode: S_IFDIR | 0o755,
			uid: 0,
			gid: 0,
			size: 0,
			nlink: 2,
			atimeMs: now,
			mtimeMs: now,
			ctimeMs: now,
			birthtimeMs: now,
			storageMode: "inline",
			inlineContent: null,
		};
		this.inodes.set(1, rootInode);
		this.dentries.set(1, new Map());
	}

	// -- Transactions --

	async transaction<T>(fn: () => Promise<T>): Promise<T> {
		return fn();
	}

	// -- Inode lifecycle --

	async createInode(attrs: CreateInodeAttrs): Promise<number> {
		const ino = this.nextIno++;
		const now = Date.now();

		let mode = attrs.mode;
		if (attrs.type === "file") mode |= S_IFREG;
		else if (attrs.type === "directory") mode |= S_IFDIR;
		else if (attrs.type === "symlink") mode |= S_IFLNK;

		const meta: InodeMeta = {
			ino,
			type: attrs.type,
			mode,
			uid: attrs.uid,
			gid: attrs.gid,
			size: 0,
			nlink: 0,
			atimeMs: now,
			mtimeMs: now,
			ctimeMs: now,
			birthtimeMs: now,
			storageMode: "inline",
			inlineContent: null,
		};
		this.inodes.set(ino, meta);

		if (attrs.type === "directory") {
			this.dentries.set(ino, new Map());
		}

		if (attrs.type === "symlink" && attrs.symlinkTarget !== undefined) {
			this.symlinkTargets.set(ino, attrs.symlinkTarget);
		}

		return ino;
	}

	async getInode(ino: number): Promise<InodeMeta | null> {
		const meta = this.inodes.get(ino);
		if (!meta) return null;
		return { ...meta };
	}

	async updateInode(ino: number, updates: Partial<InodeMeta>): Promise<void> {
		const meta = this.inodes.get(ino);
		if (!meta) return;
		Object.assign(meta, updates);
	}

	async deleteInode(ino: number): Promise<void> {
		this.inodes.delete(ino);
		this.dentries.delete(ino);
		this.symlinkTargets.delete(ino);
		this.chunks.delete(ino);
	}

	// -- Directory entries --

	async lookup(parentIno: number, name: string): Promise<number | null> {
		const dir = this.dentries.get(parentIno);
		if (!dir) return null;
		const entry = dir.get(name);
		return entry ? entry.childIno : null;
	}

	async createDentry(
		parentIno: number,
		name: string,
		childIno: number,
		type: InodeType,
	): Promise<void> {
		let dir = this.dentries.get(parentIno);
		if (!dir) {
			dir = new Map();
			this.dentries.set(parentIno, dir);
		}
		if (dir.has(name)) {
			throw new KernelError("EEXIST", `'${name}' already exists in directory`);
		}
		dir.set(name, { childIno, type });
	}

	async removeDentry(parentIno: number, name: string): Promise<void> {
		const dir = this.dentries.get(parentIno);
		if (dir) {
			dir.delete(name);
		}
	}

	async listDir(parentIno: number): Promise<DentryInfo[]> {
		const dir = this.dentries.get(parentIno);
		if (!dir) return [];
		const result: DentryInfo[] = [];
		for (const [name, entry] of dir) {
			result.push({ name, ino: entry.childIno, type: entry.type });
		}
		return result;
	}

	async listDirWithStats(parentIno: number): Promise<DentryStatInfo[]> {
		const dir = this.dentries.get(parentIno);
		if (!dir) return [];
		const result: DentryStatInfo[] = [];
		for (const [name, entry] of dir) {
			const meta = this.inodes.get(entry.childIno);
			if (meta) {
				result.push({ name, ino: entry.childIno, type: entry.type, stat: { ...meta } });
			}
		}
		return result;
	}

	async renameDentry(
		srcParentIno: number,
		srcName: string,
		dstParentIno: number,
		dstName: string,
	): Promise<void> {
		const srcDir = this.dentries.get(srcParentIno);
		if (!srcDir) return;
		const entry = srcDir.get(srcName);
		if (!entry) return;

		srcDir.delete(srcName);

		let dstDir = this.dentries.get(dstParentIno);
		if (!dstDir) {
			dstDir = new Map();
			this.dentries.set(dstParentIno, dstDir);
		}
		dstDir.set(dstName, entry);
	}

	// -- Path resolution --

	async resolvePath(path: string): Promise<number> {
		return this.resolvePathSync(path);
	}

	async resolveParentPath(
		path: string,
	): Promise<{ parentIno: number; name: string }> {
		const components = splitPathComponents(path);
		if (components.length === 0) {
			throw new KernelError("ENOENT", `cannot resolve parent of root`);
		}
		const name = components[components.length - 1]!;
		const parentComponents = components.slice(0, -1);
		const parentIno = this.resolveComponentsCore(parentComponents, 0);
		return { parentIno, name };
	}

	// -- Symlinks --

	async readSymlink(ino: number): Promise<string> {
		const target = this.symlinkTargets.get(ino);
		if (target === undefined) {
			throw new KernelError("EINVAL", `inode ${ino} is not a symlink`);
		}
		return target;
	}

	// -- Chunk mapping --

	async getChunkKey(ino: number, chunkIndex: number): Promise<string | null> {
		const map = this.chunks.get(ino);
		if (!map) return null;
		return map.get(chunkIndex) ?? null;
	}

	async setChunkKey(
		ino: number,
		chunkIndex: number,
		key: string,
	): Promise<void> {
		let map = this.chunks.get(ino);
		if (!map) {
			map = new Map();
			this.chunks.set(ino, map);
		}
		map.set(chunkIndex, key);
	}

	async getAllChunkKeys(
		ino: number,
	): Promise<{ chunkIndex: number; key: string }[]> {
		const map = this.chunks.get(ino);
		if (!map) return [];
		const entries: { chunkIndex: number; key: string }[] = [];
		for (const [chunkIndex, key] of map) {
			entries.push({ chunkIndex, key });
		}
		entries.sort((a, b) => a.chunkIndex - b.chunkIndex);
		return entries;
	}

	async deleteAllChunks(ino: number): Promise<string[]> {
		const map = this.chunks.get(ino);
		if (!map) return [];
		const keys = Array.from(map.values());
		this.chunks.delete(ino);
		return keys;
	}

	async deleteChunksFrom(ino: number, startIndex: number): Promise<string[]> {
		const map = this.chunks.get(ino);
		if (!map) return [];
		const deleted: string[] = [];
		for (const [idx, key] of map) {
			if (idx >= startIndex) {
				deleted.push(key);
				map.delete(idx);
			}
		}
		return deleted;
	}

	// -- Synchronous accessors (for prepareOpenSync in kernel) --

	resolvePathSync(path: string): number {
		const components = splitPathComponents(path);
		return this.resolveComponentsCore(components, 0);
	}

	lookupSync(parentIno: number, name: string): number | null {
		const dir = this.dentries.get(parentIno);
		if (!dir) return null;
		const entry = dir.get(name);
		return entry ? entry.childIno : null;
	}

	getInodeSync(ino: number): InodeMeta | null {
		const meta = this.inodes.get(ino);
		if (!meta) return null;
		return { ...meta };
	}

	createInodeSync(attrs: CreateInodeAttrs): number {
		const ino = this.nextIno++;
		const now = Date.now();

		let mode = attrs.mode;
		if (attrs.type === "file") mode |= S_IFREG;
		else if (attrs.type === "directory") mode |= S_IFDIR;
		else if (attrs.type === "symlink") mode |= S_IFLNK;

		const meta: InodeMeta = {
			ino,
			type: attrs.type,
			mode,
			uid: attrs.uid,
			gid: attrs.gid,
			size: 0,
			nlink: 0,
			atimeMs: now,
			mtimeMs: now,
			ctimeMs: now,
			birthtimeMs: now,
			storageMode: "inline",
			inlineContent: null,
		};
		this.inodes.set(ino, meta);

		if (attrs.type === "directory") {
			this.dentries.set(ino, new Map());
		}

		if (attrs.type === "symlink" && attrs.symlinkTarget !== undefined) {
			this.symlinkTargets.set(ino, attrs.symlinkTarget);
		}

		return ino;
	}

	updateInodeSync(ino: number, updates: Partial<InodeMeta>): void {
		const meta = this.inodes.get(ino);
		if (!meta) return;
		Object.assign(meta, updates);
	}

	createDentrySync(
		parentIno: number,
		name: string,
		childIno: number,
		type: InodeType,
	): void {
		let dir = this.dentries.get(parentIno);
		if (!dir) {
			dir = new Map();
			this.dentries.set(parentIno, dir);
		}
		if (dir.has(name)) {
			throw new KernelError("EEXIST", `'${name}' already exists in directory`);
		}
		dir.set(name, { childIno, type });
	}

	deleteAllChunksSync(ino: number): string[] {
		const map = this.chunks.get(ino);
		if (!map) return [];
		const keys = Array.from(map.values());
		this.chunks.delete(ino);
		return keys;
	}

	// -- Versioning --
	// No background cleanup task is implemented. This is intentional; callers
	// prune versions explicitly via ChunkedVFS.pruneVersions(). Version
	// retention defaults to false (no automatic cleanup).

	async createVersion(ino: number): Promise<number> {
		if (!this.versioningEnabled) {
			throw new Error("versioning is not enabled");
		}

		const meta = this.inodes.get(ino);
		if (!meta) {
			throw new KernelError("ENOENT", `inode ${ino} not found`);
		}

		const records = this.versions.get(ino) ?? [];
		const version = records.length > 0 ? records[records.length - 1]!.version + 1 : 1;

		const chunkMap: { chunkIndex: number; key: string }[] = [];
		const inoChunks = this.chunks.get(ino);
		if (inoChunks) {
			for (const [chunkIndex, key] of inoChunks) {
				chunkMap.push({ chunkIndex, key });
			}
			chunkMap.sort((a, b) => a.chunkIndex - b.chunkIndex);
		}

		const record: VersionRecord = {
			version,
			size: meta.size,
			createdAt: Date.now(),
			storageMode: meta.storageMode,
			inlineContent: meta.inlineContent ? new Uint8Array(meta.inlineContent) : null,
			chunkMap,
		};
		records.push(record);
		this.versions.set(ino, records);

		return version;
	}

	async getVersion(ino: number, version: number): Promise<VersionMeta | null> {
		if (!this.versioningEnabled) {
			throw new Error("versioning is not enabled");
		}

		const records = this.versions.get(ino);
		if (!records) return null;
		const record = records.find((r) => r.version === version);
		if (!record) return null;

		return {
			version: record.version,
			size: record.size,
			createdAt: record.createdAt,
			storageMode: record.storageMode,
			inlineContent: record.inlineContent ? new Uint8Array(record.inlineContent) : null,
		};
	}

	async listVersions(ino: number): Promise<VersionMeta[]> {
		if (!this.versioningEnabled) {
			throw new Error("versioning is not enabled");
		}

		const records = this.versions.get(ino) ?? [];
		return records
			.map((r) => ({
				version: r.version,
				size: r.size,
				createdAt: r.createdAt,
				storageMode: r.storageMode,
				inlineContent: r.inlineContent ? new Uint8Array(r.inlineContent) : null,
			}))
			.reverse();
	}

	async getVersionChunkMap(
		ino: number,
		version: number,
	): Promise<{ chunkIndex: number; key: string }[]> {
		if (!this.versioningEnabled) {
			throw new Error("versioning is not enabled");
		}

		const records = this.versions.get(ino);
		if (!records) return [];
		const record = records.find((r) => r.version === version);
		if (!record) return [];

		return record.chunkMap.map((e) => ({ chunkIndex: e.chunkIndex, key: e.key }));
	}

	async deleteVersions(ino: number, versions: number[]): Promise<string[]> {
		if (!this.versioningEnabled) {
			throw new Error("versioning is not enabled");
		}
		if (versions.length === 0) return [];

		const records = this.versions.get(ino);
		if (!records) return [];

		const versionSet = new Set(versions);

		// Collect block keys from versions being deleted.
		const deletedBlockKeys = new Set<string>();
		for (const r of records) {
			if (versionSet.has(r.version)) {
				for (const e of r.chunkMap) {
					deletedBlockKeys.add(e.key);
				}
			}
		}

		// Remove the version records.
		const remaining = records.filter((r) => !versionSet.has(r.version));
		this.versions.set(ino, remaining);

		if (deletedBlockKeys.size === 0) return [];

		// Find keys still referenced by remaining versions.
		const referencedKeys = new Set<string>();
		for (const r of remaining) {
			for (const e of r.chunkMap) {
				referencedKeys.add(e.key);
			}
		}

		// Also check the current chunk map.
		const currentChunks = this.chunks.get(ino);
		if (currentChunks) {
			for (const key of currentChunks.values()) {
				referencedKeys.add(key);
			}
		}

		// Return orphaned keys.
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

		const records = this.versions.get(ino);
		const record = records?.find((r) => r.version === version);
		if (!record) {
			throw new KernelError("ENOENT", `version ${version} not found for inode ${ino}`);
		}

		// Clear current chunk map.
		this.chunks.delete(ino);

		// Restore chunk map from version.
		if (record.chunkMap.length > 0) {
			const map = new Map<number, string>();
			for (const entry of record.chunkMap) {
				map.set(entry.chunkIndex, entry.key);
			}
			this.chunks.set(ino, map);
		}

		// Restore inode metadata from version.
		const meta = this.inodes.get(ino);
		if (meta) {
			meta.size = record.size;
			meta.storageMode = record.storageMode;
			meta.inlineContent = record.inlineContent ? new Uint8Array(record.inlineContent) : null;
			meta.mtimeMs = Date.now();
			meta.ctimeMs = Date.now();
		}
	}

	// -- Internal helpers --

	private resolveComponentsCore(
		components: string[],
		symlinkDepth: number,
	): number {
		let currentIno = 1; // root

		for (let i = 0; i < components.length; i++) {
			const name = components[i]!;
			const meta = this.inodes.get(currentIno);
			if (!meta || meta.type !== "directory") {
				throw new KernelError(
					"ENOENT",
					`no such file or directory: component '${name}'`,
				);
			}

			const dir = this.dentries.get(currentIno);
			if (!dir) {
				throw new KernelError(
					"ENOENT",
					`no such file or directory: component '${name}'`,
				);
			}

			const entry = dir.get(name);
			if (!entry) {
				throw new KernelError(
					"ENOENT",
					`no such file or directory: '${name}'`,
				);
			}

			currentIno = entry.childIno;

			// Follow symlinks.
			const childMeta = this.inodes.get(currentIno);
			if (childMeta && childMeta.type === "symlink") {
				if (symlinkDepth >= SYMLOOP_MAX) {
					throw new KernelError("ELOOP", "too many levels of symbolic links");
				}
				const target = this.symlinkTargets.get(currentIno);
				if (!target) {
					throw new KernelError("ENOENT", "dangling symlink");
				}

				// Resolve symlink target relative to current position.
				const targetComponents = splitPathComponents(target);
				const remaining = components.slice(i + 1);
				const fullComponents = target.startsWith("/")
					? [...targetComponents, ...remaining]
					: [
							...this.getPathComponents(currentIno, components.slice(0, i)),
							...targetComponents,
							...remaining,
						];

				return this.resolveComponentsCore(fullComponents, symlinkDepth + 1);
			}
		}

		return currentIno;
	}

	/**
	 * Get the parent path components for resolving a relative symlink.
	 * We need to reconstruct the parent directory path from the components
	 * we have already resolved (everything before the symlink).
	 */
	private getPathComponents(
		_symlinkIno: number,
		parentComponents: string[],
	): string[] {
		return parentComponents;
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
