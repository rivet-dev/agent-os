import { createHash } from "node:crypto";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const clientMocks = vi.hoisted(() => ({
	createClient: vi.fn(),
}));

vi.mock("@rivet-dev/agentos/client", () => ({
	createClient: clientMocks.createClient,
}));

import {
	AgentOSActorConfigurationError,
	AgentOSTemplateUnsupportedError,
	agentOSBackend,
	agentOSCoreBackend,
} from "../src/index.js";

const createInput = {
	templateKey: null,
	sessionKey: "session-1",
	runtimeContext: { appRoot: "/app" },
} as const;

function makeRegistry() {
	return {
		startAndWait: vi.fn(async () => {}),
		parseConfig: vi.fn(() => ({
			endpoint: "http://127.0.0.1:6420",
			namespace: "default",
			token: undefined,
			headers: {},
			envoy: { poolName: "default" },
		})),
	};
}

let registry = makeRegistry();

function actorBackend(actor = "vm") {
	return agentOSBackend({ actor, registry });
}

function deferred<T>() {
	let resolve!: (value: T) => void;
	let reject!: (error: unknown) => void;
	const promise = new Promise<T>((resolveValue, rejectValue) => {
		resolve = resolveValue;
		reject = rejectValue;
	});
	return { promise, reject, resolve };
}

function makeHarness() {
	const listeners = new Set<(event: unknown) => void>();
	const keys: string[][] = [];
	let nextPid = 1;
	const connection = {
		ready: Promise.resolve(),
		on: vi.fn((_event: string, listener: (event: unknown) => void) => {
			listeners.add(listener);
			return () => listeners.delete(listener);
		}),
		dispose: vi.fn(async () => {}),
		spawn: vi.fn(async () => ({ pid: nextPid++ })),
		waitProcess: vi.fn(async () => 0),
		killProcess: vi.fn(async () => {}),
		readFile: vi.fn(async () => new Uint8Array()),
		writeFile: vi.fn(async () => {}),
		exists: vi.fn(async () => true),
		mkdir: vi.fn(async () => {}),
		remove: vi.fn(async () => {}),
	};
	const connect = vi.fn(() => connection);
	const getOrCreate = vi.fn((key: string[]) => {
		keys.push(key);
		return { connect };
	});
	clientMocks.createClient.mockReturnValue({ vm: { getOrCreate } });
	return {
		connect,
		connection,
		emit(event: unknown) {
			for (const listener of listeners) listener(event);
		},
		getOrCreate,
		keys,
	};
}

async function streamText(stream: ReadableStream<Uint8Array>): Promise<string> {
	return new Response(stream).text();
}

describe("agentOSBackend", () => {
	beforeEach(() => {
		clientMocks.createClient.mockReset();
		registry = makeRegistry();
	});

	afterEach(() => {
		vi.restoreAllMocks();
	});

	it("fails closed for Eve templates", async () => {
		const backend = actorBackend();
		await expect(
			backend.prewarm({
				templateKey: "template-1",
				runtimeContext: { appRoot: "/app" },
				seedFiles: [],
			}),
		).rejects.toBeInstanceOf(AgentOSTemplateUnsupportedError);
		await expect(
			backend.create({ ...createInput, templateKey: "template-1" }),
		).rejects.toBeInstanceOf(AgentOSTemplateUnsupportedError);
		expect(clientMocks.createClient).not.toHaveBeenCalled();
	});

	it("waits for the application registry and reuses its resolved config", async () => {
		const harness = makeHarness();
		const backend = actorBackend();
		await backend.create(createInput);
		expect(registry.startAndWait).toHaveBeenCalledOnce();
		expect(clientMocks.createClient).toHaveBeenCalledWith({
			endpoint: "http://127.0.0.1:6420",
			namespace: "default",
			poolName: "default",
			token: undefined,
			headers: {},
			disableMetadataLookup: true,
		});
		expect(harness.connect).toHaveBeenCalledOnce();
	});

	it("reuses one handle and connection until idempotent shutdown", async () => {
		const harness = makeHarness();
		const backend = actorBackend();
		const first = await backend.create(createInput);
		const second = await backend.create(createInput);
		expect(second).toBe(first);
		expect(harness.connect).toHaveBeenCalledTimes(1);

		const state = await first.captureState();
		expect(state.metadata).toEqual({
			version: 1,
			actor: "vm",
			key: [
				"eve",
				"session",
				createHash("sha256").update("session-1").digest("hex"),
			],
		});

		await Promise.all([first.shutdown(), first.shutdown()]);
		expect(harness.connection.dispose).toHaveBeenCalledTimes(1);
		await backend.create(createInput);
		expect(harness.connect).toHaveBeenCalledTimes(2);
	});

	it("maps workspace paths and decodes actor binary values", async () => {
		const harness = makeHarness();
		harness.connection.readFile.mockResolvedValue([
			"$Uint8Array",
			Buffer.from("hello\nworld\n").toString("base64"),
		]);
		const handle = await actorBackend().create(createInput);

		expect(handle.session.resolvePath("src/index.ts")).toBe(
			"/workspace/src/index.ts",
		);
		expect(handle.session.resolvePath("$HOME/.config")).toBe(
			"/home/agentos/.config",
		);
		expect(handle.session.resolvePath("/workspace/a/../b")).toBe(
			"/workspace/a/../b",
		);
		await expect(
			handle.session.readTextFile({ path: "notes.txt", startLine: 2 }),
		).resolves.toBe("world\n");
		expect(harness.connection.readFile).toHaveBeenCalledWith(
			"/workspace/notes.txt",
		);

		await handle.session.writeTextFile({ path: "new/file.txt", content: "x" });
		expect(harness.connection.mkdir).toHaveBeenCalledWith("/workspace/new", {
			recursive: true,
		});
		expect(harness.connection.writeFile).toHaveBeenCalledWith(
			"/workspace/new/file.txt",
			Buffer.from("x"),
		);
		await handle.shutdown();
	});

	it("routes early concurrent output per process", async () => {
		const harness = makeHarness();
		const firstSpawn = deferred<{ pid: number }>();
		const secondSpawn = deferred<{ pid: number }>();
		harness.connection.spawn
			.mockImplementationOnce(() => firstSpawn.promise)
			.mockImplementationOnce(() => secondSpawn.promise);
		const handle = await actorBackend().create(createInput);
		const firstPending = handle.session.spawn({ command: "first" });
		const secondPending = handle.session.spawn({ command: "second" });
		harness.emit({
			pid: 11,
			stream: "stdout",
			data: new TextEncoder().encode("one"),
		});
		harness.emit({
			pid: 22,
			stream: "stdout",
			data: new TextEncoder().encode("two"),
		});
		firstSpawn.resolve({ pid: 11 });
		secondSpawn.resolve({ pid: 22 });
		const [first, second] = await Promise.all([firstPending, secondPending]);
		await expect(streamText(first.stdout)).resolves.toBe("one");
		await expect(streamText(second.stdout)).resolves.toBe("two");
		expect(harness.connection.killProcess).not.toHaveBeenCalled();
		await handle.shutdown();
	});

	it("routes early concurrent output by PID without cross-talk", async () => {
		const harness = makeHarness();
		const firstSpawn = deferred<{ pid: number }>();
		const secondSpawn = deferred<{ pid: number }>();
		harness.connection.spawn
			.mockImplementationOnce(() => firstSpawn.promise)
			.mockImplementationOnce(() => secondSpawn.promise);
		const handle = await actorBackend().create(createInput);
		const firstPending = handle.session.spawn({ command: "first" });
		const secondPending = handle.session.spawn({ command: "second" });
		harness.emit({
			pid: 22,
			stream: "stdout",
			data: ["$Uint8Array", Buffer.from("second").toString("base64")],
		});
		harness.emit({
			pid: 11,
			stream: "stdout",
			data: new TextEncoder().encode("first"),
		});
		secondSpawn.resolve({ pid: 22 });
		firstSpawn.resolve({ pid: 11 });
		const [first, second] = await Promise.all([firstPending, secondPending]);
		await expect(streamText(first.stdout)).resolves.toBe("first");
		await expect(streamText(second.stdout)).resolves.toBe("second");
		await handle.shutdown();
	});

	it("waits for and kills a spawn racing shutdown", async () => {
		const harness = makeHarness();
		const spawned = deferred<{ pid: number }>();
		const exited = deferred<number>();
		harness.connection.spawn.mockReturnValueOnce(spawned.promise);
		harness.connection.waitProcess.mockReturnValueOnce(exited.promise);
		harness.connection.killProcess.mockImplementationOnce(async (pid) => {
			expect(pid).toBe(9);
			exited.resolve(137);
		});
		const handle = await actorBackend().create(createInput);
		const spawnPromise = handle.session.spawn({ command: "slow-start" });
		const spawnRejection = expect(spawnPromise).rejects.toThrow("shut down");
		const shutdown = handle.shutdown();
		spawned.resolve({ pid: 9 });
		await Promise.all([spawnRejection, shutdown]);
		expect(harness.connection.killProcess).toHaveBeenCalledWith(9);
		expect(harness.connection.dispose).toHaveBeenCalledOnce();
	});

	it("does not block abort or shutdown on a stalled spawn RPC", async () => {
		const harness = makeHarness();
		const spawned = deferred<{ pid: number }>();
		harness.connection.spawn.mockReturnValueOnce(spawned.promise);
		const handle = await actorBackend().create(createInput);
		const controller = new AbortController();
		const spawnPromise = handle.session.spawn({
			command: "stalled",
			abortSignal: controller.signal,
		});
		controller.abort(new DOMException("cancelled", "AbortError"));
		await expect(spawnPromise).rejects.toMatchObject({ name: "AbortError" });
		await handle.shutdown();
		expect(harness.connection.dispose).toHaveBeenCalledOnce();

		spawned.resolve({ pid: 44 });
		await vi.waitFor(() =>
			expect(harness.connection.killProcess).toHaveBeenCalledWith(44),
		);
	});

	it("replaces invalid UTF-8 in command output", async () => {
		const harness = makeHarness();
		harness.connection.spawn.mockImplementationOnce(async () => {
			harness.emit({
				pid: 8,
				stream: "stdout",
				data: new Uint8Array([0xff]),
			});
			return { pid: 8 };
		});
		const handle = await actorBackend().create(createInput);
		await expect(handle.session.run({ command: "binary" })).resolves.toEqual({
			exitCode: 0,
			stdout: "�",
			stderr: "",
		});
		await handle.shutdown();
	});

	it("adapts and disposes a caller-created standalone Core VM", async () => {
		const vm = {
			dispose: vi.fn(async () => {}),
			process: {
				spawn: vi.fn(
					async (
						_command: string,
						_args: string[],
						options: { onStdout(data: Uint8Array): void },
					) => {
						queueMicrotask(() =>
							options.onStdout(new TextEncoder().encode("from core")),
						);
						return { pid: 31 };
					},
				),
				wait: vi.fn(async (pid: number) => ({
					pid,
					outcome: "exited" as const,
					exitCode: 0,
				})),
				kill: vi.fn(async () => {}),
			},
			readFile: vi.fn(async () => new Uint8Array()),
			writeFile: vi.fn(async () => {}),
			exists: vi.fn(async () => true),
			mkdir: vi.fn(async () => {}),
			remove: vi.fn(async () => {}),
		};
		const create = vi.fn(async () => vm as never);
		const backend = agentOSCoreBackend({ create });
		const first = await backend.create(createInput);
		const second = await backend.create(createInput);
		expect(second).toBe(first);
		expect(create).toHaveBeenCalledOnce();
		expect(clientMocks.createClient).not.toHaveBeenCalled();
		expect(create).toHaveBeenCalledWith({ sessionKey: "session-1" });

		await expect(
			first.session.run({ command: "echo ignored" }),
		).resolves.toEqual({
			exitCode: 0,
			stdout: "from core",
			stderr: "",
		});
		expect(vm.process.spawn).toHaveBeenCalledWith(
			"sh",
			["-lc", "echo ignored"],
			expect.objectContaining({ cwd: "/workspace" }),
		);
		await expect(first.captureState()).resolves.toEqual({
			backendName: "agentos-core-v1",
			metadata: { version: 1 },
			sessionKey: "session-1",
		});

		await first.shutdown();
		expect(vm.dispose).toHaveBeenCalledOnce();
	});
});
