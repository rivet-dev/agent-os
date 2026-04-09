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

	test("resolves undici from a direct projected node process", async () => {
		await vm.writeFile(
			"/tmp/undici-resolve.mjs",
			`console.log(require.resolve("undici"));`,
		);

		let stdout = "";
		let stderr = "";
		const { pid } = vm.spawn("node", ["/tmp/undici-resolve.mjs"], {
			onStdout: (data: Uint8Array) => {
				stdout += new TextDecoder().decode(data);
			},
			onStderr: (data: Uint8Array) => {
				stderr += new TextDecoder().decode(data);
			},
		});

		const exitCode = await vm.waitProcess(pid);
		expect(exitCode, stderr).toBe(0);
		expect(stdout).toContain("undici");
	});

	test("imports undici from a direct projected node process", async () => {
		await vm.writeFile(
			"/tmp/undici-import.mjs",
			`const mod = await import("undici"); console.log(typeof mod.fetch);`,
		);

		let stdout = "";
		let stderr = "";
		const { pid } = vm.spawn("node", ["/tmp/undici-import.mjs"], {
			onStdout: (data: Uint8Array) => {
				stdout += new TextDecoder().decode(data);
			},
			onStderr: (data: Uint8Array) => {
				stderr += new TextDecoder().decode(data);
			},
		});

		const exitCode = await vm.waitProcess(pid);
		expect(exitCode, stderr).toBe(0);
		expect(stdout).toContain("function");
	});

	test("guest child_process can run a simple JavaScript child", async () => {
		await vm.writeFile(
			"/tmp/child-hello.mjs",
			`console.log("child-hello");`,
		);
		await vm.writeFile(
			"/tmp/parent-hello.mjs",
			`
import { spawn } from "node:child_process";

const child = spawn("node", ["/tmp/child-hello.mjs"], {
	cwd: "/home/user",
	env: process.env,
	stdio: "pipe",
});

let stdout = "";
let stderr = "";
child.stdout.on("data", (chunk) => {
	stdout += String(chunk);
});
child.stderr.on("data", (chunk) => {
	stderr += String(chunk);
});

await new Promise((resolve, reject) => {
	child.on("error", reject);
	child.on("close", (code) => {
		if (code !== 0) {
			reject(new Error("child exited " + String(code) + " stderr=" + stderr));
			return;
		}
		resolve(null);
	});
});

console.log(stdout.trim());
`,
		);

		let stdout = "";
		let stderr = "";
		const { pid } = vm.spawn("node", ["/tmp/parent-hello.mjs"], {
			cwd: "/home/user",
			onStdout: (data: Uint8Array) => {
				stdout += new TextDecoder().decode(data);
			},
			onStderr: (data: Uint8Array) => {
				stderr += new TextDecoder().decode(data);
			},
		});

		const exitCode = await vm.waitProcess(pid);
		expect(exitCode, stderr).toBe(0);
		expect(stdout).toContain("child-hello");
	}, 10_000);

	test("guest child_process can import undici", async () => {
		await vm.writeFile(
			"/tmp/child-undici.mjs",
			`const mod = await import("undici"); console.log(typeof mod.fetch);`,
		);
		await vm.writeFile(
			"/tmp/parent-undici.mjs",
			`
import { spawn } from "node:child_process";

const child = spawn("node", ["/tmp/child-undici.mjs"], {
	cwd: "/home/user",
	env: process.env,
	stdio: "pipe",
});

let stdout = "";
let stderr = "";
child.stdout.on("data", (chunk) => {
	stdout += String(chunk);
});
child.stderr.on("data", (chunk) => {
	stderr += String(chunk);
});

await new Promise((resolve, reject) => {
	child.on("error", reject);
	child.on("close", (code) => {
		if (code !== 0) {
			reject(new Error("child exited " + String(code) + " stderr=" + stderr));
			return;
		}
		resolve(null);
	});
});

console.log(stdout.trim());
`,
		);

		let stdout = "";
		let stderr = "";
		const { pid } = vm.spawn("node", ["/tmp/parent-undici.mjs"], {
			cwd: "/home/user",
			onStdout: (data: Uint8Array) => {
				stdout += new TextDecoder().decode(data);
			},
			onStderr: (data: Uint8Array) => {
				stderr += new TextDecoder().decode(data);
			},
		});

		const exitCode = await vm.waitProcess(pid);
		expect(exitCode, stderr).toBe(0);
		expect(stdout).toContain("function");
	}, 10_000);
});
