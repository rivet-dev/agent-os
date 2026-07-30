import { afterEach, describe, expect, it } from "vitest";
import { POLYFILL_CODE_MAP } from "../../src/runtime.js";

function loadPolyfill<T>(name: string): T {
	const module = { exports: {} as T };
	new Function("module", "exports", "require", POLYFILL_CODE_MAP[name])(
		module,
		module.exports,
		() => {
			throw new Error(`unexpected require from ${name}`);
		},
	);
	return module.exports;
}

const globals = [
	"__agentOSEncoding",
	"_childProcessSpawnStart",
	"_childProcessSpawnSync",
	"_childProcessPoll",
	"_childProcessStdinWrite",
	"_childProcessStdinClose",
	"_childProcessKill",
] as const;
const previousGlobals = new Map<string, unknown>();

afterEach(() => {
	for (const name of globals) {
		const previous = previousGlobals.get(name);
		if (previous === undefined) {
			delete (globalThis as Record<string, unknown>)[name];
		} else {
			(globalThis as Record<string, unknown>)[name] = previous;
		}
	}
	previousGlobals.clear();
});

function setGlobal(name: (typeof globals)[number], value: unknown): void {
	if (!previousGlobals.has(name)) {
		previousGlobals.set(name, (globalThis as Record<string, unknown>)[name]);
	}
	(globalThis as Record<string, unknown>)[name] = value;
}

describe("browser stream polyfill", () => {
	it("honors writable backpressure and normal finish/close lifecycle", async () => {
		let completeWrite: ((error?: Error) => void) | undefined;
		const stream = loadPolyfill<{
			Writable: new (
				options: unknown,
			) => {
				on(event: string, listener: (...args: unknown[]) => void): unknown;
				write(chunk: Uint8Array, callback?: (error?: Error) => void): boolean;
				end(callback?: (error?: Error) => void): void;
			};
		}>("stream");
		const writable = new stream.Writable({
			highWaterMark: 1,
			write(
				_chunk: Uint8Array,
				_encoding: unknown,
				callback: (error?: Error) => void,
			) {
				completeWrite = callback;
			},
		});
		const lifecycle: string[] = [];
		writable.on("drain", () => lifecycle.push("drain"));
		writable.on("finish", () => lifecycle.push("finish"));
		writable.on("close", () => lifecycle.push("close"));

		expect(writable.write(new Uint8Array([1]))).toBe(false);
		writable.end(() => lifecycle.push("end-callback"));
		expect(lifecycle).toEqual([]);
		completeWrite?.();
		await new Promise((resolve) => queueMicrotask(resolve));
		expect(lifecycle).toEqual(["drain", "finish", "end-callback", "close"]);
	});

	it("surfaces write-after-end and WHATWG write errors", async () => {
		const stream = loadPolyfill<{
			Writable: {
				new (
					options?: unknown,
				): {
					on(event: string, listener: (...args: unknown[]) => void): unknown;
					write(chunk: Uint8Array, callback?: (error?: Error) => void): boolean;
					end(callback?: (error?: Error) => void): void;
				};
				toWeb(value: unknown): WritableStream<Uint8Array>;
			};
		}>("stream");
		const writable = new stream.Writable();
		writable.on("error", () => {});
		writable.end();
		const afterEnd = new Promise<Error>((resolve) => {
			expect(
				writable.write(new Uint8Array([1]), (error) => resolve(error as Error)),
			).toBe(false);
		});
		expect((await afterEnd).message).toMatch(/write after end/);

		const failed = new stream.Writable({
			write(
				_chunk: Uint8Array,
				_encoding: unknown,
				callback: (error: Error) => void,
			) {
				callback(new Error("sink failed"));
			},
		});
		const writer = stream.Writable.toWeb(failed).getWriter();
		await expect(writer.write(new Uint8Array([1]))).rejects.toThrow(
			"sink failed",
		);
	});

	it("preserves queued chunk types and EOF through Readable.toWeb", async () => {
		const stream = loadPolyfill<{
			Readable: {
				new (): { push(chunk: unknown): boolean };
				toWeb(value: unknown): ReadableStream<unknown>;
			};
		}>("stream");
		const readable = new stream.Readable();
		readable.push("hello");
		readable.push(null);
		const reader = stream.Readable.toWeb(readable).getReader();
		expect(await reader.read()).toEqual({ done: false, value: "hello" });
		expect(await reader.read()).toEqual({ done: true, value: undefined });
	});

	it("keeps PassThrough readable and writable lifecycle coherent", async () => {
		const stream = loadPolyfill<{
			PassThrough: new () => {
				on(event: string, listener: (...args: unknown[]) => void): unknown;
				end(chunk: Uint8Array): void;
			};
		}>("stream");
		const pass = new stream.PassThrough();
		const lifecycle: string[] = [];
		pass.on("data", (chunk) =>
			lifecycle.push(`data:${(chunk as Uint8Array)[0]}`),
		);
		pass.on("end", () => lifecycle.push("end"));
		pass.on("finish", () => lifecycle.push("finish"));
		pass.on("close", () => lifecycle.push("close"));
		pass.end(new Uint8Array([7]));
		await new Promise((resolve) => queueMicrotask(resolve));
		expect(lifecycle).toEqual(["data:7", "end", "finish", "close"]);
	});
});

describe("browser child_process polyfill", () => {
	it("preserves argv and signal identity and orders output closure before child close", async () => {
		const requests: unknown[] = [];
		const kills: number[] = [];
		const events: unknown[] = [
			{ type: "stdout", data: { __agentOSType: "bytes", base64: "aGk=" } },
			{ type: "exit", exitCode: 0, signal: 15 },
		];
		setGlobal("__agentOSEncoding", {
			encodeBytesPayload: (value: unknown) => value,
			decodeBytesPayload: (value: { base64: string }) =>
				new Uint8Array(Buffer.from(value.base64, "base64")),
		});
		setGlobal("_childProcessSpawnStart", (request: unknown) => {
			requests.push(request);
			return 41;
		});
		setGlobal("_childProcessPoll", () => events.shift() ?? null);
		setGlobal("_childProcessStdinWrite", () => undefined);
		setGlobal("_childProcessStdinClose", () => undefined);
		setGlobal("_childProcessKill", (_sessionId: number, signal: number) => {
			kills.push(signal);
			return true;
		});
		const childProcess = loadPolyfill<{
			spawn(
				command: string,
				args: string[],
				options: unknown,
			): {
				spawnargs: string[];
				exitCode: number | null;
				signalCode: string | null;
				killed: boolean;
				stdout: {
					on(event: string, listener: (...args: unknown[]) => void): unknown;
				};
				stderr: {
					on(event: string, listener: (...args: unknown[]) => void): unknown;
				};
				stdin: {
					end(callback?: (error?: Error) => void): unknown;
					write(data: string, callback?: (error?: Error) => void): boolean;
				};
				on(event: string, listener: (...args: unknown[]) => void): unknown;
				kill(signal?: string): boolean;
			};
		}>("child_process");
		const child = childProcess.spawn("/bin/tool", ["", "tail"], {
			argv0: "",
			cwd: "/work",
			env: { ONLY: "value" },
		});
		const lifecycle: string[] = [];
		child.stdout.on("data", (chunk) =>
			lifecycle.push(`stdout:${String(chunk)}`),
		);
		child.stdout.on("end", () => lifecycle.push("stdout:end"));
		child.stdout.on("close", () => lifecycle.push("stdout:close"));
		child.stderr.on("end", () => lifecycle.push("stderr:end"));
		child.stderr.on("close", () => lifecycle.push("stderr:close"));
		child.on("exit", (code, signal) =>
			lifecycle.push(`exit:${String(code)}:${String(signal)}`),
		);
		const closed = new Promise<void>((resolve) => {
			child.on("close", (code, signal) => {
				lifecycle.push(`close:${String(code)}:${String(signal)}`);
				resolve();
			});
		});

		expect(child.spawnargs).toEqual(["", "", "tail"]);
		expect(child.kill("SIGTERM")).toBe(true);
		await closed;
		expect(requests).toEqual([
			{
				command: "/bin/tool",
				args: ["", "tail"],
				options: { argv0: "", cwd: "/work", env: { ONLY: "value" } },
			},
		]);
		expect(kills).toEqual([15]);
		expect(child.killed).toBe(true);
		expect(child.exitCode).toBeNull();
		expect(child.signalCode).toBe("SIGTERM");
		expect(child.kill()).toBe(false);
		expect(lifecycle).toEqual([
			"stdout:hi",
			"exit:null:SIGTERM",
			"stdout:end",
			"stdout:close",
			"stderr:end",
			"stderr:close",
			"close:null:SIGTERM",
		]);

		await new Promise<void>((resolve, reject) =>
			child.stdin.end((error) => (error ? reject(error) : resolve())),
		);
		const writeAfterEnd = new Promise<Error>((resolve) => {
			expect(
				child.stdin.write("late", (error) => resolve(error as Error)),
			).toBe(false);
		});
		expect((await writeAfterEnd).message).toMatch(/write after end/);
	});

	it("reports spawnSync signal termination separately from exit status", () => {
		setGlobal("__agentOSEncoding", {
			encodeBytesPayload: (value: unknown) => value,
			decodeBytesPayload: (value: unknown) => value,
		});
		setGlobal("_childProcessSpawnSync", () =>
			JSON.stringify({ stdout: "", stderr: "", code: 0, signal: 9 }),
		);
		const childProcess = loadPolyfill<{
			spawnSync(
				command: string,
				args: string[],
				options: unknown,
			): {
				status: number | null;
				signal: string | null;
			};
		}>("child_process");
		expect(
			childProcess.spawnSync("tool", [], { encoding: "utf8" }),
		).toMatchObject({
			status: null,
			signal: "SIGKILL",
		});
		setGlobal("_childProcessSpawnSync", () =>
			JSON.stringify({ stdout: "", stderr: "", code: 0, signal: 6 }),
		);
		expect(
			childProcess.spawnSync("tool", [], { encoding: "utf8" }),
		).toMatchObject({
			status: null,
			signal: "SIGABRT",
		});
	});
});
