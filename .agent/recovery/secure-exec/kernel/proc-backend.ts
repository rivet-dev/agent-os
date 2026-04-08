/**
 * Proc backend.
 *
 * Standalone VirtualFileSystem that handles /proc paths.
 * Receives relative paths (e.g. "self/fd" not "/proc/self/fd").
 * Designed to be mounted at /proc via MountTable.
 */

import type { FDTableManager } from "./fd-table.js";
import type { MountEntry } from "./mount-table.js";
import type { ProcessTable } from "./process-table.js";
import { KernelError } from "./types.js";
import type { VirtualDirEntry, VirtualFileSystem, VirtualStat } from "./vfs.js";

const S_IFREG = 0o100000;
const S_IFDIR = 0o040000;
const S_IFLNK = 0o120000;
const PROC_INO_BASE = 0xfffe_0000;

const PROC_PID_ENTRIES: VirtualDirEntry[] = [
	{ name: "fd", isDirectory: true },
	{ name: "cwd", isDirectory: false, isSymbolicLink: true },
	{ name: "exe", isDirectory: false, isSymbolicLink: true },
	{ name: "environ", isDirectory: false },
];

const PROC_ROOT_ENTRIES: VirtualDirEntry[] = [
	{ name: "self", isDirectory: false, isSymbolicLink: true },
	{ name: "sys", isDirectory: true },
	{ name: "mounts", isDirectory: false },
];

const PROC_SYS_ENTRIES: VirtualDirEntry[] = [
	{ name: "kernel", isDirectory: true },
];

const PROC_SYS_KERNEL_ENTRIES: VirtualDirEntry[] = [
	{ name: "hostname", isDirectory: false },
];

export interface ProcBackendOptions {
	processTable: ProcessTable;
	fdTableManager: FDTableManager;
	hostname?: string;
	mountTable?: { getMounts(): ReadonlyArray<MountEntry> };
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

function encodeText(content: string): Uint8Array {
	return new TextEncoder().encode(content);
}

function encodeEnviron(env: Record<string, string>): Uint8Array {
	const entries = Object.entries(env);
	if (entries.length === 0) return new Uint8Array(0);
	return encodeText(
		`${entries.map(([key, value]) => `${key}=${value}`).join("\0")}\0`,
	);
}

function resolveExecPath(command: string): string {
	if (!command) return "";
	return command.startsWith("/") ? command : `/bin/${command}`;
}

function notFound(path: string): never {
	throw new KernelError("ENOENT", `no such proc entry: ${path}`);
}

function rejectWrite(path: string): never {
	throw new KernelError("EPERM", `cannot modify /proc/${path}`);
}

/**
 * Resolve /proc/self references to the given PID.
 * Paths are relative (no /proc prefix).
 */
export function resolveProcSelfPath(path: string, pid: number): string {
	if (path === "self") return `${pid}`;
	if (path.startsWith("self/")) return `${pid}${path.slice(4)}`;
	return path;
}

/**
 * Parse a relative proc path into PID + tail components.
 * "1/fd/0" -> { pid: 1, tail: ["fd", "0"] }
 */
function parsePidPath(path: string): { pid: number; tail: string[] } | null {
	const parts = path.split("/");
	const pid = Number(parts[0]);
	if (!Number.isInteger(pid) || pid < 0) return null;
	return { pid, tail: parts.slice(1) };
}

/**
 * Format mount entries in Linux /proc/mounts format.
 */
function formatMounts(mounts: ReadonlyArray<MountEntry>): string {
	return mounts
		.map((m) => {
			const fsType = m.path === "/" ? "rootfs" : "mount";
			const opts = m.readOnly ? "ro" : "rw";
			return `${fsType} ${m.path} ${fsType} ${opts} 0 0`;
		})
		.join("\n")
		.concat("\n");
}

/**
 * Create a standalone proc backend VFS.
 * All paths are relative to /proc (e.g. "self/fd", "1/environ", "mounts").
 * Mount at /proc via MountTable.
 */
export function createProcBackend(
	options: ProcBackendOptions,
): VirtualFileSystem {
	const kernelHostname = encodeText(`${options.hostname ?? "sandbox"}\n`);

	const getProcess = (pid: number) => {
		const entry = options.processTable.get(pid);
		if (!entry) throw new KernelError("ENOENT", `no such process ${pid}`);
		return entry;
	};

	const listPids = () =>
		Array.from(options.processTable.listProcesses().keys()).sort(
			(a, b) => a - b,
		);

	const listOpenFds = (pid: number) => {
		const table = options.fdTableManager.get(pid);
		if (!table) return [];
		const fds: number[] = [];
		for (const entry of table) fds.push(entry.fd);
		return fds.sort((a, b) => a - b);
	};

	const getFdEntry = (pid: number, fd: number) => {
		const table = options.fdTableManager.get(pid);
		const entry = table?.get(fd);
		if (!entry)
			throw new KernelError("ENOENT", `no such fd ${fd} for process ${pid}`);
		return entry;
	};

	const getLinkTarget = (pid: number, tail: string[]): string => {
		if (tail.length === 1 && tail[0] === "cwd") return getProcess(pid).cwd;
		if (tail.length === 1 && tail[0] === "exe")
			return resolveExecPath(getProcess(pid).command);
		if (tail.length === 2 && tail[0] === "fd") {
			const fd = Number(tail[1]);
			if (!Number.isInteger(fd) || fd < 0)
				throw new KernelError("ENOENT", `invalid fd ${tail[1]}`);
			return getFdEntry(pid, fd).description.path;
		}
		throw new KernelError("ENOENT", `unsupported proc link ${tail.join("/")}`);
	};

	const getProcFile = (pid: number, tail: string[]): Uint8Array => {
		if (tail.length === 1 && tail[0] === "cwd")
			return encodeText(getProcess(pid).cwd);
		if (tail.length === 1 && tail[0] === "exe")
			return encodeText(resolveExecPath(getProcess(pid).command));
		if (tail.length === 1 && tail[0] === "environ")
			return encodeEnviron(getProcess(pid).env);
		if (tail.length === 2 && tail[0] === "fd")
			return encodeText(getLinkTarget(pid, tail));
		throw new KernelError("ENOENT", `unsupported proc file ${tail.join("/")}`);
	};

	const getMountsContent = (): Uint8Array => {
		if (!options.mountTable) {
			return encodeText("rootfs / rootfs rw 0 0\n");
		}
		return encodeText(formatMounts(options.mountTable.getMounts()));
	};

	const getProcStat = (path: string, followSymlinks: boolean): VirtualStat => {
		// Root /proc directory
		if (path === "") return dirStat("proc");

		// /proc/self symlink
		if (path === "self") {
			return followSymlinks
				? dirStat("proc-self")
				: linkStat("proc-self-link", "self");
		}

		// /proc/mounts
		if (path === "mounts") {
			const content = getMountsContent();
			return fileStat("proc:mounts", content.length);
		}

		// /proc/sys tree
		if (path === "sys") return dirStat("proc:sys");
		if (path === "sys/kernel") return dirStat("proc:sys:kernel");
		if (path === "sys/kernel/hostname") {
			return fileStat("proc:sys:kernel:hostname", kernelHostname.length);
		}

		// /proc/[pid]/...
		const parsed = parsePidPath(path);
		if (!parsed) notFound(path);

		const { pid, tail } = parsed;
		getProcess(pid);

		if (tail.length === 0) return dirStat(`proc:${pid}`);
		if (tail.length === 1 && tail[0] === "fd") return dirStat(`proc:${pid}:fd`);
		if (tail.length === 1 && tail[0] === "environ") {
			return fileStat(
				`proc:${pid}:environ`,
				encodeEnviron(getProcess(pid).env).length,
			);
		}
		if (
			(tail.length === 1 && (tail[0] === "cwd" || tail[0] === "exe")) ||
			(tail.length === 2 && tail[0] === "fd")
		) {
			const target = getLinkTarget(pid, tail);
			if (!followSymlinks)
				return linkStat(`proc:${pid}:${tail.join(":")}`, target);
			// For symlinks when following, return file stat for the target
			return linkStat(`proc:${pid}:${tail.join(":")}`, target);
		}

		notFound(path);
	};

	const backend: VirtualFileSystem = {
		async readFile(path) {
			// Directories
			if (
				path === "" ||
				path === "self" ||
				path === "sys" ||
				path === "sys/kernel"
			) {
				throw new KernelError(
					"EISDIR",
					`illegal operation on a directory, read '/proc/${path}'`,
				);
			}

			// /proc/mounts
			if (path === "mounts") return getMountsContent();

			// /proc/sys/kernel/hostname
			if (path === "sys/kernel/hostname") return kernelHostname;

			// /proc/[pid]/...
			const parsed = parsePidPath(path);
			if (!parsed) notFound(path);

			const { pid, tail } = parsed;
			if (tail.length === 0 || (tail.length === 1 && tail[0] === "fd")) {
				throw new KernelError(
					"EISDIR",
					`illegal operation on a directory, read '/proc/${path}'`,
				);
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
			if (path === "") {
				return [
					...PROC_ROOT_ENTRIES,
					...listPids().map((pid) => ({
						name: String(pid),
						isDirectory: true,
					})),
				];
			}
			if (path === "sys") return PROC_SYS_ENTRIES;
			if (path === "sys/kernel") return PROC_SYS_KERNEL_ENTRIES;
			if (path === "self") {
				throw new KernelError(
					"ENOENT",
					`no such file or directory: /proc/${path}`,
				);
			}

			const parsed = parsePidPath(path);
			if (!parsed)
				throw new KernelError(
					"ENOENT",
					`no such file or directory: /proc/${path}`,
				);

			const { pid, tail } = parsed;
			getProcess(pid);

			if (tail.length === 0) return PROC_PID_ENTRIES;
			if (tail.length === 1 && tail[0] === "fd") {
				return listOpenFds(pid).map((fd) => ({
					name: String(fd),
					isDirectory: false,
					isSymbolicLink: true,
				}));
			}

			throw new KernelError("ENOTDIR", `not a directory: /proc/${path}`);
		},

		async writeFile(path, _content) {
			rejectWrite(path);
		},

		async createDir(path) {
			rejectWrite(path);
		},

		async mkdir(path, _options?) {
			rejectWrite(path);
		},

		async exists(path) {
			if (path === "" || path === "self" || path === "mounts") return true;
			if (
				path === "sys" ||
				path === "sys/kernel" ||
				path === "sys/kernel/hostname"
			) {
				return true;
			}

			const parsed = parsePidPath(path);
			if (!parsed) return false;

			const { pid, tail } = parsed;
			if (!options.processTable.get(pid)) return false;
			if (tail.length === 0 || (tail.length === 1 && tail[0] === "fd"))
				return true;
			if (
				tail.length === 1 &&
				(tail[0] === "cwd" || tail[0] === "exe" || tail[0] === "environ")
			)
				return true;
			if (tail.length === 2 && tail[0] === "fd") {
				const fd = Number(tail[1]);
				return (
					Number.isInteger(fd) &&
					fd >= 0 &&
					options.fdTableManager.get(pid)?.get(fd) !== undefined
				);
			}
			return false;
		},

		async stat(path) {
			return getProcStat(path, true);
		},

		async removeFile(path) {
			rejectWrite(path);
		},

		async removeDir(path) {
			rejectWrite(path);
		},

		async rename(_oldPath, _newPath) {
			throw new KernelError("EPERM", "cannot rename in /proc");
		},

		async realpath(path) {
			if (path === "" || path === "mounts") return path;
			if (path === "self") return path;
			if (
				path === "sys" ||
				path === "sys/kernel" ||
				path === "sys/kernel/hostname"
			) {
				return path;
			}

			const parsed = parsePidPath(path);
			if (!parsed) notFound(path);

			const { pid, tail } = parsed;
			getProcess(pid);

			if (tail.length === 0 || (tail.length === 1 && tail[0] === "fd"))
				return path;
			if (tail.length === 1 && tail[0] === "environ") return path;
			if (
				(tail.length === 1 && (tail[0] === "cwd" || tail[0] === "exe")) ||
				(tail.length === 2 && tail[0] === "fd")
			) {
				return getLinkTarget(pid, tail);
			}

			notFound(path);
		},

		async symlink(_target, _linkPath) {
			throw new KernelError("EPERM", "cannot create symlink in /proc");
		},

		async readlink(path) {
			if (path === "self") return "self";

			const parsed = parsePidPath(path);
			if (!parsed)
				throw new KernelError("EINVAL", `invalid argument: /proc/${path}`);

			const { pid, tail } = parsed;
			return getLinkTarget(pid, tail);
		},

		async lstat(path) {
			return getProcStat(path, false);
		},

		async link(_oldPath, _newPath) {
			throw new KernelError("EPERM", "cannot link in /proc");
		},

		async chmod(path, _mode) {
			rejectWrite(path);
		},

		async chown(path, _uid, _gid) {
			rejectWrite(path);
		},

		async utimes(path, _atime, _mtime) {
			rejectWrite(path);
		},

		async truncate(path, _length) {
			rejectWrite(path);
		},

		async pwrite(path, _offset, _data) {
			rejectWrite(path);
		},
	};

	return backend;
}
