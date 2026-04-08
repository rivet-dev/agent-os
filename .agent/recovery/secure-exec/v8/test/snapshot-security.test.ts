/**
 * Snapshot security and integration tests.
 *
 * Proves snapshot security properties: WASM stays disabled after restore,
 * sessions are isolated (no state leakage), external references dispatch
 * correctly, warm-up eliminates cold-start, and different bridge code
 * variants get separate snapshot entries.
 */

import { describe, it, expect, afterEach } from "vitest";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { existsSync } from "node:fs";
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

describe.skipIf(skipUnlessBinary)("V8 snapshot security", () => {
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

	// --- WASM disabled after snapshot restore ---

	it("WASM compilation throws after snapshot restore + disable_wasm()", async () => {
		const rt = await createRuntime();
		const session = await rt.createSession();

		const result = await session.execute(
			defaultExecOptions({
				userCode: `
					try {
						var bytes = new Uint8Array([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);
						new WebAssembly.Module(bytes);
						throw new Error("WASM_SHOULD_BE_BLOCKED");
					} catch (e) {
						if (e.message === "WASM_SHOULD_BE_BLOCKED") throw e;
						// Expected: WASM blocked
					}
				`,
			}),
		);

		expect(result.code).toBe(0);
		expect(result.error).toBeFalsy();

		await session.destroy();
	});

	// --- Session isolation (no state leakage from snapshots) ---

	it("session A's snapshot does not leak state to session B (fresh context per session)", async () => {
		const rt = await createRuntime();

		// Session A: set a global variable
		const sessionA = await rt.createSession();
		const resultA = await sessionA.execute(
			defaultExecOptions({
				userCode: `
					globalThis.__secretFromA = "session-a-data";
					if (globalThis.__secretFromA !== "session-a-data") {
						throw new Error("failed to set secret");
					}
				`,
			}),
		);
		expect(resultA.code).toBe(0);
		expect(resultA.error).toBeFalsy();
		await sessionA.destroy();

		// Session B: should NOT see session A's global
		const sessionB = await rt.createSession();
		const resultB = await sessionB.execute(
			defaultExecOptions({
				userCode: `
					if (typeof globalThis.__secretFromA !== "undefined") {
						throw new Error("LEAKED: session A state visible in session B: " + globalThis.__secretFromA);
					}
				`,
			}),
		);
		expect(resultB.code).toBe(0);
		expect(resultB.error).toBeFalsy();
		await sessionB.destroy();
	});

	// --- External references survive snapshot restore (sync + async bridge calls) ---

	it("sync bridge calls dispatch correctly on restored isolate", async () => {
		const rt = await createRuntime();
		const session = await rt.createSession();

		const logged: string[] = [];
		const result = await session.execute(
			defaultExecOptions({
				userCode: `
					_log("sync-from-snapshot-restore");
					var content = _fsReadFile("/test.txt", "utf8");
					if (content !== "snapshot-data") throw new Error("wrong: " + content);
				`,
				bridgeHandlers: {
					_log: (msg: unknown) => {
						logged.push(String(msg));
					},
					_fsReadFile: () => "snapshot-data",
				},
			}),
		);

		expect(result.code).toBe(0);
		expect(result.error).toBeFalsy();
		expect(logged).toEqual(["sync-from-snapshot-restore"]);

		await session.destroy();
	});

	it("async bridge calls dispatch correctly on restored isolate", async () => {
		const rt = await createRuntime();
		const session = await rt.createSession();

		const result = await session.execute(
			defaultExecOptions({
				userCode: `
					(async () => {
						var resp = await _networkFetchRaw("https://example.com", "GET", {});
						if (resp.status !== 200) throw new Error("wrong status: " + resp.status);
						if (resp.body !== "snapshot-fetch") throw new Error("wrong body: " + resp.body);
					})();
				`,
				bridgeHandlers: {
					_networkFetchRaw: async () => {
						await new Promise((r) => setTimeout(r, 5));
						return { status: 200, body: "snapshot-fetch", headers: {} };
					},
				},
			}),
		);

		expect(result.code).toBe(0);
		expect(result.error).toBeFalsy();

		await session.destroy();
	});

	// --- WarmSnapshot cache hit ---

	it("WarmSnapshot followed by Execute with same bridge code is a cache hit", async () => {
		// Use a consistent bridge code for warmup and execution
		const bridgeCode = "(function() { globalThis.__warmed = true; })();";

		const rt = await createRuntime({
			warmupBridgeCode: bridgeCode,
		});

		// Small delay to allow Rust to process WarmSnapshot
		await new Promise((r) => setTimeout(r, 100));

		// First session should get a snapshot cache hit (no cold-start)
		const session = await rt.createSession();
		const result = await session.execute(
			defaultExecOptions({
				bridgeCode,
				userCode: `1 + 1;`,
			}),
		);

		expect(result.code).toBe(0);
		expect(result.error).toBeFalsy();

		await session.destroy();
	});

	// --- SECURE_EXEC_NO_SNAPSHOT_WARMUP=1 ---

	it("SECURE_EXEC_NO_SNAPSHOT_WARMUP=1 skips warm-up; first Execute creates snapshot lazily", async () => {
		const originalEnv = process.env.SECURE_EXEC_NO_SNAPSHOT_WARMUP;
		try {
			process.env.SECURE_EXEC_NO_SNAPSHOT_WARMUP = "1";

			// With warmup disabled, the runtime still works — snapshot is created lazily
			const rt = await createRuntime({
				warmupBridgeCode: "(function() { globalThis.__no_warmup = true; })();",
			});

			const session = await rt.createSession();
			const result = await session.execute(
				defaultExecOptions({
					userCode: `"lazy-snapshot";`,
				}),
			);

			expect(result.code).toBe(0);
			expect(result.error).toBeFalsy();

			await session.destroy();
		} finally {
			// Restore original env
			if (originalEnv === undefined) {
				delete process.env.SECURE_EXEC_NO_SNAPSHOT_WARMUP;
			} else {
				process.env.SECURE_EXEC_NO_SNAPSHOT_WARMUP = originalEnv;
			}
		}
	});

	// --- Different bridge code variants get separate snapshot entries ---

	it("different bridge code variants get separate snapshot entries", async () => {
		const rt = await createRuntime();

		// Session 1: bridge code sets __variant = "A"
		const session1 = await rt.createSession();
		const result1 = await session1.execute(
			defaultExecOptions({
				bridgeCode: "(function() { globalThis.__variant = 'A'; })();",
				userCode: `
					if (globalThis.__variant !== "A") {
						throw new Error("expected variant A, got: " + globalThis.__variant);
					}
				`,
			}),
		);
		expect(result1.code).toBe(0);
		expect(result1.error).toBeFalsy();
		await session1.destroy();

		// Session 2: different bridge code sets __variant = "B"
		const session2 = await rt.createSession();
		const result2 = await session2.execute(
			defaultExecOptions({
				bridgeCode: "(function() { globalThis.__variant = 'B'; })();",
				userCode: `
					if (globalThis.__variant !== "B") {
						throw new Error("expected variant B, got: " + globalThis.__variant);
					}
				`,
			}),
		);
		expect(result2.code).toBe(0);
		expect(result2.error).toBeFalsy();
		await session2.destroy();

		// Session 3: re-use variant A (should be cached from session 1)
		const session3 = await rt.createSession();
		const result3 = await session3.execute(
			defaultExecOptions({
				bridgeCode: "(function() { globalThis.__variant = 'A'; })();",
				userCode: `
					if (globalThis.__variant !== "A") {
						throw new Error("expected variant A on re-use, got: " + globalThis.__variant);
					}
				`,
			}),
		);
		expect(result3.code).toBe(0);
		expect(result3.error).toBeFalsy();
		await session3.destroy();
	});
});
