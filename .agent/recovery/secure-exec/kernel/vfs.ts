/**
 * Virtual Filesystem interface.
 *
 * POSIX-complete interface that all filesystem backends must implement.
 * The primary implementation is ChunkedVFS, which composes an FsMetadataStore
 * (directory tree, inodes, chunk mapping) with an FsBlockStore (key-value blob
 * store) to provide tiered storage with optional write buffering and versioning.
 *
 * Error behavior (KernelError codes):
 * - ENOENT: path does not exist (readFile, stat, pread, pwrite, truncate, readlink, etc.)
 * - EISDIR: operation targets a directory when a file is expected (readFile, pread, pwrite)
 * - ENOTDIR: intermediate path component is not a directory
 * - EEXIST: target already exists (createDir without recursive, link to existing)
 * - ELOOP: symlink resolution exceeds 40 levels
 * - ENOTEMPTY: removeDir on non-empty directory
 * - EPERM: link to directory
 * - EXDEV: cross-mount copy (raised by MountTable, not VFS directly)
 *
 * Optional methods (fsync, copy, readDirStat) may be absent. The kernel and
 * MountTable use optional chaining and provide fallbacks where needed.
 *
 * Usage: create via `createChunkedVfs()` from `./vfs/chunked-vfs.ts`, or use
 * `createInMemoryFileSystem()` from the package root for the default in-memory VFS.
 */

export interface VirtualDirEntry {
	name: string;
	isDirectory: boolean;
	isSymbolicLink?: boolean;
	ino?: number;
}

export interface VirtualDirStatEntry extends VirtualDirEntry {
	stat: VirtualStat;
}

export interface VirtualStat {
	mode: number;
	size: number;
	isDirectory: boolean;
	isSymbolicLink: boolean;
	atimeMs: number;
	mtimeMs: number;
	ctimeMs: number;
	birthtimeMs: number;
	ino: number;
	nlink: number;
	uid: number;
	gid: number;
}

export interface VirtualFileSystem {
	// --- Core operations (existing) ---

	readFile(path: string): Promise<Uint8Array>;
	readTextFile(path: string): Promise<string>;
	readDir(path: string): Promise<string[]>;
	readDirWithTypes(path: string): Promise<VirtualDirEntry[]>;
	writeFile(path: string, content: string | Uint8Array): Promise<void>;
	createDir(path: string): Promise<void>;
	mkdir(path: string, options?: { recursive?: boolean }): Promise<void>;
	exists(path: string): Promise<boolean>;
	stat(path: string): Promise<VirtualStat>;
	removeFile(path: string): Promise<void>;
	removeDir(path: string): Promise<void>;
	rename(oldPath: string, newPath: string): Promise<void>;
	realpath(path: string): Promise<string>;

	// --- Symlinks ---

	symlink(target: string, linkPath: string): Promise<void>;
	readlink(path: string): Promise<string>;
	lstat(path: string): Promise<VirtualStat>;

	// --- Links ---

	link(oldPath: string, newPath: string): Promise<void>;

	// --- Permissions & Metadata ---

	chmod(path: string, mode: number): Promise<void>;
	chown(path: string, uid: number, gid: number): Promise<void>;
	utimes(path: string, atime: number, mtime: number): Promise<void>;
	truncate(path: string, length: number): Promise<void>;

	// --- Positional I/O ---

	/** Read a range from a file without loading the entire file into memory. */
	pread(path: string, offset: number, length: number): Promise<Uint8Array>;

	/** Write data at a specific offset without replacing the entire file. */
	pwrite(path: string, offset: number, data: Uint8Array): Promise<void>;

	/** Flush buffered writes for the given path to durable storage. */
	fsync?(path: string): Promise<void>;

	/** Copy a file within the same filesystem. */
	copy?(srcPath: string, dstPath: string): Promise<void>;

	/** Combined readdir + stat. Avoids N+1 queries for directory listings. */
	readDirStat?(path: string): Promise<VirtualDirStatEntry[]>;
}
