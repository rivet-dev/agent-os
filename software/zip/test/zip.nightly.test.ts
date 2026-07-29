// Nightly: requires a non-core registry command.
/**
 * Package-owned integration tests for the zip command.
 */

import { afterEach, expect, it } from "vitest";
import {
	C_BUILD_DIR,
	COMMANDS_DIR,
	createInMemoryFileSystem,
	createKernel,
	createWasmVmRuntime,
	describeIf,
	hasCWasmBinaries,
	type Kernel,
} from "@rivet-dev/agentos-test-harness";

describeIf(hasCWasmBinaries("zip"), "zip command", { timeout: 10_000 }, () => {
	let kernel: Kernel;

	afterEach(async () => {
		await kernel?.dispose();
	});

	it("creates a zip archive for a single file", async () => {
		const vfs = createInMemoryFileSystem();
		await vfs.writeFile("/hello.txt", "Hello, World!\n");

		kernel = createKernel({ filesystem: vfs });
		await kernel.mount(
			createWasmVmRuntime({ commandDirs: [C_BUILD_DIR, COMMANDS_DIR] }),
		);

		const result = await kernel.exec("zip /archive.zip /hello.txt");
		expect(result.exitCode, result.stderr).toBe(0);

		const archive = await vfs.readFile("/archive.zip");
		expect(archive.length).toBeGreaterThan(0);
		expect(Array.from(archive.slice(0, 2))).toEqual([0x50, 0x4b]);
	});

	it("creates a zip archive recursively", async () => {
		const vfs = createInMemoryFileSystem();
		await vfs.mkdir("/mydir");
		await vfs.writeFile("/mydir/a.txt", "file a\n");
		await vfs.writeFile("/mydir/b.txt", "file b\n");

		kernel = createKernel({ filesystem: vfs });
		await kernel.mount(
			createWasmVmRuntime({ commandDirs: [C_BUILD_DIR, COMMANDS_DIR] }),
		);

		const result = await kernel.exec("zip -r /dir.zip /mydir");
		expect(result.exitCode, result.stderr).toBe(0);
		expect(await vfs.exists("/dir.zip")).toBe(true);
	});
});
