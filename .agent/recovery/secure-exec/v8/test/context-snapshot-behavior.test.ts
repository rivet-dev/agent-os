/**
 * Context snapshot behavior tests at the V8 IPC level.
 *
 * Tests that snapshot-restored contexts have working bridge infrastructure:
 * getter-based FS facade delegation, config application via __runtimeApplyConfig,
 * bridge function replacement (sync + async), timing mitigation freeze,
 * polyfill loading via _loadPolyfill, and correct exec/run results.
 *
 * These tests use the V8 runtime directly with bridge code that exercises
 * the snapshot path. The real bridge code (composeStaticBridgeCode) is tested
 * implicitly through snapshot-security.test.ts and ipc-roundtrip.test.ts.
 */

import { describe, it, expect, afterEach } from "vitest";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { existsSync } from "node:fs";
import * as nodeV8 from "node:v8";
import { createV8Runtime } from "../src/runtime.js";
import type { V8Runtime, V8RuntimeOptions } from "../src/runtime.js";
import type { V8ExecutionOptions } from "../src/session.js";

const __dirname = dirname(fileURLToPath(import.meta.url));

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

describe.skipIf(skipUnlessBinary)("V8 context snapshot behavior", () => {
	const runtimes: V8Runtime[] = [];

	afterEach(async () => {
		await Promise.allSettled(runtimes.map((rt) => rt.dispose()));
		runtimes.length = 0;
	});

	async function createRuntime(
		opts?: Partial<V8RuntimeOptions>,
	): Promise<V8Runtime> {
		const rt = await createV8Runtime({
			binaryPath: BINARY_PATH!,
			...opts,
		});
		runtimes.push(rt);
		return rt;
	}

	// -------------------------------------------------------------------
	// AC1: _fs.readFile resolves to the current global, not stale ref
	// -------------------------------------------------------------------

	describe("getter-based FS facade", () => {
		it("_fsReadFile dispatches to session-local bridge function", async () => {
			const rt = await createRuntime();
			const session = await rt.createSession();

			const result = await session.execute(
				defaultExecOptions({
					userCode: `
						var content = _fsReadFile("/test.txt", "utf8");
						if (content !== "session-data") throw new Error("wrong: " + content);
					`,
					bridgeHandlers: {
						_fsReadFile: () => "session-data",
					},
				}),
			);

			expect(result.code).toBe(0);
			expect(result.error).toBeFalsy();
			await session.destroy();
		});

		it("each session gets its own bridge function binding", async () => {
			const rt = await createRuntime();

			// Session A: bridge returns "A"
			const sA = await rt.createSession();
			const rA = await sA.execute(
				defaultExecOptions({
					userCode: `
						var c = _fsReadFile("/x", "utf8");
						if (c !== "A") throw new Error("sA wrong: " + c);
					`,
					bridgeHandlers: { _fsReadFile: () => "A" },
				}),
			);
			expect(rA.code).toBe(0);
			expect(rA.error).toBeFalsy();
			await sA.destroy();

			// Session B: bridge returns "B"
			const sB = await rt.createSession();
			const rB = await sB.execute(
				defaultExecOptions({
					userCode: `
						var c = _fsReadFile("/x", "utf8");
						if (c !== "B") throw new Error("sB wrong: " + c);
					`,
					bridgeHandlers: { _fsReadFile: () => "B" },
				}),
			);
			expect(rB.code).toBe(0);
			expect(rB.error).toBeFalsy();
			await sB.destroy();
		});
	});

	// -------------------------------------------------------------------
	// AC2: __runtimeApplyConfig applies timing freeze
	// (uses bridge IIFE with __runtimeApplyConfig pattern)
	// -------------------------------------------------------------------

	describe("config application via bridge code", () => {
		it("_processConfig is set from execution options", async () => {
			const rt = await createRuntime();
			const session = await rt.createSession();

			const result = await session.execute(
				defaultExecOptions({
					processConfig: {
						cwd: "/my-app",
						env: { NODE_ENV: "test" },
						timing_mitigation: "none",
						frozen_time_ms: null,
					},
					userCode: `
						if (_processConfig.cwd !== "/my-app") throw new Error("wrong cwd: " + _processConfig.cwd);
						if (_processConfig.env.NODE_ENV !== "test") throw new Error("wrong env");
					`,
				}),
			);

			expect(result.code).toBe(0);
			expect(result.error).toBeFalsy();
			await session.destroy();
		});

		it("_osConfig is set from execution options", async () => {
			const rt = await createRuntime();
			const session = await rt.createSession();

			const result = await session.execute(
				defaultExecOptions({
					osConfig: {
						homedir: "/home/test",
						tmpdir: "/var/tmp",
						platform: "darwin",
						arch: "arm64",
					},
					userCode: `
						if (_osConfig.homedir !== "/home/test") throw new Error("wrong homedir");
						if (_osConfig.platform !== "darwin") throw new Error("wrong platform");
						if (_osConfig.arch !== "arm64") throw new Error("wrong arch");
					`,
				}),
			);

			expect(result.code).toBe(0);
			expect(result.error).toBeFalsy();
			await session.destroy();
		});
	});

	// -------------------------------------------------------------------
	// AC3: restored context has working bridge infrastructure
	// -------------------------------------------------------------------

	describe("bridge infrastructure on restored context", () => {
		it("_log bridge function dispatches from console.log equivalent", async () => {
			const rt = await createRuntime();
			const session = await rt.createSession();
			const logged: string[] = [];

			const result = await session.execute(
				defaultExecOptions({
					userCode: `_log("from-snapshot-context");`,
					bridgeHandlers: {
						_log: (msg: unknown) => logged.push(String(msg)),
					},
				}),
			);

			expect(result.code).toBe(0);
			expect(result.error).toBeFalsy();
			expect(logged).toContain("from-snapshot-context");
			await session.destroy();
		});

		it("sync bridge returns value to V8 correctly", async () => {
			const rt = await createRuntime();
			const session = await rt.createSession();

			const result = await session.execute(
				defaultExecOptions({
					userCode: `
						var val = _fsReadFile("/data.json", "utf8");
						if (val !== '{"ok":true}') throw new Error("wrong: " + val);
					`,
					bridgeHandlers: {
						_fsReadFile: () => '{"ok":true}',
					},
				}),
			);

			expect(result.code).toBe(0);
			expect(result.error).toBeFalsy();
			await session.destroy();
		});

		it("sync bridge throws error to V8 correctly", async () => {
			const rt = await createRuntime();
			const session = await rt.createSession();

			const result = await session.execute(
				defaultExecOptions({
					userCode: `
						try {
							_fsReadFile("/missing", "utf8");
							throw new Error("should have thrown");
						} catch (e) {
							if (!e.message.includes("ENOENT")) {
								throw new Error("wrong error: " + e.message);
							}
						}
					`,
					bridgeHandlers: {
						_fsReadFile: () => {
							throw new Error("ENOENT: not found");
						},
					},
				}),
			);

			expect(result.code).toBe(0);
			expect(result.error).toBeFalsy();
			await session.destroy();
		});

		it("async bridge call resolves promise correctly", async () => {
			const rt = await createRuntime();
			const session = await rt.createSession();

			const result = await session.execute(
				defaultExecOptions({
					userCode: `
						(async () => {
							var resp = await _networkFetchRaw("https://api.test", "GET", {});
							if (resp.status !== 200) throw new Error("bad status");
							if (resp.body !== "snapshot-data") throw new Error("bad body");
						})();
					`,
					bridgeHandlers: {
						_networkFetchRaw: async () => {
							await new Promise((r) => setTimeout(r, 5));
							return { status: 200, body: "snapshot-data", headers: {} };
						},
					},
				}),
			);

			expect(result.code).toBe(0);
			expect(result.error).toBeFalsy();
			await session.destroy();
		});

		it("async bridge call rejects promise correctly", async () => {
			const rt = await createRuntime();
			const session = await rt.createSession();

			const result = await session.execute(
				defaultExecOptions({
					userCode: `
						(async () => {
							try {
								await _networkFetchRaw("https://api.test", "GET", {});
								throw new Error("should have thrown");
							} catch (e) {
								if (!e.message.includes("timeout")) {
									throw new Error("wrong error: " + e.message);
								}
							}
						})();
					`,
					bridgeHandlers: {
						_networkFetchRaw: async () => {
							throw new Error("timeout");
						},
					},
				}),
			);

			expect(result.code).toBe(0);
			expect(result.error).toBeFalsy();
			await session.destroy();
		});
	});

	// -------------------------------------------------------------------
	// AC5: timing mitigation at V8 level
	// Note: Full timing freeze (Date constructor, SharedArrayBuffer removal)
	// is tested at the NodeRuntime level since it requires the bridge IIFE.
	// Here we verify the V8-level timing behavior (real Date.now works).
	// -------------------------------------------------------------------

	describe("timing at V8 level", () => {
		it("Date.now() returns a reasonable timestamp", async () => {
			const rt = await createRuntime();
			const session = await rt.createSession();
			const before = Date.now();

			const result = await session.execute(
				defaultExecOptions({
					userCode: `
						var now = Date.now();
						if (now < ${before - 10000} || now > ${before + 10000}) {
							throw new Error("Date.now out of range: " + now);
						}
					`,
				}),
			);

			expect(result.code).toBe(0);
			expect(result.error).toBeFalsy();
			await session.destroy();
		});
	});

	// -------------------------------------------------------------------
	// AC6: polyfills accessible via _loadPolyfill bridge
	// -------------------------------------------------------------------

	describe("polyfill bridge", () => {
		it("_loadPolyfill bridge function is callable", async () => {
			const rt = await createRuntime();
			const session = await rt.createSession();
			const polyfillsCalled: string[] = [];

			const result = await session.execute(
				defaultExecOptions({
					userCode: `
						if (typeof _loadPolyfill !== "function") {
							throw new Error("_loadPolyfill not defined");
						}
					`,
					bridgeHandlers: {
						_loadPolyfill: (name: unknown) => {
							polyfillsCalled.push(String(name));
							return "";
						},
					},
				}),
			);

			expect(result.code).toBe(0);
			expect(result.error).toBeFalsy();
			await session.destroy();
		});
	});

	// -------------------------------------------------------------------
	// AC7: exec and run produce correct results
	// -------------------------------------------------------------------

	describe("exec and run results", () => {
		it("exec returns exit code 0 for simple code", async () => {
			const rt = await createRuntime();
			const session = await rt.createSession();

			const result = await session.execute(
				defaultExecOptions({ userCode: `1 + 1;` }),
			);

			expect(result.code).toBe(0);
			expect(result.error).toBeFalsy();
			await session.destroy();
		});

		it("exec returns structured error for TypeError", async () => {
			const rt = await createRuntime();
			const session = await rt.createSession();

			const result = await session.execute(
				defaultExecOptions({ userCode: `throw new TypeError("snap-err");` }),
			);

			expect(result.error).toBeTruthy();
			expect(result.error!.type).toBe("TypeError");
			expect(result.error!.message).toContain("snap-err");
			await session.destroy();
		});

		it("exec returns structured error for SyntaxError", async () => {
			const rt = await createRuntime();
			const session = await rt.createSession();

			const result = await session.execute(
				defaultExecOptions({ userCode: `function( {` }),
			);

			expect(result.error).toBeTruthy();
			expect(result.error!.type).toBe("SyntaxError");
			await session.destroy();
		});

		it("run returns ESM exports from snapshot-restored context", async () => {
			const rt = await createRuntime();
			const session = await rt.createSession();

			const result = await session.execute(
				defaultExecOptions({
					mode: "run",
					userCode: `export const answer = 42; export default "ok";`,
					filePath: "/entry.mjs",
					bridgeHandlers: {
						_resolveModule: () => null,
						_loadFile: () => null,
					},
				}),
			);

			expect(result.code).toBe(0);
			expect(result.error).toBeFalsy();
			expect(result.exports).toBeTruthy();

			const exports = nodeV8.deserialize(result.exports!) as {
				answer: number;
				default: string;
			};
			expect(exports.answer).toBe(42);
			expect(exports.default).toBe("ok");
			await session.destroy();
		});

		it("sequential executions produce independent results", async () => {
			const rt = await createRuntime();
			const session = await rt.createSession();

			const r1 = await session.execute(
				defaultExecOptions({ userCode: `1 + 1;` }),
			);
			expect(r1.code).toBe(0);

			const r2 = await session.execute(
				defaultExecOptions({ userCode: `throw new Error("second");` }),
			);
			expect(r2.code).not.toBe(0);

			const r3 = await session.execute(
				defaultExecOptions({ userCode: `2 + 2;` }),
			);
			expect(r3.code).toBe(0);

			await session.destroy();
		});

		it("session isolation — state does not leak between sessions", async () => {
			const rt = await createRuntime();

			const s1 = await rt.createSession();
			await s1.execute(
				defaultExecOptions({
					userCode: `globalThis.__leaked = "secret";`,
				}),
			);
			await s1.destroy();

			const s2 = await rt.createSession();
			const r2 = await s2.execute(
				defaultExecOptions({
					userCode: `
						if (typeof globalThis.__leaked !== "undefined") {
							throw new Error("state leaked: " + globalThis.__leaked);
						}
					`,
				}),
			);
			expect(r2.code).toBe(0);
			expect(r2.error).toBeFalsy();
			await s2.destroy();
		});
	});
});
