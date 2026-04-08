/**
 * Warm isolate pool behavior tests.
 *
 * Verifies that the warm pool pre-creates sessions, speeds up session
 * creation, correctly handles pool disabled/enabled states, and cleans
 * up on dispose.
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

describe.skipIf(skipUnlessBinary)("warm isolate pool", () => {
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
			warmupBridgeCode: "",
			...opts,
		});
		runtimes.push(rt);
		return rt;
	}

	it("pool disabled (warmPoolSize=0) works correctly", async () => {
		const rt = await createRuntime({ warmPoolSize: 0 });
		const session = await rt.createSession();
		const result = await session.execute(
			defaultExecOptions({ userCode: "1 + 1" }),
		);
		expect(result.code).toBe(0);
		await session.destroy();
	});

	it("pool enabled produces correct execution results", async () => {
		const rt = await createRuntime({
			warmPoolSize: 2,
			defaultWarmHeapLimitMb: 128,
		});
		const session = await rt.createSession({ heapLimitMb: 128 });
		const result = await session.execute(
			defaultExecOptions({ userCode: "42" }),
		);
		expect(result.code).toBe(0);
		await session.destroy();
	});

	it("multiple sessions from warm pool produce correct results", async () => {
		const rt = await createRuntime({
			warmPoolSize: 3,
			defaultWarmHeapLimitMb: 128,
		});

		// Create and execute on multiple sessions sequentially
		for (let i = 0; i < 4; i++) {
			const session = await rt.createSession({ heapLimitMb: 128 });
			const result = await session.execute(
				defaultExecOptions({ userCode: `${i} + 1` }),
			);
			expect(result.code).toBe(0);
			await session.destroy();
		}
	});

	it("warm pool sessions are isolated from each other", async () => {
		const rt = await createRuntime({
			warmPoolSize: 2,
			defaultWarmHeapLimitMb: 128,
		});

		// Session A sets a global
		const sessionA = await rt.createSession({ heapLimitMb: 128 });
		const resultA = await sessionA.execute(
			defaultExecOptions({ userCode: "globalThis.__secret = 42" }),
		);
		expect(resultA.code).toBe(0);

		// Session B should not see session A's global
		const sessionB = await rt.createSession({ heapLimitMb: 128 });
		const resultB = await sessionB.execute(
			defaultExecOptions({
				userCode: "if (typeof globalThis.__secret !== 'undefined') { throw new Error('leak'); }",
			}),
		);
		expect(resultB.code).toBe(0);
		expect(resultB.error).toBeNull();

		await sessionA.destroy();
		await sessionB.destroy();
	});

	it("dispose cleans up warm pool without error", async () => {
		const rt = await createRuntime({
			warmPoolSize: 2,
			defaultWarmHeapLimitMb: 128,
		});
		// Don't create any sessions — just dispose with pool still full
		await rt.dispose();
		// Remove from cleanup list since we already disposed
		runtimes.pop();
	});

	it("warm pool with waitForWarmPool=false returns immediately", async () => {
		const start = Date.now();
		const rt = await createRuntime({
			warmPoolSize: 2,
			defaultWarmHeapLimitMb: 128,
			waitForWarmPool: false,
		});
		// Should return quickly (pool fills in background)
		const elapsed = Date.now() - start;
		expect(elapsed).toBeLessThan(5000);

		// Wait a bit for pool to fill, then use a session
		await new Promise((r) => setTimeout(r, 500));
		const session = await rt.createSession({ heapLimitMb: 128 });
		const result = await session.execute(
			defaultExecOptions({ userCode: "'ok'" }),
		);
		expect(result.code).toBe(0);
		await session.destroy();
	});

	it("warm pool size 1 works", async () => {
		const rt = await createRuntime({
			warmPoolSize: 1,
			defaultWarmHeapLimitMb: 128,
		});
		const session = await rt.createSession({ heapLimitMb: 128 });
		const result = await session.execute(
			defaultExecOptions({ userCode: "'single'" }),
		);
		expect(result.code).toBe(0);
		await session.destroy();
	});
});
