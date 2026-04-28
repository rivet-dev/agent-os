import { resolve } from "node:path";
import claude from "@rivet-dev/agent-os-claude";
import codex from "@rivet-dev/agent-os-codex-agent";
import opencode from "@rivet-dev/agent-os-opencode";
import pi from "@rivet-dev/agent-os-pi";
import piCli from "@rivet-dev/agent-os-pi-cli";
import { describe, expect, test } from "vitest";
import { AgentOs, type AgentInfo } from "../src/agent-os.js";
import type { SoftwareInput } from "../src/packages.js";

const MODULE_ACCESS_CWD = resolve(import.meta.dirname, "..");
const MOCK_ADAPTER_PATH = "/tmp/mock-agent-config-adapter.mjs";
const CAPTURED_ENV_KEYS = [
	"PI_ACP_PI_COMMAND",
	"CLAUDE_CODE_DISABLE_CWD_PERSIST",
	"CLAUDE_CODE_DISABLE_DEV_NULL_REDIRECT",
	"CLAUDE_CODE_NODE_SHELL_WRAPPER",
	"CLAUDE_CODE_SHELL",
	"CLAUDE_CODE_SIMPLE_SHELL_EXEC",
	"CLAUDE_CODE_SWAP_STDIO",
	"SHELL",
	"OPENCODE_CONTEXTPATHS",
] as const;

const MOCK_ACP_ADAPTER = `
const capturedEnvKeys = ${JSON.stringify(CAPTURED_ENV_KEYS)};
let buffer = "";

process.stdin.resume();
process.stdin.on("data", (chunk) => {
  const text = chunk instanceof Uint8Array ? new TextDecoder().decode(chunk) : String(chunk);
  buffer += text;

  while (true) {
    const newlineIndex = buffer.indexOf("\\n");
    if (newlineIndex === -1) break;
    const line = buffer.slice(0, newlineIndex);
    buffer = buffer.slice(newlineIndex + 1);
    if (!line.trim()) continue;

    const msg = JSON.parse(line);
    if (msg.id === undefined) continue;

    let result;
    switch (msg.method) {
      case "initialize":
        result = {
          protocolVersion: 1,
          agentInfo: {
            name: "mock-adapter",
            version: "1.0.0",
            argv: process.argv.slice(2),
            env: Object.fromEntries(
              capturedEnvKeys.map((key) => [key, process.env[key] ?? null]),
            ),
          },
        };
        break;
      case "session/new":
      case "session/cancel":
        result = msg.method === "session/new" ? { sessionId: "mock-session-1" } : {};
        break;
      default:
        process.stdout.write(JSON.stringify({
          jsonrpc: "2.0",
          id: msg.id,
          error: { code: -32601, message: "Method not found" },
        }) + "\\n");
        continue;
    }

    process.stdout.write(JSON.stringify({
      jsonrpc: "2.0",
      id: msg.id,
      result,
    }) + "\\n");
  }
});
`;

type LaunchProbe = AgentInfo & {
	argv?: string[];
	env?: Partial<Record<(typeof CAPTURED_ENV_KEYS)[number], string | null>>;
};

function useMockAdapterBin(vm: AgentOs, scriptPath: string): () => void {
	const withPrivateResolver = vm as AgentOs & {
		_resolveAdapterBin: (pkg: string) => string;
	};
	const originalResolve = withPrivateResolver._resolveAdapterBin;
	withPrivateResolver._resolveAdapterBin = () => scriptPath;
	return () => {
		withPrivateResolver._resolveAdapterBin = originalResolve;
	};
}

async function inspectLaunch(
	agentType: string,
	software: SoftwareInput[],
): Promise<LaunchProbe> {
	const vm = await AgentOs.create({
		moduleAccessCwd: MODULE_ACCESS_CWD,
		software,
	});
	let sessionId: string | undefined;
	const restore = useMockAdapterBin(vm, MOCK_ADAPTER_PATH);

	try {
		await vm.writeFile(MOCK_ADAPTER_PATH, MOCK_ACP_ADAPTER);
		sessionId = (await vm.createSession(agentType)).sessionId;
		return vm.getSessionAgentInfo(sessionId) as LaunchProbe;
	} finally {
		restore();
		if (sessionId) {
			vm.closeSession(sessionId);
		}
		await vm.dispose();
	}
}

describe("agent launch args and env", () => {
	test("Pi SDK injects the system prompt flag and resolved pi binary", async () => {
		const agentInfo = await inspectLaunch("pi", [pi]);

		expect(agentInfo.argv).toContain("--append-system-prompt");
		expect(agentInfo.env?.PI_ACP_PI_COMMAND).toContain(
			"@mariozechner/pi-coding-agent",
		);
	});

	test("Pi CLI injects the system prompt flag and resolved pi binary", async () => {
		const agentInfo = await inspectLaunch("pi-cli", [piCli]);

		expect(agentInfo.argv).toContain("--append-system-prompt");
		expect(agentInfo.env?.PI_ACP_PI_COMMAND).toContain(
			"@mariozechner/pi-coding-agent",
		);
	});

	test("Claude injects shell-safe launch env defaults", async () => {
		const agentInfo = await inspectLaunch("claude", [claude]);

		expect(agentInfo.argv).toContain("--append-system-prompt");
		expect(agentInfo.env).toMatchObject({
			CLAUDE_CODE_DISABLE_CWD_PERSIST: "1",
			CLAUDE_CODE_DISABLE_DEV_NULL_REDIRECT: "1",
			CLAUDE_CODE_NODE_SHELL_WRAPPER: "1",
			CLAUDE_CODE_SHELL: "/bin/sh",
			CLAUDE_CODE_SIMPLE_SHELL_EXEC: "1",
			CLAUDE_CODE_SWAP_STDIO: "0",
			SHELL: "/bin/sh",
		});
	});

	test("OpenCode passes instruction paths through OPENCODE_CONTEXTPATHS", async () => {
		const agentInfo = await inspectLaunch("opencode", [opencode]);
		const contextPaths = JSON.parse(
			agentInfo.env?.OPENCODE_CONTEXTPATHS ?? "[]",
		) as string[];

		expect(agentInfo.argv ?? []).not.toContain("--append-system-prompt");
		expect(contextPaths).toContain("/etc/agentos/instructions.md");
	});

	test("Codex injects developer instructions through launch args", async () => {
		const agentInfo = await inspectLaunch("codex", [codex]);

		expect(agentInfo.argv).toContain("--append-developer-instructions");
	});
});
