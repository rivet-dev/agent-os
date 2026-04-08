/**
 * Per-PID file descriptor table.
 *
 * Each process gets its own FD number space. Multiple FDs can share the
 * same FileDescription (via dup/dup2), which shares the cursor position.
 * Standard FDs 0-2 are pre-allocated per process.
 */

import type { FDEntry, FDStat, FileDescription } from "./types.js";
import {
	FILETYPE_REGULAR_FILE,
	FILETYPE_DIRECTORY,
	FILETYPE_CHARACTER_DEVICE,
	FILETYPE_PIPE,
	O_RDONLY,
	O_WRONLY,
	O_RDWR,
	O_APPEND,
	O_CLOEXEC,
	KernelError,
} from "./types.js";

/** Maximum open FDs per process before allocations are rejected (EMFILE). */
export const MAX_FDS_PER_PROCESS = 256;

/** Allocator function that creates a FileDescription with a unique ID. */
export type DescriptionAllocator = (path: string, flags: number) => FileDescription;

/**
 * FD table for a single process.
 *
 * Manages FD allocation, dup/dup2, and shared cursor via FileDescription.
 */
export class ProcessFDTable {
	private entries: Map<number, FDEntry> = new Map();
	private nextFd = 3; // 0, 1, 2 reserved
	private allocDesc: DescriptionAllocator;

	constructor(allocDesc: DescriptionAllocator) {
		this.allocDesc = allocDesc;
	}

	/** Pre-allocate stdin, stdout, stderr */
	initStdio(
		stdinDesc: FileDescription,
		stdoutDesc: FileDescription,
		stderrDesc: FileDescription,
	): void {
		this.entries.set(0, {
			fd: 0,
			description: stdinDesc,
			rights: 0n,
			filetype: FILETYPE_CHARACTER_DEVICE,
			cloexec: false,
		});
		this.entries.set(1, {
			fd: 1,
			description: stdoutDesc,
			rights: 0n,
			filetype: FILETYPE_CHARACTER_DEVICE,
			cloexec: false,
		});
		this.entries.set(2, {
			fd: 2,
			description: stderrDesc,
			rights: 0n,
			filetype: FILETYPE_CHARACTER_DEVICE,
			cloexec: false,
		});
	}

	/** Pre-allocate stdin, stdout, stderr with custom filetypes (for pipe wiring). */
	initStdioWithTypes(
		stdinDesc: FileDescription,
		stdinType: number,
		stdoutDesc: FileDescription,
		stdoutType: number,
		stderrDesc: FileDescription,
		stderrType: number,
	): void {
		// Shared descriptions (from pipes) get refCount bumped
		stdinDesc.refCount++;
		stdoutDesc.refCount++;
		stderrDesc.refCount++;
		this.entries.set(0, { fd: 0, description: stdinDesc, rights: 0n, filetype: stdinType, cloexec: false });
		this.entries.set(1, { fd: 1, description: stdoutDesc, rights: 0n, filetype: stdoutType, cloexec: false });
		this.entries.set(2, { fd: 2, description: stderrDesc, rights: 0n, filetype: stderrType, cloexec: false });
	}

	/** Open a new FD for the given path and flags */
	open(path: string, flags: number, filetype?: number): number {
		const fd = this.allocateFd();
		const cloexec = (flags & O_CLOEXEC) !== 0;
		const storedFlags = flags & ~O_CLOEXEC;
		const description = this.allocDesc(path, storedFlags);
		this.entries.set(fd, {
			fd,
			description,
			rights: 0n,
			filetype: filetype ?? FILETYPE_REGULAR_FILE,
			cloexec,
		});
		return fd;
	}

	/** Open a new FD pointing to an existing FileDescription (for pipes, inherited FDs) */
	openWith(
		description: FileDescription,
		filetype: number,
		targetFd?: number,
	): number {
		const fd = targetFd ?? this.allocateFd();
		description.refCount++;
		this.entries.set(fd, {
			fd,
			description,
			rights: 0n,
			filetype,
			cloexec: false,
		});
		return fd;
	}

	get(fd: number): FDEntry | undefined {
		return this.entries.get(fd);
	}

	/** Close an FD. Decrements the refcount on the shared FileDescription. */
	close(fd: number): boolean {
		const entry = this.entries.get(fd);
		if (!entry) return false;
		entry.description.refCount--;
		this.entries.delete(fd);
		return true;
	}

	/** Duplicate an FD — new FD shares the same FileDescription (cursor). cloexec cleared on new FD (POSIX). */
	dup(fd: number): number {
		const entry = this.entries.get(fd);
		if (!entry) throw new KernelError("EBADF", `bad file descriptor ${fd}`);
		const newFd = this.allocateFd();
		entry.description.refCount++;
		this.entries.set(newFd, {
			fd: newFd,
			description: entry.description,
			rights: entry.rights,
			filetype: entry.filetype,
			cloexec: false,
		});
		return newFd;
	}

	/** Duplicate FD to lowest available >= minFd (F_DUPFD). cloexec cleared on new FD. */
	dupMinFd(fd: number, minFd: number): number {
		const entry = this.entries.get(fd);
		if (!entry) throw new KernelError("EBADF", `bad file descriptor ${fd}`);
		if (this.entries.size >= MAX_FDS_PER_PROCESS) {
			throw new KernelError("EMFILE", "too many open files");
		}
		let newFd = minFd;
		while (this.entries.has(newFd)) newFd++;
		entry.description.refCount++;
		this.entries.set(newFd, {
			fd: newFd,
			description: entry.description,
			rights: entry.rights,
			filetype: entry.filetype,
			cloexec: false,
		});
		return newFd;
	}

	/** Duplicate oldFd to newFd. Closes newFd first if open. cloexec cleared on new FD (POSIX). */
	dup2(oldFd: number, newFd: number): void {
		const entry = this.entries.get(oldFd);
		if (!entry) throw new KernelError("EBADF", `bad file descriptor ${oldFd}`);
		if (oldFd === newFd) return;

		// Close newFd if already open
		if (this.entries.has(newFd)) {
			this.close(newFd);
		}

		entry.description.refCount++;
		this.entries.set(newFd, {
			fd: newFd,
			description: entry.description,
			rights: entry.rights,
			filetype: entry.filetype,
			cloexec: false,
		});
	}

	stat(fd: number): FDStat {
		const entry = this.entries.get(fd);
		if (!entry) throw new KernelError("EBADF", `bad file descriptor ${fd}`);
		return {
			filetype: entry.filetype,
			flags: entry.description.flags,
			rights: entry.rights,
		};
	}

	/** Create a copy of this table for a child process (FD inheritance). Skips cloexec FDs. */
	fork(): ProcessFDTable {
		const child = new ProcessFDTable(this.allocDesc);
		child.nextFd = this.nextFd;
		for (const [fd, entry] of this.entries) {
			if (entry.cloexec) continue;
			entry.description.refCount++;
			child.entries.set(fd, {
				fd,
				description: entry.description,
				rights: entry.rights,
				filetype: entry.filetype,
				cloexec: false,
			});
		}
		return child;
	}

	/** Close all FDs, decrementing all refcounts. */
	closeAll(): void {
		for (const [fd] of this.entries) {
			this.close(fd);
		}
	}

	/** Iterate all FD entries (for cleanup inspection). */
	*[Symbol.iterator](): IterableIterator<FDEntry> {
		yield* this.entries.values();
	}

	private allocateFd(): number {
		// Enforce per-process FD limit
		if (this.entries.size >= MAX_FDS_PER_PROCESS) {
			throw new KernelError("EMFILE", "too many open files");
		}
		// Find lowest available FD >= nextFd hint
		while (this.entries.has(this.nextFd)) {
			this.nextFd++;
		}
		return this.nextFd++;
	}
}

/**
 * Kernel-level FD table manager.
 * Owns per-PID FD tables and coordinates shared FileDescriptions.
 */
export class FDTableManager {
	private tables: Map<number, ProcessFDTable> = new Map();
	private nextDescriptionId = 1;

	/** Per-instance allocator bound to this manager's ID counter. */
	private allocDesc: DescriptionAllocator = (path, flags) => ({
		id: this.nextDescriptionId++,
		path,
		cursor: 0n,
		flags,
		refCount: 1,
	});

	/** Create a new FD table for a process with standard FDs. */
	create(pid: number): ProcessFDTable {
		const table = new ProcessFDTable(this.allocDesc);
		table.initStdio(
			this.allocDesc("/dev/stdin", O_RDONLY),
			this.allocDesc("/dev/stdout", O_WRONLY),
			this.allocDesc("/dev/stderr", O_WRONLY),
		);
		this.tables.set(pid, table);
		return table;
	}

	/**
	 * Create a new FD table with custom stdio FileDescriptions.
	 * Used for pipe wiring: pass a pipe read/write end as stdin/stdout/stderr.
	 * Null entries fall back to default device nodes.
	 */
	createWithStdio(
		pid: number,
		stdinOverride: { description: FileDescription; filetype: number } | null,
		stdoutOverride: { description: FileDescription; filetype: number } | null,
		stderrOverride: { description: FileDescription; filetype: number } | null,
	): ProcessFDTable {
		const table = new ProcessFDTable(this.allocDesc);
		const stdinDesc = stdinOverride
			? stdinOverride.description
			: this.allocDesc("/dev/stdin", O_RDONLY);
		const stdinType = stdinOverride?.filetype ?? FILETYPE_CHARACTER_DEVICE;
		const stdoutDesc = stdoutOverride
			? stdoutOverride.description
			: this.allocDesc("/dev/stdout", O_WRONLY);
		const stdoutType = stdoutOverride?.filetype ?? FILETYPE_CHARACTER_DEVICE;
		const stderrDesc = stderrOverride
			? stderrOverride.description
			: this.allocDesc("/dev/stderr", O_WRONLY);
		const stderrType = stderrOverride?.filetype ?? FILETYPE_CHARACTER_DEVICE;

		table.initStdioWithTypes(stdinDesc, stdinType, stdoutDesc, stdoutType, stderrDesc, stderrType);
		this.tables.set(pid, table);
		return table;
	}

	/** Create a child FD table by forking the parent's. */
	fork(parentPid: number, childPid: number): ProcessFDTable {
		const parentTable = this.tables.get(parentPid);
		if (!parentTable) {
			return this.create(childPid);
		}
		const childTable = parentTable.fork();
		this.tables.set(childPid, childTable);
		return childTable;
	}

	get(pid: number): ProcessFDTable | undefined {
		return this.tables.get(pid);
	}

	/** Check whether a PID has an FD table. */
	has(pid: number): boolean {
		return this.tables.has(pid);
	}

	/** Number of active FD tables. */
	get size(): number {
		return this.tables.size;
	}

	/** Remove and close all FDs for a process. */
	remove(pid: number): void {
		const table = this.tables.get(pid);
		if (table) {
			table.closeAll();
			this.tables.delete(pid);
		}
	}
}
