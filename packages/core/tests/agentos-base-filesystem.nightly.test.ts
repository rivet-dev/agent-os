import {
	mkdirSync,
	mkdtempSync,
	rmSync,
	symlinkSync,
	writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import coreutils from "@agentos-software/coreutils";
import { afterEach, beforeEach, describe, expect, test } from "vitest";
import { AgentOs } from "../src/agent-os.js";
import { getBaseEnvironment } from "../src/base-filesystem.js";
import type { VirtualFileSystem } from "../src/runtime-compat.js";
import { getAgentOsKernel } from "../src/test/runtime.js";

describe("AgentOs base filesystem", () => {
	let vm: AgentOs;
	const textDecoder = new TextDecoder();

	function getKernelVfs(targetVm: AgentOs): VirtualFileSystem {
		return (getAgentOsKernel(targetVm) as unknown as { vfs: VirtualFileSystem })
			.vfs;
	}

	beforeEach(async () => {
		vm = await AgentOs.create();
	});

	afterEach(async () => {
		await vm.dispose();
	});

	test("default environment matches the base environment", () => {
		const kernel = getAgentOsKernel(vm);
		expect(kernel.env).toEqual(getBaseEnvironment());
		expect((kernel as unknown as { cwd: string }).cwd).toBe("/workspace");
	});

	test("overlay writes and deletes do not mutate the shared base layer", async () => {
		const baselineProfile = textDecoder.decode(
			await vm.readFile("/etc/profile"),
		);

		await vm.writeFile("/tmp/overlay-only.txt", "overlay data");
		await vm.remove("/etc/profile");

		expect(textDecoder.decode(await vm.readFile("/tmp/overlay-only.txt"))).toBe(
			"overlay data",
		);
		await expect(vm.readFile("/etc/profile")).rejects.toThrow("ENOENT");

		const secondVm = await AgentOs.create();
		try {
			expect(await secondVm.exists("/tmp/overlay-only.txt")).toBe(false);
			expect(textDecoder.decode(await secondVm.readFile("/etc/profile"))).toBe(
				baselineProfile,
			);
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
			vm.writeFile("/home/agentos/blocked.txt", "blocked"),
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
		expect(await vm.exists("/bin/python3")).toBe(true);
		await expect(vm.writeFile("/tmp/blocked.txt", "blocked")).rejects.toThrow(
			"EROFS",
		);
	});

	test("read-only roots preseed WASM command stubs before runtime mount", async () => {
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
	});

	test("read-only roots preserve software-declared alias commands on the sidecar path", async () => {
		const packageDir = mkdtempSync(join(tmpdir(), "agentos-command-fixture-"));
		try {
			const binDir = join(packageDir, "bin");
			mkdirSync(binDir);
			writeFileSync(
				join(packageDir, "agentos-package.json"),
				JSON.stringify({
					name: "fixture-package",
					version: "1.0.0",
				}),
			);
			writeFileSync(
				join(binDir, "fixture"),
				new Uint8Array([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]),
			);
			symlinkSync("fixture", join(binDir, "fixture-alias"));

			await vm.dispose();
			vm = await AgentOs.create({
				software: [packageDir],
				rootFilesystem: {
					mode: "read-only",
					disableDefaultBaseLayer: true,
				},
			});

			expect(await vm.exists("/bin/fixture")).toBe(true);
			expect(await vm.exists("/bin/fixture-alias")).toBe(true);

			const kernel = getAgentOsKernel(vm);
			expect(kernel.commands.get("fixture")).toBe("wasmvm");
			expect(kernel.commands.get("fixture-alias")).toBe("wasmvm");
		} finally {
			rmSync(packageDir, { recursive: true, force: true });
		}
	});

	test("native sidecar filesystem exposes realpath, hard links, truncate, and utimes", async () => {
		const vfs = getKernelVfs(vm);
		await vm.writeFile("/tmp/original.txt", "hello world");
		await vfs.link("/tmp/original.txt", "/tmp/linked.txt");

		const linkedStat = await vm.stat("/tmp/linked.txt");
		expect(linkedStat.nlink).toBeGreaterThanOrEqual(2);
		expect(textDecoder.decode(await vm.readFile("/tmp/linked.txt"))).toBe(
			"hello world",
		);

		await vfs.truncate("/tmp/linked.txt", 5);
		expect(textDecoder.decode(await vm.readFile("/tmp/original.txt"))).toBe(
			"hello",
		);

		const atime = 1_700_000_000_000;
		const mtime = 1_710_000_000_000;
		await vfs.utimes("/tmp/original.txt", atime, mtime);
		const updatedStat = await vm.stat("/tmp/original.txt");
		expect(updatedStat.atimeMs).toBe(atime);
		expect(updatedStat.mtimeMs).toBe(mtime);

		await vfs.symlink("/tmp/original.txt", "/tmp/alias.txt");
		expect(await vfs.realpath("/tmp/alias.txt")).toBe("/tmp/original.txt");

		await vm.remove("/tmp/original.txt");
		expect(textDecoder.decode(await vm.readFile("/tmp/linked.txt"))).toBe(
			"hello",
		);
	});

	test("snapshotRootFilesystem exports a reusable lower snapshot", async () => {
		await vm.writeFile("/home/agentos/snap.txt", "snapshotted");
		const snapshot = await vm.exportRootFilesystem({
			maxBytes: 64 * 1024 * 1024,
		});

		const secondVm = await AgentOs.create({
			rootFilesystem: {
				disableDefaultBaseLayer: true,
				lowers: [snapshot],
			},
		});
		try {
			expect(
				textDecoder.decode(await secondVm.readFile("/home/agentos/snap.txt")),
			).toBe("snapshotted");
			expect(textDecoder.decode(await secondVm.readFile("/etc/profile"))).toBe(
				textDecoder.decode(await vm.readFile("/etc/profile")),
			);
		} finally {
			await secondVm.dispose();
		}
	});
});
