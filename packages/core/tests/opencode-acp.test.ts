import { resolve } from "node:path";
import type { LLMock } from "@copilotkit/llmock";
import type { ManagedProcess } from "../src/runtime-compat.js";
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, test } from "vitest";
import opencode from "@rivet-dev/agent-os-opencode";
import { AcpClient } from "../src/acp-client.js";
import { AgentOs } from "../src/agent-os.js";
import { createStdoutLineIterable } from "../src/stdout-lines.js";
import { getAgentOsKernel } from "../src/test/runtime.js";
import {
	DEFAULT_TEXT_FIXTURE,
	startLlmock,
	stopLlmock,
} from "./helpers/llmock-helper.js";
import {
	createVmOpenCodeHome,
	createVmWorkspace,
	resolveOpenCodeAdapterBinPath,
} from "./helpers/opencode-helper.js";
import { REGISTRY_SOFTWARE, registrySkipReason } from "./helpers/registry-commands.js";

const MODULE_ACCESS_CWD = resolve(import.meta.dirname, "..");

describe.skipIf(registrySkipReason)("OpenCode ACP manual spawn inside the VM", () => {
	let vm: AgentOs;
	let mock: LLMock;
	let mockUrl: string;
	let mockPort: number;
	let client: AcpClient | undefined;

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
			moduleAccessCwd: MODULE_ACCESS_CWD,
			software: [opencode, ...REGISTRY_SOFTWARE],
		});
	});

	afterEach(async () => {
		if (client) {
			client.close();
			client = undefined;
		}
		await vm.dispose();
	});

	async function spawnOpenCodeAcp(): Promise<{
		proc: ManagedProcess;
		client: AcpClient;
		stderr: () => string;
		workspaceDir: string;
	}> {
		const homeDir = await createVmOpenCodeHome(vm, mockUrl);
		const workspaceDir = await createVmWorkspace(vm);
		const binPath = resolveOpenCodeAdapterBinPath(MODULE_ACCESS_CWD);
		const { iterable, onStdout } = createStdoutLineIterable();

		let stderrOutput = "";
		const proc = getAgentOsKernel(vm).spawn("node", [binPath], {
			streamStdin: true,
			onStdout,
			onStderr: (data: Uint8Array) => {
				stderrOutput += new TextDecoder().decode(data);
			},
			env: {
				HOME: homeDir,
				ANTHROPIC_API_KEY: "mock-key",
			},
			cwd: workspaceDir,
		});

		const acpClient = new AcpClient(proc, iterable);
		return {
			proc,
			client: acpClient,
			stderr: () => stderrOutput,
			workspaceDir,
		};
	}

	test("real OpenCode ACP initializes and creates a session over stdio inside the VM", async () => {
		const spawned = await spawnOpenCodeAcp();
		client = spawned.client;

		let initResponse: Awaited<ReturnType<AcpClient["request"]>>;
		try {
			initResponse = await client.request("initialize", {
				protocolVersion: 1,
				clientCapabilities: {
					fs: {
						readTextFile: true,
						writeTextFile: true,
					},
					terminal: true,
				},
			});
		} catch (error) {
			throw new Error(`Initialize failed. stderr: ${spawned.stderr()}\n${error}`);
		}

		expect(initResponse.error).toBeUndefined();
		expect(
			(initResponse.result as { protocolVersion: number }).protocolVersion,
		).toBe(1);
		expect(
			(
				initResponse.result as {
					agentInfo: { name: string; version: string };
				}
			).agentInfo,
		).toMatchObject({
			name: "OpenCode",
		});

		let sessionResponse: Awaited<ReturnType<AcpClient["request"]>>;
		try {
			sessionResponse = await client.request("session/new", {
				cwd: spawned.workspaceDir,
				mcpServers: [],
			});
		} catch (error) {
			throw new Error(`session/new failed. stderr: ${spawned.stderr()}\n${error}`);
		}

		expect(sessionResponse.error).toBeUndefined();
		expect(
			(sessionResponse.result as { sessionId: string }).sessionId,
		).toBeTruthy();
		expect(
			(
				sessionResponse.result as {
					modes: {
						currentModeId: string;
						availableModes: Array<{ id: string }>;
					};
				}
			).modes.currentModeId,
		).toBe("build");
		expect(
			(
				sessionResponse.result as {
					modes: { availableModes: Array<{ id: string }> };
				}
			).modes.availableModes.map((mode) => mode.id),
		).toEqual(expect.arrayContaining(["build", "plan"]));
	}, 120_000);
});
