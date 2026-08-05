import { describe, expect, it } from "vitest";
import { BROWSER_WASI_POLYFILL_CODE } from "../../src/wasi-polyfill.js";
import { wireStatToVirtualStat } from "../../src/converged-fs-bridge.js";

type WasiInstance = {
	fdTable: Map<number, unknown>;
	wasiImport: Record<string, (...args: unknown[]) => number>;
	instance: { exports: { memory: WebAssembly.Memory } };
	_collectIovs: (...args: unknown[]) => Uint8Array;
	_boundedReadLength: (...args: unknown[]) => number;
	_writeToIovs: (...args: unknown[]) => number;
	_writeUint32: (...args: unknown[]) => number;
	_fdClose: (...args: unknown[]) => number;
	_fdPread: (...args: unknown[]) => number;
	_fdPwrite: (...args: unknown[]) => number;
	_flushPipeConsumers: (pipe: {
		chunks: Uint8Array[];
		consumers: Map<string, { childId: string }>;
		readHandleCount: number;
	}) => boolean;
	_writeFilestat: (...args: unknown[]) => number;
};

type WasiConstructor = new (options?: Record<string, unknown>) => WasiInstance;
type WasiTestContext = { wasi: WasiInstance; restore: () => void };

const ERRNO_SUCCESS = 0;
const FILETYPE_REGULAR_FILE = 4;
const LARGE_OFFSET = 2n ** 32n + 5n;
const EXACT_INO = BigInt(Number.MAX_SAFE_INTEGER) + 4n;
const EXACT_SIZE = BigInt(Number.MAX_SAFE_INTEGER) + 6n;

function loadWasi(fsModule: unknown): {
	WASI: WasiConstructor;
	restore: () => void;
} {
	const previousModule = (
		globalThis as typeof globalThis & { __agentOSWasiModule?: unknown }
	).__agentOSWasiModule;
	const previousRequire = (
		globalThis as typeof globalThis & { require?: unknown }
	).require;

	delete (globalThis as typeof globalThis & { __agentOSWasiModule?: unknown })
		.__agentOSWasiModule;
	(globalThis as typeof globalThis & { require?: unknown }).require = (
		name: string,
	) => {
		if (name === "fs" || name === "node:fs") {
			return fsModule;
		}
		return require(name);
	};

	const restore = () => {
		if (previousRequire === undefined) {
			delete (globalThis as typeof globalThis & { require?: unknown }).require;
		} else {
			(globalThis as typeof globalThis & { require?: unknown }).require =
				previousRequire;
		}
		if (previousModule === undefined) {
			delete (
				globalThis as typeof globalThis & { __agentOSWasiModule?: unknown }
			).__agentOSWasiModule;
		} else {
			(
				globalThis as typeof globalThis & { __agentOSWasiModule?: unknown }
				).__agentOSWasiModule = previousModule;
		}
	};

	const module = { exports: {} as { WASI?: WasiConstructor } };
	new Function("module", "exports", BROWSER_WASI_POLYFILL_CODE)(
		module,
		module.exports,
	);
	if (!module.exports.WASI) {
		restore();
		throw new Error("WASI constructor was not exported");
	}
	return { WASI: module.exports.WASI, restore };
}

function createWasi(fsModule: unknown): WasiTestContext {
	const { WASI, restore } = loadWasi(fsModule);
	const wasi = new WASI({ returnOnExit: true });
	wasi.instance = { exports: { memory: new WebAssembly.Memory({ initial: 1 }) } };
	wasi._writeUint32 = () => ERRNO_SUCCESS;
	return { wasi, restore };
}

describe("browser WASI polyfill", () => {
	it("exposes fd_datasync with fd_sync semantics", () => {
		const synced: number[] = [];
		const { wasi, restore } = createWasi({
			fsyncSync: (fd: number) => synced.push(fd),
		});
		try {
			wasi.fdTable.set(4, { kind: "file", realFd: 44 });
			expect(wasi.wasiImport.fd_datasync?.(4)).toBe(ERRNO_SUCCESS);
			expect(wasi.wasiImport.fd_sync?.(4)).toBe(ERRNO_SUCCESS);
			expect(synced).toEqual([44, 44]);
		} finally {
			restore();
		}
	});

	it("keeps pipe bytes for a parent-owned read handle", () => {
		const writes: unknown[][] = [];
		const previous = (
			globalThis as typeof globalThis & { __agentOSSyncRpc?: unknown }
		).__agentOSSyncRpc;
		(
			globalThis as typeof globalThis & { __agentOSSyncRpc?: unknown }
		).__agentOSSyncRpc = {
			callSync: (_method: string, args: unknown[]) => writes.push(args),
		};
		const { wasi, restore } = createWasi({});
		try {
			const pipe = {
				chunks: [new Uint8Array([1, 2, 3])],
				consumers: new Map([["child:stdin", { childId: "child" }]]),
				readHandleCount: 1,
			};
			expect(wasi._flushPipeConsumers(pipe)).toBe(false);
			expect(pipe.chunks).toHaveLength(1);
			expect(writes).toEqual([]);
		} finally {
			restore();
			(
				globalThis as typeof globalThis & { __agentOSSyncRpc?: unknown }
			).__agentOSSyncRpc = previous;
		}
	});

	it("delivers a pipe chunk to exactly one child consumer", () => {
		const writes: unknown[][] = [];
		const previous = (
			globalThis as typeof globalThis & { __agentOSSyncRpc?: unknown }
		).__agentOSSyncRpc;
		(
			globalThis as typeof globalThis & { __agentOSSyncRpc?: unknown }
		).__agentOSSyncRpc = {
			callSync: (_method: string, args: unknown[]) => writes.push(args),
		};
		const { wasi, restore } = createWasi({});
		try {
			const pipe = {
				chunks: [new Uint8Array([1, 2, 3])],
				consumers: new Map([
					["first:stdin", { childId: "first" }],
					["second:stdin", { childId: "second" }],
				]),
				readHandleCount: 0,
			};
			expect(wasi._flushPipeConsumers(pipe)).toBe(true);
			expect(pipe.chunks).toEqual([]);
			expect(writes).toHaveLength(1);
			expect(writes[0]?.[0]).toBe("first");
		} finally {
			restore();
			(
				globalThis as typeof globalThis & { __agentOSSyncRpc?: unknown }
			).__agentOSSyncRpc = previous;
		}
	});

	it("closes the backing descriptor for opened directories", () => {
		const closed: number[] = [];
		const { wasi, restore } = createWasi({
			closeSync: (fd: number) => closed.push(fd),
		});
		try {
			wasi.fdTable.set(4, { kind: "directory", realFd: 99 });

			expect(wasi._fdClose(4)).toBe(ERRNO_SUCCESS);
			expect(closed).toEqual([99]);
			expect(wasi.fdTable.has(4)).toBe(false);
		} finally {
			restore();
		}
	});

	it("passes fd_pwrite offsets above 4 GiB without wasm32 truncation", () => {
		const positions: unknown[] = [];
		const { wasi, restore } = createWasi({
			writeSync: (
				_fd: number,
				_buffer: Uint8Array,
				_offset: number,
				length: number,
				position: unknown,
			) => {
				positions.push(position);
				return length;
			},
		});
		try {
			wasi.fdTable.set(4, { kind: "file", realFd: 99 });
			wasi._collectIovs = () => new Uint8Array([1, 2, 3]);

			expect(wasi._fdPwrite(4, 0, 0, LARGE_OFFSET, 0)).toBe(ERRNO_SUCCESS);
			expect(positions).toEqual([Number(LARGE_OFFSET)]);
		} finally {
			restore();
		}
	});

	it("passes fd_pread offsets above 4 GiB without wasm32 truncation", () => {
		const positions: unknown[] = [];
		const { wasi, restore } = createWasi({
			readSync: (
				_fd: number,
				buffer: Uint8Array,
				offset: number,
				length: number,
				position: unknown,
			) => {
				positions.push(position);
				buffer.fill(7, offset, offset + length);
				return length;
			},
		});
		try {
			wasi.fdTable.set(4, { kind: "file", realFd: 99 });
			wasi._boundedReadLength = () => 3;
			wasi._writeToIovs = (_iovs, _iovsLen, bytes: Uint8Array) =>
				bytes.length;

			expect(wasi._fdPread(4, 0, 0, LARGE_OFFSET, 0)).toBe(ERRNO_SUCCESS);
			expect(positions).toEqual([Number(LARGE_OFFSET)]);
		} finally {
			restore();
		}
	});

	it("writes exact bigint inode and size values to WASI filestat", () => {
		const { wasi, restore } = createWasi({});
		try {
			const view = new DataView(wasi.instance.exports.memory.buffer);

			expect(
				wasi._writeFilestat(
					128,
					wireStatToVirtualStat({
						mode: 0o100644,
						size: EXACT_SIZE,
						blocks: 1n,
						dev: 2n,
						rdev: 0n,
						is_directory: false,
						is_symbolic_link: false,
						atime_ms: 0,
						mtime_ms: 0,
						ctime_ms: 0,
						birthtime_ms: 0,
						ino: EXACT_INO,
						nlink: 1n,
						uid: 1000,
						gid: 1000,
					}),
					FILETYPE_REGULAR_FILE,
				),
			).toBe(ERRNO_SUCCESS);

			expect(view.getBigUint64(136, true)).toBe(EXACT_INO);
			expect(view.getBigUint64(160, true)).toBe(EXACT_SIZE);
		} finally {
			restore();
		}
	});
});
