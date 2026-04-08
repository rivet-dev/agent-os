import { afterEach, describe, expect, test } from "vitest";
import {
	AgentOs,
	createInMemoryFileSystem,
	createInMemoryLayerStore,
	createSnapshotExport,
} from "../src/index.js";

describe("mount integration", () => {
	let vm: AgentOs;

	afterEach(async () => {
		await vm.dispose();
	});

	test("create with memory mount", async () => {
		vm = await AgentOs.create({
			mounts: [{ path: "/data", driver: createInMemoryFileSystem() }],
		});
		expect(await vm.exists("/data")).toBe(true);
	});

	test("writeFile and readFile round-trip through mounted backend", async () => {
		vm = await AgentOs.create({
			mounts: [{ path: "/data", driver: createInMemoryFileSystem() }],
		});
		await vm.writeFile("/data/foo.txt", "hello mount");
		const data = await vm.readFile("/data/foo.txt");
		expect(new TextDecoder().decode(data)).toBe("hello mount");
	});

	test("create with declarative native memory mount config", async () => {
		vm = await AgentOs.create({
			mounts: [
				{
					path: "/native",
					plugin: {
						id: "memory",
						config: {},
					},
				},
			],
		});
		await vm.writeFile("/native/plugin.txt", "native mount");
		const data = await vm.readFile("/native/plugin.txt");
		expect(new TextDecoder().decode(data)).toBe("native mount");
	});

	test("root FS and mount are separate", async () => {
		vm = await AgentOs.create({
			mounts: [{ path: "/data", driver: createInMemoryFileSystem() }],
		});
		await vm.writeFile("/home/user/foo.txt", "root content");
		await vm.writeFile("/data/foo.txt", "mount content");

		const rootData = await vm.readFile("/home/user/foo.txt");
		const mountData = await vm.readFile("/data/foo.txt");
		expect(new TextDecoder().decode(rootData)).toBe("root content");
		expect(new TextDecoder().decode(mountData)).toBe("mount content");
	});

	test("runtime mountFs and unmountFs work", async () => {
		vm = await AgentOs.create();

		vm.mountFs("/mnt/dynamic", createInMemoryFileSystem());
		await vm.writeFile("/mnt/dynamic/test.txt", "dynamic");
		const data = await vm.readFile("/mnt/dynamic/test.txt");
		expect(new TextDecoder().decode(data)).toBe("dynamic");

		vm.unmountFs("/mnt/dynamic");
		await expect(vm.readFile("/mnt/dynamic/test.txt")).rejects.toThrow();
	});

	test("readdir('/') includes 'data' alongside standard POSIX dirs", async () => {
		vm = await AgentOs.create({
			mounts: [{ path: "/data", driver: createInMemoryFileSystem() }],
		});
		const entries = await vm.readdir("/");
		expect(entries).toContain("data");
		// Standard POSIX dirs should also be present
		expect(entries).toContain("tmp");
		expect(entries).toContain("home");
	});

	test("rename across mounts throws EXDEV", async () => {
		vm = await AgentOs.create({
			mounts: [{ path: "/data", driver: createInMemoryFileSystem() }],
		});
		await vm.writeFile("/data/cross.txt", "cross-mount");
		await expect(
			vm.move("/data/cross.txt", "/home/user/cross.txt"),
		).rejects.toThrow("EXDEV");
	});

	test("readOnly mount blocks writeFile with EROFS", async () => {
		vm = await AgentOs.create({
			mounts: [
				{ path: "/ro", driver: createInMemoryFileSystem(), readOnly: true },
			],
		});
		await expect(
			vm.writeFile("/ro/blocked.txt", "should fail"),
		).rejects.toThrow("EROFS");
	});

	test("declarative overlay mounts create an isolated writable upper", async () => {
		const store = createInMemoryLayerStore();
		const lower = await store.importSnapshot({
			kind: "snapshot-export",
			source: createSnapshotExport([
				{
					path: "/",
					type: "directory",
					mode: "0755",
					uid: 0,
					gid: 0,
				},
				{
					path: "/seed.txt",
					type: "file",
					mode: "0644",
					uid: 0,
					gid: 0,
					content: Buffer.from("seeded").toString("base64"),
					encoding: "base64",
				},
			]).source,
		});

		vm = await AgentOs.create({
			mounts: [
				{
					path: "/data",
					filesystem: {
						type: "overlay",
						store,
						lowers: [lower],
					},
				},
			],
		});

		expect(new TextDecoder().decode(await vm.readFile("/data/seed.txt"))).toBe(
			"seeded",
		);
		await vm.writeFile("/data/new.txt", "overlay mount");
		expect(new TextDecoder().decode(await vm.readFile("/data/new.txt"))).toBe(
			"overlay mount",
		);
	});

	test("read-only overlay mounts reject writes", async () => {
		const store = createInMemoryLayerStore();
		const lower = await store.importSnapshot({
			kind: "snapshot-export",
			source: createSnapshotExport([
				{
					path: "/",
					type: "directory",
					mode: "0755",
					uid: 0,
					gid: 0,
				},
			]).source,
		});

		vm = await AgentOs.create({
			mounts: [
				{
					path: "/data",
					filesystem: {
						type: "overlay",
						store,
						mode: "read-only",
						lowers: [lower],
					},
				},
			],
		});

		await expect(
			vm.writeFile("/data/blocked.txt", "should fail"),
		).rejects.toThrow("EROFS");
	});
});
