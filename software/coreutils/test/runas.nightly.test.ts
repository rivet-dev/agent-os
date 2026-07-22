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

describeIf(hasWasmBinaries, "runas", () => {
	let kernel: Kernel | undefined;

	afterEach(async () => {
		await kernel?.dispose();
		kernel = undefined;
	});

	it("resolves bare Rust child commands through PATH after changing identity", async () => {
		kernel = createKernel({
			filesystem: createInMemoryFileSystem(),
			user: {
				uid: 0,
				gid: 0,
				euid: 0,
				egid: 0,
				username: "root",
				homedir: "/root",
				supplementaryGids: [0],
				accounts: [
					{
						uid: 1000,
						gid: 1000,
						username: "agentos",
						homedir: "/home/agentos",
						shell: "/bin/sh",
						supplementaryGids: [1000],
					},
				],
			},
		});
		await kernel.mount(createWasmVmRuntime({ commandDirs: [COMMANDS_DIR] }));

		const direct = await kernel.exec("runas -u 1000 -g 1000 -- id -u");
		expect(direct.exitCode, direct.stderr).toBe(0);
		expect(direct.stdout).toBe("1000\n");

		const nested = await kernel.exec(
			"runas -u 1000 -g 1000 -- sh -c 'id -u'",
		);
		expect(nested.exitCode, nested.stderr).toBe(0);
		expect(nested.stdout).toBe("1000\n");
	}, 30_000);
});
