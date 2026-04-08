/**
 * Node.js filesystem adapter for kernel integration.
 *
 * Implements VirtualFileSystem by delegating to node:fs/promises.
 * When the kernel uses a HostNodeFileSystem, file operations go to the
 * real host filesystem (sandboxed by the root path).
 */

import * as fs from "node:fs/promises";
import * as fsSync from "node:fs";
import * as path from "node:path";
import type { VirtualFileSystem, VirtualStat, VirtualDirEntry } from "@secure-exec/core";
import { KernelError, O_CREAT, O_EXCL, O_TRUNC } from "@secure-exec/core";

export interface HostNodeFileSystemOptions {
	/** Root directory on the host — all paths are relative to this. */
	root?: string;
}

export class HostNodeFileSystem implements VirtualFileSystem {
	private root: string;

	constructor(options?: HostNodeFileSystemOptions) {
		this.root = options?.root ?? "/";
	}

	private resolve(p: string): string {
		// Map virtual path to host path under root
		const normalized = path.posix.normalize(p);
		return path.join(this.root, normalized);
	}

	prepareOpenSync(p: string, flags: number): boolean {
		const hostPath = this.resolve(p);
		const hasCreate = (flags & O_CREAT) !== 0;
		const hasExcl = (flags & O_EXCL) !== 0;
		const hasTrunc = (flags & O_TRUNC) !== 0;
		const exists = fsSync.existsSync(hostPath);

		if (hasCreate && hasExcl && exists) {
			throw new KernelError("EEXIST", `file already exists, open '${p}'`);
		}

		let created = false;
		if (!exists && hasCreate) {
			fsSync.mkdirSync(path.dirname(hostPath), { recursive: true });
			fsSync.writeFileSync(hostPath, new Uint8Array(0));
			created = true;
		}

		if (hasTrunc) {
			try {
				fsSync.truncateSync(hostPath, 0);
			} catch (error) {
				const err = error as NodeJS.ErrnoException;
				if (err.code === "ENOENT") {
					throw new KernelError("ENOENT", `no such file or directory, open '${p}'`);
				}
				if (err.code === "EISDIR") {
					throw new KernelError("EISDIR", `illegal operation on a directory, open '${p}'`);
				}
				throw error;
			}
		}

		return created;
	}

	async readFile(p: string): Promise<Uint8Array> {
		return new Uint8Array(await fs.readFile(this.resolve(p)));
	}

	async readTextFile(p: string): Promise<string> {
		return fs.readFile(this.resolve(p), "utf-8");
	}

	async readDir(p: string): Promise<string[]> {
		return fs.readdir(this.resolve(p));
	}

	async readDirWithTypes(p: string): Promise<VirtualDirEntry[]> {
		const entries = await fs.readdir(this.resolve(p), {
			withFileTypes: true,
		});
		return entries.map((e) => ({
			name: e.name,
			isDirectory: e.isDirectory(),
			isSymbolicLink: e.isSymbolicLink(),
		}));
	}

	async writeFile(p: string, content: string | Uint8Array): Promise<void> {
		const hostPath = this.resolve(p);
		await fs.mkdir(path.dirname(hostPath), { recursive: true });
		await fs.writeFile(hostPath, content);
	}

	async createDir(p: string): Promise<void> {
		await fs.mkdir(this.resolve(p));
	}

	async mkdir(p: string, options?: { recursive?: boolean }): Promise<void> {
		await fs.mkdir(this.resolve(p), { recursive: options?.recursive ?? true });
	}

	async exists(p: string): Promise<boolean> {
		try {
			await fs.access(this.resolve(p));
			return true;
		} catch {
			return false;
		}
	}

	async stat(p: string): Promise<VirtualStat> {
		const s = await fs.stat(this.resolve(p));
		return toVirtualStat(s);
	}

	async removeFile(p: string): Promise<void> {
		await fs.unlink(this.resolve(p));
	}

	async removeDir(p: string): Promise<void> {
		await fs.rmdir(this.resolve(p));
	}

	async rename(oldPath: string, newPath: string): Promise<void> {
		await fs.rename(this.resolve(oldPath), this.resolve(newPath));
	}

	async realpath(p: string): Promise<string> {
		return fs.realpath(this.resolve(p));
	}

	async symlink(target: string, linkPath: string): Promise<void> {
		await fs.symlink(target, this.resolve(linkPath));
	}

	async readlink(p: string): Promise<string> {
		return fs.readlink(this.resolve(p));
	}

	async lstat(p: string): Promise<VirtualStat> {
		const s = await fs.lstat(this.resolve(p));
		return toVirtualStat(s);
	}

	async link(oldPath: string, newPath: string): Promise<void> {
		await fs.link(this.resolve(oldPath), this.resolve(newPath));
	}

	async chmod(p: string, mode: number): Promise<void> {
		await fs.chmod(this.resolve(p), mode);
	}

	async chown(p: string, uid: number, gid: number): Promise<void> {
		await fs.chown(this.resolve(p), uid, gid);
	}

	async utimes(p: string, atime: number, mtime: number): Promise<void> {
		await fs.utimes(this.resolve(p), atime / 1000, mtime / 1000);
	}

	async truncate(p: string, length: number): Promise<void> {
		await fs.truncate(this.resolve(p), length);
	}

	async pread(p: string, offset: number, length: number): Promise<Uint8Array> {
		const handle = await fs.open(this.resolve(p), "r");
		try {
			const buf = new Uint8Array(length);
			const { bytesRead } = await handle.read(buf, 0, length, offset);
			return bytesRead < length ? buf.slice(0, bytesRead) : buf;
		} finally {
			await handle.close();
		}
	}

	async pwrite(p: string, offset: number, data: Uint8Array): Promise<void> {
		const handle = await fs.open(this.resolve(p), "r+");
		try {
			await handle.write(data, 0, data.length, offset);
		} finally {
			await handle.close();
		}
	}
}

function toVirtualStat(s: import("node:fs").Stats): VirtualStat {
	return {
		mode: s.mode,
		size: s.size,
		isDirectory: s.isDirectory(),
		isSymbolicLink: s.isSymbolicLink(),
		atimeMs: s.atimeMs,
		mtimeMs: s.mtimeMs,
		ctimeMs: s.ctimeMs,
		birthtimeMs: s.birthtimeMs,
		ino: s.ino,
		nlink: s.nlink,
		uid: s.uid,
		gid: s.gid,
	};
}
