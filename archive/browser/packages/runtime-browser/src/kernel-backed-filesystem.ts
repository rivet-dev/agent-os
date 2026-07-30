// A VirtualFileSystem backed by the wasm kernel over the wire.
//
// Lets the existing naive-Node module resolver (`resolveModule` in runtime.ts)
// run unchanged in the converged path by giving it a filesystem whose every
// operation is a `guest_filesystem_call` to the kernel, instead of the legacy
// in-process TS filesystem. Resolution therefore reads exactly the kernel's
// view (mounts, symlinks, exports/conditions), keeping it faithful on both
// backends (the CLAUDE.md npm-compat rule).
//
// Methods are async to satisfy `VirtualFileSystem`, but the underlying transport
// is synchronous (`pushFrame`); the promises resolve on the microtask queue.

import { decodeBase64, encodeBase64 } from "./converged-base64.js";
import {
	wireStatToVirtualStat,
	type GuestFilesystemResult,
} from "./converged-fs-bridge.js";
import type { ConvergedSidecarRequestTransport } from "./converged-sync-bridge-handler.js";
import type { LiveRequestPayload } from "@rivet-dev/agentos-runtime-core/request-payloads";
import type {
	VirtualDirEntry,
	VirtualFileSystem,
	VirtualStat,
} from "./runtime.js";

export class KernelBackedFilesystem implements VirtualFileSystem {
	private readonly transport: ConvergedSidecarRequestTransport;

	constructor(transport: ConvergedSidecarRequestTransport) {
		this.transport = transport;
	}

	private call(
		payload: Extract<LiveRequestPayload, { type: "guest_filesystem_call" }>,
	): GuestFilesystemResult {
		const response = this.transport.sendRequest(payload);
		if (response.type !== "guest_filesystem_result") {
			throw new Error(
				`expected guest_filesystem_result, got ${response.type}`,
			);
		}
		return response;
	}

	async readFile(path: string): Promise<Uint8Array> {
		const result = this.call({
			type: "guest_filesystem_call",
			operation: "read_file",
			path,
		});
		const content = result.content ?? "";
		return result.encoding === "base64"
			? decodeBase64(content)
			: new TextEncoder().encode(content);
	}

	async readTextFile(path: string): Promise<string> {
		const result = this.call({
			type: "guest_filesystem_call",
			operation: "read_file",
			path,
		});
		const content = result.content ?? "";
		return result.encoding === "base64"
			? new TextDecoder().decode(decodeBase64(content))
			: content;
	}

	async readDir(path: string): Promise<string[]> {
		const result = this.call({
			type: "guest_filesystem_call",
			operation: "read_dir",
			path,
		});
		return (result.entries ?? []).map((entry) => entry.name);
	}

	async readDirWithTypes(path: string): Promise<VirtualDirEntry[]> {
		// The wire `read_dir` carries each child's type, so module resolution
		// gets typed entries in one round-trip (no per-entry lstat).
		const result = this.call({
			type: "guest_filesystem_call",
			operation: "read_dir",
			path,
		});
		return (result.entries ?? []).map((entry) => ({
			name: entry.name,
			isDirectory: entry.isDirectory,
			isSymbolicLink: entry.isSymbolicLink,
		}));
	}

	async writeFile(path: string, content: string | Uint8Array): Promise<void> {
		const payload =
			typeof content === "string"
				? { content, encoding: "utf8" as const }
				: { content: encodeBase64(content), encoding: "base64" as const };
		this.call({
			type: "guest_filesystem_call",
			operation: "write_file",
			path,
			content: payload.content,
			encoding: payload.encoding,
		});
	}

	async createDir(path: string): Promise<void> {
		this.call({ type: "guest_filesystem_call", operation: "create_dir", path });
	}

	async mkdir(path: string, options?: { recursive?: boolean }): Promise<void> {
		this.call({
			type: "guest_filesystem_call",
			operation: "mkdir",
			path,
			recursive: options?.recursive ?? false,
		});
	}

	async exists(path: string): Promise<boolean> {
		return this.call({ type: "guest_filesystem_call", operation: "exists", path })
			.exists ?? false;
	}

	async stat(path: string): Promise<VirtualStat> {
		return this.requireStat("stat", path);
	}

	async lstat(path: string): Promise<VirtualStat> {
		return this.requireStat("lstat", path);
	}

	private requireStat(
		operation: "stat" | "lstat",
		path: string,
	): VirtualStat {
		const result = this.call({
			type: "guest_filesystem_call",
			operation,
			path,
		});
		if (!result.stat) {
			throw new Error(`${operation} for ${path} returned no stat`);
		}
		return wireStatToVirtualStat(result.stat);
	}

	async removeFile(path: string): Promise<void> {
		this.call({ type: "guest_filesystem_call", operation: "remove_file", path });
	}

	async removeDir(path: string): Promise<void> {
		this.call({ type: "guest_filesystem_call", operation: "remove_dir", path });
	}

	async rename(oldPath: string, newPath: string): Promise<void> {
		this.call({
			type: "guest_filesystem_call",
			operation: "rename",
			path: oldPath,
			destination_path: newPath,
		});
	}

	async realpath(path: string): Promise<string> {
		return (
			this.call({ type: "guest_filesystem_call", operation: "realpath", path })
				.target ?? path
		);
	}

	async symlink(target: string, linkPath: string): Promise<void> {
		this.call({
			type: "guest_filesystem_call",
			operation: "symlink",
			path: linkPath,
			target,
		});
	}

	async readlink(path: string): Promise<string> {
		return (
			this.call({
				type: "guest_filesystem_call",
				operation: "read_link",
				path,
			}).target ?? ""
		);
	}

	async link(oldPath: string, newPath: string): Promise<void> {
		this.call({
			type: "guest_filesystem_call",
			operation: "link",
			path: oldPath,
			destination_path: newPath,
		});
	}

	async chmod(path: string, mode: number): Promise<void> {
		this.call({ type: "guest_filesystem_call", operation: "chmod", path, mode });
	}

	async chown(path: string, uid: number, gid: number): Promise<void> {
		this.call({
			type: "guest_filesystem_call",
			operation: "chown",
			path,
			uid,
			gid,
		});
	}

	async utimes(path: string, atime: number, mtime: number): Promise<void> {
		this.call({
			type: "guest_filesystem_call",
			operation: "utimes",
			path,
			atime_ms: atime,
			mtime_ms: mtime,
		});
	}

	async truncate(path: string, length: number): Promise<void> {
		this.call({
			type: "guest_filesystem_call",
			operation: "truncate",
			path,
			len: length,
		});
	}

	async pread(path: string, offset: number, length: number): Promise<Uint8Array> {
		const result = this.call({
			type: "guest_filesystem_call",
			operation: "pread",
			path,
			offset,
			len: length,
		});
		const content = result.content ?? "";
		return result.encoding === "base64"
			? decodeBase64(content)
			: new TextEncoder().encode(content);
	}

	async pwrite(): Promise<void> {
		// The kernel guest-filesystem wire surface has no positional write; the
		// module resolver (the converged consumer) never calls it.
		throw new Error("ENOSYS: pwrite is not supported by the kernel-backed filesystem");
	}
}
