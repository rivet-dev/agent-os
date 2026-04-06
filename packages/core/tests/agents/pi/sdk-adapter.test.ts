import { readFileSync, realpathSync } from "node:fs";
import { join, relative, resolve } from "node:path";
import type { LLMock } from "@copilotkit/llmock";
import type { ManagedProcess } from "../../../src/runtime-compat.js";
import {
	afterAll,
	afterEach,
	beforeAll,
	beforeEach,
	describe,
	expect,
	test,
} from "vitest";
import pi from "@rivet-dev/agent-os-pi";
import { AcpClient } from "../../../src/acp-client.js";
import { AgentOs } from "../../../src/agent-os.js";
import { createStdoutLineIterable } from "../../../src/stdout-lines.js";
import { getAgentOsKernel } from "../../../src/test/runtime.js";
import {
	DEFAULT_TEXT_FIXTURE,
	startLlmock,
	stopLlmock,
} from "../../helpers/llmock-helper.js";
import {
	PI_AGENT_DIR,
	PI_TEST_HOME,
	writePiAnthropicModelsOverride,
} from "./test-helper.js";

/**
 * Workspace root has shamefully-hoisted node_modules with @rivet-dev/agent-os-pi available.
 */
const MODULE_ACCESS_CWD = resolve(import.meta.dirname, "../../..");

/**
 * Resolve pi-sdk-acp bin path from host node_modules.
 * kernel.readFile() doesn't see the ModuleAccessFileSystem overlay,
 * so we read the host package.json directly and construct the VFS path.
 */
function resolvePiSdkBinPath(): string {
	const hostPkgJson = join(
		MODULE_ACCESS_CWD,
		"node_modules/@rivet-dev/agent-os-pi/package.json",
	);
	const pkg = JSON.parse(readFileSync(hostPkgJson, "utf-8"));

	let binEntry: string;
	if (typeof pkg.bin === "string") {
		binEntry = pkg.bin;
	} else if (typeof pkg.bin === "object" && pkg.bin !== null) {
		binEntry =
			(pkg.bin as Record<string, string>)["pi-sdk-acp"] ??
			Object.values(pkg.bin)[0];
	} else {
		throw new Error("No bin entry in @rivet-dev/agent-os-pi package.json");
	}

	return `/root/node_modules/@rivet-dev/agent-os-pi/${binEntry}`;
}

function resolvePiPackageDir(): string {
	const hostPackageDir = realpathSync(
		join(MODULE_ACCESS_CWD, "node_modules/@mariozechner/pi-coding-agent"),
	);
	const repoRoot = resolve(MODULE_ACCESS_CWD, "../..");
	const pnpmRoot = join(repoRoot, "node_modules/.pnpm");
	if (hostPackageDir.startsWith(`${pnpmRoot}/`)) {
		return `/root/node_modules/.pnpm/${relative(pnpmRoot, hostPackageDir)}`;
	}
	const moduleAccessNodeModules = join(MODULE_ACCESS_CWD, "node_modules");
	if (hostPackageDir.startsWith(`${moduleAccessNodeModules}/`)) {
		return `/root/node_modules/${relative(moduleAccessNodeModules, hostPackageDir)}`;
	}
	throw new Error(`Unsupported PI package directory: ${hostPackageDir}`);
}

describe("pi-sdk-acp adapter manual spawn", () => {
	let vm: AgentOs;
	let mock: LLMock;
	let mockUrl: string;
	let mockPort: number;
	let client: AcpClient;

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
			software: [pi],
		});
		await writePiAnthropicModelsOverride(vm, mockUrl);
	});

	afterEach(async () => {
		if (client) {
			client.close();
		}
		await vm.dispose();
	});

	/**
	 * Spawn pi-sdk-acp from the mounted node_modules overlay and wire up AcpClient.
	 */
	function spawnPiSdkAcp(): {
		proc: ManagedProcess;
		client: AcpClient;
		stderr: () => string;
	} {
		const binPath = resolvePiSdkBinPath();
		const { iterable, onStdout } = createStdoutLineIterable();

		let stderrOutput = "";
		const spawned = getAgentOsKernel(vm).spawn("node", [binPath], {
			streamStdin: true,
			onStdout,
			onStderr: (data: Uint8Array) => {
				stderrOutput += new TextDecoder().decode(data);
			},
			env: {
				HOME: PI_TEST_HOME,
				PI_CODING_AGENT_DIR: PI_AGENT_DIR,
				ANTHROPIC_API_KEY: "mock-key",
				ANTHROPIC_BASE_URL: mockUrl,
				PI_PACKAGE_DIR: resolvePiPackageDir(),
			},
		});

		const acpClient = new AcpClient(spawned, iterable);
		return { proc: spawned, client: acpClient, stderr: () => stderrOutput };
	}

	test("initialize returns protocolVersion and agentInfo", async () => {
		const spawned = spawnPiSdkAcp();
		client = spawned.client;

		let response: Awaited<ReturnType<AcpClient["request"]>>;
		try {
			response = await client.request("initialize", {
				protocolVersion: 1,
				clientCapabilities: {},
			});
		} catch (err) {
			throw new Error(
				`Initialize failed. stderr: ${spawned.stderr()}\n${err}`,
			);
		}

		expect(
			response.error,
			`ACP error: ${JSON.stringify(response.error)}`,
		).toBeUndefined();
		expect(response.result).toBeDefined();

		const result = response.result as Record<string, unknown>;
		expect(result.protocolVersion).toBe(1);
		expect(result.agentInfo).toBeDefined();

		const agentInfo = result.agentInfo as Record<string, unknown>;
		expect(agentInfo.name).toBe("pi-sdk-acp");
	}, 60_000);

	test("session/new creates session via Pi SDK", async () => {
		const spawned = spawnPiSdkAcp();
		client = spawned.client;

		// Must initialize first
		let initResponse: Awaited<ReturnType<AcpClient["request"]>>;
		try {
			initResponse = await client.request("initialize", {
				protocolVersion: 1,
				clientCapabilities: {},
			});
		} catch (err) {
			throw new Error(
				`Initialize failed. stderr: ${spawned.stderr()}\n${err}`,
			);
		}
		expect(initResponse.error).toBeUndefined();

		// Send session/new. The SDK adapter creates a session in-process
		// via createAgentSession() — no subprocess spawning.
		let sessionResponse: Awaited<ReturnType<AcpClient["request"]>>;
		try {
			sessionResponse = await client.request("session/new", {
				cwd: "/home/user",
				mcpServers: [],
			});
		} catch (err) {
			throw new Error(
				`session/new failed. stderr: ${spawned.stderr()}\n${err}`,
			);
		}

		if (sessionResponse.error) {
			throw new Error(
				`session/new ACP error: ${JSON.stringify(sessionResponse.error)}\nstderr: ${spawned.stderr()}`,
			);
		}
		expect(sessionResponse.id).toBeDefined();
		expect(sessionResponse.jsonrpc).toBe("2.0");
		expect(sessionResponse.result).toBeDefined();
		expect(
			(sessionResponse.result as { sessionId?: string }).sessionId,
		).toBeTruthy();
	}, 90_000);

	test("session/prompt streams events and completes", async () => {
		const spawned = spawnPiSdkAcp();
		client = spawned.client;

		// Initialize
		const initResponse = await client.request("initialize", {
			protocolVersion: 1,
			clientCapabilities: {},
		});
		expect(initResponse.error).toBeUndefined();

		// Create session
		const sessionResponse = await client.request("session/new", {
			cwd: "/home/user",
			mcpServers: [],
		});
		if (sessionResponse.error) {
			throw new Error(
				`session/new ACP error: ${JSON.stringify(sessionResponse.error)}\nstderr: ${spawned.stderr()}`,
			);
		}
		const sessionId = (
			sessionResponse.result as { sessionId: string }
		).sessionId;

		// Collect all notifications
		const notifications: Array<{ method: string; params: unknown }> = [];
		client.onNotification((notification) => {
			notifications.push(notification);
		});

		// Send prompt. The mock LLM returns a simple text response.
		// The Pi SDK may or may not produce session/update notifications
		// depending on how well llmock matches the Anthropic streaming format.
		let promptResponse: Awaited<ReturnType<AcpClient["request"]>>;
		try {
			promptResponse = await client.request("session/prompt", {
				sessionId,
				prompt: [{ type: "text", text: "Say hello" }],
			});
		} catch (err) {
			throw new Error(
				`session/prompt failed. stderr: ${spawned.stderr()}\n${err}`,
			);
		}

		expect(
			promptResponse.error,
			`Prompt error: ${JSON.stringify(promptResponse.error)}. stderr: ${spawned.stderr()}`,
		).toBeUndefined();
		expect(promptResponse.result).toBeDefined();
		const promptResult = promptResponse.result as {
			stopReason: string;
		};
		// Stop reason should be either "end_turn" (success) or "cancelled"
		expect(["end_turn", "cancelled"]).toContain(promptResult.stopReason);

		// Verify any received notifications have the right structure
		for (const n of notifications) {
			expect(n.method).toBe("session/update");
		}
	}, 90_000);
});
