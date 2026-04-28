import { createServer, type IncomingMessage } from "node:http";
import { resolve } from "node:path";
import common from "@rivet-dev/agent-os-common";
import codexAgent from "@rivet-dev/agent-os-codex-agent";
import { afterEach, describe, expect, test } from "vitest";
import { z } from "zod";
import { AgentOs, hostTool, toolKit } from "../src/index.js";

const MODULE_ACCESS_CWD = resolve(import.meta.dirname, "..");
const MOCK_ADAPTER_PATH = "/tmp/mock-migration-parity-adapter.mjs";
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
                text: "mock-parity-flow-ok",
              },
            },
          },
        });
        writeMessage({
          jsonrpc: "2.0",
          method: "session/update",
          params: {
            sessionId: "mock-session-1",
            update: {
              sessionUpdate: "tool_call",
              title: "synthetic-tool",
              status: "completed",
            },
          },
        });
        writeMessage({
          jsonrpc: "2.0",
          method: "session/update",
          params: {
            sessionId: "mock-session-1",
            update: {
              sessionUpdate: "completed",
              stopReason: "end_turn",
            },
          },
        });
        writeResponse(msg.id, { stopReason: "end_turn" });
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

const mathToolKit = toolKit({
	name: "math",
	description: "Math utilities",
	tools: {
		add: hostTool({
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
	expect("kernel" in (vm as Record<string, unknown>)).toBe(false);
	expect((vm as Record<string, unknown>).kernel).toBeUndefined();
}

async function runSpawnedProcess(
	vm: AgentOs,
	command: string,
	args: string[],
): Promise<{ exitCode: number; stdout: string; stderr: string }> {
	const stdoutChunks: string[] = [];
	const stderrChunks: string[] = [];
	const { pid } = vm.spawn(command, args, {
		onStdout: (chunk) => {
			stdoutChunks.push(textDecoder.decode(chunk));
		},
		onStderr: (chunk) => {
			stderrChunks.push(textDecoder.decode(chunk));
		},
	});

	return {
		exitCode: await vm.waitProcess(pid),
		stdout: stdoutChunks.join(""),
		stderr: stderrChunks.join(""),
	};
}

function getRequestPath(req: IncomingMessage): string {
	return req.url ?? "/";
}

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
			software: [common],
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

		const snapshot = await vm.snapshotRootFilesystem();
		const clonedVm = await AgentOs.create({
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

	test("covers registered host tools through guest command dispatch on the Rust sidecar path", async () => {
		const vm = await AgentOs.create({
			software: [common],
			toolKits: [mathToolKit],
			permissions: {
				fs: "allow",
				childProcess: "allow",
				tool: "allow",
			},
		});
		cleanups.add(async () => {
			await vm.dispose();
		});
		assertNativeSidecar(vm);

		const listed = await runSpawnedProcess(vm, "agentos", ["list-tools"]);
		expect(listed.exitCode).toBe(0);
		expect(JSON.parse(listed.stdout)).toEqual({
			ok: true,
			result: {
				toolkits: [
					{
						name: "math",
						description: "Math utilities",
						tools: ["add"],
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
		expect(result.exitCode).toBe(0);
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

		expect(result.exitCode).toBe(0);
		expect(result.stderr).toBe("");
		expect(JSON.parse(result.stdout.trim())).toEqual({
			ok: true,
			path: "/parity",
		});
	}, 60_000);

	test("covers session lifecycle and agent prompt flow on the Rust sidecar path", async () => {
		const vm = await AgentOs.create({
			moduleAccessCwd: MODULE_ACCESS_CWD,
			software: [codexAgent],
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

		const restoreAdapter = useMockAdapterBin(vm, MOCK_ADAPTER_PATH);
		cleanups.add(async () => {
			restoreAdapter();
		});
		await vm.writeFile(MOCK_ADAPTER_PATH, MOCK_ACP_ADAPTER);

		const { sessionId } = await vm.createSession("codex");

		const { response, text } = await vm.prompt(
			sessionId,
			"Run the migration parity prompt flow.",
		);

		expect(response.error).toBeUndefined();
		expect((response.result as { stopReason?: string }).stopReason).toBe(
			"end_turn",
		);
		expect(text).toContain("mock-parity-flow-ok");

		const events = vm
			.getSessionEvents(sessionId)
			.map((entry) => entry.notification);
		expect(
			events.some(
				(event) =>
					event.method === "session/update" &&
					JSON.stringify(event.params).includes("tool_call"),
			),
		).toBe(true);
		expect(
			events.some(
				(event) =>
					event.method === "session/update" &&
					JSON.stringify(event.params).includes('"completed"'),
			),
		).toBe(true);

		await vm.destroySession(sessionId);
	}, 120_000);
});
