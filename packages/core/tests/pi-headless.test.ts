import { resolve } from "node:path";
import type { LLMock } from "@copilotkit/llmock";
import {
	afterAll,
	afterEach,
	beforeAll,
	beforeEach,
	describe,
	expect,
	test,
} from "vitest";
import { AgentOs } from "../src/index.js";
import {
	DEFAULT_TEXT_FIXTURE,
	startLlmock,
	stopLlmock,
} from "./helpers/llmock-helper.js";

/**
 * Use the workspace root as module access CWD. With shamefully-hoist=true
 * in .npmrc, all transitive dependencies are hoisted to the root node_modules,
 * making them accessible via the ModuleAccessFileSystem overlay.
 */
const SESSION_MODULE_ACCESS_CWD = resolve(import.meta.dirname, "..");

describe("PI headless mode", () => {
	let vm: AgentOs;
	let mock: LLMock;
	let mockUrl: string;
	let mockPort: number;

	beforeAll(async () => {
		const result = await startLlmock([DEFAULT_TEXT_FIXTURE]);
		mock = result.mock;
		mockUrl = result.url;
		mockPort = Number(new URL(result.url).port);
	});

	afterAll(async () => {
		await stopLlmock(mock);
	});

	beforeEach(async () => {
		vm = await AgentOs.create({
			loopbackExemptPorts: [mockPort],
			moduleAccessCwd: SESSION_MODULE_ACCESS_CWD,
		});
	});

	afterEach(async () => {
		await vm.dispose();
	});

	test("mock LLM server responds to API calls from inside VM", async () => {
		// Write a script that calls the mock Anthropic API via fetch
		const apiScript = `
const response = await fetch("${mockUrl}/v1/messages", {
  method: "POST",
  headers: { "Content-Type": "application/json", "x-api-key": "mock-key" },
  body: JSON.stringify({
    model: "claude-sonnet-4-20250514",
    max_tokens: 100,
    messages: [{ role: "user", content: "say hello" }],
  }),
});
const data = await response.json();
console.log(data.content[0].text);
`;
		await vm.writeFile("/tmp/api-test.mjs", apiScript);

		let stdout = "";
		let stderr = "";

		const { pid } = vm.spawn("node", ["/tmp/api-test.mjs"], {
			onStdout: (data: Uint8Array) => {
				stdout += new TextDecoder().decode(data);
			},
			onStderr: (data: Uint8Array) => {
				stderr += new TextDecoder().decode(data);
			},
			env: {
				HOME: "/home/user",
				ANTHROPIC_API_KEY: "mock-key",
			},
		});

		const exitCode = await vm.waitProcess(pid);

		expect(exitCode, `API test failed. stderr: ${stderr}`).toBe(0);
		expect(stdout).toContain("Hello from llmock");
	}, 30_000);

	test("PI package entrypoints are mounted inside the VM", async () => {
		const loadScript = `
const fs = require("fs");
const mainPath = "/root/node_modules/@mariozechner/pi-coding-agent/dist/main.js";
const argsPath = "/root/node_modules/@mariozechner/pi-coding-agent/dist/cli/args.js";
console.log("main-exists:" + fs.existsSync(mainPath));
console.log("args-exists:" + fs.existsSync(argsPath));
console.log("main-esm:" + fs.readFileSync(mainPath, "utf8").includes("export "));
console.log("args-parse:" + fs.readFileSync(argsPath, "utf8").includes("parseArgs"));
`;
		await vm.writeFile("/tmp/pi-load-test.mjs", loadScript);

		let stdout = "";
		let stderr = "";

		const { pid } = vm.spawn("node", ["/tmp/pi-load-test.mjs"], {
			onStdout: (data: Uint8Array) => {
				stdout += new TextDecoder().decode(data);
			},
			onStderr: (data: Uint8Array) => {
				stderr += new TextDecoder().decode(data);
			},
			env: {
				HOME: "/home/user",
				PI_OFFLINE: "1",
			},
		});

		const exitCode = await vm.waitProcess(pid);

		expect(exitCode, `PI package probe failed. stderr: ${stderr}`).toBe(0);
		expect(stdout).toContain("main-exists:true");
		expect(stdout).toContain("args-exists:true");
		expect(stdout).toContain("main-esm:true");
		expect(stdout).toContain("args-parse:true");
	}, 30_000);

	test("standalone PI CLI is not exposed on the native sidecar PATH", async () => {
		let stdout = "";
		let stderr = "";

		const { pid } = vm.spawn("pi", ["-p", "--no-session", "hello"], {
			onStdout: (data: Uint8Array) => {
				stdout += new TextDecoder().decode(data);
			},
			onStderr: (data: Uint8Array) => {
				stderr += new TextDecoder().decode(data);
			},
			env: {
				HOME: "/home/user",
				PI_OFFLINE: "1",
				ANTHROPIC_API_KEY: "mock-key",
				ANTHROPIC_BASE_URL: mockUrl,
			},
		});

		const exitCode = await vm.waitProcess(pid);

		expect(exitCode).toBe(1);
		expect(stdout).toBe("");
		expect(stderr).toContain("command not found on native sidecar path: pi");
	}, 30_000);
});
