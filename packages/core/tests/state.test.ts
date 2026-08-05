import { describe, expect, it } from "vitest";
import * as protocol from "../src/generated-protocol.js";
import {
	fromGeneratedGuestFilesystemStat,
	fromGeneratedProcessSnapshotEntry,
	fromGeneratedSocketStateEntry,
} from "../src/state.js";

describe("state conversion", () => {
	it("maps generated guest filesystem stat entries to live stat entries", () => {
		expect(
			fromGeneratedGuestFilesystemStat({
				mode: 0o100644,
				size: 42n,
				blocks: 1n,
				dev: 2n,
				rdev: 0n,
				isDirectory: false,
				isSymbolicLink: false,
				atimeMs: 100n,
				mtimeMs: 200n,
				ctimeMs: 300n,
				birthtimeMs: 400n,
				ino: 10n,
				nlink: 1n,
				uid: 1000,
				gid: 1000,
			}),
		).toEqual({
			mode: 0o100644,
			size: 42n,
			blocks: 1n,
			dev: 2n,
			rdev: 0n,
			is_directory: false,
			is_symbolic_link: false,
			atime_ms: 100,
			mtime_ms: 200,
			ctime_ms: 300,
			birthtime_ms: 400,
			ino: 10n,
			nlink: 1n,
			uid: 1000,
			gid: 1000,
		});
	});

	it("preserves u64 stat identity fields above the JS safe integer range", () => {
		// Host filesystems (overlayfs, FUSE, network mounts) can report
		// dev/ino values above Number.MAX_SAFE_INTEGER; converting them to
		// JS numbers here used to throw and break every stat over the mount.
		const unsafe = BigInt(Number.MAX_SAFE_INTEGER) + 2n;

		const stat = fromGeneratedGuestFilesystemStat({
			mode: 0o100644,
			size: unsafe,
			blocks: unsafe,
			dev: unsafe,
			rdev: unsafe,
			isDirectory: false,
			isSymbolicLink: false,
			atimeMs: 100n,
			mtimeMs: 200n,
			ctimeMs: 300n,
			birthtimeMs: 400n,
			ino: unsafe,
			nlink: 1n,
			uid: 1000,
			gid: 1000,
		});

		expect(stat.dev).toBe(unsafe);
		expect(stat.rdev).toBe(unsafe);
		expect(stat.ino).toBe(unsafe);
		expect(stat.size).toBe(unsafe);
		expect(stat.blocks).toBe(unsafe);
	});

	it("converts guest stat timestamps lossily instead of throwing", () => {
		const farFuture = 2n ** 60n;

		const stat = fromGeneratedGuestFilesystemStat({
			mode: 0o100644,
			size: 42n,
			blocks: 1n,
			dev: 2n,
			rdev: 0n,
			isDirectory: false,
			isSymbolicLink: false,
			atimeMs: farFuture,
			mtimeMs: farFuture,
			ctimeMs: farFuture,
			birthtimeMs: farFuture,
			ino: 10n,
			nlink: 1n,
			uid: 1000,
			gid: 1000,
		});

		expect(stat.atime_ms).toBe(Number(farFuture));
		expect(stat.mtime_ms).toBe(Number(farFuture));
		expect(stat.ctime_ms).toBe(Number(farFuture));
		expect(stat.birthtime_ms).toBe(Number(farFuture));
	});

	it("maps generated socket state entries to live socket entries", () => {
		expect(
			fromGeneratedSocketStateEntry({
				processId: "proc",
				host: "127.0.0.1",
				port: 8080,
				path: null,
			}),
		).toEqual({
			process_id: "proc",
			host: "127.0.0.1",
			port: 8080,
		});
	});

	it("maps generated process snapshots to live process snapshots", () => {
		expect(
			fromGeneratedProcessSnapshotEntry({
				processId: "proc",
				pid: 10,
				ppid: 1,
				pgid: 10,
				sid: 10,
				driver: "native",
				command: "node",
				args: ["-e", "0"],
				cwd: "/work",
				status: protocol.ProcessSnapshotStatus.Exited,
				exitCode: 0,
			}),
		).toEqual({
			process_id: "proc",
			pid: 10,
			ppid: 1,
			pgid: 10,
			sid: 10,
			driver: "native",
			command: "node",
			args: ["-e", "0"],
			cwd: "/work",
			status: "exited",
			exit_code: 0,
		});
	});
});
