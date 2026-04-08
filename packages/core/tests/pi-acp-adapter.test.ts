import { resolve } from "node:path";
import piCli from "@rivet-dev/agent-os-pi-cli";
import { afterEach, beforeEach, describe, expect, test } from "vitest";
import { AgentOs } from "../src/agent-os.js";

const MODULE_ACCESS_CWD = resolve(
	import.meta.dirname,
	"../../../examples/quickstart",
);

describe("pi-cli software projection", () => {
	let vm: AgentOs;

	beforeEach(async () => {
		vm = await AgentOs.create({
			moduleAccessCwd: MODULE_ACCESS_CWD,
			software: [piCli],
		});
	});

	afterEach(async () => {
		await vm.dispose();
	});

	test("projects the CLI adapter package and PI agent package into the VM", async () => {
		const script = `
const fs = require("fs");
console.log("adapter:" + fs.existsSync("/root/node_modules/pi-acp/package.json"));
console.log("agent:" + fs.existsSync("/root/node_modules/@mariozechner/pi-coding-agent/package.json"));
`;
		await vm.writeFile("/tmp/pi-cli-projection.mjs", script);

		let stdout = "";
		let stderr = "";

		const { pid } = vm.spawn("node", ["/tmp/pi-cli-projection.mjs"], {
			onStdout: (data: Uint8Array) => {
				stdout += new TextDecoder().decode(data);
			},
			onStderr: (data: Uint8Array) => {
				stderr += new TextDecoder().decode(data);
			},
		});

		const exitCode = await vm.waitProcess(pid);

		expect(exitCode, `Projection probe failed. stderr: ${stderr}`).toBe(0);
		expect(stdout).toContain("adapter:true");
		expect(stdout).toContain("agent:true");
	});
});
