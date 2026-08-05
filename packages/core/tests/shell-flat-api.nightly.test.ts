import { afterEach, beforeEach, describe, expect, test } from "vitest";
import { AgentOs } from "../src/index.js";

function sleep(ms: number): Promise<void> {
	return new Promise((resolve) => setTimeout(resolve, ms));
}

describe("flat shell API", () => {
	let vm: AgentOs;

	beforeEach(async () => {
		vm = await AgentOs.create();
	});

	afterEach(async () => {
		await vm.dispose();
	});

	test("open shell, write command via writeShell, read output via onShellData", async () => {
		// Write a simple script that reads stdin and writes to stdout
		await vm.writeFile(
			"/tmp/shell-echo.mjs",
			`process.stdin.on("data", (chunk) => { process.stdout.write("GOT:" + chunk); });`,
		);

		const { shellId } = vm.openShell({
			command: "node",
			args: ["/tmp/shell-echo.mjs"],
		});

		const chunks: string[] = [];
		vm.onShellData(shellId, (event) => {
			chunks.push(new TextDecoder().decode(event.data));
		});

		vm.writeShell(shellId, "hello-flat-shell\n");

		// Wait for output to arrive
		await new Promise((r) => setTimeout(r, 1000));

		vm.closeShell(shellId);

		const output = chunks.join("");
		expect(output).toContain("hello-flat-shell");
	}, 30_000);

	test("default shell echoes typed characters before newline", async () => {
		const { shellId } = vm.openShell();

		const chunks: string[] = [];
		vm.onShellData(shellId, (event) => {
			chunks.push(new TextDecoder().decode(event.data));
		});

		await sleep(100);
		vm.writeShell(shellId, "abc");
		await sleep(100);

		vm.closeShell(shellId);

		const output = chunks.join("");
		expect(output).toContain("sh-0.4$ ");
		expect(output).toContain("abc");
	}, 30_000);
});
