/**
 * In-memory filesystem for browser environments.
 *
 * Expanded from the original Agent OS in-memory filesystem with POSIX
 * extensions (symlinks, hard links, chmod, chown, utimes, truncate)
 * needed by the kernel VFS interface.
 */

import type {
	VirtualDirEntry,
	VirtualFileSystem,
	VirtualStat,
} from "./runtime.js";

const S_IFREG = 0o100000;
const S_IFDIR = 0o040000;
const S_IFLNK = 0o120000;

const MAX_SYMLINK_DEPTH = 40;

function normalizePath(path: string): string {
	if (!path) return "/";
	let normalized = path.startsWith("/") ? path : `/${path}`;
	normalized = normalized.replace(/\/+/g, "/");
	if (normalized.length > 1 && normalized.endsWith("/")) {
		normalized = normalized.slice(0, -1);
	}
	// Resolve . and ..
	const parts = normalized.split("/");
	const resolved: string[] = [];
	for (const part of parts) {
		if (part === "." || part === "") continue;
		if (part === "..") {
			resolved.pop();
		} else {
			resolved.push(part);
		}
	}
	return "/" + resolved.join("/") || "/";
}

function dirname(path: string): string {
	const parts = normalizePath(path).split("/").filter(Boolean);
	if (parts.length <= 1) return "/";
	return "/" + parts.slice(0, -1).join("/");
}

interface FileEntry {
	type: "file";
	data: Uint8Array;
	mode: number;
	uid: number;
	gid: number;
	nlink: number;
	ino: number;
	atimeMs: number;
	mtimeMs: number;
	ctimeMs: number;
	birthtimeMs: number;
}

interface DirEntry {
	type: "dir";
	mode: number;
	uid: number;
	gid: number;
	nlink: number;
	ino: number;
	atimeMs: number;
	mtimeMs: number;
	ctimeMs: number;
	birthtimeMs: number;
}

interface SymlinkEntry {
	type: "symlink";
	target: string;
	mode: number;
	uid: number;
	gid: number;
	nlink: number;
	ino: number;
	atimeMs: number;
	mtimeMs: number;
	ctimeMs: number;
	birthtimeMs: number;
}

type Entry = FileEntry | DirEntry | SymlinkEntry;

let nextIno = 1;

export class InMemoryFileSystem implements VirtualFileSystem {
	private entries: Map<string, Entry> = new Map();

	constructor() {
		// Root directory
		this.entries.set("/", this.newDir());
	}

	// --- Core operations ---

	async readFile(path: string): Promise<Uint8Array> {
		const entry = this.resolveEntry(path);
		if (!entry || entry.type !== "file") {
			throw this.enoent("open", path);
		}
		entry.atimeMs = Date.now();
		return entry.data;
	}

	async readTextFile(path: string): Promise<string> {
		const data = await this.readFile(path);
		return new TextDecoder().decode(data);
	}

	async readDir(path: string): Promise<string[]> {
		return (await this.readDirWithTypes(path)).map((e) => e.name);
	}

	async readDirWithTypes(path: string): Promise<VirtualDirEntry[]> {
		const resolved = this.resolvePath(path);
		const dir = this.entries.get(resolved);
		if (!dir || dir.type !== "dir") {
			throw this.enoent("scandir", path);
		}

		const prefix = resolved === "/" ? "/" : resolved + "/";
		const names = new Map<string, VirtualDirEntry>();

		for (const [entryPath, entry] of this.entries) {
			if (entryPath.startsWith(prefix)) {
				const rest = entryPath.slice(prefix.length);
				if (rest && !rest.includes("/")) {
					names.set(rest, {
						name: rest,
						isDirectory: entry.type === "dir",
						isSymbolicLink: entry.type === "symlink",
					});
				}
			}
		}

		return Array.from(names.values());
	}

	async writeFile(path: string, content: string | Uint8Array): Promise<void> {
		const normalized = normalizePath(path);
		// Ensure parent exists
		await this.mkdir(dirname(normalized), { recursive: true });

		const data =
			typeof content === "string" ? new TextEncoder().encode(content) : content;

		const existing = this.entries.get(normalized);
		if (existing && existing.type === "file") {
			existing.data = data;
			existing.mtimeMs = Date.now();
			existing.ctimeMs = Date.now();
			return;
		}

		const now = Date.now();
		this.entries.set(normalized, {
			type: "file",
			data,
			mode: S_IFREG | 0o644,
			uid: 1000,
			gid: 1000,
			nlink: 1,
			ino: nextIno++,
			atimeMs: now,
			mtimeMs: now,
			ctimeMs: now,
			birthtimeMs: now,
		});
	}

	async createDir(path: string): Promise<void> {
		const normalized = normalizePath(path);
		const parent = dirname(normalized);
		if (!this.entries.has(parent)) {
			throw this.enoent("mkdir", path);
		}
		if (!this.entries.has(normalized)) {
			this.entries.set(normalized, this.newDir());
		}
	}

	async mkdir(path: string, options?: { recursive?: boolean }): Promise<void> {
		const normalized = normalizePath(path);
		if (options?.recursive !== false) {
			// Recursive: create all missing parents
			const parts = normalized.split("/").filter(Boolean);
			let current = "";
			for (const part of parts) {
				current += "/" + part;
				if (!this.entries.has(current)) {
					this.entries.set(current, this.newDir());
				}
			}
		} else {
			await this.createDir(path);
		}
	}

	async exists(path: string): Promise<boolean> {
		try {
			const resolved = this.resolvePath(path);
			return this.entries.has(resolved);
		} catch {
			return false;
		}
	}

	async stat(path: string): Promise<VirtualStat> {
		const entry = this.resolveEntry(path);
		if (!entry) throw this.enoent("stat", path);
		return this.toStat(entry);
	}

	async removeFile(path: string): Promise<void> {
		const resolved = this.resolvePath(path);
		const entry = this.entries.get(resolved);
		if (!entry || entry.type === "dir") {
			throw this.enoent("unlink", path);
		}
		this.entries.delete(resolved);
	}

	async removeDir(path: string): Promise<void> {
		const resolved = this.resolvePath(path);
		if (resolved === "/") {
			throw new Error("EPERM: operation not permitted, rmdir '/'");
		}
		const entry = this.entries.get(resolved);
		if (!entry || entry.type !== "dir") {
			throw this.enoent("rmdir", path);
		}

		// Check if empty
		const prefix = resolved + "/";
		for (const key of this.entries.keys()) {
			if (key.startsWith(prefix)) {
				throw new Error(`ENOTEMPTY: directory not empty, rmdir '${path}'`);
			}
		}
		this.entries.delete(resolved);
	}

	async realpath(path: string): Promise<string> {
		return this.resolvePath(path);
	}

	async rename(oldPath: string, newPath: string): Promise<void> {
		const oldResolved = this.resolvePath(oldPath);
		const newNorm = normalizePath(newPath);
		const entry = this.entries.get(oldResolved);
		if (!entry) throw this.enoent("rename", oldPath);

		// Ensure parent of target exists
		if (!this.entries.has(dirname(newNorm))) {
			throw this.enoent("rename", newPath);
		}

		if (entry.type !== "dir") {
			this.entries.set(newNorm, entry);
			this.entries.delete(oldResolved);
			return;
		}

		// Move directory and all children
		const prefix = oldResolved + "/";
		const toMove: [string, Entry][] = [];
		for (const [key, val] of this.entries) {
			if (key === oldResolved || key.startsWith(prefix)) {
				toMove.push([key, val]);
			}
		}
		for (const [key] of toMove) {
			this.entries.delete(key);
		}
		for (const [key, val] of toMove) {
			const newKey =
				key === oldResolved ? newNorm : newNorm + key.slice(oldResolved.length);
			this.entries.set(newKey, val);
		}
	}

	// --- Symlinks ---

	async symlink(target: string, linkPath: string): Promise<void> {
		const normalized = normalizePath(linkPath);
		if (this.entries.has(normalized)) {
			throw new Error(`EEXIST: file already exists, symlink '${linkPath}'`);
		}
		const now = Date.now();
		this.entries.set(normalized, {
			type: "symlink",
			target,
			mode: S_IFLNK | 0o777,
			uid: 1000,
			gid: 1000,
			nlink: 1,
			ino: nextIno++,
			atimeMs: now,
			mtimeMs: now,
			ctimeMs: now,
			birthtimeMs: now,
		});
	}

	async readlink(path: string): Promise<string> {
		const normalized = normalizePath(path);
		const entry = this.entries.get(normalized);
		if (!entry || entry.type !== "symlink") {
			throw this.enoent("readlink", path);
		}
		return entry.target;
	}

	async lstat(path: string): Promise<VirtualStat> {
		const normalized = normalizePath(path);
		const entry = this.entries.get(normalized);
		if (!entry) throw this.enoent("lstat", path);
		return this.toStat(entry);
	}

	// --- Links ---

	async link(oldPath: string, newPath: string): Promise<void> {
		const entry = this.resolveEntry(oldPath);
		if (!entry || entry.type !== "file") {
			throw this.enoent("link", oldPath);
		}
		const newNorm = normalizePath(newPath);
		if (this.entries.has(newNorm)) {
			throw new Error(`EEXIST: file already exists, link '${newPath}'`);
		}
		entry.nlink++;
		this.entries.set(newNorm, entry);
	}

	// --- Permissions & Metadata ---

	async chmod(path: string, mode: number): Promise<void> {
		const entry = this.resolveEntry(path);
		if (!entry) throw this.enoent("chmod", path);
		const callerTypeBits = mode & 0o170000;
		if (callerTypeBits !== 0) {
			entry.mode = mode;
		} else {
			entry.mode = (entry.mode & 0o170000) | (mode & 0o7777);
		}
		entry.ctimeMs = Date.now();
	}

	async chown(path: string, uid: number, gid: number): Promise<void> {
		const entry = this.resolveEntry(path);
		if (!entry) throw this.enoent("chown", path);
		entry.uid = uid;
		entry.gid = gid;
		entry.ctimeMs = Date.now();
	}

	async utimes(path: string, atime: number, mtime: number): Promise<void> {
		const entry = this.resolveEntry(path);
		if (!entry) throw this.enoent("utimes", path);
		entry.atimeMs = atime;
		entry.mtimeMs = mtime;
		entry.ctimeMs = Date.now();
	}

	async truncate(path: string, length: number): Promise<void> {
		const entry = this.resolveEntry(path);
		if (!entry || entry.type !== "file") {
			throw this.enoent("truncate", path);
		}
		if (length < entry.data.length) {
			entry.data = entry.data.slice(0, length);
		} else if (length > entry.data.length) {
			const newData = new Uint8Array(length);
			newData.set(entry.data);
			entry.data = newData;
		}
		entry.mtimeMs = Date.now();
		entry.ctimeMs = Date.now();
	}

	async pread(
		path: string,
		offset: number,
		length: number,
	): Promise<Uint8Array> {
		const entry = this.resolveEntry(path);
		if (!entry || entry.type !== "file") {
			throw this.enoent("open", path);
		}
		entry.atimeMs = Date.now();
		if (offset >= entry.data.length) return new Uint8Array(0);
		return entry.data.slice(
			offset,
			Math.min(offset + length, entry.data.length),
		);
	}

	async pwrite(
		path: string,
		offset: number,
		data: Uint8Array,
	): Promise<void> {
		const entry = this.resolveEntry(path);
		if (!entry || entry.type !== "file") {
			throw this.enoent("open", path);
		}
		const endPos = offset + data.length;
		const newContent = new Uint8Array(Math.max(entry.data.length, endPos));
		newContent.set(entry.data);
		newContent.set(data, offset);
		entry.data = newContent;
		const now = Date.now();
		entry.mtimeMs = now;
		entry.ctimeMs = now;
	}

	// --- Helpers ---

	/**
	 * Resolve symlinks to get the final path. Returns the normalized path
	 * after following all symlinks.
	 */
	private resolvePath(path: string, depth = 0): string {
		if (depth > MAX_SYMLINK_DEPTH) {
			throw new Error(`ELOOP: too many levels of symbolic links, '${path}'`);
		}
		const normalized = normalizePath(path);
		const entry = this.entries.get(normalized);
		if (!entry) return normalized;
		if (entry.type === "symlink") {
			const target = entry.target.startsWith("/")
				? entry.target
				: dirname(normalized) + "/" + entry.target;
			return this.resolvePath(target, depth + 1);
		}
		return normalized;
	}

	/** Resolve a path and return the entry (following symlinks). */
	private resolveEntry(path: string): Entry | undefined {
		const resolved = this.resolvePath(path);
		return this.entries.get(resolved);
	}

	private newDir(): DirEntry {
		const now = Date.now();
		return {
			type: "dir",
			mode: S_IFDIR | 0o755,
			uid: 1000,
			gid: 1000,
			nlink: 2,
			ino: nextIno++,
			atimeMs: now,
			mtimeMs: now,
			ctimeMs: now,
			birthtimeMs: now,
		};
	}

	private toStat(entry: Entry): VirtualStat {
		return {
			mode: entry.mode,
			size: entry.type === "file" ? entry.data.length : 4096,
			isDirectory: entry.type === "dir",
			isSymbolicLink: entry.type === "symlink",
			atimeMs: entry.atimeMs,
			mtimeMs: entry.mtimeMs,
			ctimeMs: entry.ctimeMs,
			birthtimeMs: entry.birthtimeMs,
			ino: entry.ino,
			nlink: entry.nlink,
			uid: entry.uid,
			gid: entry.gid,
		};
	}

	private enoent(op: string, path: string): Error {
		const err = new Error(`ENOENT: no such file or directory, ${op} '${path}'`);
		(err as NodeJS.ErrnoException).code = "ENOENT";
		return err;
	}
}

export function createInMemoryFileSystem(): InMemoryFileSystem {
	return new InMemoryFileSystem();
}
