import { readFileSync } from "node:fs";
import * as posixPath from "node:path/posix";
import { KernelError, type VirtualFileSystem } from "./runtime-compat.js";
import { createOverlayBackend } from "./overlay-filesystem.js";
import {
	createFilesystemFromEntries,
	type FilesystemEntry,
} from "./filesystem-snapshot.js";

export interface BaseFilesystemEnvironment {
	env: Record<string, string>;
	prompt: string;
}

export type BaseFilesystemEntry = FilesystemEntry;

export interface BaseFilesystemSnapshot {
	source?: {
		snapshotPath?: string;
		image?: string;
		snapshotCreatedAt?: string;
		builtAt?: string;
		transforms?: string[];
	};
	environment: BaseFilesystemEnvironment;
	filesystem: {
		entries: BaseFilesystemEntry[];
	};
}

const SNAPSHOT_URL = new URL("../fixtures/base-filesystem.json", import.meta.url);
const SUPPRESSED_KERNEL_BOOTSTRAP_DIRS = new Set([
	"/boot",
	"/usr/games",
	"/usr/include",
	"/usr/libexec",
	"/usr/man",
]);
const SUPPRESSED_KERNEL_BOOTSTRAP_FILES = new Set([
	"/usr/bin/env",
]);

let snapshotCache: BaseFilesystemSnapshot | null = null;

function loadSnapshot(): BaseFilesystemSnapshot {
	if (snapshotCache) {
		return snapshotCache;
	}

	snapshotCache = JSON.parse(
		readFileSync(SNAPSHOT_URL, "utf-8"),
	) as BaseFilesystemSnapshot;

	return snapshotCache;
}

function normalizePath(path: string): string {
	const normalized = posixPath.normalize(path);
	return normalized === "." ? "/" : normalized;
}

export async function createBaseLowerFilesystem(): Promise<VirtualFileSystem> {
	return createFilesystemFromEntries(getBaseFilesystemEntries());
}

export function createBootstrapAwareFilesystem(
	filesystem: VirtualFileSystem,
	existingRoot: VirtualFileSystem,
	options?: { readOnlyAfterBootstrap?: boolean },
): {
	filesystem: VirtualFileSystem;
	finishKernelBootstrap: () => void;
} {
	let bootstrapActive = true;
	let writesLocked = false;

	function throwReadOnly(): never {
		throw new KernelError("EROFS", "read-only file system");
	}

	async function rootHasPath(path: string): Promise<boolean> {
		try {
			return await existingRoot.exists(path);
		} catch {
			return false;
		}
	}

	const wrapped: VirtualFileSystem = {
		...filesystem,
		async createDir(path: string): Promise<void> {
			if (writesLocked) {
				throwReadOnly();
			}
			const normalized = normalizePath(path);
			if (
				bootstrapActive
				&& SUPPRESSED_KERNEL_BOOTSTRAP_DIRS.has(normalized)
				&& !(await rootHasPath(normalized))
			) {
				return;
			}
			return filesystem.createDir(path);
		},

		async mkdir(
			path: string,
			options?: { recursive?: boolean },
		): Promise<void> {
			if (writesLocked) {
				if (options?.recursive && await rootHasPath(path)) {
					return;
				}
				throwReadOnly();
			}
			const normalized = normalizePath(path);
			if (
				bootstrapActive
				&& options?.recursive
				&& SUPPRESSED_KERNEL_BOOTSTRAP_DIRS.has(normalized)
				&& !(await rootHasPath(normalized))
			) {
				return;
			}
			return filesystem.mkdir(path, options);
		},

		async writeFile(
			path: string,
			content: string | Uint8Array,
		): Promise<void> {
			if (writesLocked) {
				throwReadOnly();
			}
			const normalized = normalizePath(path);
			if (
				bootstrapActive
				&& SUPPRESSED_KERNEL_BOOTSTRAP_FILES.has(normalized)
			) {
				return;
			}
			return filesystem.writeFile(path, content);
		},

		async removeFile(path: string): Promise<void> {
			if (writesLocked) {
				throwReadOnly();
			}
			return filesystem.removeFile(path);
		},

		async removeDir(path: string): Promise<void> {
			if (writesLocked) {
				throwReadOnly();
			}
			return filesystem.removeDir(path);
		},

		async rename(oldPath: string, newPath: string): Promise<void> {
			if (writesLocked) {
				throwReadOnly();
			}
			return filesystem.rename(oldPath, newPath);
		},

		async symlink(target: string, linkPath: string): Promise<void> {
			if (writesLocked) {
				throwReadOnly();
			}
			return filesystem.symlink(target, linkPath);
		},

		async link(oldPath: string, newPath: string): Promise<void> {
			if (writesLocked) {
				throwReadOnly();
			}
			return filesystem.link(oldPath, newPath);
		},

		async chmod(path: string, mode: number): Promise<void> {
			if (writesLocked) {
				throwReadOnly();
			}
			return filesystem.chmod(path, mode);
		},

		async chown(path: string, uid: number, gid: number): Promise<void> {
			if (writesLocked) {
				throwReadOnly();
			}
			return filesystem.chown(path, uid, gid);
		},

		async utimes(path: string, atime: number, mtime: number): Promise<void> {
			if (writesLocked) {
				throwReadOnly();
			}
			return filesystem.utimes(path, atime, mtime);
		},

		async truncate(path: string, length: number): Promise<void> {
			if (writesLocked) {
				throwReadOnly();
			}
			return filesystem.truncate(path, length);
		},

		async pwrite(
			path: string,
			offset: number,
			data: Uint8Array,
		): Promise<void> {
			if (writesLocked) {
				throwReadOnly();
			}
			return filesystem.pwrite(path, offset, data);
		},
	};

	return {
		filesystem: wrapped,
		finishKernelBootstrap(): void {
			bootstrapActive = false;
			writesLocked = options?.readOnlyAfterBootstrap ?? false;
		},
	};
}

export function getBaseFilesystemSnapshot(): BaseFilesystemSnapshot {
	return loadSnapshot();
}

export function getBaseEnvironment(): Record<string, string> {
	const snapshot = loadSnapshot();

	return {
		...snapshot.environment.env,
		PATH: "/bin:/usr/bin:/usr/local/bin:/sbin:/usr/sbin:/usr/local/sbin",
		PS1: snapshot.environment.prompt,
	};
}

export function getBaseFilesystemEntries(): BaseFilesystemEntry[] {
	return loadSnapshot().filesystem.entries;
}

export async function createBaseRootFilesystem(): Promise<{
	filesystem: VirtualFileSystem;
	finishKernelBootstrap: () => void;
}> {
	const lower = await createBaseLowerFilesystem();
	const overlay = createOverlayBackend({ lower });
	return createBootstrapAwareFilesystem(overlay, lower);
}
