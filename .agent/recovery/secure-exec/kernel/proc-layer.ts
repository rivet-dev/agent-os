import type { FDTableManager } from "./fd-table.js";
import type { ProcessTable } from "./process-table.js";
import type { VirtualDirEntry, VirtualFileSystem, VirtualStat } from "./vfs.js";
import { KernelError } from "./types.js";

const S_IFREG = 0o100000;
const S_IFDIR = 0o040000;
const S_IFLNK = 0o120000;
const PROC_INO_BASE = 0xfffe_0000;
const PROC_SELF_PREFIX = "/proc/self";
const PROC_SYS_PREFIX = "/proc/sys";
const PROC_SYS_KERNEL_PREFIX = "/proc/sys/kernel";
const PROC_SYS_KERNEL_HOSTNAME_PATH = "/proc/sys/kernel/hostname";
const PROC_PID_ENTRIES: VirtualDirEntry[] = [
	{ name: "fd", isDirectory: true },
	{ name: "cwd", isDirectory: false, isSymbolicLink: true },
	{ name: "exe", isDirectory: false, isSymbolicLink: true },
	{ name: "environ", isDirectory: false },
];
const PROC_ROOT_ENTRIES: VirtualDirEntry[] = [
	{ name: "self", isDirectory: false, isSymbolicLink: true },
	{ name: "sys", isDirectory: true },
];
const PROC_SYS_ENTRIES: VirtualDirEntry[] = [
	{ name: "kernel", isDirectory: true },
];
const PROC_SYS_KERNEL_ENTRIES: VirtualDirEntry[] = [
	{ name: "hostname", isDirectory: false },
];

export interface ProcLayerOptions {
	processTable: ProcessTable;
	fdTableManager: FDTableManager;
	hostname?: string;
}

function normalizePath(path: string): string {
	if (!path) return "/";
	let normalized = path.startsWith("/") ? path : `/${path}`;
	normalized = normalized.replace(/\/+/g, "/");
	if (normalized.length > 1 && normalized.endsWith("/")) {
		normalized = normalized.slice(0, -1);
	}
	const parts = normalized.split("/");
	const resolved: string[] = [];
	for (const part of parts) {
		if (!part || part === ".") continue;
		if (part === "..") {
			resolved.pop();
			continue;
		}
		resolved.push(part);
	}
	return resolved.length === 0 ? "/" : `/${resolved.join("/")}`;
}

function isProcPath(path: string): boolean {
	const normalized = normalizePath(path);
	return normalized === "/proc" || normalized.startsWith("/proc/");
}

function procIno(seed: string): number {
	let hash = 0;
	for (let i = 0; i < seed.length; i++) {
		hash = ((hash * 33) ^ seed.charCodeAt(i)) >>> 0;
	}
	return PROC_INO_BASE + (hash & 0xffff);
}

function dirStat(seed: string): VirtualStat {
	const now = Date.now();
	return {
		mode: S_IFDIR | 0o555,
		size: 0,
		isDirectory: true,
		isSymbolicLink: false,
		atimeMs: now,
		mtimeMs: now,
		ctimeMs: now,
		birthtimeMs: now,
		ino: procIno(seed),
		nlink: 2,
		uid: 0,
		gid: 0,
	};
}

function fileStat(seed: string, size: number): VirtualStat {
	const now = Date.now();
	return {
		mode: S_IFREG | 0o444,
		size,
		isDirectory: false,
		isSymbolicLink: false,
		atimeMs: now,
		mtimeMs: now,
		ctimeMs: now,
		birthtimeMs: now,
		ino: procIno(seed),
		nlink: 1,
		uid: 0,
		gid: 0,
	};
}

function linkStat(seed: string, target: string): VirtualStat {
	const now = Date.now();
	return {
		mode: S_IFLNK | 0o777,
		size: target.length,
		isDirectory: false,
		isSymbolicLink: true,
		atimeMs: now,
		mtimeMs: now,
		ctimeMs: now,
		birthtimeMs: now,
		ino: procIno(seed),
		nlink: 1,
		uid: 0,
		gid: 0,
	};
}

function parseProcPath(path: string): { pid: number; tail: string[] } | null {
	const normalized = normalizePath(path);
	if (normalized === "/proc" || normalized === PROC_SELF_PREFIX || !normalized.startsWith("/proc/")) {
		return null;
	}
	const parts = normalized.slice("/proc/".length).split("/");
	const pid = Number(parts[0]);
	if (!Number.isInteger(pid) || pid < 0) return null;
	return { pid, tail: parts.slice(1) };
}

function encodeText(content: string): Uint8Array {
	return new TextEncoder().encode(content);
}

function encodeEnviron(env: Record<string, string>): Uint8Array {
	const entries = Object.entries(env);
	if (entries.length === 0) return new Uint8Array(0);
	return encodeText(entries.map(([key, value]) => `${key}=${value}`).join("\0") + "\0");
}

function resolveExecPath(command: string): string {
	if (!command) return "";
	return command.startsWith("/") ? command : `/bin/${command}`;
}

function clonePathArg(path: string, normalized: string): string {
	return path === normalized ? path : normalized;
}

export function resolveProcSelfPath(path: string, pid: number): string {
	const normalized = normalizePath(path);
	if (normalized === PROC_SELF_PREFIX) return `/proc/${pid}`;
	if (normalized.startsWith(`${PROC_SELF_PREFIX}/`)) {
		return `/proc/${pid}${normalized.slice(PROC_SELF_PREFIX.length)}`;
	}
	return normalized;
}

export function createProcessScopedFileSystem(vfs: VirtualFileSystem, pid: number): VirtualFileSystem {
	const selfTarget = `/proc/${pid}`;
	return {
		readFile: (path) => vfs.readFile(resolveProcSelfPath(path, pid)),
		readTextFile: (path) => vfs.readTextFile(resolveProcSelfPath(path, pid)),
		readDir: (path) => vfs.readDir(resolveProcSelfPath(path, pid)),
		readDirWithTypes: (path) => vfs.readDirWithTypes(resolveProcSelfPath(path, pid)),
		writeFile: (path, content) => vfs.writeFile(resolveProcSelfPath(path, pid), content),
		createDir: (path) => vfs.createDir(resolveProcSelfPath(path, pid)),
		mkdir: (path, options) => vfs.mkdir(resolveProcSelfPath(path, pid), options),
		exists: (path) => vfs.exists(resolveProcSelfPath(path, pid)),
		stat: (path) => vfs.stat(resolveProcSelfPath(path, pid)),
		removeFile: (path) => vfs.removeFile(resolveProcSelfPath(path, pid)),
		removeDir: (path) => vfs.removeDir(resolveProcSelfPath(path, pid)),
		rename: (oldPath, newPath) => vfs.rename(resolveProcSelfPath(oldPath, pid), resolveProcSelfPath(newPath, pid)),
		realpath: async (path) => {
			const normalized = normalizePath(path);
			if (normalized === PROC_SELF_PREFIX) return selfTarget;
			return vfs.realpath(resolveProcSelfPath(path, pid));
		},
		symlink: (target, linkPath) => vfs.symlink(target, resolveProcSelfPath(linkPath, pid)),
		readlink: async (path) => {
			const normalized = normalizePath(path);
			if (normalized === PROC_SELF_PREFIX) return selfTarget;
			return vfs.readlink(resolveProcSelfPath(path, pid));
		},
		lstat: async (path) => {
			const normalized = normalizePath(path);
			if (normalized === PROC_SELF_PREFIX) return linkStat("self", selfTarget);
			return vfs.lstat(resolveProcSelfPath(path, pid));
		},
		link: (oldPath, newPath) => vfs.link(resolveProcSelfPath(oldPath, pid), resolveProcSelfPath(newPath, pid)),
		chmod: (path, mode) => vfs.chmod(resolveProcSelfPath(path, pid), mode),
		chown: (path, uid, gid) => vfs.chown(resolveProcSelfPath(path, pid), uid, gid),
		utimes: (path, atime, mtime) => vfs.utimes(resolveProcSelfPath(path, pid), atime, mtime),
		truncate: (path, length) => vfs.truncate(resolveProcSelfPath(path, pid), length),
		pread: (path, offset, length) => vfs.pread(resolveProcSelfPath(path, pid), offset, length),
		pwrite: (path, offset, data) => vfs.pwrite(resolveProcSelfPath(path, pid), offset, data),
	};
}

export function createProcLayer(vfs: VirtualFileSystem, options: ProcLayerOptions): VirtualFileSystem {
	const syncVfs = vfs as VirtualFileSystem & {
		prepareOpenSync?: (path: string, flags: number) => boolean;
	};
	const kernelHostname = encodeText(`${options.hostname ?? "sandbox"}\n`);

	const getProcess = (pid: number) => {
		const entry = options.processTable.get(pid);
		if (!entry) throw new KernelError("ENOENT", `no such process ${pid}`);
		return entry;
	};

	const getFdEntry = (pid: number, fd: number) => {
		const table = options.fdTableManager.get(pid);
		const entry = table?.get(fd);
		if (!entry) throw new KernelError("ENOENT", `no such fd ${fd} for process ${pid}`);
		return entry;
	};

	const listPids = () => Array.from(options.processTable.listProcesses().keys()).sort((a, b) => a - b);
	const listOpenFds = (pid: number) => {
		const table = options.fdTableManager.get(pid);
		if (!table) return [];
		const fds: number[] = [];
		for (const entry of table) fds.push(entry.fd);
		return fds.sort((a, b) => a - b);
	};

	const getLinkTarget = (pid: number, tail: string[]): string => {
		if (tail.length === 1 && tail[0] === "cwd") return getProcess(pid).cwd;
		if (tail.length === 1 && tail[0] === "exe") return resolveExecPath(getProcess(pid).command);
		if (tail.length === 2 && tail[0] === "fd") {
			const fd = Number(tail[1]);
			if (!Number.isInteger(fd) || fd < 0) throw new KernelError("ENOENT", `invalid fd ${tail[1]}`);
			return getFdEntry(pid, fd).description.path;
		}
		throw new KernelError("ENOENT", `unsupported proc link ${tail.join("/")}`);
	};

	const getProcFile = (pid: number, tail: string[]): Uint8Array => {
		if (tail.length === 1 && tail[0] === "cwd") return encodeText(getProcess(pid).cwd);
		if (tail.length === 1 && tail[0] === "exe") return encodeText(resolveExecPath(getProcess(pid).command));
		if (tail.length === 1 && tail[0] === "environ") return encodeEnviron(getProcess(pid).env);
		if (tail.length === 2 && tail[0] === "fd") return encodeText(getLinkTarget(pid, tail));
		throw new KernelError("ENOENT", `unsupported proc file ${tail.join("/")}`);
	};

	const getProcStat = async (path: string, followSymlinks: boolean): Promise<VirtualStat> => {
		const normalized = normalizePath(path);
		if (normalized === "/proc") return dirStat("proc");
		if (normalized === PROC_SYS_PREFIX) return dirStat("proc:sys");
		if (normalized === PROC_SYS_KERNEL_PREFIX) return dirStat("proc:sys:kernel");
		if (normalized === PROC_SYS_KERNEL_HOSTNAME_PATH) {
			return fileStat("proc:sys:kernel:hostname", kernelHostname.length);
		}
		if (normalized === PROC_SELF_PREFIX) {
			return followSymlinks ? dirStat("proc-self") : linkStat("proc-self-link", PROC_SELF_PREFIX);
		}

		const parsed = parseProcPath(normalized);
		if (!parsed) throw new KernelError("ENOENT", `no such file or directory: ${normalized}`);

		const { pid, tail } = parsed;
		getProcess(pid);

		if (tail.length === 0) return dirStat(`proc:${pid}`);
		if (tail.length === 1 && tail[0] === "fd") return dirStat(`proc:${pid}:fd`);
		if (tail.length === 1 && tail[0] === "environ") {
			return fileStat(`proc:${pid}:environ`, encodeEnviron(getProcess(pid).env).length);
		}
		if ((tail.length === 1 && (tail[0] === "cwd" || tail[0] === "exe"))
			|| (tail.length === 2 && tail[0] === "fd")) {
			const target = getLinkTarget(pid, tail);
			if (!followSymlinks) return linkStat(`proc:${pid}:${tail.join(":")}`, target);
			if (target.startsWith("/proc/")) {
				return getProcStat(target, true);
			}
			try {
				return await vfs.stat(target);
			} catch {
				return linkStat(`proc:${pid}:${tail.join(":")}`, target);
			}
		}

		throw new KernelError("ENOENT", `no such proc entry: ${normalized}`);
	};

	const rejectMutation = (path: string) => {
		if (isProcPath(path)) throw new KernelError("EPERM", `cannot modify ${normalizePath(path)}`);
	};

	const wrapped: VirtualFileSystem & {
		prepareOpenSync?: (path: string, flags: number) => boolean;
	} = {
		prepareOpenSync(path: string, flags: number) {
			const normalized = normalizePath(path);
			if (isProcPath(normalized)) return false;
			return syncVfs.prepareOpenSync?.(clonePathArg(path, normalized), flags) ?? false;
		},

		async readFile(path) {
			const normalized = normalizePath(path);
			if (!isProcPath(normalized)) return vfs.readFile(clonePathArg(path, normalized));
			if (normalized === "/proc" || normalized === PROC_SELF_PREFIX) {
				throw new KernelError("EISDIR", `illegal operation on a directory, read '${normalized}'`);
			}
			if (normalized === PROC_SYS_PREFIX || normalized === PROC_SYS_KERNEL_PREFIX) {
				throw new KernelError("EISDIR", `illegal operation on a directory, read '${normalized}'`);
			}
			if (normalized === PROC_SYS_KERNEL_HOSTNAME_PATH) {
				return kernelHostname;
			}
			const parsed = parseProcPath(normalized);
			if (!parsed) throw new KernelError("ENOENT", `no such file or directory: ${normalized}`);
			const { pid, tail } = parsed;
			if (tail.length === 0 || (tail.length === 1 && tail[0] === "fd")) {
				throw new KernelError("EISDIR", `illegal operation on a directory, read '${normalized}'`);
			}
			return getProcFile(pid, tail);
		},

		async pread(path, offset, length) {
			const content = await this.readFile(path);
			if (offset >= content.length) return new Uint8Array(0);
			return content.slice(offset, offset + length);
		},

		async readTextFile(path) {
			const content = await this.readFile(path);
			return new TextDecoder().decode(content);
		},

		async readDir(path) {
			return (await this.readDirWithTypes(path)).map((entry) => entry.name);
		},

		async readDirWithTypes(path) {
			const normalized = normalizePath(path);
			if (!isProcPath(normalized)) return vfs.readDirWithTypes(clonePathArg(path, normalized));
			if (normalized === "/proc") {
				return [
					...PROC_ROOT_ENTRIES,
					...listPids().map((pid) => ({ name: String(pid), isDirectory: true })),
				];
			}
			if (normalized === PROC_SYS_PREFIX) {
				return PROC_SYS_ENTRIES;
			}
			if (normalized === PROC_SYS_KERNEL_PREFIX) {
				return PROC_SYS_KERNEL_ENTRIES;
			}
			if (normalized === PROC_SELF_PREFIX) {
				throw new KernelError("ENOENT", `no such file or directory: ${normalized}`);
			}
			const parsed = parseProcPath(normalized);
			if (!parsed) throw new KernelError("ENOENT", `no such file or directory: ${normalized}`);
			const { pid, tail } = parsed;
			getProcess(pid);
			if (tail.length === 0) return PROC_PID_ENTRIES;
			if (tail.length === 1 && tail[0] === "fd") {
				return listOpenFds(pid).map((fd) => ({ name: String(fd), isDirectory: false, isSymbolicLink: true }));
			}
			throw new KernelError("ENOTDIR", `not a directory: ${normalized}`);
		},

		async writeFile(path, content) {
			const normalized = normalizePath(path);
			rejectMutation(normalized);
			return vfs.writeFile(clonePathArg(path, normalized), content);
		},

		async createDir(path) {
			const normalized = normalizePath(path);
			rejectMutation(normalized);
			return vfs.createDir(clonePathArg(path, normalized));
		},

		async mkdir(path, optionsArg) {
			const normalized = normalizePath(path);
			rejectMutation(normalized);
			return vfs.mkdir(clonePathArg(path, normalized), optionsArg);
		},

		async exists(path) {
			const normalized = normalizePath(path);
			if (!isProcPath(normalized)) return vfs.exists(clonePathArg(path, normalized));
			if (
				normalized === "/proc" ||
				normalized === PROC_SELF_PREFIX ||
				normalized === PROC_SYS_PREFIX ||
				normalized === PROC_SYS_KERNEL_PREFIX ||
				normalized === PROC_SYS_KERNEL_HOSTNAME_PATH
			) {
				return true;
			}
			const parsed = parseProcPath(normalized);
			if (!parsed) return false;
			const { pid, tail } = parsed;
			if (!options.processTable.get(pid)) return false;
			if (tail.length === 0 || (tail.length === 1 && tail[0] === "fd")) return true;
			if (tail.length === 1 && (tail[0] === "cwd" || tail[0] === "exe" || tail[0] === "environ")) return true;
			if (tail.length === 2 && tail[0] === "fd") {
				const fd = Number(tail[1]);
				return Number.isInteger(fd) && fd >= 0 && options.fdTableManager.get(pid)?.get(fd) !== undefined;
			}
			return false;
		},

		async stat(path) {
			const normalized = normalizePath(path);
			if (!isProcPath(normalized)) return vfs.stat(clonePathArg(path, normalized));
			return getProcStat(normalized, true);
		},

		async removeFile(path) {
			const normalized = normalizePath(path);
			rejectMutation(normalized);
			return vfs.removeFile(clonePathArg(path, normalized));
		},

		async removeDir(path) {
			const normalized = normalizePath(path);
			rejectMutation(normalized);
			return vfs.removeDir(clonePathArg(path, normalized));
		},

		async rename(oldPath, newPath) {
			const normalizedOld = normalizePath(oldPath);
			const normalizedNew = normalizePath(newPath);
			rejectMutation(normalizedOld);
			rejectMutation(normalizedNew);
			return vfs.rename(clonePathArg(oldPath, normalizedOld), clonePathArg(newPath, normalizedNew));
		},

		async realpath(path) {
			const normalized = normalizePath(path);
			if (!isProcPath(normalized)) return vfs.realpath(clonePathArg(path, normalized));
			if (
				normalized === "/proc" ||
				normalized === PROC_SELF_PREFIX ||
				normalized === PROC_SYS_PREFIX ||
				normalized === PROC_SYS_KERNEL_PREFIX ||
				normalized === PROC_SYS_KERNEL_HOSTNAME_PATH
			) {
				return normalized;
			}
			const parsed = parseProcPath(normalized);
			if (!parsed) throw new KernelError("ENOENT", `no such file or directory: ${normalized}`);
			const { pid, tail } = parsed;
			getProcess(pid);
			if (tail.length === 0 || (tail.length === 1 && tail[0] === "fd")) return normalized;
			if (tail.length === 1 && tail[0] === "environ") return normalized;
			if ((tail.length === 1 && (tail[0] === "cwd" || tail[0] === "exe"))
				|| (tail.length === 2 && tail[0] === "fd")) {
				return getLinkTarget(pid, tail);
			}
			throw new KernelError("ENOENT", `no such file or directory: ${normalized}`);
		},

		async symlink(target, linkPath) {
			const normalized = normalizePath(linkPath);
			rejectMutation(normalized);
			return vfs.symlink(target, clonePathArg(linkPath, normalized));
		},

		async readlink(path) {
			const normalized = normalizePath(path);
			if (!isProcPath(normalized)) return vfs.readlink(clonePathArg(path, normalized));
			if (normalized === PROC_SELF_PREFIX) return PROC_SELF_PREFIX;
			const parsed = parseProcPath(normalized);
			if (!parsed) throw new KernelError("EINVAL", `invalid argument: ${normalized}`);
			const { pid, tail } = parsed;
			return getLinkTarget(pid, tail);
		},

		async lstat(path) {
			const normalized = normalizePath(path);
			if (!isProcPath(normalized)) return vfs.lstat(clonePathArg(path, normalized));
			return getProcStat(normalized, false);
		},

		async link(oldPath, newPath) {
			const normalizedOld = normalizePath(oldPath);
			const normalizedNew = normalizePath(newPath);
			rejectMutation(normalizedOld);
			rejectMutation(normalizedNew);
			return vfs.link(clonePathArg(oldPath, normalizedOld), clonePathArg(newPath, normalizedNew));
		},

		async chmod(path, mode) {
			const normalized = normalizePath(path);
			rejectMutation(normalized);
			return vfs.chmod(clonePathArg(path, normalized), mode);
		},

		async chown(path, uid, gid) {
			const normalized = normalizePath(path);
			rejectMutation(normalized);
			return vfs.chown(clonePathArg(path, normalized), uid, gid);
		},

		async utimes(path, atime, mtime) {
			const normalized = normalizePath(path);
			rejectMutation(normalized);
			return vfs.utimes(clonePathArg(path, normalized), atime, mtime);
		},

		async truncate(path, length) {
			const normalized = normalizePath(path);
			rejectMutation(normalized);
			return vfs.truncate(clonePathArg(path, normalized), length);
		},

		async pwrite(path, offset, data) {
			const normalized = normalizePath(path);
			rejectMutation(normalized);
			return vfs.pwrite(clonePathArg(path, normalized), offset, data);
		},
	};

	return wrapped;
}
