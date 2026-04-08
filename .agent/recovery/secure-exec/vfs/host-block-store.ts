/**
 * Host filesystem-backed FsBlockStore for local dev environments.
 *
 * Stores blocks as files on the host filesystem. Block key "ino/chunkIndex"
 * maps to file at "{baseDir}/ino/chunkIndex". Directories are created on
 * demand for inode subdirectories.
 */

import * as fs from "node:fs/promises";
import * as path from "node:path";
import { KernelError } from "../kernel/types.js";
import type { FsBlockStore } from "./types.js";

export class HostBlockStore implements FsBlockStore {
	private baseDir: string;

	constructor(baseDir: string) {
		this.baseDir = baseDir;
	}

	private keyToPath(key: string): string {
		return path.join(this.baseDir, key);
	}

	async read(key: string): Promise<Uint8Array> {
		const filePath = this.keyToPath(key);
		try {
			const buf = await fs.readFile(filePath);
			return new Uint8Array(buf);
		} catch (err: unknown) {
			if (isNodeError(err) && err.code === "ENOENT") {
				throw new KernelError("ENOENT", `block not found: ${key}`);
			}
			throw err;
		}
	}

	async readRange(
		key: string,
		offset: number,
		length: number,
	): Promise<Uint8Array> {
		const filePath = this.keyToPath(key);
		let handle: fs.FileHandle | undefined;
		try {
			handle = await fs.open(filePath, "r");
			const stat = await handle.stat();
			const available = Math.max(0, stat.size - offset);
			const toRead = Math.min(length, available);
			if (toRead === 0) {
				return new Uint8Array(0);
			}
			const buf = Buffer.alloc(toRead);
			const { bytesRead } = await handle.read(buf, 0, toRead, offset);
			return new Uint8Array(buf.buffer, buf.byteOffset, bytesRead);
		} catch (err: unknown) {
			if (isNodeError(err) && err.code === "ENOENT") {
				throw new KernelError("ENOENT", `block not found: ${key}`);
			}
			throw err;
		} finally {
			if (handle) await handle.close();
		}
	}

	async write(key: string, data: Uint8Array): Promise<void> {
		const filePath = this.keyToPath(key);
		const dir = path.dirname(filePath);
		await fs.mkdir(dir, { recursive: true });
		await fs.writeFile(filePath, data);
	}

	async delete(key: string): Promise<void> {
		const filePath = this.keyToPath(key);
		try {
			await fs.unlink(filePath);
		} catch (err: unknown) {
			if (isNodeError(err) && err.code === "ENOENT") {
				return;
			}
			throw err;
		}
	}

	async deleteMany(keys: string[]): Promise<void> {
		await Promise.all(keys.map((key) => this.delete(key)));
	}

	async copy(srcKey: string, dstKey: string): Promise<void> {
		const srcPath = this.keyToPath(srcKey);
		const dstPath = this.keyToPath(dstKey);
		const dstDir = path.dirname(dstPath);
		try {
			await fs.mkdir(dstDir, { recursive: true });
			await fs.copyFile(srcPath, dstPath);
		} catch (err: unknown) {
			if (isNodeError(err) && err.code === "ENOENT") {
				throw new KernelError("ENOENT", `block not found: ${srcKey}`);
			}
			throw err;
		}
	}
}

function isNodeError(err: unknown): err is NodeJS.ErrnoException {
	return err instanceof Error && "code" in err;
}
