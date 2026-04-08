/**
 * Pure JS Map-based FsBlockStore for ephemeral VMs and tests.
 *
 * All blocks live in memory as Uint8Array values keyed by string.
 * Suitable for short-lived processes where persistence is not needed.
 */

import { KernelError } from "../kernel/types.js";
import type { FsBlockStore } from "./types.js";

export class InMemoryBlockStore implements FsBlockStore {
	private blocks = new Map<string, Uint8Array>();

	async read(key: string): Promise<Uint8Array> {
		const data = this.blocks.get(key);
		if (!data) {
			throw new KernelError("ENOENT", `block not found: ${key}`);
		}
		return data;
	}

	async readRange(
		key: string,
		offset: number,
		length: number,
	): Promise<Uint8Array> {
		const data = this.blocks.get(key);
		if (!data) {
			throw new KernelError("ENOENT", `block not found: ${key}`);
		}
		// Short read: return available bytes if range extends beyond block size.
		const end = Math.min(offset + length, data.length);
		return data.slice(offset, end);
	}

	async write(key: string, data: Uint8Array): Promise<void> {
		this.blocks.set(key, new Uint8Array(data));
	}

	async delete(key: string): Promise<void> {
		this.blocks.delete(key);
	}

	async deleteMany(keys: string[]): Promise<void> {
		for (const key of keys) {
			this.blocks.delete(key);
		}
	}

	async copy(srcKey: string, dstKey: string): Promise<void> {
		const data = this.blocks.get(srcKey);
		if (!data) {
			throw new KernelError("ENOENT", `block not found: ${srcKey}`);
		}
		// Create a new copy, not a reference.
		this.blocks.set(dstKey, new Uint8Array(data));
	}
}
