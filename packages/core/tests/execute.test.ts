import { afterEach, beforeEach, describe, expect, test } from "vitest";
import { AgentOs } from "../src/index.js";
import { REGISTRY_SOFTWARE } from "./helpers/registry-commands.js";

describe("command execution", () => {
	let vm: AgentOs;

	beforeEach(async () => {
		vm = await AgentOs.create({ software: REGISTRY_SOFTWARE });
	});

	afterEach(async () => {
		await vm.dispose();
	});

	test("exec returns stdout with exit code 0", async () => {
		const result = await vm.exec("echo hello");
		expect(result.exitCode).toBe(0);
		expect(result.stdout.trim()).toBe("hello");
		expect(result.stderr).toBe("");
	});

	test("exec returns stderr and non-zero exit code", async () => {
		const result = await vm.exec("echo error >&2 && exit 1");
		expect(result.exitCode).toBe(1);
		expect(result.stderr.trim()).toBe("error");
	});

	test("exec with env vars passes them through", async () => {
		const result = await vm.exec("echo $MY_VAR", {
			env: { MY_VAR: "test-value" },
		});
		expect(result.exitCode).toBe(0);
		expect(result.stdout.trim()).toBe("test-value");
	});

	test("exec with cwd sets working directory", async () => {
		await vm.mkdir("/tmp/testdir");
		const result = await vm.exec("printf found > marker.txt && cat marker.txt", {
			cwd: "/tmp/testdir",
		});
		expect(result.exitCode).toBe(0);
		expect(result.stdout).toContain("found");
	});

	test("spawn and interact with process", async () => {
		const { pid } = vm.spawn("cat", []);
		vm.writeProcessStdin(pid, "hello from stdin\n");
		vm.closeProcessStdin(pid);
		const exitCode = await vm.waitProcess(pid);
		expect(exitCode).toBe(0);
	});

	test("exec node script", async () => {
		await vm.writeFile("/tmp/test.js", 'console.log("node-output");');
		const result = await vm.exec("node /tmp/test.js");
		expect(result.exitCode).toBe(0);
		expect(result.stdout).toContain("node-output");
	});

	test(
		"exec shell pipeline",
		async () => {
			for (let attempt = 0; attempt < 5; attempt += 1) {
				const result = await vm.exec("echo hello | cat");
				expect(result.exitCode, result.stderr || result.stdout).toBe(0);
				expect(result.stdout).toContain("hello");
			}
		},
		120_000,
	);
});
