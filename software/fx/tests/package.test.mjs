import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { readFileSync } from "node:fs";
import { createServer } from "node:http";
import { resolve } from "node:path";
import test from "node:test";

const packageDir = resolve(import.meta.dirname, "..");
const promptSentinel = "FX_AGENTOS_PROMPT_SENTINEL";

function json(res, status, value) {
	const body = JSON.stringify(value);
	res.writeHead(status, {
		"content-type": "application/json",
		"content-length": Buffer.byteLength(body),
	});
	res.end(body);
}

async function body(req) {
	const chunks = [];
	for await (const chunk of req) chunks.push(Buffer.from(chunk));
	return Buffer.concat(chunks).toString("utf8");
}

async function startGateway() {
	const requests = [];
	const server = createServer(async (req, res) => {
		if (req.method === "GET") {
			json(res, 200, {
				object: "list",
				data: [{ id: "sdk/test-model", type: "language", released: 1, tags: ["tool-use"] }],
			});
			return;
		}
		if (req.method !== "POST") {
			json(res, 405, { error: "method_not_allowed" });
			return;
		}
		requests.push(await body(req));
		res.writeHead(200, { "content-type": "text/event-stream" });
		if (requests.length === 1) {
			res.write(
				'data: {"type":"tool-call","toolCallId":"workspace-check","toolName":"terminal","input":{"action":"exec","command":"node -e \'if(process.env.AI_GATEWAY_API_KEY)process.exit(9);process.stdout.write(\\"fx-sdk-workspace-ok\\"+\\"x\\".repeat(70000))\'"}}\n',
			);
			res.write(
				'data: {"type":"finish","finishReason":{"unified":"tool-calls"},"usage":{"inputTokens":{"total":2},"outputTokens":{"total":3}}}\n',
			);
		} else {
			res.write('data: {"type":"text-delta","delta":"hello from fx"}\n');
			res.write(
				'data: {"type":"finish","finishReason":{"unified":"stop"},"usage":{"inputTokens":{"total":4},"outputTokens":{"total":3}}}\n',
			);
		}
		res.end("data: [DONE]\n");
	});
	await new Promise((resolve, reject) => {
		server.once("error", reject);
		server.listen(0, "127.0.0.1", resolve);
	});
	const address = server.address();
	assert(address && typeof address !== "string");
	return {
		requests,
		url: `http://127.0.0.1:${address.port}`,
		close: () => new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve())),
	};
}

function startAcp(env) {
	const child = spawn(process.execPath, ["--experimental-wasm-jspi", resolve(packageDir, "dist/adapter.js")], {
		cwd: packageDir,
		env: { ...process.env, ...env },
		stdio: ["pipe", "pipe", "pipe"],
	});
	let stdout = "";
	let stderr = "";
	let nextId = 1;
	const responses = new Map();
	const updates = [];
	child.stderr.setEncoding("utf8").on("data", (chunk) => { stderr += chunk; });
	child.stdout.setEncoding("utf8").on("data", (chunk) => {
		stdout += chunk;
		for (;;) {
			const newline = stdout.indexOf("\n");
			if (newline < 0) break;
			const message = JSON.parse(stdout.slice(0, newline));
			stdout = stdout.slice(newline + 1);
			if (message.id !== undefined && responses.has(message.id)) {
				responses.get(message.id)(message);
				responses.delete(message.id);
			} else {
				updates.push(message);
			}
		}
	});
	return {
		child,
		stderr: () => stderr,
		updates,
		request(method, params = {}) {
			const id = nextId++;
			const response = new Promise((resolve) => responses.set(id, resolve));
			child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`);
			return response;
		},
	};
}

test("ships pinned FX WebAssembly and speaks ACP through the adapter", async () => {
	const wasm = readFileSync(resolve(packageDir, "dist/fx-core.wasm"));
	assert.equal(WebAssembly.validate(wasm), true);
	const metadata = JSON.parse(readFileSync(resolve(packageDir, "dist/fx-upstream.json"), "utf8"));
	assert.equal(metadata.sourceCommit, "8a61ffa670d80729a2ff1e04d5605d8effae6170");
	assert.equal(metadata.zigVersion, "0.16.0");

	const gateway = await startGateway();
	const acp = startAcp({
		ACP_APPEND_SYSTEM_PROMPT: promptSentinel,
		AI_GATEWAY_API_KEY: "test-key",
		FX_GATEWAY_BASE_URL: gateway.url,
		FX_GATEWAY_CHAT_URL: `${gateway.url}/v3/ai/language-model`,
	});
	try {
		const initialized = await acp.request("initialize", { protocolVersion: 1, clientCapabilities: {} });
		assert.equal(initialized.result.agentInfo.name, "fx");
		assert.equal(initialized.result.agentCapabilities.loadSession, true);

		const created = await acp.request("session/new");
		assert.ok(created.result.sessionId);
		const prompted = await acp.request("session/prompt", {
			sessionId: created.result.sessionId,
			prompt: [{ type: "text", text: "Say hello." }],
		});
		assert.equal(
			prompted.result.stopReason,
			"end_turn",
			`${acp.stderr()}\nupdates=${JSON.stringify(acp.updates)}\nrequests=${JSON.stringify(gateway.requests)}`,
		);
		assert.ok(acp.updates.some((message) => JSON.stringify(message).includes("hello from fx")));
		assert.ok(gateway.requests.some((request) => request.includes(promptSentinel)));
		assert.equal(gateway.requests.length, 2);
		assert.match(gateway.requests[1], /fx-sdk-workspace-ok/);
		assert.match(gateway.requests[1], /truncated/);
		assert.ok(gateway.requests[1].length < 100_000, "workspace output must stay bounded");

		acp.child.stdin.end();
		const [code] = await new Promise((resolve) => acp.child.once("close", (...args) => resolve(args)));
		assert.equal(code, 0, acp.stderr());
	} finally {
		if (!acp.child.killed && acp.child.exitCode === null) acp.child.kill("SIGTERM");
		await gateway.close();
	}
});
