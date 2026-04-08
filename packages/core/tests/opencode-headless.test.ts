import { resolve } from "node:path";
import { afterEach, beforeEach, describe, expect, test } from "vitest";
import { AgentOs } from "../src/index.js";

const MODULE_ACCESS_CWD = resolve(import.meta.dirname, "..");

describe("OpenCode VM package", () => {
	let vm: AgentOs;

	beforeEach(async () => {
		vm = await AgentOs.create({
			moduleAccessCwd: MODULE_ACCESS_CWD,
		});
	});

	afterEach(async () => {
		await vm.dispose();
	});

	test("mounts the source-built OpenCode ACP bundle inside the VM", async () => {
		const script = `
const fs = require("fs");

const pkgPath = "/root/node_modules/@rivet-dev/agent-os-opencode/package.json";
const manifestPath =
  "/root/node_modules/@rivet-dev/agent-os-opencode/dist/opencode-acp.manifest.json";
const bundlePath =
  "/root/node_modules/@rivet-dev/agent-os-opencode/dist/opencode-acp/acp.js";

const pkg = JSON.parse(fs.readFileSync(pkgPath, "utf-8"));
const manifest = JSON.parse(fs.readFileSync(manifestPath, "utf-8"));

console.log("pkg:" + pkg.name);
console.log("bundle:" + fs.existsSync(bundlePath));
console.log("sourceRepo:" + manifest.source.repository);
console.log("sourceVersion:" + manifest.source.version);
console.log("legacyWrapper:" + fs.existsSync("/root/node_modules/opencode-ai/package.json"));
`;
		await vm.writeFile("/tmp/check-opencode-package.mjs", script);

		let stdout = "";
		let stderr = "";

		const { pid } = vm.spawn("node", ["/tmp/check-opencode-package.mjs"], {
			onStdout: (data: Uint8Array) => {
				stdout += new TextDecoder().decode(data);
			},
			onStderr: (data: Uint8Array) => {
				stderr += new TextDecoder().decode(data);
			},
		});

		const exitCode = await vm.waitProcess(pid);

		expect(exitCode, `Failed. stderr: ${stderr}`).toBe(0);
		expect(stdout).toContain("pkg:@rivet-dev/agent-os-opencode");
		expect(stdout).toContain("bundle:true");
		expect(stdout).toContain("sourceRepo:anomalyco/opencode");
		expect(stdout).toContain("sourceVersion:1.3.13");
		expect(stdout).toContain("legacyWrapper:false");
	}, 30_000);

	test("bundled ACP source exposes the expected command symbol without a host wrapper", async () => {
		const script = `
const fs = require("fs");
const modulePath =
  "/root/node_modules/@rivet-dev/agent-os-opencode/dist/opencode-acp/acp.js";
const source = fs.readFileSync(modulePath, "utf8");
console.log("hasAcpCommand:" + source.includes("AcpCommand"));
`;
		await vm.writeFile("/tmp/import-opencode-acp.mjs", script);

		let stdout = "";
		let stderr = "";

		const { pid } = vm.spawn("node", ["/tmp/import-opencode-acp.mjs"], {
			onStdout: (data: Uint8Array) => {
				stdout += new TextDecoder().decode(data);
			},
			onStderr: (data: Uint8Array) => {
				stderr += new TextDecoder().decode(data);
			},
		});

		const exitCode = await vm.waitProcess(pid);

		expect(exitCode, `Failed. stderr: ${stderr}`).toBe(0);
		expect(stdout).toContain("hasAcpCommand:true");
	}, 30_000);
});
