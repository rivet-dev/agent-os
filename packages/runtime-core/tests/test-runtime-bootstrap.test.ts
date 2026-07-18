import { resolve } from "node:path";
import { afterEach, describe, expect, test } from "vitest";
import {
	allowAll,
	createInMemoryFileSystem,
	createKernel,
	createWasmVmRuntime,
	type Kernel,
} from "../src/test-runtime.js";

describe("test runtime bootstrap ownership", () => {
	let kernel: Kernel | undefined;

	afterEach(async () => {
		await kernel?.dispose();
		kernel = undefined;
	});

	test("matches production ownership for guest-writable home and workspace", async () => {
		const filesystem = createInMemoryFileSystem();
		kernel = createKernel({ filesystem, permissions: allowAll });
		await kernel.mount(
			createWasmVmRuntime({
				commandDirs: [resolve(import.meta.dirname, "../commands")],
			}),
		);

		const root = await filesystem.stat("/");
		const home = await filesystem.stat("/home/agentos");
		const workspace = await filesystem.stat("/workspace");

		expect({ uid: root.uid, gid: root.gid, mode: root.mode & 0o7777 }).toEqual({
			uid: 0,
			gid: 0,
			mode: 0o755,
		});
		expect({ uid: home.uid, gid: home.gid, mode: home.mode & 0o7777 }).toEqual({
			uid: 1000,
			gid: 1000,
			mode: 0o2755,
		});
		expect({
			uid: workspace.uid,
			gid: workspace.gid,
			mode: workspace.mode & 0o7777,
		}).toEqual({ uid: 1000, gid: 1000, mode: 0o755 });
	});
});
