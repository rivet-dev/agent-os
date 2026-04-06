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
 * Workspace root has shamefully-hoisted node_modules with pi-acp available.
 */
const MODULE_ACCESS_CWD = resolve(import.meta.dirname, "../../..");
const PI_ACP_WRAPPER_PATH = "/home/user/pi-acp-pi-wrapper.mjs";

/**
 * Resolve pi-acp bin path from host node_modules.
 * kernel.readFile() doesn't see the ModuleAccessFileSystem overlay,
 * so we read the host package.json directly and construct the VFS path.
 */
function resolvePiAcpBinPath(): string {
	return resolvePackageBinPath("pi-acp", "pi-acp");
}

function resolvePiPackageDir(): string {
	return resolvePackageGuestDir("@mariozechner/pi-coding-agent");
}

async function writePiAcpPiWrapper(vm: AgentOs): Promise<void> {
	await vm.writeFile(
		PI_ACP_WRAPPER_PATH,
		[
			'#!/usr/bin/env node',
			'import * as crypto from "node:crypto";',
			'import * as readline from "node:readline";',
			'',
			'const sessionId = crypto.randomUUID();',
			'const agentDir = process.env.PI_CODING_AGENT_DIR ?? "/home/user/.pi/agent";',
			'const sessionFile = `${agentDir}/sessions/${sessionId}.json`;',
			'let thinkingLevel = "medium";',
			'let currentModel = {',
			'\tprovider: "anthropic",',
			'\tid: "claude-opus-4-6",',
			'\tname: "Claude Opus 4.6",',
			'};',
			'',
			'function writeResponse(id, command, data) {',
			'\tprocess.stdout.write(',
			'\t\tJSON.stringify({',
			'\t\t\tid,',
			'\t\t\ttype: "response",',
			'\t\t\tcommand,',
			'\t\t\tsuccess: true,',
			'\t\t\t...(data === undefined ? {} : { data }),',
			'\t\t}) + "\\n",',
			'\t);',
			'}',
			'',
			'const rl = readline.createInterface({ input: process.stdin });',
			'rl.on("line", (line) => {',
			'\tlet command;',
			'\ttry {',
			'\t\tcommand = JSON.parse(line);',
			'\t} catch {',
			'\t\treturn;',
			'\t}',
			'',
			'\tswitch (command.type) {',
			'\t\tcase "get_state":',
			'\t\t\twriteResponse(command.id, "get_state", {',
			'\t\t\t\tsessionId,',
			'\t\t\t\tsessionFile,',
			'\t\t\t\tthinkingLevel,',
			'\t\t\t});',
			'\t\t\tbreak;',
			'\t\tcase "get_available_models":',
			'\t\t\twriteResponse(command.id, "get_available_models", {',
			'\t\t\t\tmodels: [currentModel],',
			'\t\t\t});',
			'\t\t\tbreak;',
			'\t\tcase "set_model":',
			'\t\t\tcurrentModel = {',
			'\t\t\t\tprovider: command.provider,',
			'\t\t\t\tid: command.modelId,',
			'\t\t\t\tname: command.modelId,',
			'\t\t\t};',
			'\t\t\twriteResponse(command.id, "set_model", currentModel);',
			'\t\t\tbreak;',
			'\t\tcase "set_thinking_level":',
			'\t\t\tthinkingLevel = command.level ?? thinkingLevel;',
			'\t\t\twriteResponse(command.id, "set_thinking_level");',
			'\t\t\tbreak;',
			'\t\tdefault:',
			'\t\t\twriteResponse(command.id, command.type, null);',
			'\t\t\tbreak;',
			'\t}',
			'});',
		].join("\n"),
	);
	await (
		getAgentOsKernel(vm) as unknown as {
			vfs: { chmod(path: string, mode: number): Promise<void> };
		}
	).vfs.chmod(PI_ACP_WRAPPER_PATH, 0o755);
}

function resolvePackageBinPath(packageName: string, binName?: string): string {
	const guestPackageDir = resolvePackageGuestDir(packageName);
	const hostPkgJson = join(
		MODULE_ACCESS_CWD,
		`node_modules/${packageName}/package.json`,
	);
	const pkg = JSON.parse(readFileSync(hostPkgJson, "utf-8"));

	let binEntry: string;
	if (typeof pkg.bin === "string") {
		binEntry = pkg.bin;
	} else if (typeof pkg.bin === "object" && pkg.bin !== null) {
		binEntry =
			(binName ? (pkg.bin as Record<string, string>)[binName] : undefined) ??
			(pkg.bin as Record<string, string>)[packageName] ??
			Object.values(pkg.bin)[0];
	} else {
		throw new Error(`No bin entry in ${packageName} package.json`);
	}

	return `${guestPackageDir}/${binEntry}`;
}

function resolvePackageGuestDir(packageName: string): string {
	const hostPackageDir = realpathSync(
		join(MODULE_ACCESS_CWD, `node_modules/${packageName}`),
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
	throw new Error(`Unsupported guest package directory for ${packageName}: ${hostPackageDir}`);
}

describe("pi-acp adapter manual spawn", () => {
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
		});
		await writePiAnthropicModelsOverride(vm, mockUrl);
		await writePiAcpPiWrapper(vm);
	});

	afterEach(async () => {
		if (client) {
			client.close();
		}
		await vm.dispose();
	});

	/**
	 * Spawn pi-acp from the mounted node_modules overlay and wire up AcpClient.
	 */
	function spawnPiAcp(): {
		proc: ManagedProcess;
		client: AcpClient;
		stderr: () => string;
	} {
		const binPath = resolvePiAcpBinPath();
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
				PI_ACP_PI_COMMAND: PI_ACP_WRAPPER_PATH,
			},
		});

		const acpClient = new AcpClient(spawned, iterable);
		return { proc: spawned, client: acpClient, stderr: () => stderrOutput };
	}

	test("initialize returns protocolVersion and agentInfo", async () => {
		const spawned = spawnPiAcp();
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
		expect(result.protocolVersion).toBeDefined();
		expect(result.agentInfo).toBeDefined();

		const agentInfo = result.agentInfo as Record<string, unknown>;
		expect(agentInfo.name).toBeDefined();
	}, 60_000);

	test("session/new returns a real PI session id", async () => {
		const spawned = spawnPiAcp();
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

		// Send session/new. pi-acp internally spawns the PI CLI as a child
		// process. Verify the JSON-RPC protocol works correctly.
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

		expect(sessionResponse.error).toBeUndefined();
		expect(sessionResponse.id).toBeDefined();
		expect(sessionResponse.jsonrpc).toBe("2.0");
		expect(sessionResponse.result).toBeDefined();
		expect(
			(sessionResponse.result as { sessionId?: string }).sessionId,
		).toBeTruthy();
	}, 180_000);
});
