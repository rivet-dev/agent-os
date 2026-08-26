import {
	createServer,
	type IncomingMessage,
	type ServerResponse,
} from "node:http";
import fx from "@agentos-software/fx";
import sh from "@agentos-software/sh";
import { describe, expect, test } from "vitest";
import { AgentOs } from "../src/agent-os.js";
import { promptResultText } from "./helpers/session-result.js";

const instructionsSentinel = "FX_AGENTOS_INSTRUCTIONS_SENTINEL";

async function requestBody(req: IncomingMessage): Promise<string> {
	const chunks: Buffer[] = [];
	for await (const chunk of req) chunks.push(Buffer.from(chunk));
	return Buffer.concat(chunks).toString("utf8");
}

function writeJson(res: ServerResponse, value: unknown): void {
	const payload = JSON.stringify(value);
	res.writeHead(200, {
		"content-type": "application/json",
		"content-length": Buffer.byteLength(payload),
	});
	res.end(payload);
}

async function startFxGateway(): Promise<{
	port: number;
	requests: string[];
	url: string;
	close(): Promise<void>;
}> {
	const requests: string[] = [];
	const server = createServer(async (req, res) => {
		if (req.method === "GET") {
			writeJson(res, {
				object: "list",
				data: [
					{
						id: "sdk/test-model",
						type: "language",
						released: 1,
						tags: ["tool-use"],
					},
				],
			});
			return;
		}

		const body = await requestBody(req);
		requests.push(body);
		res.writeHead(200, { "content-type": "text/event-stream" });
		if (requests.length === 1) {
			res.write(
				`data: {"type":"tool-call","toolCallId":"write-marker","toolName":"terminal","input":{"action":"exec","command":"node -e 'if(process.env.AI_GATEWAY_API_KEY)process.exit(9);require(\\"node:fs\\").writeFileSync(\\"fx-marker.txt\\",\\"fx-workspace-ok\\")'"}}\n`,
			);
			res.write(
				'data: {"type":"finish","finishReason":{"unified":"tool-calls"},"usage":{"inputTokens":{"total":2},"outputTokens":{"total":3}}}\n',
			);
		} else {
			res.write('data: {"type":"text-delta","delta":"fx VM complete"}\n');
			res.write(
				'data: {"type":"finish","finishReason":{"unified":"stop"},"usage":{"inputTokens":{"total":4},"outputTokens":{"total":3}}}\n',
			);
		}
		res.end("data: [DONE]\n");
	});
	await new Promise<void>((resolve, reject) => {
		server.once("error", reject);
		server.listen(0, "127.0.0.1", resolve);
	});
	server.unref();
	const address = server.address();
	if (!address || typeof address === "string") {
		throw new Error("FX Gateway fixture did not expose a TCP port");
	}
	return {
		port: address.port,
		requests,
		url: `http://127.0.0.1:${address.port}`,
		close: () =>
			new Promise<void>((resolve, reject) => {
				server.close((error) => (error ? reject(error) : resolve()));
			}),
	};
}

describe("FX WebAssembly ACP integration", () => {
	test("runs a full prompt and workspace command inside the agentOS VM", async () => {
		const gateway = await startFxGateway();
		const vm = await AgentOs.create({
			defaultSoftware: false,
			loopbackExemptPorts: [gateway.port],
			software: [sh, fx],
		});
		const sessionId = "fx-wasm-agent";
		const workspace = "/home/agentos/workspace";
		let opened = false;
		try {
			await vm.mkdir(workspace, { recursive: true });
			await vm.openSession({
				sessionId,
				agent: "fx",
				cwd: workspace,
				permissionPolicy: "allow_all",
				additionalInstructions: instructionsSentinel,
				env: {
					AI_GATEWAY_API_KEY: "test-key",
					FX_GATEWAY_BASE_URL: gateway.url,
					FX_GATEWAY_CHAT_URL: `${gateway.url}/v3/ai/language-model`,
				},
			});
			opened = true;

			const info = await vm.getSessionAgentInfo({ sessionId });
			expect(info.name).toBe("fx");
			const result = await vm.prompt({
				sessionId,
				content: [{ type: "text", text: "Create the requested marker." }],
			});
			expect(result.stopReason).toBe("end_turn");
			expect(promptResultText(result)).toContain("fx VM complete");
			expect(
				new TextDecoder().decode(
					await vm.readFile(`${workspace}/fx-marker.txt`),
				),
			).toBe("fx-workspace-ok");
			expect(
				gateway.requests.some((request) =>
					request.includes(instructionsSentinel),
				),
			).toBe(true);
			expect(gateway.requests.length).toBe(2);
		} finally {
			if (opened) await vm.unloadSession({ sessionId });
			await vm.dispose();
			await gateway.close();
		}
	}, 120_000);
});
