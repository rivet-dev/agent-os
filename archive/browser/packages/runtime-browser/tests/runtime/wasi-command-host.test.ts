import * as nodePath from "node:path";
import { readFileSync } from "node:fs";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
	getRuntimePolyfillCode,
	type WasiCommandProcessLimits,
} from "../../src/runtime.js";

type WasiOptions = {
	args: string[];
	env: Record<string, string>;
	preopens: Record<string, string>;
};

type HostProcessImports = {
	proc_spawn(...args: number[]): number;
	proc_spawn_v2(...args: number[]): number;
	proc_spawn_v3(...args: number[]): number;
	proc_exec(...args: number[]): number;
	proc_fexec(...args: number[]): number;
	proc_waitpid(...args: number[]): number;
	proc_waitpid_v2(...args: number[]): number;
	proc_kill(pid: number, signal: number): number;
	proc_getpgid(pid: number, retPgid: number): number;
	proc_setpgid(pid: number, pgid: number): number;
	proc_sigaction(...args: number[]): number;
	proc_signal_mask_v2(...args: number[]): number;
	fd_dup(fd: number, retNewFd: number): number;
	fd_getfd(fd: number, retFlags: number): number;
	fd_setfd(fd: number, flags: number): number;
	fd_dup_min(fd: number, minFd: number, retNewFd: number): number;
	fd_dup2(oldFd: number, newFd: number): number;
	fd_pipe(retReadFd: number, retWriteFd: number): number;
	proc_closefrom(lowFd: number): number;
};

type CommandHost = {
	imports: {
		host_fs: {
			fchmod(fd: number, mode: number): number;
			ftruncate(fd: number, length: bigint): number;
		};
		host_process: HostProcessImports;
	};
	installBlockingStdin(processLike: unknown): CommandHost;
	setMemory(memory: WebAssembly.Memory): CommandHost;
	setParentWasi(wasi: unknown): CommandHost;
};

type CommandHostFactory = (options: {
	WASI: new (options: WasiOptions) => unknown;
	commands: Record<string, WebAssembly.Module>;
	cwd?: string;
	maxSpawnFileActions?: number;
	maxSpawnFileActionBytes?: number;
}) => Promise<CommandHost>;

const EMPTY_WASM = new WebAssembly.Module(
	new Uint8Array([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]),
);
const MEMORY_WASM = new WebAssembly.Module(
	new Uint8Array([
		0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x05, 0x03, 0x01, 0x00,
		0x01, 0x07, 0x0a, 0x01, 0x06, 0x6d, 0x65, 0x6d, 0x6f, 0x72, 0x79, 0x02,
		0x00,
	]),
);
const ERRNO_SUCCESS = 0;
const ERRNO_2BIG = 1;
const ERRNO_BADF = 8;
const ERRNO_CHILD = 10;
const ERRNO_INVAL = 28;
const ERRNO_MFILE = 33;
const ERRNO_NOENT = 44;
const ERRNO_NOSYS = 52;
const ERRNO_PERM = 63;
const ERRNO_SRCH = 71;
const FIRST_SYNTHETIC_FD = 1 << 20;

let previousWasiHost: unknown;

afterEach(() => {
	if (previousWasiHost === undefined) {
		delete (globalThis as typeof globalThis & { __agentOSWasiHost?: unknown })
			.__agentOSWasiHost;
	} else {
		(
			globalThis as typeof globalThis & { __agentOSWasiHost?: unknown }
		).__agentOSWasiHost = previousWasiHost;
	}
	previousWasiHost = undefined;
	vi.restoreAllMocks();
});

function loadCommandHostFactory(
	fsModule: unknown,
	processLimits?: WasiCommandProcessLimits,
): CommandHostFactory {
	const module = {
		exports: {} as { createWasiCommandHost?: CommandHostFactory },
	};
	const source = getRuntimePolyfillCode(
		"agentos:wasi-command-host",
		processLimits,
	);
	if (!source) throw new Error("command host polyfill was not found");
	new Function("module", "exports", "require", source)(
		module,
		module.exports,
		(name: string) => {
			if (name === "node:fs" || name === "fs") return fsModule;
			if (name === "node:path" || name === "path") return nodePath;
			throw new Error(`unexpected require: ${name}`);
		},
	);
	if (!module.exports.createWasiCommandHost) {
		throw new Error("command host factory was not exported");
	}
	return module.exports.createWasiCommandHost;
}

function writeValue(
	memory: WebAssembly.Memory,
	offset: number,
	value: string,
): [number, number] {
	const encoded = new TextEncoder().encode(value);
	new Uint8Array(memory.buffer).set(encoded, offset);
	return [offset, encoded.length];
}

function writeList(
	memory: WebAssembly.Memory,
	offset: number,
	values: string[],
): [number, number] {
	return writeValue(memory, offset, `${values.join("\0")}\0`);
}

function readU32(memory: WebAssembly.Memory, offset: number): number {
	return new DataView(memory.buffer).getUint32(offset, true);
}

function writeSpawnActions(
	memory: WebAssembly.Memory,
	offset: number,
	actions: Array<{
		command: number;
		fd: number;
		sourceFd?: number;
		oflag?: number;
		mode?: number;
		path?: string;
	}>,
): [number, number] {
	const encoder = new TextEncoder();
	const encoded = actions.map((action) => ({
		action,
		path: encoder.encode(action.path || ""),
	}));
	const length = encoded.reduce(
		(total, entry) => total + 24 + entry.path.length,
		0,
	);
	const value = new Uint8Array(memory.buffer, offset, length);
	const data = new DataView(memory.buffer, offset, length);
	let cursor = 0;
	for (const { action, path } of encoded) {
		data.setUint32(cursor, action.command, true);
		data.setInt32(cursor + 4, action.fd, true);
		data.setInt32(cursor + 8, action.sourceFd ?? -1, true);
		data.setInt32(cursor + 12, action.oflag ?? 0, true);
		data.setUint32(cursor + 16, action.mode ?? 0, true);
		data.setUint32(cursor + 20, path.length, true);
		value.set(path, cursor + 24);
		cursor += 24 + path.length;
	}
	return [offset, length];
}

function spawn(
	processImports: HostProcessImports,
	memory: WebAssembly.Memory,
	command: string,
	argv: string[],
	env: string[],
	stdio: [number, number, number] = [0, 1, 2],
): { errno: number; pid: number } {
	const [commandPtr, commandLen] = writeValue(memory, 256, command);
	const [argvPtr, argvLen] = writeList(memory, 1024, argv);
	const [envPtr, envLen] = writeList(memory, 2048, env);
	const [cwdPtr, cwdLen] = writeValue(memory, 3072, "/work");
	const retPid = 128;
	const errno = processImports.proc_spawn_v2(
		commandPtr,
		commandLen,
		argvPtr,
		argvLen,
		envPtr,
		envLen,
		...stdio,
		cwdPtr,
		cwdLen,
		retPid,
	);
	return { errno, pid: readU32(memory, retPid) };
}

function spawnV3WithActions(
	processImports: HostProcessImports,
	memory: WebAssembly.Memory,
	actions: Parameters<typeof writeSpawnActions>[2],
): number {
	const [commandPtr, commandLen] = writeValue(memory, 256, "/bin/actions");
	const [argvPtr, argvLen] = writeList(memory, 1024, ["/bin/actions"]);
	const [envPtr, envLen] = writeList(memory, 2048, []);
	const [cwdPtr, cwdLen] = writeValue(memory, 3072, "/work");
	const [actionsPtr, actionsLen] = writeSpawnActions(memory, 4096, actions);
	return processImports.proc_spawn_v3(
		commandPtr,
		commandLen,
		argvPtr,
		argvLen,
		envPtr,
		envLen,
		actionsPtr,
		actionsLen,
		cwdPtr,
		cwdLen,
		0,
		0,
		0,
		0,
		0,
		0,
		128,
	);
}

describe("browser WASI command host Linux parity", () => {
	it("projects unused SSH and rejects it only when that process image executes", async () => {
		class FakeWasi {
			readonly wasiImport = {};
			readonly fdTable = new Map<number, unknown>();
			nextFd = 3;
			constructor(readonly options: WasiOptions) {}
		}
		const sshModule = new WebAssembly.Module(
			readFileSync(
				new URL("../../../../software/ssh/bin/ssh", import.meta.url),
			),
		);
		const imports = WebAssembly.Module.imports(sshModule);
		expect(imports).toEqual(
			expect.arrayContaining([
				expect.objectContaining({ module: "host_net", name: "net_socket" }),
			]),
		);

		const memory = new WebAssembly.Memory({ initial: 1 });
		const host = await loadCommandHostFactory({})({
			WASI: FakeWasi,
			commands: { "/opt/agentos/bin/ssh": sshModule },
		});
		host
			.setMemory(memory)
			.setParentWasi(new FakeWasi({ args: [], env: {}, preopens: {} }));

		let thrown: unknown;
		try {
			spawn(
				host.imports.host_process,
				memory,
				"/opt/agentos/bin/ssh",
				["ssh"],
				[],
			);
		} catch (error) {
			thrown = error;
		}
		expect(thrown).toMatchObject({
			code: "ERR_AGENTOS_BROWSER_WASM_NETWORK_UNSUPPORTED",
		});
	});

	it("applies the same typed browser network boundary to filesystem-loaded exec images", async () => {
		const sshBytes = readFileSync(
			new URL("../../../../software/ssh/bin/ssh", import.meta.url),
		);
		const fsModule = {
			statSync: () => ({
				isFile: () => true,
				mode: 0o100755,
				size: sshBytes.byteLength,
			}),
			readFileSync: () => sshBytes,
		};
		const memory = new WebAssembly.Memory({ initial: 1 });
		let processImports: HostProcessImports;
		class FakeWasi {
			readonly wasiImport = {};
			readonly fdTable = new Map<number, unknown>();
			nextFd = 3;
			constructor(readonly options: WasiOptions) {}
			start(): number {
				if (this.options.args[0] === "old-image") {
					processImports.proc_exec(4096, 12, 4352, 13, 4608, 1);
				}
				return 0;
			}
		}
		const parent = new FakeWasi({
			args: ["old-image"],
			env: {},
			preopens: { "/": "/work" },
		});
		const host = await loadCommandHostFactory(fsModule)({
			WASI: FakeWasi,
			commands: {},
			cwd: "/work",
		});
		processImports = host.imports.host_process;
		host.setMemory(memory).setParentWasi(parent);
		writeValue(memory, 4096, "/dynamic/ssh");
		writeList(memory, 4352, ["ssh", "host"]);
		writeList(memory, 4608, []);

		let thrown: unknown;
		try {
			parent.start({ exports: { memory } } as unknown as WebAssembly.Instance);
		} catch (error) {
			thrown = error;
		}
		expect(thrown).toMatchObject({
			code: "ERR_AGENTOS_BROWSER_WASM_NETWORK_UNSUPPORTED",
		});
	});

	it("keeps the checked-in command spawn/wait ABI and its raw exit status", async () => {
		class FakeWasi {
			readonly wasiImport = {};
			readonly fdTable = new Map<number, unknown>();
			nextFd = 3;
			constructor(readonly options: WasiOptions) {}
			start(): number {
				return 137;
			}
		}
		const memory = new WebAssembly.Memory({ initial: 1 });
		const host = await loadCommandHostFactory({})({
			WASI: FakeWasi,
			commands: { "/bin/legacy": EMPTY_WASM },
			cwd: "/work",
		});
		host
			.setMemory(memory)
			.setParentWasi(new FakeWasi({ args: [], env: {}, preopens: {} }));
		const processImports = host.imports.host_process;
		const [argvPtr, argvLen] = writeList(memory, 1024, ["/bin/legacy", "arg"]);
		const [envPtr, envLen] = writeList(memory, 2048, ["ONLY=value"]);
		const [cwdPtr, cwdLen] = writeValue(memory, 3072, "/work");

		expect(
			processImports.proc_spawn(
				argvPtr,
				argvLen,
				envPtr,
				envLen,
				0,
				1,
				2,
				cwdPtr,
				cwdLen,
				128,
			),
		).toBe(ERRNO_SUCCESS);
		const pid = readU32(memory, 128);
		expect(processImports.proc_waitpid(pid, 0, 72, 80)).toBe(ERRNO_SUCCESS);
		expect(readU32(memory, 72)).toBe(137);
		expect(readU32(memory, 80)).toBe(pid);
	});

	it("preserves argv0, empty argv entries, exact env, and executable path resolution", async () => {
		const starts: WasiOptions[] = [];
		class FakeWasi {
			readonly wasiImport = {};
			readonly fdTable = new Map<number, unknown>();
			nextFd = 3;
			constructor(readonly options: WasiOptions) {}
			start(_instance: WebAssembly.Instance): number {
				starts.push(this.options);
				return 0;
			}
		}
		const memory = new WebAssembly.Memory({ initial: 1 });
		const host = await loadCommandHostFactory({})({
			WASI: FakeWasi,
			commands: { "/custom/bin/tool": EMPTY_WASM },
			cwd: "/work",
		});
		host
			.setMemory(memory)
			.setParentWasi(new FakeWasi({ args: [], env: {}, preopens: {} }));
		const processImports = host.imports.host_process;

		expect(
			spawn(
				processImports,
				memory,
				"/custom/bin/tool",
				["-tool", "", "tail", ""],
				["ONLY=value"],
			),
		).toMatchObject({ errno: ERRNO_SUCCESS });
		expect(starts[0]).toMatchObject({
			args: ["-tool", "", "tail", ""],
			env: { ONLY: "value" },
			preopens: { "/": "/work" },
		});
		expect(starts[0]?.env).not.toHaveProperty("PATH");

		expect(
			spawn(processImports, memory, "tool", ["tool"], ["PATH=/custom/bin"]),
		).toMatchObject({ errno: ERRNO_SUCCESS });
		expect(
			spawn(
				processImports,
				memory,
				"/wrong/tool",
				["tool"],
				["PATH=/custom/bin"],
			),
		).toMatchObject({ errno: ERRNO_NOENT });
		expect(
			spawn(processImports, memory, "tool", ["tool"], ["PATH=/nowhere"]),
		).toMatchObject({ errno: ERRNO_NOENT });
		expect(spawn(processImports, memory, "", [""], [])).toMatchObject({
			errno: ERRNO_NOENT,
		});
	});

	it("replaces PID 1 without resuming the old image and inherits its open fds", async () => {
		const starts: WasiOptions[] = [];
		const memory = new WebAssembly.Memory({ initial: 1 });
		let processImports: HostProcessImports;
		let oldImageResumed = false;
		let replacementWasi: FakeWasi | undefined;
		class FakeWasi {
			readonly wasiImport = {};
			readonly fdTable = new Map<number, unknown>();
			nextFd = 3;
			constructor(readonly options: WasiOptions) {}
			start(_instance?: WebAssembly.Instance): number {
				starts.push(this.options);
				if (this.options.args[0] === "old-image") {
					processImports.proc_exec(4096, 16, 4352, 7, 4608, 11);
					oldImageResumed = true;
					return 99;
				}
				replacementWasi = this;
				return 23;
			}
		}
		const parent = new FakeWasi({
			args: ["old-image"],
			env: { OLD: "value" },
			preopens: { "/": "/work" },
		});
		const inheritedEntry = { kind: "file", realFd: 42, offset: 7 };
		parent.fdTable.set(5, inheritedEntry);
		parent.nextFd = 9;
		const host = await loadCommandHostFactory({})({
			WASI: FakeWasi,
			commands: { "/custom/bin/tool": MEMORY_WASM },
			cwd: "/work",
		});
		processImports = host.imports.host_process;
		host.setMemory(memory).setParentWasi(parent);
		writeValue(memory, 4096, "/custom/bin/tool");
		writeList(memory, 4352, ["", "", "tail"]);
		writeList(memory, 4608, ["ONLY=value"]);

		const exitCode = parent.start({
			exports: { memory },
		} as unknown as WebAssembly.Instance);

		expect(exitCode).toBe(23);
		expect(oldImageResumed).toBe(false);
		expect(starts[1]).toEqual({
			returnOnExit: true,
			args: ["", "", "tail"],
			env: { ONLY: "value" },
			preopens: {},
		});
		expect(replacementWasi?.fdTable.get(5)).toBe(inheritedEntry);
		expect(replacementWasi?.nextFd).toBe(9);
	});

	it("keeps a child PID across exec and closes only FD_CLOEXEC descriptors", async () => {
		const memory = new WebAssembly.Memory({ initial: 1 });
		let processImports: HostProcessImports;
		let oldImageResumed = false;
		let replacementWasi: FakeWasi | undefined;
		class FakeWasi {
			readonly wasiImport = {};
			readonly fdTable = new Map<number, unknown>();
			constructor(readonly options: WasiOptions) {}
			start(instance: WebAssembly.Instance): number {
				if (this.options.args[0] === "old-child") {
					const childMemory = instance.exports.memory as WebAssembly.Memory;
					const ordinaryEntry = { kind: "file", realFd: 51 };
					const indistinguishableCloexecEntry = { kind: "file", realFd: 52 };
					this.fdTable.set(5, ordinaryEntry);
					this.fdTable.set(6, indistinguishableCloexecEntry);
					writeValue(childMemory, 4096, "/bin/new-child");
					writeList(childMemory, 4352, ["new-argv0"]);
					writeList(childMemory, 4608, []);
					new DataView(childMemory.buffer).setUint32(4864, 6, true);
					processImports.proc_exec(4096, 14, 4352, 10, 4608, 1, 4864, 1);
					oldImageResumed = true;
					return 99;
				}
				replacementWasi = this;
				return 17;
			}
		}
		const host = await loadCommandHostFactory({})({
			WASI: FakeWasi,
			commands: {
				"/bin/old-child": MEMORY_WASM,
				"/bin/new-child": MEMORY_WASM,
			},
			cwd: "/work",
		});
		processImports = host.imports.host_process;
		host
			.setMemory(memory)
			.setParentWasi(new FakeWasi({ args: [], env: {}, preopens: {} }));

		const child = spawn(
			processImports,
			memory,
			"/bin/old-child",
			["old-child"],
			[],
		);
		expect(child.errno).toBe(ERRNO_SUCCESS);
		expect(oldImageResumed).toBe(false);
		expect(processImports.proc_waitpid_v2(child.pid, 0, 72, 76, 80, 88)).toBe(
			ERRNO_SUCCESS,
		);
		expect(readU32(memory, 72)).toBe(17);
		expect(readU32(memory, 80)).toBe(child.pid);
		expect(replacementWasi?.options.args).toEqual(["new-argv0"]);
		expect(replacementWasi?.options.env).toEqual({});
		expect(replacementWasi?.fdTable.has(5)).toBe(true);
		expect(replacementWasi?.fdTable.has(6)).toBe(false);
	});

	it("executes an unlinked shebang fd through /proc and rejects script FD_CLOEXEC", async () => {
		const script = new TextEncoder().encode("#!/bin/sh\n");
		const starts: WasiOptions[] = [];
		const memory = new WebAssembly.Memory({ initial: 1 });
		let processImports: HostProcessImports;
		let oldImageResumed = false;
		class FakeWasi {
			readonly wasiImport = {};
			readonly fdTable = new Map<number, unknown>();
			nextFd = 8;
			constructor(readonly options: WasiOptions) {}
			start(instance: WebAssembly.Instance): number {
				starts.push(this.options);
				if (this.options.args[0] === "old-image") {
					const childMemory = instance.exports.memory as WebAssembly.Memory;
					writeList(childMemory, 4352, ["discarded", "script-argument"]);
					writeList(childMemory, 4608, []);
					processImports.proc_fexec(7, 4352, 26, 4608, 1, 4864, 0);
					oldImageResumed = true;
					return 99;
				}
				return 0;
			}
		}
		const parent = new FakeWasi({ args: ["old-image"], env: {}, preopens: {} });
		parent.fdTable.set(7, { kind: "file", realFd: 91, offset: 123 });
		const host = await loadCommandHostFactory({
			fstatSync: () => ({
				size: script.length,
				mode: 0o100755,
				isFile: () => true,
			}),
			readSync: (
				_fd: number,
				target: Uint8Array,
				offset: number,
				length: number,
				position: number,
			) => {
				expect(position).toBe(0);
				target.set(script.subarray(0, length), offset);
				return length;
			},
		})({
			WASI: FakeWasi,
			commands: { "/bin/sh": MEMORY_WASM },
			cwd: "/work",
		});
		processImports = host.imports.host_process;
		host.setMemory(memory).setParentWasi(parent);
		writeList(memory, 4352, ["discarded", "script-argument"]);
		writeList(memory, 4608, []);
		new DataView(memory.buffer).setUint32(4864, 7, true);
		expect(processImports.proc_fexec(7, 4352, 26, 4608, 1, 4864, 1)).toBe(
			ERRNO_NOENT,
		);

		const exitCode = parent.start({
			exports: { memory },
		} as unknown as WebAssembly.Instance);
		expect(exitCode).toBe(0);
		expect(oldImageResumed).toBe(false);
		expect(starts[1]?.args).toEqual([
			"/bin/sh",
			"/proc/self/fd/7",
			"script-argument",
		]);
		expect(parent.fdTable.get(7)).toMatchObject({ offset: 123 });
	});

	it("implements WNOHANG, invalid wait options, and truthful signal identity", async () => {
		class FakeWasi {
			readonly wasiImport = {};
			readonly fdTable = new Map<number, unknown>();
			constructor(readonly options: WasiOptions) {}
			start(): number {
				return 0;
			}
		}
		const memory = new WebAssembly.Memory({ initial: 1 });
		const host = await loadCommandHostFactory({})({
			WASI: FakeWasi,
			commands: { "/bin/tool": EMPTY_WASM },
		});
		host
			.setMemory(memory)
			.setParentWasi(new FakeWasi({ args: [], env: {}, preopens: {} }));
		const processImports = host.imports.host_process;
		expect(processImports.fd_pipe(64, 68)).toBe(ERRNO_SUCCESS);
		const readFd = readU32(memory, 64);
		const writeFd = readU32(memory, 68);
		expect(processImports.fd_dup(writeFd, 84)).toBe(ERRNO_SUCCESS);
		const duplicatedWriteFd = readU32(memory, 84);
		expect(processImports.proc_closefrom(duplicatedWriteFd)).toBe(
			ERRNO_SUCCESS,
		);
		expect(processImports.fd_dup(writeFd, 84)).toBe(ERRNO_SUCCESS);
		expect(processImports.proc_closefrom(readU32(memory, 84))).toBe(
			ERRNO_SUCCESS,
		);
		const child = spawn(
			processImports,
			memory,
			"/bin/tool",
			["tool"],
			[],
			[readFd, 1, 2],
		);
		expect(child.errno).toBe(ERRNO_SUCCESS);

		expect(processImports.proc_waitpid_v2(child.pid, 2, 72, 76, 80)).toBe(
			ERRNO_INVAL,
		);
		expect(processImports.proc_waitpid_v2(child.pid, 1, 72, 76, 80, 88)).toBe(
			ERRNO_SUCCESS,
		);
		expect(readU32(memory, 80)).toBe(0);
		expect(readU32(memory, 88)).toBe(0);
		expect(processImports.proc_kill(child.pid, 0)).toBe(ERRNO_SUCCESS);
		expect(processImports.proc_kill(child.pid, 12)).toBe(ERRNO_NOSYS);
		expect(processImports.proc_kill(child.pid, 15)).toBe(ERRNO_SUCCESS);
		expect(processImports.proc_waitpid_v2(child.pid, 0, 72, 76, 80, 88)).toBe(
			ERRNO_SUCCESS,
		);
		expect(readU32(memory, 72)).toBe(0);
		expect(readU32(memory, 76)).toBe(15);
		expect(readU32(memory, 80)).toBe(child.pid);
		expect(readU32(memory, 88)).toBe(0);
		expect(processImports.proc_waitpid_v2(child.pid, 0, 72, 76, 80)).toBe(
			ERRNO_CHILD,
		);
		expect(processImports.proc_kill(child.pid, 0)).toBe(ERRNO_SRCH);
		expect(processImports.proc_sigaction()).toBe(ERRNO_NOSYS);
		expect(processImports.proc_closefrom(writeFd)).toBe(ERRNO_SUCCESS);
	});

	it("keeps the host signal mask authoritative and never blocks SIGKILL or SIGSTOP", async () => {
		class FakeWasi {
			readonly wasiImport = {};
			readonly fdTable = new Map<number, unknown>();
			constructor(readonly options: WasiOptions) {}
			start(): number {
				return 0;
			}
		}
		const memory = new WebAssembly.Memory({ initial: 1 });
		const host = await loadCommandHostFactory({})({
			WASI: FakeWasi,
			commands: {},
		});
		host
			.setMemory(memory)
			.setParentWasi(new FakeWasi({ args: [], env: {}, preopens: {} }));
		const processImports = host.imports.host_process;
		const sigterm = 1 << (15 - 1);
		const sigkill = 1 << (9 - 1);
		const sigstop = 1 << (19 - 1);

		expect(
			processImports.proc_signal_mask_v2(
				0,
				sigterm | sigkill | sigstop,
				0,
				72,
				76,
			),
		).toBe(ERRNO_SUCCESS);
		expect(readU32(memory, 72)).toBe(0);
		expect(processImports.proc_signal_mask_v2(3, 0, 0, 72, 76)).toBe(
			ERRNO_SUCCESS,
		);
		expect(readU32(memory, 72)).toBe(sigterm);
		expect(readU32(memory, 76)).toBe(0);

		expect(processImports.proc_signal_mask_v2(1, sigterm, 0, 72, 76)).toBe(
			ERRNO_SUCCESS,
		);
		expect(readU32(memory, 72)).toBe(sigterm);
		expect(processImports.proc_signal_mask_v2(3, 0, 0, 72, 76)).toBe(
			ERRNO_SUCCESS,
		);
		expect(readU32(memory, 72)).toBe(0);
		expect(processImports.proc_signal_mask_v2(99, 0, 0, 72, 76)).toBe(
			ERRNO_INVAL,
		);
	});

	it("reports and validates process groups through the host imports", async () => {
		class FakeWasi {
			readonly wasiImport = {};
			readonly fdTable = new Map<number, unknown>();
			constructor(readonly options: WasiOptions) {}
			start(): number {
				return 0;
			}
		}
		const memory = new WebAssembly.Memory({ initial: 1 });
		const host = await loadCommandHostFactory({})({
			WASI: FakeWasi,
			commands: {},
		});
		host
			.setMemory(memory)
			.setParentWasi(new FakeWasi({ args: [], env: {}, preopens: {} }));
		const processImports = host.imports.host_process;

		expect(processImports.proc_getpgid(0, 72)).toBe(ERRNO_SUCCESS);
		expect(readU32(memory, 72)).toBe(1);
		expect(processImports.proc_setpgid(0, 0)).toBe(ERRNO_SUCCESS);
		expect(processImports.proc_getpgid(1, 72)).toBe(ERRNO_SUCCESS);
		expect(readU32(memory, 72)).toBe(1);
		expect(processImports.proc_setpgid(1, 777)).toBe(ERRNO_PERM);
		expect(processImports.proc_getpgid(777, 72)).toBe(ERRNO_SRCH);
	});

	it("applies ordered spawn actions without changing the parent fd table", async () => {
		const childTables: Array<Map<number, Record<string, unknown>>> = [];
		class FakeWasi {
			readonly wasiImport = { fd_close: () => ERRNO_SUCCESS };
			readonly fdTable = new Map<number, Record<string, unknown>>();
			nextFd = 3;
			constructor(readonly options: WasiOptions) {}
			start(): number {
				const inherited = this.fdTable.get(42);
				if (inherited) inherited.offset = 29;
				childTables.push(new Map(this.fdTable));
				return 0;
			}
		}
		const memory = new WebAssembly.Memory({ initial: 1 });
		const parent = new FakeWasi({ args: [], env: {}, preopens: {} });
		parent.fdTable.set(5, {
			kind: "file",
			realFd: 42,
			offset: 7,
			readOnly: true,
			rightsBase: 1n << 1n,
		});
		const host = await loadCommandHostFactory({})({
			WASI: FakeWasi,
			commands: { "/bin/actions": EMPTY_WASM },
			cwd: "/work",
		});
		host.setMemory(memory).setParentWasi(parent);
		const processImports = host.imports.host_process;
		const [commandPtr, commandLen] = writeValue(memory, 256, "/bin/actions");
		const [argvPtr, argvLen] = writeList(memory, 1024, ["/bin/actions"]);
		const [envPtr, envLen] = writeList(memory, 2048, []);
		const [cwdPtr, cwdLen] = writeValue(memory, 3072, "/work");
		const [actionsPtr, actionsLen] = writeSpawnActions(memory, 4096, [
			{ command: 2, fd: 42, sourceFd: 5 },
			{ command: 1, fd: 5 },
		]);

		expect(
			processImports.proc_spawn_v3(
				commandPtr,
				commandLen,
				argvPtr,
				argvLen,
				envPtr,
				envLen,
				actionsPtr,
				actionsLen,
				cwdPtr,
				cwdLen,
				0,
				0,
				0,
				0,
				0,
				0,
				128,
			),
		).toBe(ERRNO_SUCCESS);
		expect(childTables).toHaveLength(1);
		expect(childTables[0]?.has(5)).toBe(false);
		expect(childTables[0]?.get(42)).toMatchObject({ realFd: 42, offset: 29 });
		expect(parent.fdTable.has(5)).toBe(true);
		expect(parent.fdTable.get(5)?.offset).toBe(29);
	});

	it("enforces trusted spawn action limits despite hostile guest overrides", async () => {
		class FakeWasi {
			readonly wasiImport = { fd_close: () => ERRNO_SUCCESS };
			readonly fdTable = new Map<number, Record<string, unknown>>();
			nextFd = 3;
			constructor(readonly options: WasiOptions) {}
			start(): number {
				return 0;
			}
		}
		const messages: string[] = [];
		const write = vi.spyOn(process.stderr, "write").mockImplementation(((
			chunk: string | Uint8Array,
		) => {
			messages.push(String(chunk));
			return true;
		}) as typeof process.stderr.write);

		const smallMemory = new WebAssembly.Memory({ initial: 1 });
		const countBounded = await loadCommandHostFactory(
			{},
			{
				maxSpawnFileActions: 1,
				maxSpawnFileActionBytes: 128,
			},
		)({
			WASI: FakeWasi,
			commands: { "/bin/actions": EMPTY_WASM },
			maxSpawnFileActions: 44_000,
			maxSpawnFileActionBytes: 1_100_000,
		});
		countBounded
			.setMemory(smallMemory)
			.setParentWasi(new FakeWasi({ args: [], env: {}, preopens: {} }));
		expect(
			spawnV3WithActions(countBounded.imports.host_process, smallMemory, [
				{ command: 1, fd: 10 },
				{ command: 1, fd: 11 },
			]),
		).toBe(ERRNO_2BIG);
		expect(messages.join("")).toContain("limits.process.maxSpawnFileActions");
		expect(messages.join("")).toContain(
			"near limits.process.maxSpawnFileActions",
		);

		messages.length = 0;
		const bytesBounded = await loadCommandHostFactory(
			{},
			{
				maxSpawnFileActions: 10,
				maxSpawnFileActionBytes: 23,
			},
		)({
			WASI: FakeWasi,
			commands: { "/bin/actions": EMPTY_WASM },
			maxSpawnFileActions: 44_000,
			maxSpawnFileActionBytes: 1_100_000,
		});
		bytesBounded
			.setMemory(smallMemory)
			.setParentWasi(new FakeWasi({ args: [], env: {}, preopens: {} }));
		expect(
			spawnV3WithActions(bytesBounded.imports.host_process, smallMemory, [
				{ command: 1, fd: 10 },
			]),
		).toBe(ERRNO_2BIG);
		expect(messages.join("")).toContain(
			"limits.process.maxSpawnFileActionBytes",
		);

		messages.length = 0;
		const largeMemory = new WebAssembly.Memory({ initial: 20 });
		const aboveDefaults = Array.from({ length: 43_700 }, () => ({
			command: 6,
			fd: 1_000_000,
		}));
		const aboveDefaultCount = aboveDefaults.slice(0, 4097);
		const defaults = await loadCommandHostFactory({})({
			WASI: FakeWasi,
			commands: { "/bin/actions": EMPTY_WASM },
		});
		defaults
			.setMemory(largeMemory)
			.setParentWasi(new FakeWasi({ args: [], env: {}, preopens: {} }));
		expect(
			spawnV3WithActions(
				defaults.imports.host_process,
				largeMemory,
				aboveDefaultCount,
			),
		).toBe(ERRNO_2BIG);
		expect(
			spawnV3WithActions(
				defaults.imports.host_process,
				largeMemory,
				aboveDefaults,
			),
		).toBe(ERRNO_2BIG);
		const raised = await loadCommandHostFactory(
			{},
			{
				maxSpawnFileActions: 44_000,
				maxSpawnFileActionBytes: 1_100_000,
			},
		)({
			WASI: FakeWasi,
			commands: { "/bin/actions": EMPTY_WASM },
		});
		raised
			.setMemory(largeMemory)
			.setParentWasi(new FakeWasi({ args: [], env: {}, preopens: {} }));
		expect(
			spawnV3WithActions(
				raised.imports.host_process,
				largeMemory,
				aboveDefaults,
			),
		).toBe(ERRNO_SUCCESS);
		expect(messages.join("")).toContain(
			"near limits.process.maxSpawnFileActions",
		);
		expect(messages.join("")).toContain(
			"near limits.process.maxSpawnFileActionBytes",
		);
		write.mockRestore();
	});

	it("keeps child closefrom and inherited CLOEXEC state isolated from the parent", async () => {
		let processImports: HostProcessImports;
		const childFds: number[][] = [];
		class FakeWasi {
			readonly wasiImport = { fd_close: () => ERRNO_SUCCESS };
			readonly fdTable = new Map<number, Record<string, unknown>>();
			nextFd = 3;
			constructor(readonly options: WasiOptions) {}
			start(): number {
				childFds.push([...this.fdTable.keys()]);
				expect(processImports.proc_closefrom(5)).toBe(ERRNO_SUCCESS);
				return 0;
			}
		}
		const memory = new WebAssembly.Memory({ initial: 1 });
		const parent = new FakeWasi({ args: [], env: {}, preopens: {} });
		parent.fdTable.set(5, { kind: "file", realFd: 43, offset: 0 });
		const host = await loadCommandHostFactory({})({
			WASI: FakeWasi,
			commands: { "/bin/isolation": EMPTY_WASM },
			cwd: "/work",
		});
		host.setMemory(memory).setParentWasi(parent);
		processImports = host.imports.host_process;
		expect(processImports.fd_setfd(5, 1)).toBe(ERRNO_SUCCESS);
		expect(
			spawn(processImports, memory, "/bin/isolation", ["isolation"], []).errno,
		).toBe(ERRNO_SUCCESS);
		expect(childFds[0]).not.toContain(5);
		expect(parent.fdTable.has(5)).toBe(true);
		expect(processImports.fd_getfd(5, 84)).toBe(ERRNO_SUCCESS);
		expect(readU32(memory, 84)).toBe(1);
	});

	it("shares open-file offsets, closes ordinary and synthetic fds, and reuses bounded fds", async () => {
		const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
		const closes: number[] = [];
		const chmods: Array<[number, number]> = [];
		const truncates: Array<[number, number]> = [];
		const fsModule = {
			closeSync(fd: number) {
				closes.push(fd);
			},
			fchmodSync(fd: number, mode: number) {
				chmods.push([fd, mode]);
			},
			ftruncateSync(fd: number, length: number) {
				truncates.push([fd, length]);
			},
		};
		class FakeWasi {
			readonly wasiImport = { fd_close: () => ERRNO_SUCCESS };
			readonly fdTable = new Map<number, Record<string, unknown>>();
			constructor(readonly options: WasiOptions) {}
			start(): number {
				return 0;
			}
		}
		const memory = new WebAssembly.Memory({ initial: 1 });
		const parent = new FakeWasi({ args: [], env: {}, preopens: {} });
		parent.fdTable.set(5, {
			kind: "file",
			realFd: 42,
			offset: 7,
			readOnly: true,
			rightsBase: 1n << 1n,
		});
		const host = await loadCommandHostFactory(fsModule)({
			WASI: FakeWasi,
			commands: {},
		});
		host.setMemory(memory).setParentWasi(parent);
		previousWasiHost = (
			globalThis as typeof globalThis & { __agentOSWasiHost?: unknown }
		).__agentOSWasiHost;
		host.installBlockingStdin({ stdin: { read: () => null } });
		const processImports = host.imports.host_process;

		expect(host.imports.host_fs.fchmod(5, 0o10644)).toBe(ERRNO_SUCCESS);
		expect(chmods).toEqual([[42, 0o644]]);
		expect(host.imports.host_fs.ftruncate(5, 2n)).toBe(ERRNO_INVAL);
		expect(host.imports.host_fs.ftruncate(1, 2n)).toBe(ERRNO_INVAL);
		expect(host.imports.host_fs.ftruncate(999, 2n)).toBe(ERRNO_BADF);

		expect(processImports.fd_dup(5, 84)).toBe(ERRNO_SUCCESS);
		const duplicateFd = readU32(memory, 84);
		const lookup = (
			globalThis as typeof globalThis & {
				__agentOSWasiHost: { lookupFdHandle(fd: number): { position: number } };
			}
		).__agentOSWasiHost.lookupFdHandle;
		const duplicate = lookup(duplicateFd);
		duplicate.position = 29;
		expect(lookup(5).position).toBe(29);
		expect(parent.fdTable.get(5)?.offset).toBe(29);
		expect(parent.fdTable.get(duplicateFd)?.offset).toBe(29);

		for (let index = 1; index < 4096; index += 1) {
			expect(processImports.fd_dup(5, 84)).toBe(ERRNO_SUCCESS);
		}
		expect(processImports.fd_dup(5, 84)).toBe(ERRNO_MFILE);
		expect(warn).toHaveBeenCalledWith(
			expect.stringContaining("maxSyntheticFds"),
		);
		expect(processImports.proc_closefrom(5)).toBe(ERRNO_SUCCESS);
		expect(closes).toEqual([42]);
		expect(parent.fdTable.has(5)).toBe(false);
		expect(processImports.fd_dup(duplicateFd, 84)).toBe(ERRNO_BADF);

		parent.fdTable.set(5, { kind: "file", realFd: 43, offset: 0 });
		expect(processImports.fd_dup(5, 84)).toBe(ERRNO_SUCCESS);
		expect(readU32(memory, 84)).toBe(FIRST_SYNTHETIC_FD);
		expect(processImports.proc_closefrom(FIRST_SYNTHETIC_FD)).toBe(
			ERRNO_SUCCESS,
		);
		expect(processImports.fd_dup_min(5, FIRST_SYNTHETIC_FD + 100, 84)).toBe(
			ERRNO_SUCCESS,
		);
		expect(readU32(memory, 84)).toBe(FIRST_SYNTHETIC_FD + 100);
		expect(processImports.fd_dup_min(5, -1, 84)).toBe(ERRNO_INVAL);
		expect(processImports.proc_closefrom(FIRST_SYNTHETIC_FD + 100)).toBe(
			ERRNO_SUCCESS,
		);
		expect(processImports.fd_dup2(5, 3)).toBe(ERRNO_SUCCESS);
		expect(processImports.proc_closefrom(5)).toBe(ERRNO_SUCCESS);
		expect(closes).toEqual([42]);
		expect(host.imports.host_fs.ftruncate(3, 3n)).toBe(ERRNO_SUCCESS);
		expect(truncates).toEqual([[43, 3]]);
		expect(processImports.proc_closefrom(3)).toBe(ERRNO_SUCCESS);
		expect(closes).toEqual([42, 43]);
	});
});
