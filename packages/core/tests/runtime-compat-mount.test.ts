import { afterEach, describe, expect, test } from "vitest";
import {
	createKernel,
	createNodeRuntime,
	type Kernel,
} from "../src/runtime-compat.js";
import { createInMemoryFileSystem } from "../src/test/runtime.js";

describe("runtime-compat mountFs bookkeeping", () => {
	let kernel: Kernel | undefined;

	afterEach(async () => {
		await kernel?.dispose();
		kernel = undefined;
	});

	test("unmountFs cancels a queued mount before kernel initialization", async () => {
		const mounted = createInMemoryFileSystem();
		await mounted.writeFile("/file.txt", "should not be visible");

		kernel = createKernel({
			filesystem: createInMemoryFileSystem(),
		});
		await kernel.mountFs("/queued", mounted);
		await kernel.unmountFs("/queued");

		await expect(kernel.readFile("/queued/file.txt")).rejects.toThrow();
	});

	test("dispose does not copy an active mount into the bound filesystem", async () => {
		const root = createInMemoryFileSystem();
		await root.mkdir("/root");
		const mounted = createInMemoryFileSystem();
		await mounted.mkdir("/package");
		await mounted.writeFile("/package/index.js", "mounted");

		kernel = createKernel({ filesystem: root });
		await kernel.mountFs("/root/node_modules", mounted, { readOnly: true });
		await kernel.mount(createNodeRuntime());
		await kernel.dispose();
		kernel = undefined;

		await expect(root.exists("/root/node_modules")).resolves.toBe(false);
	});
});
