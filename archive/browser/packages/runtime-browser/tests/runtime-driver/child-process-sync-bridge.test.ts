import { afterEach, describe, expect, it } from "vitest";
import type { BrowserChildProcessSpawnRequest } from "../../src/child-process-bridge.js";
import { BrowserRuntimeDriver } from "../../src/runtime-driver.js";
import {
	FAKE_CONVERGED_CONFIG,
	fakeConvergedFactoryOptions,
} from "./fake-converged-sidecar.js";
import type {
	CommandExecutor,
	VirtualFileSystem,
	RuntimeDriverOptions,
} from "../../src/runtime.js";
import {
	SYNC_BRIDGE_SIGNAL_KIND_INDEX,
	SYNC_BRIDGE_SIGNAL_LENGTH_INDEX,
	SYNC_BRIDGE_SIGNAL_STATE_INDEX,
	SYNC_BRIDGE_SIGNAL_STATE_READY,
	SYNC_BRIDGE_SIGNAL_STATUS_INDEX,
	SYNC_BRIDGE_KIND_JSON,
	SYNC_BRIDGE_KIND_NONE,
	SYNC_BRIDGE_KIND_TEXT,
	SYNC_BRIDGE_STATUS_ERROR,
	SYNC_BRIDGE_STATUS_OK,
} from "../../src/sync-bridge.js";

const encoder = new TextEncoder();
const decoder = new TextDecoder();

type WorkerHandler = (event: { data: unknown }) => void;

type InitMessage = {
	controlToken: string;
	id: number;
	type: "init";
	payload: {
		processLimits?: {
			maxSpawnFileActions?: number;
			maxSpawnFileActionBytes?: number;
		};
		syncBridge: {
			signalBuffer: SharedArrayBuffer;
			dataBuffer: SharedArrayBuffer;
		};
	};
};

type ExecMessage = {
	controlToken: string;
	id: number;
	type: "exec";
	payload: {
		executionId: string;
	};
};

class FakeWorker {
	static instances: FakeWorker[] = [];
	static expectedSyncStatus = SYNC_BRIDGE_STATUS_OK;
	static expectedSyncKind = SYNC_BRIDGE_KIND_JSON;
	static syncRequestOverride: Partial<{
		executionId: string;
		processRequestId: number;
	}> = {};
	static syncOperation = "child_process.spawn";
	static syncArgs: unknown[] = [
		{
			command: "echo",
			args: ["", "hi"],
			options: { argv0: "", cwd: "/work", env: { HELLO: "world" } },
		} satisfies BrowserChildProcessSpawnRequest,
	];

	onmessage: WorkerHandler | null = null;
	onerror: WorkerHandler | null = null;
	syncResponse: unknown;
	initPayload: InitMessage["payload"] | null = null;
	private syncBridge: InitMessage["payload"]["syncBridge"] | null = null;

	constructor() {
		FakeWorker.instances.push(this);
	}

	postMessage(message: InitMessage | ExecMessage): void {
		if (message.type === "init") {
			this.initPayload = message.payload;
			this.syncBridge = message.payload.syncBridge;
			queueMicrotask(() => {
				this.onmessage?.({
					data: {
						controlToken: message.controlToken,
						id: message.id,
						type: "response",
						ok: true,
						result: null,
					},
				});
			});
			return;
		}

		if (message.type === "exec") {
			void this.handleExec(message);
		}
	}

	terminate(): void {}

	private async handleExec(message: ExecMessage): Promise<void> {
		if (!this.syncBridge) {
			throw new Error("FakeWorker was not initialized");
		}

		new Int32Array(this.syncBridge.signalBuffer).fill(0);
		new Uint8Array(this.syncBridge.dataBuffer).fill(0);
		this.onmessage?.({
			data: {
				controlToken: message.controlToken,
				executionId:
					FakeWorker.syncRequestOverride.executionId ??
					message.payload.executionId,
				processRequestId:
					FakeWorker.syncRequestOverride.processRequestId ?? message.id,
				requestId: 1,
				type: "sync-request",
				operation: FakeWorker.syncOperation,
				args: FakeWorker.syncArgs,
			},
		});

		const signal = new Int32Array(this.syncBridge.signalBuffer);
		const data = new Uint8Array(this.syncBridge.dataBuffer);
		while (
			Atomics.load(signal, SYNC_BRIDGE_SIGNAL_STATE_INDEX) !==
			SYNC_BRIDGE_SIGNAL_STATE_READY
		) {
			await new Promise((resolve) => setTimeout(resolve, 0));
		}

		expect(Atomics.load(signal, SYNC_BRIDGE_SIGNAL_STATUS_INDEX)).toBe(
			FakeWorker.expectedSyncStatus,
		);
		expect(Atomics.load(signal, SYNC_BRIDGE_SIGNAL_KIND_INDEX)).toBe(
			FakeWorker.expectedSyncKind,
		);
		const kind = Atomics.load(signal, SYNC_BRIDGE_SIGNAL_KIND_INDEX);
		const length = Atomics.load(signal, SYNC_BRIDGE_SIGNAL_LENGTH_INDEX);
		const responseText = decoder.decode(data.subarray(0, length));
		this.syncResponse =
			kind === SYNC_BRIDGE_KIND_JSON
				? JSON.parse(responseText)
				: kind === SYNC_BRIDGE_KIND_TEXT
					? responseText
					: null;

		this.onmessage?.({
			data: {
				controlToken: message.controlToken,
				id: message.id,
				type: "response",
				ok: true,
				result: { code: 0, stdout: "done" },
			},
		});
	}
}

function createOptions(
	commandExecutor: CommandExecutor,
	filesystem?: VirtualFileSystem,
): RuntimeDriverOptions {
	return {
		system: {
			commandExecutor,
			filesystem,
			runtime: {
				process: {},
				os: {},
			},
		},
		runtime: {
			process: {},
			os: {},
		},
	};
}

describe("browser child_process sync bridge", () => {
	const originalWorker = globalThis.Worker;

	afterEach(() => {
		Object.defineProperty(globalThis, "Worker", {
			value: originalWorker,
			configurable: true,
			writable: true,
		});
		FakeWorker.instances.length = 0;
		FakeWorker.expectedSyncStatus = SYNC_BRIDGE_STATUS_OK;
		FakeWorker.expectedSyncKind = SYNC_BRIDGE_KIND_JSON;
		FakeWorker.syncRequestOverride = {};
		FakeWorker.syncOperation = "child_process.spawn";
		FakeWorker.syncArgs = [
			{
				command: "echo",
				args: ["", "hi"],
				options: { argv0: "", cwd: "/work", env: { HELLO: "world" } },
			} satisfies BrowserChildProcessSpawnRequest,
		];
	});

	it("propagates trusted converged VM process limits into worker policy", () => {
		Object.defineProperty(globalThis, "Worker", {
			value: FakeWorker,
			configurable: true,
			writable: true,
		});
		const factoryOptions = fakeConvergedFactoryOptions();
		factoryOptions.convergedSidecar.config = {
			...FAKE_CONVERGED_CONFIG,
			limits: {
				process: {
					maxSpawnFileActions: 7,
					maxSpawnFileActionBytes: 321,
				},
			},
		} as typeof factoryOptions.convergedSidecar.config;
		const driver = new BrowserRuntimeDriver(
			createOptions({
				spawn() {
					throw new Error("unexpected spawn");
				},
			}),
			factoryOptions,
		);
		expect(FakeWorker.instances[0]?.initPayload?.processLimits).toEqual({
			maxSpawnFileActions: 7,
			maxSpawnFileActionBytes: 321,
		});
		driver.dispose();
	});

	it("runs browser child_process requests through the driver command executor", async () => {
		Object.defineProperty(globalThis, "Worker", {
			value: FakeWorker,
			configurable: true,
			writable: true,
		});
		const spawns: Array<{
			command: string;
			args: string[];
			argv0?: string;
			cwd?: string;
			env?: Record<string, string>;
		}> = [];
		const executor: CommandExecutor = {
			spawn(command, args, options) {
				spawns.push({
					command,
					args,
					argv0: options?.argv0,
					cwd: options?.cwd,
					env: options?.env,
				});
				options?.onStdout?.(encoder.encode("hello"));
				return {
					async wait() {
						return 0;
					},
					writeStdin() {},
					closeStdin() {},
					kill() {},
				};
			},
		};

		const driver = new BrowserRuntimeDriver(
			createOptions(executor),
			fakeConvergedFactoryOptions(),
		);
		const result = await driver.exec("require('child_process').spawn('echo')");

		expect(result).toMatchObject({ code: 0, stdout: "done" });
		expect(spawns).toEqual([
			{
				command: "echo",
				args: ["", "hi"],
				argv0: "",
				cwd: "/work",
				env: { HELLO: "world" },
			},
		]);
		expect(FakeWorker.instances[0]?.syncResponse).toBe(1);
		const childProcessSessions = (
			driver as unknown as {
				childProcessSessions: Map<number, { executionId: string }>;
			}
		).childProcessSessions;
		expect(childProcessSessions.size).toBe(0);
		driver.dispose();
	});

	// Module resolution is now serviced by the converged kernel-backed resolver;
	// see converged-module-servicer.test.ts (exports/import-mode coverage).

	it("forwards spawnSync input to the command executor stdin", async () => {
		Object.defineProperty(globalThis, "Worker", {
			value: FakeWorker,
			configurable: true,
			writable: true,
		});
		FakeWorker.syncOperation = "child_process.spawn_sync";
		FakeWorker.syncArgs = [
			{
				command: "cat",
				args: [],
				options: {
					argv0: "reader",
					cwd: "/work",
					input: { __agentOSType: "bytes", base64: "c3RkaW4=" },
				},
			} satisfies BrowserChildProcessSpawnRequest,
		];
		FakeWorker.expectedSyncKind = SYNC_BRIDGE_KIND_TEXT;
		const stdinChunks: string[] = [];
		let closed = false;
		const executor: CommandExecutor = {
			spawn(command, args, options) {
				expect(command).toBe("cat");
				expect(args).toEqual([]);
				expect(options?.argv0).toBe("reader");
				expect(options?.cwd).toBe("/work");
				options?.onStdout?.(encoder.encode("stdout"));
				return {
					async wait() {
						return { exitCode: null, signal: 9 };
					},
					writeStdin(data) {
						stdinChunks.push(decoder.decode(data as Uint8Array));
					},
					closeStdin() {
						closed = true;
					},
					kill() {},
				};
			},
		};

		const driver = new BrowserRuntimeDriver(
			createOptions(executor),
			fakeConvergedFactoryOptions(),
		);
		await driver.exec("require('child_process').spawnSync('cat')");

		expect(stdinChunks).toEqual(["stdin"]);
		expect(closed).toBe(true);
		expect(
			JSON.parse(String(FakeWorker.instances[0]?.syncResponse)),
		).toMatchObject({
			code: null,
			signal: 9,
		});
		driver.dispose();
	});

	it("rejects sync bridge requests that do not match the pending execution request", async () => {
		Object.defineProperty(globalThis, "Worker", {
			value: FakeWorker,
			configurable: true,
			writable: true,
		});
		FakeWorker.expectedSyncStatus = SYNC_BRIDGE_STATUS_ERROR;
		FakeWorker.syncRequestOverride = { processRequestId: 999 };
		const executor: CommandExecutor = {
			spawn() {
				throw new Error("forged sync request reached command executor");
			},
		};

		const driver = new BrowserRuntimeDriver(
			createOptions(executor),
			fakeConvergedFactoryOptions(),
		);
		const result = await driver.exec("require('child_process').spawn('echo')");

		expect(result).toMatchObject({ code: 0, stdout: "done" });
		expect(FakeWorker.instances[0]?.syncResponse).toMatchObject({
			message:
				"Browser runtime sync bridge request for unknown execution exec-1",
		});
		driver.dispose();
	});

	it("clears process signal state registrations when browser worker executions finish", async () => {
		Object.defineProperty(globalThis, "Worker", {
			value: FakeWorker,
			configurable: true,
			writable: true,
		});
		FakeWorker.syncOperation = "process.signal_state";
		FakeWorker.syncArgs = [15, "user", "[2]", 0];
		FakeWorker.expectedSyncKind = SYNC_BRIDGE_KIND_NONE;
		const driver = new BrowserRuntimeDriver(
			createOptions({
				spawn() {
					throw new Error("process.signal_state should not spawn commands");
				},
			}),
			fakeConvergedFactoryOptions(),
		);

		await driver.exec("process.on('SIGTERM', () => {})");

		const signalStates = (
			driver as unknown as {
				signalStates: Map<
					string,
					Map<number, { action: string; mask: number[]; flags: number }>
				>;
			}
		).signalStates;
		expect(signalStates.has("exec-1")).toBe(false);
		driver.dispose();
	});

	it("rejects browser child_process session operations from a different execution", async () => {
		Object.defineProperty(globalThis, "Worker", {
			value: FakeWorker,
			configurable: true,
			writable: true,
		});
		FakeWorker.syncOperation = "child_process.kill";
		FakeWorker.syncArgs = [42, 15];
		FakeWorker.expectedSyncStatus = SYNC_BRIDGE_STATUS_ERROR;
		let killed = false;
		const driver = new BrowserRuntimeDriver(
			createOptions({
				spawn() {
					throw new Error("child_process.kill should not spawn commands");
				},
			}),
			fakeConvergedFactoryOptions(),
		);
		const childProcessSessions = (
			driver as unknown as {
				childProcessSessions: Map<
					number,
					{
						executionId: string;
						process: {
							writeStdin(data: Uint8Array): void;
							closeStdin(): void;
							kill(signal?: number): void;
							wait(): Promise<number>;
						};
						events: unknown[];
						exited: boolean;
					}
				>;
			}
		).childProcessSessions;
		childProcessSessions.set(42, {
			executionId: "exec-owner",
			process: {
				writeStdin() {},
				closeStdin() {},
				kill() {
					killed = true;
				},
				async wait() {
					return 0;
				},
			},
			events: [],
			exited: false,
		});

		await driver.exec("require('child_process').spawn('echo').kill()");

		expect(FakeWorker.instances[0]?.syncResponse).toMatchObject({
			message: "unknown child_process session 42",
		});
		expect(killed).toBe(false);
		expect(childProcessSessions.has(42)).toBe(true);
		driver.dispose();
	});
});
