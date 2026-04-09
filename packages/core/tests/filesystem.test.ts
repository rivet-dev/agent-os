import { afterEach, beforeEach, describe, expect, test } from "vitest";
import { AgentOs } from "../src/index.js";
import {
	REGISTRY_SOFTWARE,
	registrySkipReason,
} from "./helpers/registry-commands.js";

describe("filesystem operations", () => {
	let vm: AgentOs;

	beforeEach(async () => {
		vm = await AgentOs.create();
	});

	afterEach(async () => {
		await vm.dispose();
	});

	test("writeFile and readFile round-trip", async () => {
		const content = "hello filesystem";
		await vm.writeFile("/tmp/roundtrip.txt", content);
		const data = await vm.readFile("/tmp/roundtrip.txt");
		expect(new TextDecoder().decode(data)).toBe(content);
	});

	test.skipIf(registrySkipReason)(
		"writeFile is visible to WASM guest commands",
		async () => {
			await vm.dispose();
			vm = await AgentOs.create({ software: REGISTRY_SOFTWARE });

			await vm.writeFile("/tmp/test.txt", "hello");

			const cat = await vm.exec("cat /tmp/test.txt");
			expect(cat.exitCode, cat.stderr || cat.stdout).toBe(0);
			expect(cat.stdout.trim()).toBe("hello");

			const ls = await vm.exec("ls /tmp/");
			expect(ls.exitCode, ls.stderr || ls.stdout).toBe(0);
			expect(ls.stdout).toContain("test.txt");
		},
	);

	test("mkdir and readdir", async () => {
		await vm.mkdir("/tmp/testdir");
		await vm.writeFile("/tmp/testdir/a.txt", "a");
		await vm.writeFile("/tmp/testdir/b.txt", "b");
		const entries = await vm.readdir("/tmp/testdir");
		expect(entries).toContain("a.txt");
		expect(entries).toContain("b.txt");
	});

	test("stat returns file info", async () => {
		await vm.writeFile("/tmp/statfile.txt", "stat me");
		const info = await vm.stat("/tmp/statfile.txt");
		expect(info).toBeDefined();
		expect(info.size).toBeGreaterThan(0);
	});

	test("exists returns true for existing file", async () => {
		await vm.writeFile("/tmp/exists.txt", "here");
		const result = await vm.exists("/tmp/exists.txt");
		expect(result).toBe(true);
	});

	test("exists returns false for missing file", async () => {
		const result = await vm.exists("/tmp/nonexistent-file.txt");
		expect(result).toBe(false);
	});
});
