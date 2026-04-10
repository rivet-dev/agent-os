import { resolve } from "node:path";
import codex from "@rivet-dev/agent-os-codex-agent";
import { describe, expect, test } from "vitest";
import { AgentOs } from "../src/agent-os.js";
import type { SoftwareInput } from "../src/packages.js";

const MODULE_ACCESS_CWD = resolve(import.meta.dirname, "..");
const MOCK_ADAPTER_PATH = "/tmp/mock-synthetic-session-updates-adapter.mjs";

const MOCK_ACP_ADAPTER = `
let buffer = "";

const sessionState = {
  modeId: "default",
  configOptions: [
    {
      id: "model",
      category: "model",
      label: "Model",
      currentValue: "gpt-5-codex",
    },
    {
      id: "thought_level",
      category: "thought_level",
      label: "Thought Level",
      currentValue: "medium",
    },
  ],
};

function writeResponse(id, result) {
  process.stdout.write(JSON.stringify({
    jsonrpc: "2.0",
    id,
    result,
  }) + "\\n");
}

function writeError(id, message, data) {
  process.stdout.write(JSON.stringify({
    jsonrpc: "2.0",
    id,
    error: {
      code: -32602,
      message,
      ...(data ? { data } : {}),
    },
  }) + "\\n");
}

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

    switch (msg.method) {
      case "initialize":
        writeResponse(msg.id, {
          protocolVersion: 1,
          agentInfo: {
            name: "mock-no-update-agent",
            version: "1.0.0",
          },
          agentCapabilities: {
            plan_mode: true,
            tool_calls: false,
            promptCapabilities: {},
          },
          modes: {
            currentModeId: sessionState.modeId,
            availableModes: [
              { id: "default", label: "Default" },
              { id: "plan", label: "Plan" },
            ],
          },
          configOptions: sessionState.configOptions,
        });
        break;
      case "session/new":
        writeResponse(msg.id, {
          sessionId: "mock-session-1",
          modes: {
            currentModeId: sessionState.modeId,
            availableModes: [
              { id: "default", label: "Default" },
              { id: "plan", label: "Plan" },
            ],
          },
          configOptions: sessionState.configOptions,
        });
        break;
      case "session/set_mode":
        sessionState.modeId = msg.params?.modeId ?? sessionState.modeId;
        writeResponse(msg.id, {});
        break;
      case "session/set_config_option": {
        const configId = msg.params?.configId;
        const value = msg.params?.value;
        if (typeof configId !== "string" || typeof value !== "string") {
          writeError(msg.id, "invalid config option params");
          break;
        }
        const option = sessionState.configOptions.find((entry) => entry.id === configId);
        if (!option) {
          writeError(msg.id, "unknown config option", { configId });
          break;
        }
        option.currentValue = value;
        writeResponse(msg.id, { configOptions: sessionState.configOptions });
        break;
      }
      case "session/cancel":
        writeResponse(msg.id, {});
        break;
      default:
        process.stdout.write(JSON.stringify({
          jsonrpc: "2.0",
          id: msg.id,
          error: { code: -32601, message: "Method not found", data: { method: msg.method } },
        }) + "\\n");
        break;
    }
  }
});
`;

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

async function createMockCodexVm(software: SoftwareInput[]): Promise<AgentOs> {
	return AgentOs.create({
		moduleAccessCwd: MODULE_ACCESS_CWD,
		software,
	});
}

describe("synthetic session/update compatibility", () => {
	test("surfaces synthetic mode and config updates when the ACP adapter omits notifications", async () => {
		const vm = await createMockCodexVm([codex]);
		const restore = useMockAdapterBin(vm, MOCK_ADAPTER_PATH);
		let sessionId: string | undefined;

		try {
			await vm.writeFile(MOCK_ADAPTER_PATH, MOCK_ACP_ADAPTER);
			sessionId = (await vm.createSession("codex")).sessionId;

			const receivedEvents: string[] = [];
			const unsubscribe = vm.onSessionEvent(sessionId, (event) => {
				if (event.method === "session/update") {
					receivedEvents.push(JSON.stringify(event.params));
				}
			});

			await vm.setSessionModel(sessionId, "gpt-5.4");
			await vm.setSessionThoughtLevel(sessionId, "high");
			await vm.setSessionMode(sessionId, "plan");
			unsubscribe();

			expect(vm.getSessionModes(sessionId)?.currentModeId).toBe("plan");
			const configOptions = vm.getSessionConfigOptions(sessionId);
			expect(
				configOptions.find((option) => option.category === "model")?.currentValue,
			).toBe("gpt-5.4");
			expect(
				configOptions.find((option) => option.category === "thought_level")
					?.currentValue,
			).toBe("high");

			const sessionUpdateEvents = vm
				.getSessionEvents(sessionId)
				.map((entry) => JSON.stringify(entry.notification.params));
			expect(
				sessionUpdateEvents.some((event) =>
					event.includes('"sessionUpdate":"current_mode_update"'),
				),
			).toBe(true);
			expect(
				sessionUpdateEvents.filter((event) =>
					event.includes('"sessionUpdate":"config_option_update"'),
				).length,
			).toBeGreaterThanOrEqual(2);
			expect(
				receivedEvents.some((event) =>
					event.includes('"sessionUpdate":"current_mode_update"'),
				),
			).toBe(true);
			expect(
				receivedEvents.filter((event) =>
					event.includes('"sessionUpdate":"config_option_update"'),
				).length,
			).toBeGreaterThanOrEqual(2);
		} finally {
			restore();
			if (sessionId) {
				vm.closeSession(sessionId);
			}
			await vm.dispose();
		}
	});
});
