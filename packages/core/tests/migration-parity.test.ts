import { createServer, type IncomingMessage } from "node:http";
import { resolve } from "node:path";
import { moduleAccessMounts } from "./helpers/node-modules-mount.js";
import { afterEach, describe, expect, test } from "vitest";
import { z } from "zod";
import { AgentOs, binding, bindings } from "../src/index.js";
import type { SessionStreamEntry } from "../src/session-api.js";
import { createProjectedAgentPackage } from "./helpers/projected-agent-package.js";
import { promptResultText } from "./helpers/session-result.js";

const MODULE_ACCESS_CWD = resolve(import.meta.dirname, "..");
const textDecoder = new TextDecoder();
const MOCK_ACP_ADAPTER = `
let buffer = "";

function writeMessage(message) {
  process.stdout.write(JSON.stringify(message) + "\\n");
}

function writeResponse(id, result) {
  writeMessage({
    jsonrpc: "2.0",
    id,
    result,
  });
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
            name: "mock-migration-parity-agent",
            version: "1.0.0",
          },
          agentCapabilities: {
            plan_mode: false,
            tool_calls: false,
            promptCapabilities: {},
          },
          modes: {
            currentModeId: "default",
            availableModes: [{ id: "default", label: "Default" }],
          },
          configOptions: [],
        });
        break;
      case "session/new":
        writeResponse(msg.id, {
          sessionId: "mock-session-1",
          modes: {
            currentModeId: "default",
            availableModes: [{ id: "default", label: "Default" }],
          },
          configOptions: [],
        });
        break;
      case "session/prompt":
        writeMessage({
          jsonrpc: "2.0",
          method: "session/update",
          params: {
            sessionId: "mock-session-1",
            update: {
              sessionUpdate: "agent_message_chunk",
              content: {
                type: "text",
                text: "mock-parity-flow-ok",
              },
            },
          },
        });
        writeResponse(msg.id, { stopReason: "end_turn" });
        // OpenCode can resolve session/prompt just before its final tool update.
        // The transport must keep the completed turn attached long enough to
        // stream and durably record that trailing notification.
        setTimeout(() => writeMessage({
          jsonrpc: "2.0",
          method: "session/update",
          params: {
            sessionId: "mock-session-1",
            update: {
              sessionUpdate: "tool_call",
              toolCallId: "synthetic-tool-1",
              title: "synthetic-tool",
              kind: "other",
              status: "completed",
            },
          },
        }), 20);
        break;
      case "session/cancel":
        writeResponse(msg.id, {});
        break;
      default:
        writeMessage({
          jsonrpc: "2.0",
          id: msg.id,
          error: {
            code: -32601,
            message: "Method not found",
            data: { method: msg.method },
          },
        });
        break;
    }
  }
});
`.trim();

const mathBindings = bindings({
	name: "math",
	description: "Math utilities",
	bindings: {
		add: binding({
			description: "Add two numbers",
			inputSchema: z.object({
				a: z.number(),
				b: z.number(),
			}),
			execute: ({ a, b }) => ({ sum: a + b }),
		}),
	},
});

function assertNativeSidecar(vm: AgentOs): void {
	expect(vm.sidecar.describe()).toMatchObject({
		state: "ready",
	});
	expect("kernel" in (vm as unknown as Record<string, unknown>)).toBe(false);
	expect(
		(vm as unknown as Record<string, unknown>).kernel,
	).toBeUndefined();
}

async function runSpawnedProcess(
	vm: AgentOs,
	command: string,
	args: string[],
): Promise<{ exitCode: number; stdout: string; stderr: string }> {
	return vm.execArgv(command, args);
}

function getRequestPath(req: IncomingMessage): string {
	return req.url ?? "/";
}

describe("native sidecar migration parity gate", () => {
	const cleanups = new Set<() => Promise<void>>();

	afterEach(async () => {
		for (const stop of cleanups) {
			await stop();
		}
		cleanups.clear();
	});

	test("covers filesystem, process execution, and reusable layer snapshots on the Rust sidecar path", async () => {
		const vm = await AgentOs.create({
			defaultSoftware: false,
			permissions: {
				fs: "allow",
				childProcess: "allow",
			},
		});
		cleanups.add(async () => {
			await vm.dispose();
		});
		assertNativeSidecar(vm);

		await vm.mkdir("/workspace", { recursive: true });
		await vm.writeFile("/workspace/source.txt", "filesystem-ok");

		const processResult = await runSpawnedProcess(vm, "node", [
			"-e",
			[
				'const fs = require("node:fs");',
				'const input = fs.readFileSync("/workspace/source.txt", "utf8");',
				'fs.writeFileSync("/workspace/process.txt", `${input}:process-ok`);',
				'console.log(JSON.stringify({ input, wrote: "/workspace/process.txt" }));',
			].join("\n"),
		]);

		expect(processResult.exitCode).toBe(0);
		expect(processResult.stderr).toBe("");
		expect(JSON.parse(processResult.stdout.trim())).toEqual({
			input: "filesystem-ok",
			wrote: "/workspace/process.txt",
		});
		expect(
			textDecoder.decode(await vm.readFile("/workspace/process.txt")),
		).toBe("filesystem-ok:process-ok");

		const snapshot = await vm.exportRootFilesystem({
			maxBytes: 64 * 1024 * 1024,
		});
		const clonedVm = await AgentOs.create({
			defaultSoftware: false,
			rootFilesystem: {
				disableDefaultBaseLayer: true,
				lowers: [snapshot],
			},
			permissions: {
				fs: "allow",
			},
		});
		cleanups.add(async () => {
			await clonedVm.dispose();
		});
		assertNativeSidecar(clonedVm);

		expect(
			textDecoder.decode(await clonedVm.readFile("/workspace/process.txt")),
		).toBe("filesystem-ok:process-ok");
		expect(
			textDecoder.decode(await clonedVm.readFile("/workspace/source.txt")),
		).toBe("filesystem-ok");
	}, 60_000);

	test("covers registered bindings through guest command dispatch on the Rust sidecar path", async () => {
		const vm = await AgentOs.create({
			defaultSoftware: false,
			bindings: [mathBindings],
			permissions: {
				fs: "allow",
				childProcess: "allow",
				binding: "allow",
			},
		});
		cleanups.add(async () => {
			await vm.dispose();
		});
		assertNativeSidecar(vm);

		const listed = await runSpawnedProcess(vm, "agentos", ["list-bindings"]);
		expect(listed.exitCode).toBe(0);
		expect(JSON.parse(listed.stdout)).toEqual({
			ok: true,
			result: {
				bindings: [
					{
						name: "math",
						description: "Math utilities",
						bindings: ["add"],
					},
				],
			},
		});

		const result = await runSpawnedProcess(vm, "agentos-math", [
			"add",
			"--a",
			"8",
			"--b",
			"13",
		]);
		expect(result.exitCode, result.stderr).toBe(0);
		expect(JSON.parse(result.stdout)).toEqual({
			ok: true,
			result: { sum: 21 },
		});
	}, 60_000);

	test("covers guest loopback networking through the Rust sidecar path", async () => {
		const server = createServer((req, res) => {
			res.writeHead(200, { "content-type": "application/json" });
			res.end(
				JSON.stringify({
					ok: true,
					path: getRequestPath(req),
				}),
			);
		});
		await new Promise<void>((resolveListen) => {
			server.listen(0, "127.0.0.1", () => resolveListen());
		});
		cleanups.add(
			async () =>
				await new Promise<void>((resolveClose, reject) => {
					server.close((error) => {
						if (error) {
							reject(error);
							return;
						}
						resolveClose();
					});
				}),
		);

		const address = server.address();
		if (!address || typeof address === "string") {
			throw new Error("host fixture did not expose a TCP port");
		}

		const vm = await AgentOs.create({
			defaultSoftware: false,
			loopbackExemptPorts: [address.port],
			permissions: {
				fs: "allow",
				childProcess: "allow",
				network: "allow",
			},
		});
		cleanups.add(async () => {
			await vm.dispose();
		});
		assertNativeSidecar(vm);

		const result = await runSpawnedProcess(vm, "node", [
			"-e",
			[
				"async function main() {",
				`  const response = await fetch("http://127.0.0.1:${address.port}/parity");`,
				"  const body = await response.json();",
				"  console.log(JSON.stringify(body));",
				"}",
				"main().catch((error) => {",
				"  console.error(error);",
				"  process.exit(1);",
				"});",
			].join("\n"),
		]);

		expect(result.exitCode, result.stderr).toBe(0);
		expect(result.stderr).toBe("");
		expect(JSON.parse(result.stdout.trim())).toEqual({
			ok: true,
			path: "/parity",
		});
	}, 60_000);

	test("covers session lifecycle and agent prompt flow on the Rust sidecar path", async () => {
		const agentPackage = createProjectedAgentPackage({
			name: "migration-parity",
			adapterScript: MOCK_ACP_ADAPTER,
		});
		cleanups.add(async () => {
			agentPackage.cleanup();
		});

		const vm = await AgentOs.create({
			mounts: moduleAccessMounts(MODULE_ACCESS_CWD),
			defaultSoftware: false,
			software: [agentPackage.software],
			permissions: {
				fs: "allow",
				childProcess: "allow",
				network: "allow",
			},
		});
		cleanups.add(async () => {
			await vm.dispose();
		});
		assertNativeSidecar(vm);

		const sessionId = "migration-parity";
		await vm.openSession({ sessionId, agent: "migration-parity" });

		const events: SessionStreamEntry[] = [];
		const unsubscribeEvents = vm.onSessionEvent(sessionId, (event) => {
			events.push(event);
		});
		const result = await vm.prompt({
			sessionId,
			content: [
				{ type: "text", text: "Run the migration parity prompt flow." },
			],
		});
		unsubscribeEvents();

		expect(result.stopReason).toBe("end_turn");
		expect(promptResultText(result)).toContain("mock-parity-flow-ok");

		expect(events.some((event) => event.type === "tool_call")).toBe(true);
		const history = await vm.readHistory({
			sessionId,
			after: 0,
			limit: 100,
		});
		expect(history.events.some((event) => event.type === "tool_call")).toBe(
			true,
		);

		await vm.deleteSession({ sessionId });
	}, 120_000);
});
