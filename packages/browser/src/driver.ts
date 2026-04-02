import type {
	Permissions,
	VirtualFileSystem,
} from "./runtime.js";
import type {
	NetworkAdapter,
	SystemDriver,
} from "./runtime.js";
import {
	createCommandExecutorStub,
	createFsStub,
	createNetworkStub,
	wrapFileSystem,
	wrapNetworkAdapter,
	createInMemoryFileSystem,
	createEnosysError,
} from "./runtime.js";

const S_IFREG = 0o100000;
const S_IFDIR = 0o040000;

const BROWSER_SYSTEM_DRIVER_OPTIONS = Symbol.for(
	"agent-os.browserSystemDriverOptions",
);

export interface BrowserRuntimeSystemOptions {
	filesystem: "opfs" | "memory";
	networkEnabled: boolean;
}

type BrowserSystemDriver = SystemDriver & {
	[BROWSER_SYSTEM_DRIVER_OPTIONS]?: BrowserRuntimeSystemOptions;
};

function normalizePath(path: string): string {
	if (!path) return "/";
	let normalized = path.startsWith("/") ? path : `/${path}`;
	normalized = normalized.replace(/\/+/g, "/");
	if (normalized.length > 1 && normalized.endsWith("/")) {
		normalized = normalized.slice(0, -1);
	}
	return normalized;
}

function splitPath(path: string): string[] {
	const normalized = normalizePath(path);
	return normalized === "/" ? [] : normalized.slice(1).split("/");
}

function dirname(path: string): string {
	const parts = splitPath(path);
	if (parts.length <= 1) return "/";
	return `/${parts.slice(0, -1).join("/")}`;
}

async function getRootHandle(): Promise<FileSystemDirectoryHandle> {
	if (!("storage" in navigator) || !("getDirectory" in navigator.storage)) {
		throw createEnosysError("opfs");
	}
	return navigator.storage.getDirectory();
}

/**
 * VFS backed by the Origin Private File System (OPFS) API. Falls back to
 * InMemoryFileSystem when OPFS is unavailable. Rename is not supported
 * (throws ENOSYS) since OPFS doesn't provide atomic rename.
 */
export class OpfsFileSystem implements VirtualFileSystem {
	private rootPromise: Promise<FileSystemDirectoryHandle>;

	constructor() {
		this.rootPromise = getRootHandle();
	}

	private async getDirHandle(path: string, create = false): Promise<FileSystemDirectoryHandle> {
		const root = await this.rootPromise;
		const parts = splitPath(path);
		let current = root;
		for (const part of parts) {
			current = await current.getDirectoryHandle(part, { create });
		}
		return current;
	}

	private async getFileHandle(
		path: string,
		create = false,
	): Promise<FileSystemFileHandle> {
		const normalized = normalizePath(path);
		const parent = dirname(normalized);
		const name = normalized.split("/").pop() || "";
		const dir = await this.getDirHandle(parent, create);
		return dir.getFileHandle(name, { create });
	}

	async readFile(path: string): Promise<Uint8Array> {
		const handle = await this.getFileHandle(path);
		const file = await handle.getFile();
		const buffer = await file.arrayBuffer();
		return new Uint8Array(buffer);
	}

	async readTextFile(path: string): Promise<string> {
		const handle = await this.getFileHandle(path);
		const file = await handle.getFile();
		return file.text();
	}

	async readDir(path: string): Promise<string[]> {
		const dir = await this.getDirHandle(path);
		const entries: string[] = [];
		for await (const [name] of dir.entries()) {
			entries.push(name);
		}
		return entries;
	}

	async readDirWithTypes(
		path: string,
	): Promise<Array<{ name: string; isDirectory: boolean }>> {
		const dir = await this.getDirHandle(path);
		const entries: Array<{ name: string; isDirectory: boolean }> = [];
		for await (const [name, handle] of dir.entries()) {
			entries.push({
				name,
				isDirectory: handle.kind === "directory",
			});
		}
		return entries;
	}

	async writeFile(path: string, content: string | Uint8Array): Promise<void> {
		const normalized = normalizePath(path);
		await this.mkdir(dirname(normalized));
		const handle = await this.getFileHandle(normalized, true);
		const writable = await handle.createWritable();
		if (typeof content === "string") {
			await writable.write(content);
		} else {
			await writable.write(content as unknown as FileSystemWriteChunkType);
		}
		await writable.close();
	}

	async createDir(path: string): Promise<void> {
		const normalized = normalizePath(path);
		const parent = dirname(normalized);
		await this.getDirHandle(parent, false);
		await this.getDirHandle(normalized, true);
	}

	async mkdir(path: string, _options?: { recursive?: boolean }): Promise<void> {
		const parts = splitPath(path);
		let current = "";
		for (const part of parts) {
			current += `/${part}`;
			await this.getDirHandle(current, true);
		}
	}

	async exists(path: string): Promise<boolean> {
		try {
			await this.getFileHandle(path);
			return true;
		} catch {
			try {
				await this.getDirHandle(path);
				return true;
			} catch {
				return false;
			}
		}
	}

	async stat(path: string) {
		try {
			const handle = await this.getFileHandle(path);
			const file = await handle.getFile();
			return {
				mode: S_IFREG | 0o644,
				size: file.size,
				isDirectory: false,
				isSymbolicLink: false,
				atimeMs: file.lastModified,
				mtimeMs: file.lastModified,
				ctimeMs: file.lastModified,
				birthtimeMs: file.lastModified,
				ino: 0,
				nlink: 1,
				uid: 0,
				gid: 0,
			};
		} catch {
			const normalized = normalizePath(path);
			try {
				await this.getDirHandle(normalized);
				const now = Date.now();
				return {
					mode: S_IFDIR | 0o755,
					size: 4096,
					isDirectory: true,
					isSymbolicLink: false,
					atimeMs: now,
					mtimeMs: now,
					ctimeMs: now,
					birthtimeMs: now,
					ino: 0,
					nlink: 2,
					uid: 0,
					gid: 0,
				};
			} catch {
				throw new Error(
					`ENOENT: no such file or directory, stat '${normalized}'`,
				);
			}
		}
	}

	async removeFile(path: string): Promise<void> {
		const normalized = normalizePath(path);
		const parent = dirname(normalized);
		const name = normalized.split("/").pop() || "";
		const dir = await this.getDirHandle(parent);
		await dir.removeEntry(name);
	}

	async removeDir(path: string): Promise<void> {
		const normalized = normalizePath(path);
		if (normalized === "/") {
			throw new Error("EPERM: operation not permitted, rmdir '/'");
		}
		const parent = dirname(normalized);
		const name = normalized.split("/").pop() || "";
		const dir = await this.getDirHandle(parent);
		await dir.removeEntry(name);
	}

	async rename(_oldPath: string, _newPath: string): Promise<void> {
		throw createEnosysError("rename");
	}

	async symlink(_target: string, _linkPath: string): Promise<void> {
		throw createEnosysError("symlink");
	}

	async readlink(_path: string): Promise<string> {
		throw createEnosysError("readlink");
	}

	async lstat(path: string) {
		return this.stat(path);
	}

	async link(_oldPath: string, _newPath: string): Promise<void> {
		throw createEnosysError("link");
	}

	async chmod(_path: string, _mode: number): Promise<void> {
		// No-op: OPFS does not support POSIX permissions
	}

	async chown(_path: string, _uid: number, _gid: number): Promise<void> {
		// No-op: OPFS does not support POSIX ownership
	}

	async utimes(_path: string, _atime: number, _mtime: number): Promise<void> {
		// No-op: OPFS does not support timestamp manipulation
	}

	async truncate(path: string, length: number): Promise<void> {
		const handle = await this.getFileHandle(path);
		const writable = await handle.createWritable({ keepExistingData: true });
		await writable.truncate(length);
		await writable.close();
	}

	async realpath(path: string): Promise<string> {
		const normalized = normalizePath(path);
		if (await this.exists(normalized)) return normalized;
		throw new Error(`ENOENT: no such file or directory, realpath '${normalized}'`);
	}

	async pread(path: string, offset: number, length: number): Promise<Uint8Array> {
		const data = await this.readFile(path);
		return data.slice(offset, offset + length);
	}

	async pwrite(path: string, offset: number, data: Uint8Array): Promise<void> {
		const content = await this.readFile(path);
		const endPos = offset + data.length;
		const newContent = new Uint8Array(Math.max(content.length, endPos));
		newContent.set(content);
		newContent.set(data, offset);
		await this.writeFile(path, newContent);
	}
}

export interface BrowserDriverOptions {
	filesystem?: "opfs" | "memory";
	permissions?: Permissions;
	useDefaultNetwork?: boolean;
}

/** Create an OPFS-backed filesystem, falling back to in-memory if OPFS is unavailable. */
export async function createOpfsFileSystem(): Promise<VirtualFileSystem> {
	if (!("storage" in navigator) || typeof navigator.storage.getDirectory !== "function") {
		return createInMemoryFileSystem();
	}
	return new OpfsFileSystem();
}

/** Network adapter that delegates to the browser's native `fetch`. DNS and http2 are unsupported. */
export function createBrowserNetworkAdapter(): NetworkAdapter {
	return {
		async fetch(url, options) {
			const response = await fetch(url, {
				method: options?.method || "GET",
				headers: options?.headers,
				body: options?.body as RequestInit["body"],
			});
			const headers: Record<string, string> = {};
			response.headers.forEach((v, k) => {
				headers[k] = v;
			});

			const contentType = response.headers.get("content-type") || "";
			const isBinary =
				contentType.includes("octet-stream") ||
				contentType.includes("gzip") ||
				url.endsWith(".tgz");

			let body: string;
			if (isBinary) {
				const buffer = await response.arrayBuffer();
				body = btoa(String.fromCharCode(...new Uint8Array(buffer)));
				headers["x-body-encoding"] = "base64";
			} else {
				body = await response.text();
			}

			return {
				ok: response.ok,
				status: response.status,
				statusText: response.statusText,
				headers,
				body,
				url: response.url,
				redirected: response.redirected,
			};
		},

		async dnsLookup(_hostname) {
			return { error: "DNS not supported in browser", code: "ENOSYS" };
		},

		async httpRequest(url, options) {
			const response = await fetch(url, {
				method: options?.method || "GET",
				headers: options?.headers,
				body: options?.body as RequestInit["body"],
			});
			const headers: Record<string, string> = {};
			response.headers.forEach((v, k) => {
				headers[k] = v;
			});
			const body = await response.text();
			return {
				status: response.status,
				statusText: response.statusText,
				headers,
				body,
				url: response.url,
			};
		},
	};
}

/** Recover runtime-driver options from a browser SystemDriver instance. */
export function getBrowserSystemDriverOptions(
	systemDriver: SystemDriver,
): BrowserRuntimeSystemOptions {
	const options = (systemDriver as BrowserSystemDriver)[
		BROWSER_SYSTEM_DRIVER_OPTIONS
	];
	if (options) {
		return options;
	}
	return {
		filesystem: "opfs",
		networkEnabled: Boolean(systemDriver.network),
	};
}

/** Assemble a browser-side SystemDriver with permission-wrapped adapters. */
export async function createBrowserDriver(
	options: BrowserDriverOptions = {},
): Promise<SystemDriver> {
	const permissions = options.permissions;
	const filesystemMode = options.filesystem ?? "opfs";
	const filesystem =
		filesystemMode === "memory"
			? createInMemoryFileSystem()
			: await createOpfsFileSystem();
	const networkAdapter = options.useDefaultNetwork
		? wrapNetworkAdapter(createBrowserNetworkAdapter(), permissions)
		: undefined;

	const systemDriver: BrowserSystemDriver = {
		filesystem: wrapFileSystem(filesystem, permissions),
		network: networkAdapter,
		commandExecutor: createCommandExecutorStub(),
		permissions,
		runtime: {
			process: {},
			os: {},
		},
	};

	systemDriver[BROWSER_SYSTEM_DRIVER_OPTIONS] = {
		filesystem: filesystemMode,
		networkEnabled: Boolean(networkAdapter),
	};

	return systemDriver;
}

export {
	createCommandExecutorStub,
	createFsStub,
	createNetworkStub,
};
