import * as fs from "node:fs/promises";
import * as fsSync from "node:fs";
import path from "node:path";
import {
	filterEnv,
} from "@secure-exec/core/internal/shared/permissions";
import { ModuleAccessFileSystem } from "./module-access.js";
import { NodeExecutionDriver } from "./execution-driver.js";
import {
	createDefaultNetworkAdapter,
	isPrivateIp,
} from "./default-network-adapter.js";
export type { DefaultNetworkAdapterOptions } from "./default-network-adapter.js";
import type {
	OSConfig,
	ProcessConfig,
} from "@secure-exec/core/internal/shared/api-types";
import type {
	Permissions,
	VirtualFileSystem,
} from "@secure-exec/core";
import { KernelError, O_CREAT, O_EXCL, O_TRUNC } from "@secure-exec/core";
import type {
	CommandExecutor,
	NetworkAdapter,
	NodeRuntimeDriverFactory,
	SystemDriver,
} from "@secure-exec/core";
import type { ModuleAccessOptions } from "./module-access.js";

/** Options for assembling a Node.js-backed SystemDriver. */
export interface NodeDriverOptions {
	filesystem?: VirtualFileSystem;
	moduleAccess?: ModuleAccessOptions;
	networkAdapter?: NetworkAdapter;
	commandExecutor?: CommandExecutor;
	permissions?: Permissions;
	useDefaultNetwork?: boolean;
	/** Loopback ports that bypass SSRF checks when using the default network adapter (`useDefaultNetwork: true`). */
	loopbackExemptPorts?: number[];
	processConfig?: ProcessConfig;
	osConfig?: OSConfig;
}

export interface NodeRuntimeDriverFactoryOptions {
	createIsolate?(memoryLimit: number): unknown;
}

/** Thin VFS adapter that delegates directly to `node:fs/promises`. */
export class NodeFileSystem implements VirtualFileSystem {
	prepareOpenSync(filePath: string, flags: number): boolean {
		const hasCreate = (flags & O_CREAT) !== 0;
		const hasExcl = (flags & O_EXCL) !== 0;
		const hasTrunc = (flags & O_TRUNC) !== 0;
		const exists = fsSync.existsSync(filePath);

		if (hasCreate && hasExcl && exists) {
			throw new KernelError("EEXIST", `file already exists, open '${filePath}'`);
		}

		let created = false;
		if (!exists && hasCreate) {
			fsSync.mkdirSync(path.dirname(filePath), { recursive: true });
			fsSync.writeFileSync(filePath, new Uint8Array(0));
			created = true;
		}

		if (hasTrunc) {
			try {
				fsSync.truncateSync(filePath, 0);
			} catch (error) {
				const err = error as NodeJS.ErrnoException;
				if (err.code === "ENOENT") {
					throw new KernelError("ENOENT", `no such file or directory, open '${filePath}'`);
				}
				if (err.code === "EISDIR") {
					throw new KernelError("EISDIR", `illegal operation on a directory, open '${filePath}'`);
				}
				throw error;
			}
		}

		return created;
	}

	async readFile(path: string): Promise<Uint8Array> {
		return fs.readFile(path);
	}

	async readTextFile(path: string): Promise<string> {
		return fs.readFile(path, "utf8");
	}

	async readDir(path: string): Promise<string[]> {
		return fs.readdir(path);
	}

	async readDirWithTypes(
		path: string,
	): Promise<Array<{ name: string; isDirectory: boolean }>> {
		const entries = await fs.readdir(path, { withFileTypes: true });
		return entries.map((entry) => ({
			name: entry.name,
			isDirectory: entry.isDirectory(),
		}));
	}

	async writeFile(path: string, content: string | Uint8Array): Promise<void> {
		await fs.writeFile(path, content);
	}

	async createDir(path: string): Promise<void> {
		await fs.mkdir(path);
	}

	async mkdir(path: string, _options?: { recursive?: boolean }): Promise<void> {
		await fs.mkdir(path, { recursive: true });
	}

	async exists(path: string): Promise<boolean> {
		try {
			await fs.access(path);
			return true;
		} catch {
			return false;
		}
	}

	async stat(path: string) {
		const info = await fs.stat(path);
		return {
			mode: info.mode,
			size: info.size,
			isDirectory: info.isDirectory(),
			isSymbolicLink: false,
			atimeMs: info.atimeMs,
			mtimeMs: info.mtimeMs,
			ctimeMs: info.ctimeMs,
			birthtimeMs: info.birthtimeMs,
			ino: info.ino,
			nlink: info.nlink,
			uid: info.uid,
			gid: info.gid,
		};
	}

	async removeFile(path: string): Promise<void> {
		await fs.unlink(path);
	}

	async removeDir(path: string): Promise<void> {
		await fs.rmdir(path);
	}

	async rename(oldPath: string, newPath: string): Promise<void> {
		await fs.rename(oldPath, newPath);
	}

	async symlink(target: string, linkPath: string): Promise<void> {
		await fs.symlink(target, linkPath);
	}

	async readlink(path: string): Promise<string> {
		return fs.readlink(path);
	}

	async lstat(path: string) {
		const info = await fs.lstat(path);
		return {
			mode: info.mode,
			size: info.size,
			isDirectory: info.isDirectory(),
			isSymbolicLink: info.isSymbolicLink(),
			atimeMs: info.atimeMs,
			mtimeMs: info.mtimeMs,
			ctimeMs: info.ctimeMs,
			birthtimeMs: info.birthtimeMs,
			ino: info.ino,
			nlink: info.nlink,
			uid: info.uid,
			gid: info.gid,
		};
	}

	async link(oldPath: string, newPath: string): Promise<void> {
		await fs.link(oldPath, newPath);
	}

	async chmod(path: string, mode: number): Promise<void> {
		await fs.chmod(path, mode);
	}

	async chown(path: string, uid: number, gid: number): Promise<void> {
		await fs.chown(path, uid, gid);
	}

	async utimes(path: string, atime: number, mtime: number): Promise<void> {
		await fs.utimes(path, atime, mtime);
	}

	async truncate(path: string, length: number): Promise<void> {
		await fs.truncate(path, length);
	}

	async realpath(path: string): Promise<string> {
		return fs.realpath(path);
	}

	async pread(path: string, offset: number, length: number): Promise<Uint8Array> {
		const handle = await fs.open(path, "r");
		try {
			const buf = new Uint8Array(length);
			const { bytesRead } = await handle.read(buf, 0, length, offset);
			return buf.slice(0, bytesRead);
		} finally {
			await handle.close();
		}
	}

	async pwrite(path: string, offset: number, data: Uint8Array): Promise<void> {
		const handle = await fs.open(path, "r+");
		try {
			await handle.write(data, 0, data.length, offset);
		} finally {
			await handle.close();
		}
	}
}

/**
 * Assemble a SystemDriver from Node.js-native adapters. Wraps the filesystem
 * in a ModuleAccessFileSystem overlay and keeps capabilities deny-by-default
 * unless explicit permissions are provided.
 */
export function createNodeDriver(options: NodeDriverOptions = {}): SystemDriver {
	const filesystem = new ModuleAccessFileSystem(
		options.filesystem,
		options.moduleAccess ?? {},
	);
	const permissions = options.permissions;
	const networkAdapter = options.networkAdapter
		? options.networkAdapter
		: options.useDefaultNetwork
			? createDefaultNetworkAdapter(
					options.loopbackExemptPorts?.length
						? { initialExemptPorts: options.loopbackExemptPorts }
						: undefined,
				)
			: undefined;

	return {
		filesystem,
		network: networkAdapter,
		commandExecutor: options.commandExecutor,
		permissions,
		runtime: {
			process: {
				...(options.processConfig ?? {}),
			},
			os: {
				...(options.osConfig ?? {}),
			},
		},
	};
}

export function createNodeRuntimeDriverFactory(
	options: NodeRuntimeDriverFactoryOptions = {},
): NodeRuntimeDriverFactory {
	return {
		createRuntimeDriver: (runtimeOptions) =>
			new NodeExecutionDriver({
				...runtimeOptions,
				createIsolate: options.createIsolate,
			}),
	};
}

export {
	createDefaultNetworkAdapter,
	filterEnv,
	isPrivateIp,
	NodeExecutionDriver,
};
export type { ModuleAccessOptions };
