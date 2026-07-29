// Nightly: requires a non-core registry command.
/**
 * Package-owned integration tests for GNU findutils commands.
 */

import { afterEach, expect, it } from "vitest";
import {
	COMMANDS_DIR,
	createInMemoryFileSystem,
	createKernel,
	createWasmVmRuntime,
	describeIf,
	hasWasmBinaries,
	type Kernel,
} from "@rivet-dev/agentos-test-harness";

function parseLines(stdout: string): string[] {
	return stdout
		.split("\n")
		.map((line) => line.trim())
		.filter(Boolean)
		.sort();
}

describeIf(hasWasmBinaries, "findutils commands", { timeout: 10_000 }, () => {
	let kernel: Kernel;

	afterEach(async () => {
		await kernel?.dispose();
	});

	it("find matches files by name", async () => {
		const vfs = createInMemoryFileSystem();
		await vfs.mkdir("/project/src", { recursive: true });
		await vfs.mkdir("/project/docs", { recursive: true });
		await vfs.writeFile("/project/src/main.js", "console.log('main')\n");
		await vfs.writeFile("/project/src/helper.ts", "export {}\n");
		await vfs.writeFile("/project/docs/readme.md", "# Readme\n");

		kernel = createKernel({ filesystem: vfs });
		await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

		const result = await kernel.exec("find /project -name '*.js'");
		expect(parseLines(result.stdout)).toEqual(["/project/src/main.js"]);
	});

	it("find filters directories", async () => {
		const vfs = createInMemoryFileSystem();
		await vfs.mkdir("/project/src", { recursive: true });
		await vfs.mkdir("/project/docs", { recursive: true });
		await vfs.writeFile("/project/src/main.js", "console.log('main')\n");

		kernel = createKernel({ filesystem: vfs });
		await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

		const result = await kernel.exec("find /project -type d");
		expect(parseLines(result.stdout)).toEqual([
			"/project",
			"/project/docs",
			"/project/src",
		]);
	});

	it("find supports depth limits", async () => {
		const vfs = createInMemoryFileSystem();
		await vfs.mkdir("/project/src/nested", { recursive: true });
		await vfs.mkdir("/project/docs", { recursive: true });
		await vfs.writeFile("/project/src/main.js", "console.log('main')\n");
		await vfs.writeFile("/project/src/nested/helper.ts", "export {}\n");
		await vfs.writeFile("/project/docs/readme.md", "# Readme\n");

		kernel = createKernel({ filesystem: vfs });
		await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

		const result = await kernel.exec(
			"find /project -mindepth 2 -maxdepth 2 -type f -name '*.js'",
		);
		expect(parseLines(result.stdout)).toEqual(["/project/src/main.js"]);
	});

	it("xargs passes stdin arguments to a command", async () => {
		const vfs = createInMemoryFileSystem();
		await vfs.writeFile("/args.txt", "alpha\nbeta\n");

		kernel = createKernel({ filesystem: vfs });
		await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

		const result = await kernel.exec("xargs echo < /args.txt");
		expect(result.stdout.trim()).toBe("alpha beta");
	});

	it("xargs batches arguments across spawned commands", async () => {
		const vfs = createInMemoryFileSystem();
		await vfs.writeFile("/args.txt", "one two three four five\n");

		kernel = createKernel({ filesystem: vfs });
		await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

		const result = await kernel.exec("xargs -n 2 echo < /args.txt");
		expect(result.stdout.trim().split("\n")).toEqual([
			"one two",
			"three four",
			"five",
		]);
	});
});
