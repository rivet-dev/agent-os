/**
 * VFS storage layer interfaces.
 *
 * FsMetadataStore owns the filesystem tree (inodes, directory entries, symlinks,
 * chunk mapping). FsBlockStore is a dumb key-value byte store for file content.
 * ChunkedVFS composes the two into a VirtualFileSystem.
 */

// ---------------------------------------------------------------------------
// Inode types
// ---------------------------------------------------------------------------

export type InodeType = "file" | "directory" | "symlink";

export interface CreateInodeAttrs {
	type: InodeType;
	mode: number;
	uid: number;
	gid: number;
	/** Required for symlinks. */
	symlinkTarget?: string;
}

export interface InodeMeta {
	ino: number;
	type: InodeType;
	mode: number;
	uid: number;
	gid: number;
	size: number;
	nlink: number;
	atimeMs: number;
	mtimeMs: number;
	ctimeMs: number;
	birthtimeMs: number;
	/**
	 * 'inline': content stored in inlineContent (small files).
	 * 'chunked': content stored as blocks in the block store.
	 */
	storageMode: "inline" | "chunked";
	/** Inline content for small files. Null if chunked. */
	inlineContent: Uint8Array | null;
}

// ---------------------------------------------------------------------------
// Directory entry types
// ---------------------------------------------------------------------------

export interface DentryInfo {
	name: string;
	ino: number;
	type: InodeType;
}

export interface DentryStatInfo extends DentryInfo {
	stat: InodeMeta;
}

// ---------------------------------------------------------------------------
// FsMetadataStore
// ---------------------------------------------------------------------------

/**
 * Owns the filesystem tree, inode metadata, and chunk mapping.
 * No file content. All path resolution happens here.
 *
 * Implementations:
 * - InMemoryMetadataStore: pure JS Map-based, for ephemeral VMs and tests.
 * - SqliteMetadataStore: SQLite-backed, for persistent local and cloud storage.
 */
export interface FsMetadataStore {
	// -- Transactions --

	/**
	 * Execute a callback atomically. All metadata mutations within
	 * the callback either fully commit or fully roll back.
	 * SQLite: wraps in BEGIN/COMMIT.
	 * InMemory: just calls the callback (single-threaded JS, no rollback needed).
	 */
	transaction<T>(fn: () => Promise<T>): Promise<T>;

	// -- Inode lifecycle --

	/** Create a new inode. Returns the allocated inode number. */
	createInode(attrs: CreateInodeAttrs): Promise<number>;

	/** Get inode metadata by number. Returns null if not found. */
	getInode(ino: number): Promise<InodeMeta | null>;

	/** Update inode metadata fields (partial update). */
	updateInode(ino: number, updates: Partial<InodeMeta>): Promise<void>;

	/** Delete an inode and all associated data (chunk map, symlink target). */
	deleteInode(ino: number): Promise<void>;

	// -- Directory entries --

	/** Look up a child name in a directory. Returns child ino or null. */
	lookup(parentIno: number, name: string): Promise<number | null>;

	/** Create a directory entry. Throws EEXIST if name already exists. */
	createDentry(
		parentIno: number,
		name: string,
		childIno: number,
		type: InodeType,
	): Promise<void>;

	/** Remove a directory entry. Does NOT delete the child inode. */
	removeDentry(parentIno: number, name: string): Promise<void>;

	/** List all entries in a directory. */
	listDir(parentIno: number): Promise<DentryInfo[]>;

	/**
	 * List all entries with full inode metadata (avoids N+1).
	 * SQLite: single JOIN query. InMemory: iterate + Map lookup.
	 */
	listDirWithStats(parentIno: number): Promise<DentryStatInfo[]>;

	/**
	 * Move a directory entry. Atomic: removes from src parent,
	 * adds to dst parent. Handles same-parent rename.
	 */
	renameDentry(
		srcParentIno: number,
		srcName: string,
		dstParentIno: number,
		dstName: string,
	): Promise<void>;

	// -- Path resolution --

	/**
	 * Walk the dentry tree from root, following symlinks.
	 * Returns the resolved inode number.
	 * Throws ENOENT if any component does not exist.
	 * Throws ELOOP if symlink depth exceeds 40 (SYMLOOP_MAX).
	 */
	resolvePath(path: string): Promise<number>;

	/**
	 * Resolve all intermediate path components but NOT the final one.
	 * Returns the parent inode and the final component name.
	 * Used for lstat, readlink, unlink, and creating new entries.
	 * Throws ENOENT if any intermediate component does not exist.
	 */
	resolveParentPath(path: string): Promise<{ parentIno: number; name: string }>;

	// -- Symlinks --

	/** Get the symlink target for a symlink inode. */
	readSymlink(ino: number): Promise<string>;

	// -- Chunk mapping --

	/** Get the block store key for a chunk. Null if not set (sparse hole). */
	getChunkKey(ino: number, chunkIndex: number): Promise<string | null>;

	/** Set the block store key for a chunk. Creates or updates. */
	setChunkKey(ino: number, chunkIndex: number, key: string): Promise<void>;

	/** Get all chunk keys for a file, ordered by chunk index. */
	getAllChunkKeys(
		ino: number,
	): Promise<{ chunkIndex: number; key: string }[]>;

	/** Delete all chunk mappings for an inode. Returns the deleted keys. */
	deleteAllChunks(ino: number): Promise<string[]>;

	/**
	 * Delete chunk mappings for indices >= startIndex.
	 * Returns the deleted keys. Used by truncate.
	 */
	deleteChunksFrom(ino: number, startIndex: number): Promise<string[]>;
}

// ---------------------------------------------------------------------------
// FsBlockStore
// ---------------------------------------------------------------------------

/**
 * Dumb key-value byte store. Knows nothing about files, directories, or inodes.
 *
 * Error contracts:
 * - read/readRange: throw KernelError("ENOENT") if key not found.
 * - readRange beyond block size: return available bytes (short read).
 * - write: overwrite if key exists.
 * - delete/deleteMany: no-op for non-existent keys.
 * - copy: throw KernelError("ENOENT") if source key not found.
 */
export interface FsBlockStore {
	/** Read an entire block. Throws if key not found. */
	read(key: string): Promise<Uint8Array>;

	/** Read a byte range within a block. Throws if key not found. */
	readRange(key: string, offset: number, length: number): Promise<Uint8Array>;

	/** Write a block (creates or overwrites). */
	write(key: string, data: Uint8Array): Promise<void>;

	/** Delete a block. No-op if key does not exist. */
	delete(key: string): Promise<void>;

	/** Delete multiple blocks. No-op for keys that don't exist. */
	deleteMany(keys: string[]): Promise<void>;

	/**
	 * Server-side copy. Optional.
	 * If not implemented, callers fall back to read + write.
	 */
	copy?(srcKey: string, dstKey: string): Promise<void>;
}

// ---------------------------------------------------------------------------
// Versioning types
// ---------------------------------------------------------------------------

export interface VersionMeta {
	version: number;
	size: number;
	createdAt: number;
	storageMode: "inline" | "chunked";
	inlineContent: Uint8Array | null;
}

/**
 * Optional versioning extension for FsMetadataStore.
 *
 * Implementations that support versioning (e.g., SqliteMetadataStore) can
 * implement this interface to allow ChunkedVFS to snapshot, list, and
 * restore file versions.
 *
 * Both SqliteMetadataStore and InMemoryMetadataStore implement versioning
 * when the `versioning` option is enabled at construction time.
 */
export interface FsMetadataStoreVersioning {
	/** Snapshot current chunk map + size. Returns the version number. */
	createVersion(ino: number): Promise<number>;

	/** Get version info. Returns null if the version does not exist. */
	getVersion(ino: number, version: number): Promise<VersionMeta | null>;

	/** List versions, newest first. */
	listVersions(ino: number): Promise<VersionMeta[]>;

	/** Get chunk map for a specific version. */
	getVersionChunkMap(
		ino: number,
		version: number,
	): Promise<{ chunkIndex: number; key: string }[]>;

	/**
	 * Delete version records. Returns block keys that are no longer
	 * referenced by ANY version or the current chunk map.
	 */
	deleteVersions(ino: number, versions: number[]): Promise<string[]>;

	/** Restore current chunk map to match a version. */
	restoreVersion(ino: number, version: number): Promise<void>;
}

// ---------------------------------------------------------------------------
// Retention policy types (for ChunkedVFS versioning API)
// ---------------------------------------------------------------------------

export type RetentionPolicy =
	/** Keep the N most recent versions. Delete the rest immediately. */
	| { type: "count"; keep: number }
	/** Keep versions newer than maxAgeMs. Delete older immediately. */
	| { type: "age"; maxAgeMs: number }
	/**
	 * Mark old metadata as pruned but do NOT delete blocks.
	 * Used with block stores that have their own TTL/lifecycle
	 * (e.g., S3 lifecycle rules). The block store handles cleanup.
	 */
	| { type: "deferred" };

// ---------------------------------------------------------------------------
// ChunkedVFS versioning API types
// ---------------------------------------------------------------------------

export interface VersionInfo {
	version: number;
	size: number;
	createdAt: number;
}
