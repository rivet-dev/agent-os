import { existsSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import {
	createInMemoryFileSystem,
	createKernel,
	createWasmVmRuntime,
	describeIf,
	type Kernel,
} from "@rivet-dev/agentos-test-harness";
import { afterEach, beforeEach, expect, it } from "vitest";

const COMMAND_DIR = fileURLToPath(new URL("../bin", import.meta.url));
const hasGetconf = existsSync(join(COMMAND_DIR, "getconf"));

describeIf(hasGetconf, "getconf command", { timeout: 30_000 }, () => {
	let kernel: Kernel | undefined;

	beforeEach(async () => {
		kernel = createKernel({ filesystem: createInMemoryFileSystem() });
		await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMAND_DIR] }));
	}, 60_000);

	afterEach(async () => {
		await kernel?.dispose();
		kernel = undefined;
	}, 60_000);

	async function run(variable: string) {
		if (!kernel) throw new Error("kernel not mounted");
		let stdout = "";
		let stderr = "";
		const process = kernel.spawn("getconf", [variable], {
			onStdout: (chunk) => {
				stdout += Buffer.from(chunk).toString("utf8");
			},
			onStderr: (chunk) => {
				stderr += Buffer.from(chunk).toString("utf8");
			},
		});
		return { exitCode: await process.wait(), stdout, stderr };
	}

	it.each(["PAGE_SIZE", "PAGESIZE", "_NPROCESSORS_CONF", "_NPROCESSORS_ONLN", "LONG_BIT", "ULONG_MAX"])(
		"reports %s as a positive integer",
		async (variable) => {
			const result = await run(variable);
			expect(result.exitCode, result.stderr).toBe(0);
			expect(result.stderr).toBe("");
			expect(result.stdout).toMatch(/^[1-9][0-9]*\n$/);
		},
	);
});
