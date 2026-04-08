/**
 * Integration tests for the full V8 runtime IPC round-trip.
 *
 * Exercises: spawn Rust process, authenticate, create session,
 * inject globals, execute code with sync and async bridge calls,
 * and destroy/cleanup.
 */

import { describe, it, expect, afterEach } from "vitest";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { existsSync } from "node:fs";
import { createV8Runtime } from "../src/runtime.js";
import type { V8Runtime, V8RuntimeOptions } from "../src/runtime.js";
import type {
	V8Session,
	V8ExecutionOptions,
	V8ExecutionResult,
	BridgeHandlers,
} from "../src/session.js";

const __dirname = dirname(fileURLToPath(import.meta.url));

// Resolve the Rust binary (debug build from crate target).
const BINARY_PATH = (() => {
	const release = resolve(
		__dirname,
		"../../../native/v8-runtime/target/release/secure-exec-v8",
	);
	if (existsSync(release)) return release;
	const debug = resolve(
		__dirname,
		"../../../native/v8-runtime/target/debug/secure-exec-v8",
	);
	if (existsSync(debug)) return debug;
	return undefined;
})();

const skipUnlessBinary = !BINARY_PATH;

/** Default execution options with minimal config. */
function defaultExecOptions(
	overrides: Partial<V8ExecutionOptions> = {},
): V8ExecutionOptions {
	return {
		bridgeCode: "",
		userCode: "",
		mode: "exec",
		processConfig: {
			cwd: "/tmp",
			env: {},
			timing_mitigation: "none",
			frozen_time_ms: null,
		},
		osConfig: {
			homedir: "/root",
			tmpdir: "/tmp",
			platform: "linux",
			arch: "x64",
		},
		bridgeHandlers: {},
		...overrides,
	};
}

describe.skipIf(skipUnlessBinary)("V8 IPC round-trip", () => {
	let runtime: V8Runtime | null = null;

	afterEach(async () => {
		if (runtime) {
			await runtime.dispose();
			runtime = null;
		}
	});

	async function createRuntime(
		opts?: Partial<V8RuntimeOptions>,
	): Promise<V8Runtime> {
		runtime = await createV8Runtime({
			binaryPath: BINARY_PATH!,
			...opts,
		});
		return runtime;
	}

	// --- Lifecycle ---

	it("spawns the Rust binary, authenticates, and disposes cleanly", async () => {
		const rt = await createRuntime();
		// If we got here, spawn + auth succeeded
		await rt.dispose();
		runtime = null;
	});

	it("creates a session and destroys it", async () => {
		const rt = await createRuntime();
		const session = await rt.createSession();
		await session.destroy();
	});

	it("creates a session with resource budgets", async () => {
		const rt = await createRuntime();
		const session = await rt.createSession({
			heapLimitMb: 64,
			cpuTimeLimitMs: 5000,
		});
		await session.destroy();
	});

	// --- Simple execution ---

	it("executes simple code and returns exit code 0", async () => {
		const rt = await createRuntime();
		const session = await rt.createSession();

		const result = await session.execute(
			defaultExecOptions({
				userCode: "1 + 1;",
			}),
		);

		expect(result.code).toBe(0);
		expect(result.error).toBeFalsy();

		await session.destroy();
	});

	it("executes code that accesses injected _processConfig", async () => {
		const rt = await createRuntime();
		const session = await rt.createSession();

		const logs: string[] = [];
		const result = await session.execute(
			defaultExecOptions({
				bridgeCode: "",
				userCode: `
					const config = globalThis._processConfig;
					if (config.cwd !== "/sandbox") throw new Error("wrong cwd: " + config.cwd);
					if (config.env.MY_VAR !== "hello") throw new Error("wrong env");
				`,
				processConfig: {
					cwd: "/sandbox",
					env: { MY_VAR: "hello" },
					timing_mitigation: "none",
					frozen_time_ms: null,
				},
				bridgeHandlers: {
					_log: (...args: unknown[]) => {
						logs.push(String(args[0]));
					},
				},
			}),
		);

		expect(result.code).toBe(0);
		expect(result.error).toBeFalsy();

		await session.destroy();
	});

	it("executes code that accesses injected _osConfig", async () => {
		const rt = await createRuntime();
		const session = await rt.createSession();

		const result = await session.execute(
			defaultExecOptions({
				userCode: `
					const os = globalThis._osConfig;
					if (os.platform !== "linux") throw new Error("wrong platform: " + os.platform);
					if (os.arch !== "x64") throw new Error("wrong arch: " + os.arch);
					if (os.homedir !== "/home/test") throw new Error("wrong homedir");
					if (os.tmpdir !== "/tmp") throw new Error("wrong tmpdir");
				`,
				osConfig: {
					homedir: "/home/test",
					tmpdir: "/tmp",
					platform: "linux",
					arch: "x64",
				},
			}),
		);

		expect(result.code).toBe(0);
		expect(result.error).toBeFalsy();

		await session.destroy();
	});

	// --- Error handling ---

	it("returns structured error for syntax errors", async () => {
		const rt = await createRuntime();
		const session = await rt.createSession();

		const result = await session.execute(
			defaultExecOptions({
				userCode: "function( {",
			}),
		);

		expect(result.error).toBeTruthy();
		expect(result.error!.type).toBe("SyntaxError");
		expect(result.error!.message).toBeTruthy();

		await session.destroy();
	});

	it("returns structured error for runtime errors", async () => {
		const rt = await createRuntime();
		const session = await rt.createSession();

		const result = await session.execute(
			defaultExecOptions({
				userCode: "throw new TypeError('test error');",
			}),
		);

		expect(result.error).toBeTruthy();
		expect(result.error!.type).toBe("TypeError");
		expect(result.error!.message).toContain("test error");
		expect(result.error!.stack).toBeTruthy();

		await session.destroy();
	});

	// --- Sync-blocking bridge call ---

	it("exercises a sync-blocking bridge call (_log)", async () => {
		const rt = await createRuntime();
		const session = await rt.createSession();

		const logged: unknown[][] = [];
		const result = await session.execute(
			defaultExecOptions({
				userCode: `_log("hello", "world");`,
				bridgeHandlers: {
					_log: (...args: unknown[]) => {
						logged.push(args);
					},
				},
			}),
		);

		expect(result.code).toBe(0);
		expect(result.error).toBeFalsy();
		expect(logged.length).toBe(1);
		expect(logged[0]).toEqual(["hello", "world"]);

		await session.destroy();
	});

	it("exercises a sync-blocking bridge call that returns a value (_fsReadFile)", async () => {
		const rt = await createRuntime();
		const session = await rt.createSession();

		const result = await session.execute(
			defaultExecOptions({
				userCode: `
					const content = _fsReadFile("/test.txt", "utf8");
					if (content !== "file contents here") {
						throw new Error("unexpected content: " + content);
					}
				`,
				bridgeHandlers: {
					_fsReadFile: (_path: unknown, _encoding: unknown) => {
						return "file contents here";
					},
				},
			}),
		);

		expect(result.code).toBe(0);
		expect(result.error).toBeFalsy();

		await session.destroy();
	});

	it("exercises a sync-blocking bridge call that returns binary data", async () => {
		const rt = await createRuntime();
		const session = await rt.createSession();

		const testData = new Uint8Array([0xde, 0xad, 0xbe, 0xef]);

		const result = await session.execute(
			defaultExecOptions({
				userCode: `
					const buf = _fsReadFileBinary("/test.bin");
					if (!(buf instanceof Uint8Array)) throw new Error("not Uint8Array");
					if (buf.length !== 4) throw new Error("wrong length: " + buf.length);
					if (buf[0] !== 0xde || buf[3] !== 0xef) throw new Error("wrong data");
				`,
				bridgeHandlers: {
					_fsReadFileBinary: () => testData,
				},
			}),
		);

		expect(result.code).toBe(0);
		expect(result.error).toBeFalsy();

		await session.destroy();
	});

	it("exercises a sync-blocking bridge call that throws an error", async () => {
		const rt = await createRuntime();
		const session = await rt.createSession();

		const result = await session.execute(
			defaultExecOptions({
				userCode: `
					try {
						_fsReadFile("/nonexistent", "utf8");
						throw new Error("should have thrown");
					} catch (e) {
						if (!e.message.includes("ENOENT")) {
							throw new Error("wrong error: " + e.message);
						}
					}
				`,
				bridgeHandlers: {
					_fsReadFile: () => {
						throw new Error("ENOENT: no such file or directory");
					},
				},
			}),
		);

		expect(result.code).toBe(0);
		expect(result.error).toBeFalsy();

		await session.destroy();
	});

	// --- Async promise-returning bridge call ---

	it("exercises an async bridge call (_networkFetchRaw)", async () => {
		const rt = await createRuntime();
		const session = await rt.createSession();

		const result = await session.execute(
			defaultExecOptions({
				userCode: `
					(async () => {
						const resp = await _networkFetchRaw("https://example.com", "GET", {});
						if (resp.status !== 200) throw new Error("wrong status: " + resp.status);
						if (resp.body !== "mock response body") throw new Error("wrong body");
					})();
				`,
				bridgeHandlers: {
					_networkFetchRaw: async (
						_url: unknown,
						_method: unknown,
						_opts: unknown,
					) => {
						// Simulate async network latency
						await new Promise((r) => setTimeout(r, 10));
						return { status: 200, body: "mock response body", headers: {} };
					},
				},
			}),
		);

		expect(result.code).toBe(0);
		expect(result.error).toBeFalsy();

		await session.destroy();
	});

	it("exercises an async bridge call that rejects", async () => {
		const rt = await createRuntime();
		const session = await rt.createSession();

		const result = await session.execute(
			defaultExecOptions({
				userCode: `
					(async () => {
						try {
							await _networkFetchRaw("https://example.com", "GET", {});
							throw new Error("should have thrown");
						} catch (e) {
							if (!e.message.includes("network timeout")) {
								throw new Error("wrong error: " + e.message);
							}
						}
					})();
				`,
				bridgeHandlers: {
					_networkFetchRaw: async () => {
						throw new Error("network timeout");
					},
				},
			}),
		);

		expect(result.code).toBe(0);
		expect(result.error).toBeFalsy();

		await session.destroy();
	});

	// --- Multiple bridge calls in one execution ---

	it("handles multiple sync bridge calls in sequence", async () => {
		const rt = await createRuntime();
		const session = await rt.createSession();

		const calls: string[] = [];

		const result = await session.execute(
			defaultExecOptions({
				userCode: `
					_log("first");
					_log("second");
					_log("third");
				`,
				bridgeHandlers: {
					_log: (msg: unknown) => {
						calls.push(String(msg));
					},
				},
			}),
		);

		expect(result.code).toBe(0);
		expect(calls).toEqual(["first", "second", "third"]);

		await session.destroy();
	});

	it("handles mixed sync and async bridge calls", async () => {
		const rt = await createRuntime();
		const session = await rt.createSession();

		const calls: string[] = [];

		const result = await session.execute(
			defaultExecOptions({
				userCode: `
					(async () => {
						_log("sync-before");
						const data = await _networkFetchRaw("https://example.com", "GET", {});
						_log("sync-after: " + data.status);
					})();
				`,
				bridgeHandlers: {
					_log: (msg: unknown) => {
						calls.push(String(msg));
					},
					_networkFetchRaw: async () => {
						return { status: 200, body: "", headers: {} };
					},
				},
			}),
		);

		expect(result.code).toBe(0);
		expect(calls).toEqual(["sync-before", "sync-after: 200"]);

		await session.destroy();
	});

	// --- Session cleanup ---

	it("destroys session cleanly after execution", async () => {
		const rt = await createRuntime();
		const session = await rt.createSession();

		const result = await session.execute(
			defaultExecOptions({ userCode: "1 + 1;" }),
		);
		expect(result.code).toBe(0);

		// Destroy should not throw
		await session.destroy();
	});

	it("creates multiple sessions sequentially", async () => {
		const rt = await createRuntime();

		for (let i = 0; i < 3; i++) {
			const session = await rt.createSession();
			const result = await session.execute(
				defaultExecOptions({ userCode: `${i};` }),
			);
			expect(result.code).toBe(0);
			await session.destroy();
		}
	});

	it("runs concurrent sessions without bridge calls", async () => {
		const rt = await createRuntime({ maxSessions: 3 });

		const sessions = await Promise.all([
			rt.createSession(),
			rt.createSession(),
			rt.createSession(),
		]);

		// Execute simple code without bridge calls to test basic concurrency
		const results = await Promise.all(
			sessions.map((s, i) =>
				s.execute(
					defaultExecOptions({
						userCode: `var x = ${i} * 2;`,
					}),
				),
			),
		);

		for (const result of results) {
			expect(result.code).toBe(0);
			expect(result.error).toBeFalsy();
		}

		await Promise.all(sessions.map((s) => s.destroy()));
	});

	// --- Bridge call with no handler ---

	it("returns error for unhandled bridge method", async () => {
		const rt = await createRuntime();
		const session = await rt.createSession();

		const result = await session.execute(
			defaultExecOptions({
				userCode: `
					try {
						_fsReadFile("/test.txt", "utf8");
						throw new Error("should have thrown");
					} catch (e) {
						if (!e.message.includes("No handler")) {
							throw new Error("wrong error: " + e.message);
						}
					}
				`,
				bridgeHandlers: {
					// Intentionally no _fsReadFile handler
				},
			}),
		);

		expect(result.code).toBe(0);
		expect(result.error).toBeFalsy();

		await session.destroy();
	});

	// --- Globals are frozen ---

	it("verifies _processConfig is frozen and non-configurable", async () => {
		const rt = await createRuntime();
		const session = await rt.createSession();

		const result = await session.execute(
			defaultExecOptions({
				userCode: `
					// Should not be able to overwrite
					try {
						globalThis._processConfig = { cwd: "/hacked" };
					} catch (e) {
						// Expected in strict mode
					}
					// Original should remain
					if (globalThis._processConfig.cwd !== "/tmp") {
						throw new Error("processConfig was overwritten: " + globalThis._processConfig.cwd);
					}
				`,
			}),
		);

		expect(result.code).toBe(0);
		expect(result.error).toBeFalsy();

		await session.destroy();
	});

	// --- WASM disabled ---

	it("verifies WebAssembly compilation is disabled", async () => {
		const rt = await createRuntime();
		const session = await rt.createSession();

		const result = await session.execute(
			defaultExecOptions({
				userCode: `
					try {
						// Minimal valid WASM module
						const bytes = new Uint8Array([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);
						new WebAssembly.Module(bytes);
						throw new Error("should have thrown");
					} catch (e) {
						if (!e.message.toLowerCase().includes("wasm")) {
							// Some V8 errors mention "WebAssembly" not "wasm"
							if (!e.message.includes("WebAssembly") && !e.message.includes("disallowed")) {
								throw new Error("unexpected error: " + e.message);
							}
						}
					}
				`,
			}),
		);

		expect(result.code).toBe(0);
		expect(result.error).toBeFalsy();

		await session.destroy();
	});

	// --- Batch module resolution ---

	it("batch-resolves multiple ESM imports in one round-trip", async () => {
		const rt = await createRuntime();
		const session = await rt.createSession();

		// Track which bridge methods are called
		const methodCalls: string[] = [];

		const result = await session.execute(
			defaultExecOptions({
				mode: "run",
				userCode: `
					import { a } from './a.mjs';
					import { b } from './b.mjs';
					import { c } from './c.mjs';
					export const sum = a + b + c;
				`,
				filePath: "/app/main.mjs",
				bridgeHandlers: {
					_batchResolveModules: async (requests: unknown) => {
						methodCalls.push("_batchResolveModules");
						const reqs = requests as [string, string][];
						return reqs.map(([specifier]) => {
							const name = specifier.replace("./", "").replace(".mjs", "");
							const values: Record<string, number> = { a: 10, b: 20, c: 30 };
							const val = values[name] ?? 0;
							return {
								resolved: `/${name}.mjs`,
								source: `export const ${name} = ${val};`,
							};
						});
					},
					_resolveModule: (request: unknown, _fromDir: unknown) => {
						methodCalls.push("_resolveModule");
						const name = String(request).replace("./", "").replace(".mjs", "");
						return `/${name}.mjs`;
					},
					_loadFile: (path: unknown) => {
						methodCalls.push("_loadFile");
						const name = String(path).replace("/", "").replace(".mjs", "");
						const values: Record<string, number> = { a: 10, b: 20, c: 30 };
						return `export const ${name} = ${values[name] ?? 0};`;
					},
				},
			}),
		);

		expect(result.code).toBe(0);
		expect(result.error).toBeFalsy();

		// Verify correct result
		const v8 = await import("node:v8");
		const exports = v8.deserialize(result.exports!) as { sum: number };
		expect(exports.sum).toBe(60);

		// Verify batch was used (not individual calls)
		expect(methodCalls).toContain("_batchResolveModules");
		expect(methodCalls).not.toContain("_resolveModule");
		expect(methodCalls).not.toContain("_loadFile");

		await session.destroy();
	});

	it("falls back to individual resolution when batch handler errors", async () => {
		const rt = await createRuntime();
		const session = await rt.createSession();

		const result = await session.execute(
			defaultExecOptions({
				mode: "run",
				userCode: `
					import { val } from './dep.mjs';
					export const result = val;
				`,
				filePath: "/app/main.mjs",
				bridgeHandlers: {
					_batchResolveModules: async () => {
						throw new Error("batch not supported");
					},
					_resolveModule: (_request: unknown, _fromDir: unknown) => {
						return "/dep.mjs";
					},
					_loadFile: (_path: unknown) => {
						return "export const val = 99;";
					},
				},
			}),
		);

		expect(result.code).toBe(0);
		expect(result.error).toBeFalsy();

		const v8 = await import("node:v8");
		const exports = v8.deserialize(result.exports!) as { result: number };
		expect(exports.result).toBe(99);

		await session.destroy();
	});
});
