/**
 * Crash isolation tests for the V8 runtime.
 *
 * Proves that V8 OOM kills only the child process (not the host),
 * that timeout termination works for infinite loops, and that
 * SIGKILL on the child process succeeds as a last resort.
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

describe.skipIf(skipUnlessBinary)("V8 crash isolation", () => {
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

	// --- OOM crash isolation ---

	it("OOM in sandbox kills child process but host survives", async () => {
		const rt = await createRuntime();
		const session = await rt.createSession({ heapLimitMb: 8 });

		// Allocate until OOM — V8 aborts the Rust process
		const result = await session.execute(
			defaultExecOptions({
				userCode: `
					const arrays = [];
					while (true) {
						arrays.push(new Array(1024 * 1024).fill(42));
					}
				`,
			}),
		);

		// Host process is still alive — we got a result, not a crash
		expect(result.code).not.toBe(0);
		expect(result.error).toBeTruthy();

		// Runtime is no longer usable after child crash
		await rt.dispose();
		runtime = null;
	});

	it("OOM error is surfaced as ExecutionResult, not host crash", async () => {
		const rt = await createRuntime();
		const session = await rt.createSession({ heapLimitMb: 8 });

		const result = await session.execute(
			defaultExecOptions({
				userCode: `
					const leak = [];
					for (let i = 0; i < 1e9; i++) {
						leak.push(new ArrayBuffer(1024 * 1024));
					}
				`,
			}),
		);

		expect(result.error).toBeTruthy();
		expect(result.code).toBe(1);
		// Error should indicate a crash or process exit
		expect(result.error!.message).toBeTruthy();

		await rt.dispose();
		runtime = null;
	});

	// --- Timeout termination ---

	it("infinite loop is terminated by timeout", async () => {
		const rt = await createRuntime();
		const session = await rt.createSession({ cpuTimeLimitMs: 500 });

		const start = Date.now();
		const result = await session.execute(
			defaultExecOptions({
				userCode: "while (true) {}",
			}),
		);
		const elapsed = Date.now() - start;

		expect(result.code).toBe(1);
		expect(result.error).toBeTruthy();
		expect(result.error!.message).toContain("timed out");
		expect(result.error!.code).toBe("ERR_SCRIPT_EXECUTION_TIMEOUT");

		// Should have terminated roughly around the timeout
		expect(elapsed).toBeLessThan(5000);

		await session.destroy();
	});

	it("timeout terminates sync bridge call blocked on host", async () => {
		const rt = await createRuntime();
		const session = await rt.createSession({ cpuTimeLimitMs: 500 });

		const result = await session.execute(
			defaultExecOptions({
				userCode: `
					// _fsReadFile is a registered sync bridge function
					// The host handler sleeps longer than the timeout
					_fsReadFile("/slow-file", "utf8");
				`,
				bridgeHandlers: {
					_fsReadFile: () => {
						// Simulate a bridge call that takes longer than timeout
						return new Promise((resolve) =>
							setTimeout(resolve, 10000),
						);
					},
				},
			}),
		);

		// Timeout should have fired and terminated execution
		expect(result.code).toBe(1);
		expect(result.error).toBeTruthy();

		// Dispose (child may have been terminated)
		await rt.dispose();
		runtime = null;
	});

	it("runtime remains usable after timeout in one session", async () => {
		const rt = await createRuntime();

		// First session — times out
		const session1 = await rt.createSession({ cpuTimeLimitMs: 500 });
		const result1 = await session1.execute(
			defaultExecOptions({ userCode: "while (true) {}" }),
		);
		expect(result1.error).toBeTruthy();
		expect(result1.error!.code).toBe("ERR_SCRIPT_EXECUTION_TIMEOUT");
		await session1.destroy();

		// Second session — should work normally
		const session2 = await rt.createSession();
		const result2 = await session2.execute(
			defaultExecOptions({ userCode: "1 + 1;" }),
		);
		expect(result2.code).toBe(0);
		expect(result2.error).toBeFalsy();
		await session2.destroy();
	});

	// --- SIGKILL as last resort ---

	it("child process can be killed via dispose when stuck", async () => {
		const rt = await createRuntime();
		const session = await rt.createSession();

		// Start an execution (no timeout) and immediately dispose
		// The execution will never complete normally
		const execPromise = session.execute(
			defaultExecOptions({
				userCode: `
					// Spin forever with a sync bridge call to block the thread
					while (true) {
						_spin();
					}
				`,
				bridgeHandlers: {
					_spin: () => {
						// Return quickly so V8 keeps looping
					},
				},
			}),
		);

		// Give V8 a moment to start executing
		await new Promise((r) => setTimeout(r, 200));

		// Dispose kills the child process (SIGTERM then SIGKILL after 5s)
		await rt.dispose();
		runtime = null;

		// The execute promise should resolve with an error (not hang)
		const result = await execPromise;
		expect(result.code).toBe(1);
		expect(result.error).toBeTruthy();
	});
});
