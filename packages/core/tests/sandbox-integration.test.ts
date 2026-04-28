import common from "@rivet-dev/agent-os-common";
import {
	createSandboxFs,
	createSandboxToolkit,
} from "../../../registry/tool/sandbox/src/index.js";
import type { MockSandboxAgentHandle } from "../src/test/sandbox-agent.js";
import { startMockSandboxAgent } from "../src/test/sandbox-agent.js";
import { afterAll, afterEach, beforeAll, describe, expect, test } from "vitest";
import { AgentOs } from "../src/index.js";

const SANDBOX_QUICKSTART_PERMISSIONS = {
	fs: "allow",
	network: "allow",
	childProcess: "allow",
	env: "allow",
	tool: "allow",
} as const;

const SANDBOX_MOUNT_PATH = "/sandbox";
const SANDBOX_FILE_PATH = `${SANDBOX_MOUNT_PATH}/hello.txt`;
const SANDBOX_FILE_CONTENT = "Hello from agentOS!";

describe("sandbox quickstart truth test", () => {
	let sandbox: MockSandboxAgentHandle | null = null;
	let vm: AgentOs | null = null;

	beforeAll(async () => {
		sandbox = await startMockSandboxAgent();
	}, 150_000);

	afterEach(async () => {
		if (vm) {
			await vm.dispose();
			vm = null;
		}
	});

	afterAll(async () => {
		if (vm) {
			await vm.dispose();
			vm = null;
		}
		if (sandbox) {
			await sandbox.stop();
			sandbox = null;
		}
	});

	test("mounts createSandboxFs and exercises run-command plus list-processes from createSandboxToolkit", async () => {
		if (!sandbox) {
			throw new Error("Sandbox test harness did not start.");
		}

		vm = await AgentOs.create({
			permissions: SANDBOX_QUICKSTART_PERMISSIONS,
			software: [common],
			mounts: [
				{
					path: SANDBOX_MOUNT_PATH,
					plugin: createSandboxFs({ client: sandbox.client }),
				},
			],
			toolKits: [createSandboxToolkit({ client: sandbox.client })],
		});

		await sandbox.client.writeFsFile(
			{ path: "/hello.txt" },
			new TextEncoder().encode(SANDBOX_FILE_CONTENT),
		);
		const content = await vm.readFile(SANDBOX_FILE_PATH);
		expect(new TextDecoder().decode(content)).toBe(SANDBOX_FILE_CONTENT);

		const toolkit = createSandboxToolkit({ client: sandbox.client });
		const runCommandResponse = (await toolkit.tools["run-command"].execute({
			command: "echo",
			args: ["hello from sandbox"],
		})) as {
			stdout: string;
			stderr: string;
			exitCode: number;
		};
		expect(runCommandResponse.exitCode).toBe(0);
		expect(runCommandResponse.stderr).toBe("");
		expect(runCommandResponse.stdout.trim()).toBe("hello from sandbox");

		const createdProcess = (await toolkit.tools["create-process"].execute({
			command: "sleep",
			args: ["60"],
		})) as {
			id: string;
			status: string;
		};
		expect(createdProcess.status).toBe("running");

		const listProcessesResponse = (await toolkit.tools["list-processes"].execute(
			{},
		)) as {
			processes: Array<{
				command: string;
				args?: string[];
				status: string;
			}>;
		};
		expect(Array.isArray(listProcessesResponse.processes)).toBe(true);
		expect(listProcessesResponse.processes.length).toBeGreaterThan(0);
		expect(
			listProcessesResponse.processes.some(
				(processInfo) =>
					processInfo.status === "running" && processInfo.command === "sleep",
			),
		).toBe(true);

		await toolkit.tools["kill-process"].execute({ id: createdProcess.id });
	}, 150_000);
});
