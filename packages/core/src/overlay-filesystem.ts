/**
 * Overlay (copy-on-write) filesystem backend.
 *
 * Layers an optional writable upper filesystem over zero or more lower
 * filesystems. Reads resolve from highest precedence to lowest. Writes
 * go to the writable upper only, with copy-up and whiteout behavior.
 */

import * as posixPath from "node:path/posix";
import {
	createInMemoryFileSystem,
	KernelError,
	type VirtualDirEntry,
	type VirtualFileSystem,
	type VirtualStat,
} from "./runtime-compat.js";

export interface OverlayBackendOptions {
	/** Legacy single lower layer. */
	lower?: VirtualFileSystem;
	/** Lower layers ordered highest-precedence first. */
	lowers?: VirtualFileSystem[];
	/** Writable upper layer. Defaults to a fresh in-memory filesystem in ephemeral mode. */
	upper?: VirtualFileSystem;
	/** Overlay mode. Defaults to ephemeral. */
	mode?: "ephemeral" | "read-only";
}

export function createOverlayBackend(
	options: OverlayBackendOptions,
): VirtualFileSystem {
	const mode = options.mode ?? "ephemeral";
	if (mode === "read-only" && options.upper) {
		throw new Error("Read-only overlays cannot accept a writable upper layer");
	}

	const configuredLowers = options.lowers
		?? (options.lower ? [options.lower] : []);
	const lowers = configuredLowers.length > 0
		? configuredLowers
		: [createInMemoryFileSystem()];
	const upper = mode === "read-only"
		? null
		: options.upper ?? createInMemoryFileSystem();
	const whiteouts = new Set<string>();

	function normPath(path: string): string {
		return posixPath.normalize(path);
	}

	function isWhitedOut(path: string): boolean {
		return whiteouts.has(normPath(path));
	}

	function addWhiteout(path: string): void {
		whiteouts.add(normPath(path));
	}

	function removeWhiteout(path: string): void {
		whiteouts.delete(normPath(path));
	}

	function throwReadOnly(): never {
		throw new KernelError("EROFS", "read-only file system");
	}

	async function existsInFilesystem(
		filesystem: VirtualFileSystem,
		path: string,
	): Promise<boolean> {
		return hasEntryInFilesystem(filesystem, path);
	}

	async function hasEntryInFilesystem(
		filesystem: VirtualFileSystem,
		path: string,
	): Promise<boolean> {
		try {
			if (path === "/") {
				await filesystem.stat(path);
			} else {
				await filesystem.lstat(path);
			}
			return true;
		} catch {
			return false;
		}
	}

	async function existsInUpper(path: string): Promise<boolean> {
		if (!upper) {
			return false;
		}
		return existsInFilesystem(upper, path);
	}

	async function hasEntryInUpper(path: string): Promise<boolean> {
		if (!upper) {
			return false;
		}
		return hasEntryInFilesystem(upper, path);
	}

	async function findLowerByExists(
		path: string,
	): Promise<VirtualFileSystem | null> {
		for (const lower of lowers) {
			if (await existsInFilesystem(lower, path)) {
				return lower;
			}
		}
		return null;
	}

	async function findLowerByEntry(
		path: string,
	): Promise<{ filesystem: VirtualFileSystem; stat: VirtualStat } | null> {
		for (const lower of lowers) {
			try {
				return {
					filesystem: lower,
					stat: path === "/" ? await lower.stat(path) : await lower.lstat(path),
				};
			} catch {
				// Try the next lower layer.
			}
		}
		return null;
	}

	async function mergedLstat(path: string): Promise<VirtualStat> {
		if (isWhitedOut(path)) {
			throw new KernelError("ENOENT", `no such file: ${path}`);
		}
		if (await hasEntryInUpper(path)) {
			return upper!.lstat(path);
		}
		const lower = await findLowerByEntry(path);
		if (!lower) {
			throw new KernelError("ENOENT", `no such file: ${path}`);
		}
		return lower.stat;
	}

	async function ensureAncestorDirectoriesInUpper(path: string): Promise<void> {
		if (!upper) {
			throwReadOnly();
		}

		const normalized = normPath(path);
		const parts = normalized.split("/").filter(Boolean);
		let current = "";

		for (let index = 0; index < parts.length - 1; index++) {
			current += `/${parts[index]}`;
			if (await existsInUpper(current)) {
				continue;
			}

			const lower = await findLowerByExists(current);
			if (lower) {
				const stat = await lower.stat(current);
				if (!stat.isDirectory) {
					throw new KernelError("ENOTDIR", `not a directory: ${current}`);
				}
				await upper.mkdir(current);
				await upper.chmod(current, stat.mode);
				await upper.chown(current, stat.uid, stat.gid);
				continue;
			}

			await upper.mkdir(current);
		}
	}

	async function copyUpPath(path: string): Promise<void> {
		if (!upper) {
			throwReadOnly();
		}
		if (await hasEntryInUpper(path)) {
			return;
		}

		await ensureAncestorDirectoriesInUpper(path);

		const lower = await findLowerByEntry(path);
		if (!lower) {
			throw new KernelError("ENOENT", `no such file: ${path}`);
		}

		if (lower.stat.isSymbolicLink) {
			const target = await lower.filesystem.readlink(path);
			await upper.symlink(target, path);
			return;
		}

		if (lower.stat.isDirectory) {
			await upper.mkdir(path);
			await upper.chmod(path, lower.stat.mode);
			await upper.chown(path, lower.stat.uid, lower.stat.gid);
			return;
		}

		const data = await lower.filesystem.readFile(path);
		await upper.writeFile(path, data);
		await upper.chmod(path, lower.stat.mode);
		await upper.chown(path, lower.stat.uid, lower.stat.gid);
	}

	async function pathExistsInMergedView(path: string): Promise<boolean> {
		if (isWhitedOut(path)) {
			return false;
		}
		if (await hasEntryInUpper(path)) {
			return true;
		}
		return (await findLowerByEntry(path)) !== null;
	}

	const backend: VirtualFileSystem = {
		async readFile(path: string): Promise<Uint8Array> {
			if (isWhitedOut(path)) {
				throw new KernelError("ENOENT", `no such file: ${path}`);
			}
			if (await existsInUpper(path)) {
				return upper!.readFile(path);
			}
			const lower = await findLowerByExists(path);
			if (!lower) {
				throw new KernelError("ENOENT", `no such file: ${path}`);
			}
			return lower.readFile(path);
		},

		async readTextFile(path: string): Promise<string> {
			if (isWhitedOut(path)) {
				throw new KernelError("ENOENT", `no such file: ${path}`);
			}
			if (await existsInUpper(path)) {
				return upper!.readTextFile(path);
			}
			const lower = await findLowerByExists(path);
			if (!lower) {
				throw new KernelError("ENOENT", `no such file: ${path}`);
			}
			return lower.readTextFile(path);
		},

		async readDir(path: string): Promise<string[]> {
			if (isWhitedOut(path)) {
				throw new KernelError("ENOENT", `no such directory: ${path}`);
			}

			let directoryExists = false;
			const entries = new Set<string>();

			for (let index = lowers.length - 1; index >= 0; index--) {
				try {
					const lowerEntries = await lowers[index].readDir(path);
					directoryExists = true;
					for (const entry of lowerEntries) {
						if (entry === "." || entry === "..") continue;
						const childPath = posixPath.join(normPath(path), entry);
						if (!isWhitedOut(childPath)) {
							entries.add(entry);
						}
					}
				} catch {
					// This lower does not contribute a directory here.
				}
			}

			if (upper) {
				try {
					const upperEntries = await upper.readDir(path);
					directoryExists = true;
					for (const entry of upperEntries) {
						if (entry === "." || entry === "..") continue;
						entries.add(entry);
					}
				} catch {
					// No upper directory at this path.
				}
			}

			if (!directoryExists) {
				throw new KernelError("ENOENT", `no such directory: ${path}`);
			}

			return [...entries];
		},

		async readDirWithTypes(path: string): Promise<VirtualDirEntry[]> {
			if (isWhitedOut(path)) {
				throw new KernelError("ENOENT", `no such directory: ${path}`);
			}

			let directoryExists = false;
			const entriesByName = new Map<string, VirtualDirEntry>();

			for (let index = lowers.length - 1; index >= 0; index--) {
				try {
					const lowerEntries = await lowers[index].readDirWithTypes(path);
					directoryExists = true;
					for (const entry of lowerEntries) {
						if (entry.name === "." || entry.name === "..") continue;
						const childPath = posixPath.join(normPath(path), entry.name);
						if (!isWhitedOut(childPath)) {
							entriesByName.set(entry.name, entry);
						}
					}
				} catch {
					// This lower does not contribute a directory here.
				}
			}

			if (upper) {
				try {
					const upperEntries = await upper.readDirWithTypes(path);
					directoryExists = true;
					for (const entry of upperEntries) {
						if (entry.name === "." || entry.name === "..") continue;
						entriesByName.set(entry.name, entry);
					}
				} catch {
					// No upper directory at this path.
				}
			}

			if (!directoryExists) {
				throw new KernelError("ENOENT", `no such directory: ${path}`);
			}

			return [...entriesByName.values()];
		},

		async writeFile(
			path: string,
			content: string | Uint8Array,
		): Promise<void> {
			if (!upper) {
				throwReadOnly();
			}
			removeWhiteout(path);
			if (await findLowerByEntry(path)) {
				await copyUpPath(path);
			} else {
				await ensureAncestorDirectoriesInUpper(path);
			}
			return upper.writeFile(path, content);
		},

		async createDir(path: string): Promise<void> {
			if (!upper) {
				throwReadOnly();
			}
			removeWhiteout(path);
			if (await pathExistsInMergedView(path)) {
				throw new KernelError("EEXIST", `file exists: ${path}`);
			}
			await ensureAncestorDirectoriesInUpper(path);
			return upper.createDir(path);
		},

		async mkdir(
			path: string,
			options?: { recursive?: boolean },
		): Promise<void> {
			removeWhiteout(path);
			if (await pathExistsInMergedView(path)) {
				const stat = await mergedLstat(path);
				if (options?.recursive && stat.isDirectory && !stat.isSymbolicLink) {
					return;
				}
				throw new KernelError("EEXIST", `file exists: ${path}`);
			}
			if (!upper) {
				throwReadOnly();
			}
			await ensureAncestorDirectoriesInUpper(path);
			return upper.mkdir(path, options);
		},

		async exists(path: string): Promise<boolean> {
			return pathExistsInMergedView(path);
		},

		async stat(path: string): Promise<VirtualStat> {
			if (isWhitedOut(path)) {
				throw new KernelError("ENOENT", `no such file: ${path}`);
			}
			if (await existsInUpper(path)) {
				return upper!.stat(path);
			}
			const lower = await findLowerByExists(path);
			if (!lower) {
				throw new KernelError("ENOENT", `no such file: ${path}`);
			}
			return lower.stat(path);
		},

		async removeFile(path: string): Promise<void> {
			if (isWhitedOut(path)) {
				throw new KernelError("ENOENT", `no such file: ${path}`);
			}
			const lower = await findLowerByExists(path);
			const upperExists = await existsInUpper(path);
			if (!upperExists && !lower) {
				throw new KernelError("ENOENT", `no such file: ${path}`);
			}
			if (!upper) {
				throwReadOnly();
			}
			if (upperExists) {
				await upper.removeFile(path);
			}
			addWhiteout(path);
		},

		async removeDir(path: string): Promise<void> {
			if (isWhitedOut(path)) {
				throw new KernelError("ENOENT", `no such directory: ${path}`);
			}
			const lower = await findLowerByExists(path);
			const upperExists = await existsInUpper(path);
			if (!upperExists && !lower) {
				throw new KernelError("ENOENT", `no such directory: ${path}`);
			}
			if (!upper) {
				throwReadOnly();
			}
			if (upperExists) {
				await upper.removeDir(path);
			}
			addWhiteout(path);
		},

		async rename(oldPath: string, newPath: string): Promise<void> {
			if (!upper) {
				throwReadOnly();
			}
			const data = await backend.readFile(oldPath);
			await backend.writeFile(newPath, data);
			await backend.removeFile(oldPath);
		},

		async realpath(path: string): Promise<string> {
			if (isWhitedOut(path)) {
				throw new KernelError("ENOENT", `no such file: ${path}`);
			}
			if (await existsInUpper(path)) {
				return upper!.realpath(path);
			}
			const lower = await findLowerByExists(path);
			if (!lower) {
				throw new KernelError("ENOENT", `no such file: ${path}`);
			}
			return lower.realpath(path);
		},

		async symlink(target: string, linkPath: string): Promise<void> {
			if (!upper) {
				throwReadOnly();
			}
			removeWhiteout(linkPath);
			await ensureAncestorDirectoriesInUpper(linkPath);
			return upper.symlink(target, linkPath);
		},

		async readlink(path: string): Promise<string> {
			if (isWhitedOut(path)) {
				throw new KernelError("ENOENT", `no such file: ${path}`);
			}
			if (await hasEntryInUpper(path)) {
				return upper!.readlink(path);
			}
			const lower = await findLowerByEntry(path);
			if (!lower) {
				throw new KernelError("ENOENT", `no such file: ${path}`);
			}
			return lower.filesystem.readlink(path);
		},

		async lstat(path: string): Promise<VirtualStat> {
			if (isWhitedOut(path)) {
				throw new KernelError("ENOENT", `no such file: ${path}`);
			}
			if (await hasEntryInUpper(path)) {
				return path === "/" ? upper!.stat(path) : upper!.lstat(path);
			}
			const lower = await findLowerByEntry(path);
			if (!lower) {
				throw new KernelError("ENOENT", `no such file: ${path}`);
			}
			return lower.stat;
		},

		async link(oldPath: string, newPath: string): Promise<void> {
			if (!upper) {
				throwReadOnly();
			}
			removeWhiteout(newPath);
			await copyUpPath(oldPath);
			await ensureAncestorDirectoriesInUpper(newPath);
			return upper.link(oldPath, newPath);
		},

		async chmod(path: string, modeValue: number): Promise<void> {
			if (isWhitedOut(path)) {
				throw new KernelError("ENOENT", `no such file: ${path}`);
			}
			if (!upper) {
				throwReadOnly();
			}
			if (!(await existsInUpper(path))) {
				await copyUpPath(path);
			}
			return upper.chmod(path, modeValue);
		},

		async chown(path: string, uid: number, gid: number): Promise<void> {
			if (isWhitedOut(path)) {
				throw new KernelError("ENOENT", `no such file: ${path}`);
			}
			if (!upper) {
				throwReadOnly();
			}
			if (!(await existsInUpper(path))) {
				await copyUpPath(path);
			}
			return upper.chown(path, uid, gid);
		},

		async utimes(path: string, atime: number, mtime: number): Promise<void> {
			if (isWhitedOut(path)) {
				throw new KernelError("ENOENT", `no such file: ${path}`);
			}
			if (!upper) {
				throwReadOnly();
			}
			if (!(await existsInUpper(path))) {
				await copyUpPath(path);
			}
			await upper.utimes(path, atime, mtime);
			const updated = await upper.stat(path);
			// Some backends interpret utimes inputs as seconds rather than
			// milliseconds. Normalize them here so the overlay presents a
			// consistent millisecond-based contract.
			if (updated.atimeMs === atime * 1000 && updated.mtimeMs === mtime * 1000) {
				await upper.utimes(path, atime / 1000, mtime / 1000);
			}
		},

		async truncate(path: string, length: number): Promise<void> {
			if (isWhitedOut(path)) {
				throw new KernelError("ENOENT", `no such file: ${path}`);
			}
			if (!upper) {
				throwReadOnly();
			}
			if (!(await existsInUpper(path))) {
				await copyUpPath(path);
			}
			return upper.truncate(path, length);
		},

		async pread(
			path: string,
			offset: number,
			length: number,
		): Promise<Uint8Array> {
			if (isWhitedOut(path)) {
				throw new KernelError("ENOENT", `no such file: ${path}`);
			}
			if (await existsInUpper(path)) {
				return upper!.pread(path, offset, length);
			}
			const lower = await findLowerByExists(path);
			if (!lower) {
				throw new KernelError("ENOENT", `no such file: ${path}`);
			}
			return lower.pread(path, offset, length);
		},

		async pwrite(
			path: string,
			offset: number,
			data: Uint8Array,
		): Promise<void> {
			if (isWhitedOut(path)) {
				throw new KernelError("ENOENT", `no such file: ${path}`);
			}
			if (!upper) {
				throwReadOnly();
			}
			if (!(await existsInUpper(path))) {
				await copyUpPath(path);
			}
			return upper.pwrite(path, offset, data);
		},
	};

	return backend;
}
