/**
 * Device backend.
 *
 * Standalone VirtualFileSystem that handles device nodes.
 * Receives relative paths (e.g. "null" not "/dev/null").
 * Designed to be mounted at /dev via MountTable.
 */

import { KernelError } from "./types.js";
import type { VirtualDirEntry, VirtualFileSystem, VirtualStat } from "./vfs.js";

const DEVICE_NAMES = new Set([
	"null",
	"zero",
	"stdin",
	"stdout",
	"stderr",
	"urandom",
	"random",
	"tty",
	"console",
	"full",
	"ptmx",
]);

const DEVICE_INO: Record<string, number> = {
	null: 0xffff_0001,
	zero: 0xffff_0002,
	stdin: 0xffff_0003,
	stdout: 0xffff_0004,
	stderr: 0xffff_0005,
	urandom: 0xffff_0006,
	random: 0xffff_0007,
	tty: 0xffff_0008,
	console: 0xffff_0009,
	full: 0xffff_000a,
	ptmx: 0xffff_000b,
};

/** Device pseudo-directories that contain dynamic entries. */
const DEVICE_DIRS = new Set(["fd", "pts", "shm"]);

function isDeviceName(path: string): boolean {
	return (
		DEVICE_NAMES.has(path) || path.startsWith("fd/") || path.startsWith("pts/")
	);
}

function isDeviceDir(path: string): boolean {
	return path === "" || DEVICE_DIRS.has(path);
}

function deviceStat(path: string): VirtualStat {
	const now = Date.now();
	return {
		mode: 0o666,
		size: 0,
		isDirectory: false,
		isSymbolicLink: false,
		atimeMs: now,
		mtimeMs: now,
		ctimeMs: now,
		birthtimeMs: now,
		ino: DEVICE_INO[path] ?? 0xffff_0000,
		nlink: 1,
		uid: 0,
		gid: 0,
	};
}

function dirStat(path: string): VirtualStat {
	const now = Date.now();
	return {
		mode: 0o755,
		size: 0,
		isDirectory: true,
		isSymbolicLink: false,
		atimeMs: now,
		mtimeMs: now,
		ctimeMs: now,
		birthtimeMs: now,
		ino: DEVICE_INO[path] ?? 0xffff_0000,
		nlink: 2,
		uid: 0,
		gid: 0,
	};
}

const DEV_DIR_ENTRIES: VirtualDirEntry[] = [
	{ name: "null", isDirectory: false },
	{ name: "zero", isDirectory: false },
	{ name: "stdin", isDirectory: false },
	{ name: "stdout", isDirectory: false },
	{ name: "stderr", isDirectory: false },
	{ name: "urandom", isDirectory: false },
	{ name: "random", isDirectory: false },
	{ name: "tty", isDirectory: false },
	{ name: "console", isDirectory: false },
	{ name: "full", isDirectory: false },
	{ name: "ptmx", isDirectory: false },
	{ name: "fd", isDirectory: true },
	{ name: "pts", isDirectory: true },
	{ name: "shm", isDirectory: true },
];

function randomBytes(length: number): Uint8Array {
	const buf = new Uint8Array(length);
	if (typeof globalThis.crypto?.getRandomValues === "function") {
		globalThis.crypto.getRandomValues(buf);
	} else {
		for (let i = 0; i < buf.length; i++) {
			buf[i] = (Math.random() * 256) | 0;
		}
	}
	return buf;
}

function notFound(path: string): never {
	throw new KernelError("ENOENT", `no such device: ${path}`);
}

/**
 * Create a standalone device backend VFS.
 * All paths are relative to /dev (e.g. "null", "zero", "pts/0").
 * Mount at /dev via MountTable.
 */
export function createDeviceBackend(): VirtualFileSystem {
	const backend: VirtualFileSystem = {
		async readFile(path) {
			if (path === "null" || path === "full") return new Uint8Array(0);
			if (path === "zero") return new Uint8Array(4096);
			if (path === "urandom" || path === "random") return randomBytes(4096);
			if (path === "tty" || path === "console" || path === "ptmx")
				return new Uint8Array(0);
			if (path === "stdin" || path === "stdout" || path === "stderr")
				return new Uint8Array(0);
			notFound(path);
		},

		async pread(path, _offset, length) {
			if (path === "null" || path === "full") return new Uint8Array(0);
			if (path === "zero") return new Uint8Array(length);
			if (path === "urandom" || path === "random") return randomBytes(length);
			if (path === "tty" || path === "console" || path === "ptmx")
				return new Uint8Array(0);
			if (path === "stdin" || path === "stdout" || path === "stderr")
				return new Uint8Array(0);
			notFound(path);
		},

		async readTextFile(path) {
			const bytes = await this.readFile(path);
			return new TextDecoder().decode(bytes);
		},

		async readDir(path) {
			if (path === "") return DEV_DIR_ENTRIES.map((e) => e.name);
			if (DEVICE_DIRS.has(path)) return [];
			notFound(path);
		},

		async readDirWithTypes(path) {
			if (path === "") return DEV_DIR_ENTRIES;
			if (DEVICE_DIRS.has(path)) return [];
			notFound(path);
		},

		async writeFile(path, _content) {
			if (path === "full")
				throw new KernelError("ENOSPC", "No space left on device");
			if (
				DEVICE_NAMES.has(path) ||
				path.startsWith("fd/") ||
				path.startsWith("pts/")
			) {
				return;
			}
			notFound(path);
		},

		async pwrite(path, _offset, _data) {
			if (path === "full")
				throw new KernelError("ENOSPC", "No space left on device");
			if (
				DEVICE_NAMES.has(path) ||
				path.startsWith("fd/") ||
				path.startsWith("pts/")
			) {
				return;
			}
			notFound(path);
		},

		async createDir(path) {
			if (isDeviceDir(path)) return;
			throw new KernelError("EPERM", "cannot create directory in /dev");
		},

		async mkdir(path, _options?) {
			if (isDeviceDir(path)) return;
			throw new KernelError("EPERM", "cannot create directory in /dev");
		},

		async exists(path) {
			return isDeviceName(path) || isDeviceDir(path);
		},

		async stat(path) {
			if (isDeviceName(path)) return deviceStat(path);
			if (isDeviceDir(path)) return dirStat(path);
			notFound(path);
		},

		async removeFile(path) {
			if (isDeviceName(path))
				throw new KernelError("EPERM", "cannot remove device");
			notFound(path);
		},

		async removeDir(path) {
			if (isDeviceDir(path))
				throw new KernelError("EPERM", "cannot remove device directory");
			notFound(path);
		},

		async rename(_oldPath, _newPath) {
			throw new KernelError("EPERM", "cannot rename device");
		},

		async realpath(path) {
			if (isDeviceName(path) || isDeviceDir(path)) return path;
			notFound(path);
		},

		async symlink(_target, _linkPath) {
			throw new KernelError("EPERM", "cannot create symlink in /dev");
		},

		async readlink(path) {
			notFound(path);
		},

		async lstat(path) {
			return this.stat(path);
		},

		async link(_oldPath, _newPath) {
			throw new KernelError("EPERM", "cannot link device");
		},

		async chmod(path, _mode) {
			if (isDeviceName(path) || isDeviceDir(path)) return;
			notFound(path);
		},

		async chown(path, _uid, _gid) {
			if (isDeviceName(path) || isDeviceDir(path)) return;
			notFound(path);
		},

		async utimes(path, _atime, _mtime) {
			if (isDeviceName(path) || isDeviceDir(path)) return;
			notFound(path);
		},

		async truncate(path, _length) {
			if (isDeviceName(path) || isDeviceDir(path)) return;
			notFound(path);
		},
	};
	return backend;
}
