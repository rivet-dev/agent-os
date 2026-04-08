/**
 * Device layer.
 *
 * Intercepts device node paths (/dev/*) before they reach the VFS backend.
 * Wraps a VirtualFileSystem and handles device-specific read/write semantics.
 */

import type { VirtualFileSystem, VirtualStat, VirtualDirEntry } from "./vfs.js";
import { KernelError } from "./types.js";

const DEVICE_PATHS = new Set([
	"/dev/null",
	"/dev/zero",
	"/dev/stdin",
	"/dev/stdout",
	"/dev/stderr",
	"/dev/urandom",
	"/dev/random",
	"/dev/tty",
	"/dev/console",
	"/dev/full",
	"/dev/ptmx",
]);

const DEVICE_INO: Record<string, number> = {
	"/dev/null": 0xffff_0001,
	"/dev/zero": 0xffff_0002,
	"/dev/stdin": 0xffff_0003,
	"/dev/stdout": 0xffff_0004,
	"/dev/stderr": 0xffff_0005,
	"/dev/urandom": 0xffff_0006,
	"/dev/random": 0xffff_0007,
	"/dev/tty": 0xffff_0008,
	"/dev/console": 0xffff_0009,
	"/dev/full": 0xffff_000a,
	"/dev/ptmx": 0xffff_000b,
};

/** Device pseudo-directories that contain dynamic entries. */
const DEVICE_DIRS = new Set(["/dev/fd", "/dev/pts", "/dev/shm"]);

function isDevicePath(path: string): boolean {
	return DEVICE_PATHS.has(path) || path.startsWith("/dev/fd/") || path.startsWith("/dev/pts/");
}

function isDeviceDir(path: string): boolean {
	return path === "/dev" || DEVICE_DIRS.has(path);
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

/**
 * Wrap a VFS with device node interception.
 * Device paths are handled directly; all other paths pass through.
 */
export function createDeviceLayer(vfs: VirtualFileSystem): VirtualFileSystem {
	const wrapped: VirtualFileSystem & {
		prepareOpenSync?: (path: string, flags: number) => boolean;
	} = {
		prepareOpenSync(path, flags) {
			if (isDevicePath(path) || isDeviceDir(path)) return false;
			const syncVfs = vfs as VirtualFileSystem & {
				prepareOpenSync?: (targetPath: string, openFlags: number) => boolean;
			};
			return syncVfs.prepareOpenSync?.(path, flags) ?? false;
		},

		async readFile(path) {
			if (path === "/dev/null" || path === "/dev/full") return new Uint8Array(0);
			if (path === "/dev/zero") return new Uint8Array(4096);
			if (path === "/dev/urandom" || path === "/dev/random") {
				const buf = new Uint8Array(4096);
				if (typeof globalThis.crypto?.getRandomValues === "function") {
					globalThis.crypto.getRandomValues(buf);
				} else {
					for (let i = 0; i < buf.length; i++) {
						buf[i] = (Math.random() * 256) | 0;
					}
				}
				return buf;
			}
			if (path === "/dev/tty" || path === "/dev/console" || path === "/dev/ptmx") return new Uint8Array(0);
			return vfs.readFile(path);
		},

		async pread(path, offset, length) {
			if (path === "/dev/null" || path === "/dev/full") return new Uint8Array(0);
			if (path === "/dev/zero") return new Uint8Array(length);
			if (path === "/dev/urandom" || path === "/dev/random") {
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
			if (path === "/dev/tty" || path === "/dev/console" || path === "/dev/ptmx") return new Uint8Array(0);
			return vfs.pread(path, offset, length);
		},

		async readTextFile(path) {
			if (path === "/dev/null") return "";
			const bytes = await this.readFile(path);
			return new TextDecoder().decode(bytes);
		},

		async readDir(path) {
			if (path === "/dev") {
				return DEV_DIR_ENTRIES.map((e) => e.name);
			}
			// /dev/fd and /dev/pts are dynamic — return empty at VFS level
			if (DEVICE_DIRS.has(path)) return [];
			return vfs.readDir(path);
		},

		async readDirWithTypes(path) {
			if (path === "/dev") {
				return DEV_DIR_ENTRIES;
			}
			if (DEVICE_DIRS.has(path)) return [];
			return vfs.readDirWithTypes(path);
		},

		async writeFile(path, content) {
			// /dev/full always returns ENOSPC on write (POSIX behavior)
			if (path === "/dev/full") throw new KernelError("ENOSPC", "No space left on device");
			// Discard writes to sink devices
			if (path === "/dev/null" || path === "/dev/zero" || path === "/dev/urandom"
				|| path === "/dev/random" || path === "/dev/tty" || path === "/dev/console"
				|| path === "/dev/ptmx") return;
			return vfs.writeFile(path, content);
		},

		async pwrite(path, offset, data) {
			if (path === "/dev/full") throw new KernelError("ENOSPC", "No space left on device");
			if (path === "/dev/null" || path === "/dev/zero" || path === "/dev/urandom"
				|| path === "/dev/random" || path === "/dev/tty" || path === "/dev/console"
				|| path === "/dev/ptmx") return;
			return vfs.pwrite(path, offset, data);
		},

		async createDir(path) {
			if (isDeviceDir(path)) return;
			return vfs.createDir(path);
		},

		async mkdir(path, options?) {
			if (isDeviceDir(path)) return;
			return vfs.mkdir(path, options);
		},

		async exists(path) {
			if (isDevicePath(path) || isDeviceDir(path)) return true;
			return vfs.exists(path);
		},

		async stat(path) {
			if (isDevicePath(path)) return deviceStat(path);
			if (isDeviceDir(path)) {
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
			return vfs.stat(path);
		},

		async removeFile(path) {
			if (isDevicePath(path)) throw new KernelError("EPERM", "cannot remove device");
			return vfs.removeFile(path);
		},

		async removeDir(path) {
			if (isDeviceDir(path)) throw new KernelError("EPERM", `cannot remove ${path}`);
			return vfs.removeDir(path);
		},

		async rename(oldPath, newPath) {
			if (isDevicePath(oldPath) || isDevicePath(newPath)) {
				throw new KernelError("EPERM", "cannot rename device");
			}
			return vfs.rename(oldPath, newPath);
		},

		async realpath(path) {
			if (isDevicePath(path) || isDeviceDir(path)) return path;
			return vfs.realpath(path);
		},

		// Passthrough for POSIX extensions
		async symlink(target, linkPath) {
			return vfs.symlink(target, linkPath);
		},

		async readlink(path) {
			return vfs.readlink(path);
		},

		async lstat(path) {
			if (isDevicePath(path)) return deviceStat(path);
			if (isDeviceDir(path)) return this.stat(path);
			return vfs.lstat(path);
		},

		async link(oldPath, newPath) {
			if (isDevicePath(oldPath)) throw new KernelError("EPERM", "cannot link device");
			return vfs.link(oldPath, newPath);
		},

		async chmod(path, mode) {
			if (isDevicePath(path)) return;
			return vfs.chmod(path, mode);
		},

		async chown(path, uid, gid) {
			if (isDevicePath(path)) return;
			return vfs.chown(path, uid, gid);
		},

		async utimes(path, atime, mtime) {
			if (isDevicePath(path)) return;
			return vfs.utimes(path, atime, mtime);
		},

		async truncate(path, length) {
			if (isDevicePath(path)) return;
			return vfs.truncate(path, length);
		},
	};
	return wrapped;
}
