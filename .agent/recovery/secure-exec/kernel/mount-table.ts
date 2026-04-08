/**
 * Mount Table.
 *
 * Linux-style VFS mount table that routes paths to mounted filesystem backends
 * by longest-prefix matching. Replaces the hardcoded layer composition
 * (DeviceLayer wraps ProcLayer wraps base FS) with a unified routing table.
 */

import { KernelError } from "./types.js";
import type { VirtualDirEntry, VirtualDirStatEntry, VirtualFileSystem, VirtualStat } from "./vfs.js";

export interface MountOptions {
	readOnly?: boolean;
}

export interface MountEntry {
	path: string;
	readOnly: boolean;
}

interface InternalMount {
	path: string;
	fs: VirtualFileSystem;
	readOnly: boolean;
}

/**
 * Resolve a path to its mount and relative path within that mount.
 */
interface ResolvedPath {
	mount: InternalMount;
	relativePath: string;
}

/**
 * Normalize a path: collapse //, ., .., strip trailing /.
 */
function normalizePath(path: string): string {
	if (!path || path === "/") return "/";
	const parts = path.split("/");
	const stack: string[] = [];
	for (const part of parts) {
		if (part === "" || part === ".") continue;
		if (part === "..") {
			stack.pop();
		} else {
			stack.push(part);
		}
	}
	return `/${stack.join("/")}`;
}

/**
 * Get parent directory path.
 */
function parentPath(p: string): string {
	if (p === "/") return "/";
	const idx = p.lastIndexOf("/");
	if (idx <= 0) return "/";
	return p.slice(0, idx);
}

/**
 * Get basename of a path.
 */
function basename(p: string): string {
	const idx = p.lastIndexOf("/");
	return p.slice(idx + 1);
}

export class MountTable implements VirtualFileSystem {
	/**
	 * Mounts sorted by path length descending so longest-prefix match is first hit.
	 */
	private mounts: InternalMount[];

	constructor(rootFs: VirtualFileSystem) {
		this.mounts = [{ path: "/", fs: rootFs, readOnly: false }];
	}

	/**
	 * Mount a filesystem at the given path.
	 * Auto-creates the mount point directory in the parent filesystem if needed.
	 */
	mount(path: string, fs: VirtualFileSystem, options?: MountOptions): void {
		const normalized = normalizePath(path);

		if (normalized === "/") {
			throw new KernelError("EINVAL", "cannot mount over root");
		}

		// Check if already mounted
		if (this.mounts.some((m) => m.path === normalized)) {
			throw new KernelError("EEXIST", `already mounted at ${normalized}`);
		}

		// Auto-create mount point directory in the parent filesystem.
		// Resolve *before* inserting the new mount so the path goes to the current owner.
		const { mount: parentMount, relativePath } = this.resolve(normalized);
		void parentMount.fs
			.mkdir(relativePath || "/", { recursive: true })
			.catch(() => {
				/* directory may already exist */
			});

		const entry: InternalMount = {
			path: normalized,
			fs,
			readOnly: options?.readOnly ?? false,
		};

		this.mounts.push(entry);
		// Sort by path length descending for longest-prefix-first matching
		this.mounts.sort((a, b) => b.path.length - a.path.length);
	}

	/**
	 * Unmount the filesystem at the given path.
	 */
	unmount(path: string): void {
		const normalized = normalizePath(path);

		if (normalized === "/") {
			throw new KernelError("EINVAL", "cannot unmount root");
		}

		const idx = this.mounts.findIndex((m) => m.path === normalized);
		if (idx === -1) {
			throw new KernelError("EINVAL", `not a mount point: ${normalized}`);
		}

		this.mounts.splice(idx, 1);
	}

	/**
	 * List all current mounts.
	 */
	getMounts(): ReadonlyArray<MountEntry> {
		return this.mounts.map((m) => ({
			path: m.path,
			readOnly: m.readOnly,
		}));
	}

	// -----------------------------------------------------------------------
	// Path resolution
	// -----------------------------------------------------------------------

	private resolve(fullPath: string): ResolvedPath {
		const normalized = normalizePath(fullPath);

		for (const mount of this.mounts) {
			if (mount.path === "/") {
				// Root mount: forward path as-is
				return { mount, relativePath: normalized };
			}
			if (
				normalized === mount.path ||
				normalized.startsWith(`${mount.path}/`)
			) {
				const rel =
					normalized === mount.path
						? ""
						: normalized.slice(mount.path.length + 1);
				return { mount, relativePath: rel };
			}
		}

		// Should never happen since root mount always matches
		throw new KernelError("ENOENT", `no mount for path: ${fullPath}`);
	}

	private assertWritable(mount: InternalMount, path: string): void {
		if (mount.readOnly) {
			throw new KernelError("EROFS", `read-only filesystem: ${path}`);
		}
	}

	// -----------------------------------------------------------------------
	// Read operations
	// -----------------------------------------------------------------------

	async readFile(path: string): Promise<Uint8Array> {
		const { mount, relativePath } = this.resolve(path);
		return mount.fs.readFile(relativePath);
	}

	async readTextFile(path: string): Promise<string> {
		const { mount, relativePath } = this.resolve(path);
		return mount.fs.readTextFile(relativePath);
	}

	async readDir(path: string): Promise<string[]> {
		const { mount, relativePath } = this.resolve(path);
		const entries = await mount.fs.readDir(relativePath);

		// Merge mount point basenames for child mounts
		const normalized = normalizePath(path);
		const mountBasenames = this.getChildMountBasenames(normalized);

		if (mountBasenames.length === 0) return entries;

		const entrySet = new Set(entries);
		for (const name of mountBasenames) {
			entrySet.add(name);
		}
		return [...entrySet];
	}

	async readDirWithTypes(path: string): Promise<VirtualDirEntry[]> {
		const { mount, relativePath } = this.resolve(path);
		const entries = await mount.fs.readDirWithTypes(relativePath);

		// Merge mount point basenames as directory entries
		const normalized = normalizePath(path);
		const mountBasenames = this.getChildMountBasenames(normalized);

		if (mountBasenames.length === 0) return entries;

		const nameSet = new Set(entries.map((e) => e.name));
		for (const name of mountBasenames) {
			if (!nameSet.has(name)) {
				entries.push({
					name,
					isDirectory: true,
					isSymbolicLink: false,
				});
			}
		}
		return entries;
	}

	async exists(path: string): Promise<boolean> {
		const { mount, relativePath } = this.resolve(path);
		return mount.fs.exists(relativePath);
	}

	async stat(path: string): Promise<VirtualStat> {
		const { mount, relativePath } = this.resolve(path);
		return mount.fs.stat(relativePath);
	}

	async lstat(path: string): Promise<VirtualStat> {
		const { mount, relativePath } = this.resolve(path);
		return mount.fs.lstat(relativePath);
	}

	async realpath(path: string): Promise<string> {
		const { mount, relativePath } = this.resolve(path);
		const resolved = await mount.fs.realpath(relativePath);
		if (mount.path === "/") return resolved;
		// Re-prefix the mount path for non-root mounts
		return `${mount.path}/${resolved}`;
	}

	async readlink(path: string): Promise<string> {
		const { mount, relativePath } = this.resolve(path);
		return mount.fs.readlink(relativePath);
	}

	async pread(
		path: string,
		offset: number,
		length: number,
	): Promise<Uint8Array> {
		const { mount, relativePath } = this.resolve(path);
		return mount.fs.pread(relativePath, offset, length);
	}

	async pwrite(
		path: string,
		offset: number,
		data: Uint8Array,
	): Promise<void> {
		const { mount, relativePath } = this.resolve(path);
		this.assertWritable(mount, path);
		return mount.fs.pwrite(relativePath, offset, data);
	}

	// -----------------------------------------------------------------------
	// Write operations (check readOnly before forwarding)
	// -----------------------------------------------------------------------

	async writeFile(path: string, content: string | Uint8Array): Promise<void> {
		const { mount, relativePath } = this.resolve(path);
		this.assertWritable(mount, path);
		return mount.fs.writeFile(relativePath, content);
	}

	async createDir(path: string): Promise<void> {
		const { mount, relativePath } = this.resolve(path);
		this.assertWritable(mount, path);
		return mount.fs.createDir(relativePath);
	}

	async mkdir(path: string, options?: { recursive?: boolean }): Promise<void> {
		const { mount, relativePath } = this.resolve(path);
		this.assertWritable(mount, path);
		return mount.fs.mkdir(relativePath, options);
	}

	async removeFile(path: string): Promise<void> {
		const { mount, relativePath } = this.resolve(path);
		this.assertWritable(mount, path);
		return mount.fs.removeFile(relativePath);
	}

	async removeDir(path: string): Promise<void> {
		const { mount, relativePath } = this.resolve(path);
		this.assertWritable(mount, path);
		return mount.fs.removeDir(relativePath);
	}

	async symlink(target: string, linkPath: string): Promise<void> {
		const { mount, relativePath } = this.resolve(linkPath);
		this.assertWritable(mount, linkPath);
		return mount.fs.symlink(target, relativePath);
	}

	async link(oldPath: string, newPath: string): Promise<void> {
		const oldResolved = this.resolve(oldPath);
		const newResolved = this.resolve(newPath);

		if (oldResolved.mount !== newResolved.mount) {
			throw new KernelError(
				"EXDEV",
				`link across mounts: ${oldPath} -> ${newPath}`,
			);
		}

		this.assertWritable(oldResolved.mount, oldPath);
		return oldResolved.mount.fs.link(
			oldResolved.relativePath,
			newResolved.relativePath,
		);
	}

	async rename(oldPath: string, newPath: string): Promise<void> {
		const oldResolved = this.resolve(oldPath);
		const newResolved = this.resolve(newPath);

		if (oldResolved.mount !== newResolved.mount) {
			throw new KernelError(
				"EXDEV",
				`rename across mounts: ${oldPath} -> ${newPath}`,
			);
		}

		this.assertWritable(oldResolved.mount, oldPath);
		return oldResolved.mount.fs.rename(
			oldResolved.relativePath,
			newResolved.relativePath,
		);
	}

	async chmod(path: string, mode: number): Promise<void> {
		const { mount, relativePath } = this.resolve(path);
		this.assertWritable(mount, path);
		return mount.fs.chmod(relativePath, mode);
	}

	async chown(path: string, uid: number, gid: number): Promise<void> {
		const { mount, relativePath } = this.resolve(path);
		this.assertWritable(mount, path);
		return mount.fs.chown(relativePath, uid, gid);
	}

	async utimes(path: string, atime: number, mtime: number): Promise<void> {
		const { mount, relativePath } = this.resolve(path);
		this.assertWritable(mount, path);
		return mount.fs.utimes(relativePath, atime, mtime);
	}

	async truncate(path: string, length: number): Promise<void> {
		const { mount, relativePath } = this.resolve(path);
		this.assertWritable(mount, path);
		return mount.fs.truncate(relativePath, length);
	}

	async fsync(path: string): Promise<void> {
		const { mount, relativePath } = this.resolve(path);
		await mount.fs.fsync?.(relativePath);
	}

	async copy(srcPath: string, dstPath: string): Promise<void> {
		const srcResolved = this.resolve(srcPath);
		const dstResolved = this.resolve(dstPath);

		if (srcResolved.mount !== dstResolved.mount) {
			throw new KernelError(
				"EXDEV",
				`copy across mounts: ${srcPath} -> ${dstPath}`,
			);
		}

		this.assertWritable(srcResolved.mount, dstPath);

		if (srcResolved.mount.fs.copy) {
			return srcResolved.mount.fs.copy(
				srcResolved.relativePath,
				dstResolved.relativePath,
			);
		}

		// Fallback: readFile + writeFile.
		const content = await srcResolved.mount.fs.readFile(srcResolved.relativePath);
		await srcResolved.mount.fs.writeFile(dstResolved.relativePath, content);
	}

	async readDirStat(path: string): Promise<VirtualDirStatEntry[]> {
		const { mount, relativePath } = this.resolve(path);

		if (mount.fs.readDirStat) {
			return mount.fs.readDirStat(relativePath);
		}

		// Fallback: readDirWithTypes + stat for each entry.
		const entries = await mount.fs.readDirWithTypes(relativePath);
		const normalized = normalizePath(path);
		const results: VirtualDirStatEntry[] = [];
		for (const entry of entries) {
			const entryPath = normalized === "/" ? `/${entry.name}` : `${normalized}/${entry.name}`;
			const stat = await mount.fs.stat(entryPath);
			results.push({ ...entry, stat });
		}
		return results;
	}

	// -----------------------------------------------------------------------
	// Helpers
	// -----------------------------------------------------------------------

	/**
	 * Get basenames of child mount points under a directory.
	 * For example, if mounts exist at /dev and /proc, calling with "/" returns ["dev", "proc"].
	 */
	private getChildMountBasenames(dirPath: string): string[] {
		const names: string[] = [];
		for (const mount of this.mounts) {
			if (mount.path === "/") continue;
			const mountParent = parentPath(mount.path);
			if (mountParent === dirPath) {
				names.push(basename(mount.path));
			}
		}
		return names;
	}

	/**
	 * Synchronous open preparation (O_CREAT, O_EXCL, O_TRUNC).
	 * Delegates to the underlying backend's prepareOpenSync if it exists.
	 */
	prepareOpenSync(path: string, flags: number): boolean {
		const { mount, relativePath } = this.resolve(path);
		if (flags & ~0 && mount.readOnly) {
			// Check for write flags (O_CREAT=0o100, O_TRUNC=0o1000)
			const O_CREAT = 0o100;
			const O_TRUNC = 0o1000;
			if ((flags & O_CREAT) || (flags & O_TRUNC)) {
				this.assertWritable(mount, path);
			}
		}
		const backend = mount.fs as { prepareOpenSync?: (path: string, flags: number) => boolean };
		return backend.prepareOpenSync?.(relativePath, flags) ?? false;
	}
}
