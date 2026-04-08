import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { afterEach, beforeEach, describe, expect, test } from "vitest";
import { AgentOs, createHostDirBackend } from "../src/index.js";

describe("host_dir native mount integration", () => {
	let vm: AgentOs;
	let tmpDir: string;

	beforeEach(() => {
		tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "host-dir-test-"));
		fs.writeFileSync(path.join(tmpDir, "hello.txt"), "hello from host");
		fs.mkdirSync(path.join(tmpDir, "subdir"));
		fs.writeFileSync(
			path.join(tmpDir, "subdir", "nested.txt"),
			"nested content",
		);
	});

	afterEach(async () => {
		if (vm) await vm.dispose();
		fs.rmSync(tmpDir, { recursive: true, force: true });
	});

	test("path traversal attempt (../../etc/passwd) is blocked", async () => {
		vm = await AgentOs.create({
			mounts: [
				{
					path: "/hostmnt",
					plugin: createHostDirBackend({ hostPath: tmpDir }),
				},
			],
		});
		await expect(vm.readFile("/hostmnt/../../etc/passwd")).rejects.toThrow();
	});

	test("mounted host directory exposes existing host files", async () => {
		vm = await AgentOs.create({
			mounts: [
				{
					path: "/hostmnt",
					plugin: createHostDirBackend({ hostPath: tmpDir }),
				},
			],
		});
		const content = new TextDecoder().decode(
			await vm.readFile("/hostmnt/hello.txt"),
		);
		expect(content).toBe("hello from host");
	});

	test("symlink escape attempt is blocked", async () => {
		const escapePath = path.join(tmpDir, "escape");
		fs.symlinkSync("/etc", escapePath);

		vm = await AgentOs.create({
			mounts: [
				{
					path: "/hostmnt",
					plugin: createHostDirBackend({ hostPath: tmpDir }),
				},
			],
		});
		await expect(vm.readFile("/hostmnt/escape/hostname")).rejects.toThrow(
			"EACCES",
		);
	});

	test("write blocked when helper defaults to readOnly", async () => {
		vm = await AgentOs.create({
			mounts: [
				{
					path: "/hostmnt",
					plugin: createHostDirBackend({ hostPath: tmpDir }),
				},
			],
		});
		await expect(
			vm.writeFile("/hostmnt/new.txt", "should fail"),
		).rejects.toThrow("EROFS");
	});

	test("write works when readOnly: false", async () => {
		vm = await AgentOs.create({
			mounts: [
				{
					path: "/hostmnt",
					plugin: createHostDirBackend({ hostPath: tmpDir, readOnly: false }),
				},
			],
		});
		await vm.writeFile("/hostmnt/writable.txt", "written from VM");

		// Verify on host
		const content = fs.readFileSync(path.join(tmpDir, "writable.txt"), "utf-8");
		expect(content).toBe("written from VM");
	});

	test("rename and delete update the host directory when writable", async () => {
		vm = await AgentOs.create({
			mounts: [
				{
					path: "/hostmnt",
					plugin: createHostDirBackend({ hostPath: tmpDir, readOnly: false }),
				},
			],
		});

		await vm.writeFile("/hostmnt/to-rename.txt", "rename me");
		await vm.move("/hostmnt/to-rename.txt", "/hostmnt/renamed.txt");
		expect(fs.existsSync(path.join(tmpDir, "to-rename.txt"))).toBe(false);
		expect(fs.readFileSync(path.join(tmpDir, "renamed.txt"), "utf-8")).toBe(
			"rename me",
		);

		await vm.delete("/hostmnt/renamed.txt");
		expect(fs.existsSync(path.join(tmpDir, "renamed.txt"))).toBe(false);
	});
});
