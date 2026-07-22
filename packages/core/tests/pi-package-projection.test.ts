import pi from "@agentos-software/pi";
import { afterEach, beforeEach, describe, expect, test } from "vitest";
import { AgentOs } from "../src/agent-os.js";

describe("Pi package projection", () => {
	let vm: AgentOs;

	beforeEach(async () => {
		vm = await AgentOs.create({ defaultSoftware: false });
		await vm.linkSoftware(pi);
	});

	afterEach(async () => {
		await vm.dispose();
	});

	test("projects the standard Pi ACP adapter and native Pi CLI", async () => {
		expect(await vm.listSoftware()).toEqual(
			expect.arrayContaining([
				expect.objectContaining({
					commands: expect.arrayContaining(["pi", "pi-acp"]),
				}),
			]),
		);
		expect(await vm.listAgents()).toEqual(
			expect.arrayContaining([
				expect.objectContaining({ id: "pi", installed: true }),
			]),
		);
		let stdout = "";
		let stderr = "";
		const { pid } = vm.spawn("pi", ["--version"], {
			onStdout: (data: Uint8Array) => {
				stdout += new TextDecoder().decode(data);
			},
			onStderr: (data: Uint8Array) => {
				stderr += new TextDecoder().decode(data);
			},
		});

		const exitCode = await vm.waitProcess(pid);
		expect(exitCode, stderr).toBe(0);
		expect(stdout).toContain("0.80.10");
	});

	test("resolves the ACP SDK from Pi's package-local dependency closure", async () => {
		let stdout = "";
		let stderr = "";
		const { pid } = vm.spawn(
			"node",
			[
				"-e",
				`
const { createRequire } = require("node:module");
const fs = require("node:fs");
const path = require("node:path");
const requireFromPi = createRequire("/opt/agentos/pkgs/pi/0.0.1/node_modules/@agentos-software/pi/dist/pi-acp/index.js");
const sdkPath = requireFromPi.resolve("@agentclientprotocol/sdk");
console.log(JSON.stringify({
  path: sdkPath,
  version: JSON.parse(fs.readFileSync(path.join(path.dirname(sdkPath), "../package.json"), "utf8")).version,
}));
`,
			],
			{
				onStdout: (data: Uint8Array) => {
					stdout += new TextDecoder().decode(data);
				},
				onStderr: (data: Uint8Array) => {
					stderr += new TextDecoder().decode(data);
				},
			},
		);

		const exitCode = await vm.waitProcess(pid);
		expect(exitCode, stderr).toBe(0);
		const resolution = JSON.parse(stdout.trim()) as {
			path: string;
			version: string;
		};
		expect(resolution.version).toBe("1.2.1");
		expect(resolution.path).toContain("/opt/agentos/pkgs/pi/0.0.1/");
	});
});
