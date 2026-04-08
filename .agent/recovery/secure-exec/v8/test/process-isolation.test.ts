/**
 * Process isolation integration tests for the V8 runtime.
 *
 * Proves that two separate V8Runtime instances (separate OS processes) are
 * crash-isolated: a crash in one does not affect the other. Also verifies
 * that runtimes sharing the same V8Runtime handle share crash fate.
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

describe.skipIf(skipUnlessBinary)("V8 process isolation", () => {
	const runtimes: V8Runtime[] = [];

	afterEach(async () => {
		// Dispose all runtimes created during the test
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

	// --- Cross-process crash isolation ---

	it("crash in one V8Runtime does not affect another", async () => {
		// Create two separate V8Runtime instances (two OS processes)
		const rtCrash = await createRuntime();
		const rtHealthy = await createRuntime();

		// Create sessions on each runtime
		const sessionCrash = await rtCrash.createSession({ heapLimitMb: 8 });
		const sessionHealthy = await rtHealthy.createSession();

		// Start both executions concurrently:
		// - crash runtime: allocate until OOM
		// - healthy runtime: simple computation
		const [crashResult, healthyResult] = await Promise.all([
			sessionCrash.execute(
				defaultExecOptions({
					userCode: `
						const arrays = [];
						while (true) {
							arrays.push(new Array(1024 * 1024).fill(42));
						}
					`,
				}),
			),
			sessionHealthy.execute(
				defaultExecOptions({
					userCode: `1 + 1;`,
				}),
			),
		]);

		// Crashed runtime should report an error
		expect(crashResult.code).not.toBe(0);
		expect(crashResult.error).toBeTruthy();

		// Healthy runtime should complete successfully
		expect(healthyResult.code).toBe(0);
		expect(healthyResult.error).toBeFalsy();

		await sessionHealthy.destroy();
	});

	it("crashed runtime reports ERR_V8_PROCESS_CRASH while healthy runtime returns exit code 0", async () => {
		const rtCrash = await createRuntime();
		const rtHealthy = await createRuntime();

		const sessionCrash = await rtCrash.createSession({ heapLimitMb: 8 });
		const sessionHealthy = await rtHealthy.createSession();

		// Start OOM on crash runtime, simple code on healthy runtime
		const [crashResult, healthyResult] = await Promise.all([
			sessionCrash.execute(
				defaultExecOptions({
					userCode: `
						const leak = [];
						for (let i = 0; i < 1e9; i++) {
							leak.push(new ArrayBuffer(1024 * 1024));
						}
					`,
				}),
			),
			sessionHealthy.execute(
				defaultExecOptions({
					userCode: `42;`,
				}),
			),
		]);

		// Crashed runtime: error with process crash code
		expect(crashResult.code).toBe(1);
		expect(crashResult.error).toBeTruthy();
		expect(crashResult.error!.code).toBe("ERR_V8_PROCESS_CRASH");

		// Healthy runtime: clean exit
		expect(healthyResult.code).toBe(0);
		expect(healthyResult.error).toBeFalsy();

		await sessionHealthy.destroy();
	});

	it("healthy runtime can create new sessions after peer runtime crashes", async () => {
		const rtCrash = await createRuntime();
		const rtHealthy = await createRuntime();

		// Crash one runtime
		const sessionCrash = await rtCrash.createSession({ heapLimitMb: 8 });
		const crashResult = await sessionCrash.execute(
			defaultExecOptions({
				userCode: `
					const arrays = [];
					while (true) {
						arrays.push(new Array(1024 * 1024).fill(42));
					}
				`,
			}),
		);
		expect(crashResult.error).toBeTruthy();

		// Healthy runtime should still work — create a new session
		const session2 = await rtHealthy.createSession();
		const result = await session2.execute(
			defaultExecOptions({ userCode: `"hello";` }),
		);

		expect(result.code).toBe(0);
		expect(result.error).toBeFalsy();

		await session2.destroy();
	});

	// --- Shared crash fate ---

	it("runtimes using the same v8Runtime handle share crash fate", async () => {
		// Create a single V8Runtime process
		const sharedProcess = await createRuntime();

		// Create two sessions on the same process
		const session1 = await sharedProcess.createSession({ heapLimitMb: 8 });
		const session2 = await sharedProcess.createSession();

		// Trigger OOM in session1 — this kills the process, affecting session2 too
		const [result1, result2] = await Promise.all([
			session1.execute(
				defaultExecOptions({
					userCode: `
						const arrays = [];
						while (true) {
							arrays.push(new Array(1024 * 1024).fill(42));
						}
					`,
				}),
			),
			session2.execute(
				defaultExecOptions({
					userCode: `
						// Slow code to ensure it's still running when OOM kills the process
						let sum = 0;
						for (let i = 0; i < 1e9; i++) { sum += i; }
						sum;
					`,
				}),
			),
		]);

		// Both sessions should have errors — they share the crashed process
		expect(result1.code).not.toBe(0);
		expect(result1.error).toBeTruthy();

		expect(result2.code).not.toBe(0);
		expect(result2.error).toBeTruthy();
	});

	it("multiple sessions on same process all get ERR_V8_PROCESS_CRASH on OOM", async () => {
		const sharedProcess = await createRuntime();

		// Create three sessions
		const sessions = await Promise.all([
			sharedProcess.createSession({ heapLimitMb: 8 }),
			sharedProcess.createSession(),
			sharedProcess.createSession(),
		]);

		// OOM in first session; other sessions block on a sync bridge call
		// that never returns — ensuring they're still running when OOM kills the process
		const neverResolve = () => new Promise(() => {});
		const results = await Promise.all([
			sessions[0].execute(
				defaultExecOptions({
					userCode: `
						const leak = [];
						while (true) { leak.push(new ArrayBuffer(1024 * 1024)); }
					`,
				}),
			),
			sessions[1].execute(
				defaultExecOptions({
					userCode: `_block();`,
					bridgeHandlers: { _block: neverResolve },
				}),
			),
			sessions[2].execute(
				defaultExecOptions({
					userCode: `_block();`,
					bridgeHandlers: { _block: neverResolve },
				}),
			),
		]);

		// All should report errors (process crashed)
		for (const result of results) {
			expect(result.code).not.toBe(0);
			expect(result.error).toBeTruthy();
		}
	});

	// --- Isolation with separate processes ---

	it("separate V8Runtime processes have independent session namespaces", async () => {
		const rt1 = await createRuntime();
		const rt2 = await createRuntime();

		// Both runtimes can run sessions independently
		const session1 = await rt1.createSession();
		const session2 = await rt2.createSession();

		const [result1, result2] = await Promise.all([
			session1.execute(
				defaultExecOptions({ userCode: `"from-rt1";` }),
			),
			session2.execute(
				defaultExecOptions({ userCode: `"from-rt2";` }),
			),
		]);

		expect(result1.code).toBe(0);
		expect(result2.code).toBe(0);

		await Promise.all([session1.destroy(), session2.destroy()]);
	});
});
