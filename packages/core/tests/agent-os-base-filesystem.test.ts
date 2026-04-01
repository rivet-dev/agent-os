import { afterEach, beforeEach, describe, expect, test } from "vitest";
import coreutils from "@rivet-dev/agent-os-coreutils";
import { AgentOs } from "../src/agent-os.js";
import {
	getBaseEnvironment,
	getBaseFilesystemEntries,
} from "../src/base-filesystem.js";
import { hasRegistryCommands } from "./helpers/registry-commands.js";

describe("AgentOs base filesystem", () => {
	let vm: AgentOs;
	const textDecoder = new TextDecoder();

	beforeEach(async () => {
		vm = await AgentOs.create();
	});

	afterEach(async () => {
		await vm.dispose();
	});

	test("default environment matches the base environment", () => {
		expect(vm.kernel.env).toEqual(getBaseEnvironment());
		expect((vm.kernel as unknown as { cwd: string }).cwd).toBe("/home/user");
	});

	test("default filesystem matches the base layer", async () => {
		const vfs = (vm.kernel as unknown as {
			vfs: {
				lstat: (path: string) => Promise<{
					mode: number;
					uid: number;
					gid: number;
					isDirectory: boolean;
					isSymbolicLink: boolean;
				}>;
				readlink: (path: string) => Promise<string>;
			};
		}).vfs;

		for (const entry of getBaseFilesystemEntries()) {
			if (entry.type === "symlink") {
				const stat = await vfs.lstat(entry.path);
				expect(stat.isSymbolicLink).toBe(true);
				expect(stat.isDirectory).toBe(false);
				expect(stat.mode & 0o7777).toBe(Number.parseInt(entry.mode, 8));
				expect(stat.uid).toBe(entry.uid);
				expect(stat.gid).toBe(entry.gid);
				expect(await vfs.readlink(entry.path)).toBe(entry.target);
				continue;
			}

			const stat = await vm.stat(entry.path);
			expect(stat.isDirectory).toBe(entry.type === "directory");
			expect(stat.isSymbolicLink).toBe(false);
			expect(stat.mode & 0o7777).toBe(Number.parseInt(entry.mode, 8));
			expect(stat.uid).toBe(entry.uid);
			expect(stat.gid).toBe(entry.gid);

			if (entry.type === "file" && entry.content !== undefined) {
				const data = await vm.readFile(entry.path);
				expect(textDecoder.decode(data)).toBe(entry.content);
			}
		}
	});

	test("overlay writes and deletes do not mutate the shared base layer", async () => {
		const baselineProfile = textDecoder.decode(
			await vm.readFile("/etc/profile"),
		);

		await vm.writeFile("/tmp/overlay-only.txt", "overlay data");
		await vm.delete("/etc/profile");

		expect(textDecoder.decode(await vm.readFile("/tmp/overlay-only.txt"))).toBe(
			"overlay data",
		);
		await expect(vm.readFile("/etc/profile")).rejects.toThrow("ENOENT");

		const secondVm = await AgentOs.create();
		try {
			expect(await secondVm.exists("/tmp/overlay-only.txt")).toBe(false);
			expect(
				textDecoder.decode(await secondVm.readFile("/etc/profile")),
			).toBe(baselineProfile);
		} finally {
			await secondVm.dispose();
		}
	});

	test("rootFilesystem can disable the bundled base layer", async () => {
		await vm.dispose();
		vm = await AgentOs.create({
			rootFilesystem: {
				disableDefaultBaseLayer: true,
			},
		});

		await expect(vm.readFile("/etc/profile")).rejects.toThrow("ENOENT");
		await vm.mkdir("/work");
		await vm.writeFile("/work/hello.txt", "from empty root");
		expect(textDecoder.decode(await vm.readFile("/work/hello.txt"))).toBe(
			"from empty root",
		);
	});

	test("read-only roots expose lowers but reject writes", async () => {
		await vm.dispose();
		vm = await AgentOs.create({
			rootFilesystem: {
				mode: "read-only",
			},
		});

		expect(textDecoder.decode(await vm.readFile("/etc/profile"))).toContain(
			"PATH",
		);
		await expect(
			vm.writeFile("/home/user/blocked.txt", "blocked"),
		).rejects.toThrow("EROFS");
	});

	test("read-only roots can boot from a preseeded lower without a writable upper", async () => {
		await vm.dispose();
		vm = await AgentOs.create({
			rootFilesystem: {
				mode: "read-only",
				disableDefaultBaseLayer: true,
			},
		});

		expect(await vm.exists("/boot")).toBe(true);
		expect(await vm.exists("/usr/bin/env")).toBe(true);
		expect(await vm.exists("/bin/node")).toBe(true);
		expect(await vm.exists("/bin/python")).toBe(true);
		await expect(
			vm.writeFile("/tmp/blocked.txt", "blocked"),
		).rejects.toThrow("EROFS");
	});

	test.skipIf(!hasRegistryCommands)(
		"read-only roots preseed WASM command stubs before runtime mount",
		async () => {
			await vm.dispose();
			vm = await AgentOs.create({
				software: [coreutils],
				rootFilesystem: {
					mode: "read-only",
					disableDefaultBaseLayer: true,
				},
			});

			expect(await vm.exists("/bin/sh")).toBe(true);
			expect(await vm.exists("/bin/ls")).toBe(true);
			expect(await vm.exists("/bin/env")).toBe(true);
		},
	);

	test("snapshotRootFilesystem exports a reusable lower snapshot", async () => {
		await vm.writeFile("/home/user/snap.txt", "snapshotted");
		const snapshot = await vm.snapshotRootFilesystem();

		const secondVm = await AgentOs.create({
			rootFilesystem: {
				disableDefaultBaseLayer: true,
				lowers: [snapshot],
			},
		});
		try {
			expect(
				textDecoder.decode(await secondVm.readFile("/home/user/snap.txt")),
			).toBe("snapshotted");
			expect(
				textDecoder.decode(await secondVm.readFile("/etc/profile")),
			).toBe(textDecoder.decode(await vm.readFile("/etc/profile")));
		} finally {
			await secondVm.dispose();
		}
	});
});
