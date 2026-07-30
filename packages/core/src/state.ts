import * as protocol from "./generated-protocol.js";
import { fromGeneratedProcessSnapshotStatus } from "./protocol-maps.js";

// The u64 identity/size fields stay bigint: host filesystems (overlayfs,
// FUSE, network mounts) can report dev/ino values above
// Number.MAX_SAFE_INTEGER, and the Rust client keeps the same fields as u64.
// Consumers that need JS numbers convert (lossily, like Node's default
// fs.stat) at their own boundary.
export interface LiveGuestFilesystemStat {
	mode: number;
	size: bigint;
	blocks: bigint;
	dev: bigint;
	rdev: bigint;
	is_directory: boolean;
	is_symbolic_link: boolean;
	atime_ms: number;
	mtime_ms: number;
	ctime_ms: number;
	birthtime_ms: number;
	ino: bigint;
	nlink: bigint;
	uid: number;
	gid: number;
}

export interface LiveSocketStateEntry {
	process_id: string;
	host?: string;
	port?: number;
	path?: string;
}

export interface LiveProcessSnapshotEntry {
	process_id: string;
	pid: number;
	ppid: number;
	pgid: number;
	sid: number;
	driver: string;
	command: string;
	args?: string[];
	cwd: string;
	status: "running" | "exited" | "stopped";
	exit_code?: number;
}

export function fromGeneratedGuestFilesystemStat(
	stat: protocol.GuestFilesystemStat,
): LiveGuestFilesystemStat {
	return {
		mode: stat.mode,
		size: stat.size,
		blocks: stat.blocks,
		dev: stat.dev,
		rdev: stat.rdev,
		is_directory: stat.isDirectory,
		is_symbolic_link: stat.isSymbolicLink,
		atime_ms: Number(stat.atimeMs),
		mtime_ms: Number(stat.mtimeMs),
		ctime_ms: Number(stat.ctimeMs),
		birthtime_ms: Number(stat.birthtimeMs),
		ino: stat.ino,
		nlink: stat.nlink,
		uid: stat.uid,
		gid: stat.gid,
	};
}

export function fromGeneratedSocketStateEntry(
	entry: protocol.SocketStateEntry,
): LiveSocketStateEntry {
	return {
		process_id: entry.processId,
		...(entry.host !== null ? { host: entry.host } : {}),
		...(entry.port !== null ? { port: entry.port } : {}),
		...(entry.path !== null ? { path: entry.path } : {}),
	};
}

export function fromGeneratedProcessSnapshotEntry(
	entry: protocol.ProcessSnapshotEntry,
): LiveProcessSnapshotEntry {
	return {
		process_id: entry.processId,
		pid: entry.pid,
		ppid: entry.ppid,
		pgid: entry.pgid,
		sid: entry.sid,
		driver: entry.driver,
		command: entry.command,
		args: [...entry.args],
		cwd: entry.cwd,
		status: fromGeneratedProcessSnapshotStatus(entry.status),
		...(entry.exitCode !== null ? { exit_code: entry.exitCode } : {}),
	};
}
