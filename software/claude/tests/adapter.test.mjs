import test from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { Readable, Writable } from "node:stream";
import { resolve as resolvePath } from "node:path";
import {
	ClientSideConnection,
	PROTOCOL_VERSION,
	ndJsonStream,
} from "@agentclientprotocol/sdk";
import { LLMock } from "@copilotkit/llmock";

const packageDir = resolvePath(import.meta.dirname, "..");
const adapterPath = resolvePath(packageDir, "dist", "adapter.js");
const claudePath = resolvePath(packageDir, "dist", "claude-cli.mjs");

class TestClient {
	async writeTextFile() {
		return {};
	}

	async readTextFile() {
		return { content: "" };
	}

	async requestPermission() {
		return { outcome: { outcome: "cancelled" } };
	}

	async sessionUpdate() {}
}

async function withAdapter(run, extraEnv = {}) {
	const child = spawn(process.execPath, [adapterPath], {
		cwd: packageDir,
			env: {
			...process.env,
			CLAUDE_CODE_EXECUTABLE: claudePath,
				ANTHROPIC_API_KEY: "agentos-test-key",
				DISABLE_TELEMETRY: "1",
				...extraEnv,
		},
		stdio: ["pipe", "pipe", "pipe"],
	});
	let stderr = "";
	child.stderr.setEncoding("utf8");
	child.stderr.on("data", (chunk) => {
		stderr += chunk;
	});
	const connection = new ClientSideConnection(
		() => new TestClient(),
		ndJsonStream(
			Writable.toWeb(child.stdin),
			Readable.toWeb(child.stdout),
		),
	);
	try {
		return await run(connection, child, () => stderr);
	} finally {
		if (child.exitCode === null && child.signalCode === null) {
			child.kill("SIGTERM");
		}
		await Promise.race([
			once(child, "exit"),
			new Promise((resolve) => setTimeout(resolve, 2_000)),
		]);
		assert.equal(
			child.exitCode === 0 || child.signalCode === "SIGTERM",
			true,
			`Claude ACP exited unexpectedly: ${stderr}`,
		);
	}
}

test("published Claude Agent ACP command initializes over stdio", async () => {
	await withAdapter(async (connection) => {
		const result = await connection.initialize({
			protocolVersion: PROTOCOL_VERSION,
			clientCapabilities: {},
			clientInfo: { name: "agentos-test", version: "0.0.1" },
		});

		assert.equal(result.protocolVersion, PROTOCOL_VERSION);
		assert.equal(
			result.agentInfo?.name,
			"@agentclientprotocol/claude-agent-acp",
		);
		assert.deepEqual(result.agentCapabilities?.sessionCapabilities?.list, {});
		assert.deepEqual(result.agentCapabilities?.sessionCapabilities?.resume, {});
		assert.deepEqual(result.agentCapabilities?.sessionCapabilities?.close, {});
	});
});

test("published Claude Agent ACP creates a session with the packaged upstream CLI", async () => {
	await withAdapter(async (connection, _child, getStderr) => {
		await connection.initialize({
			protocolVersion: PROTOCOL_VERSION,
			clientCapabilities: {},
			clientInfo: { name: "agentos-test", version: "0.0.1" },
		});

		let timeout;
		const deadline = new Promise((_, reject) => {
			timeout = setTimeout(
				() => reject(new Error(`session/new timed out: ${getStderr()}`)),
				30_000,
			);
		});
		try {
			const session = await Promise.race([
				connection.newSession({ cwd: packageDir, mcpServers: [] }),
				deadline,
			]);
			assert.equal(typeof session.sessionId, "string");
			assert.notEqual(session.sessionId.length, 0);
			const models = session.configOptions?.find(
				(option) => option.id === "model",
			);
			assert.equal(models?.type, "select");
			assert.ok(models.options.length > 1);
			const modelValues = new Set(models.options.map((option) => option.value));
			const qualified = models.options.find((option) => {
					const separator = option.value.lastIndexOf("/");
					return separator > 0 && modelValues.has(option.value.slice(0, separator));
				});
			assert.ok(
				qualified,
				"Claude ACP should expose variant-qualified model values for per-model discovery",
			);
			const effort = session.configOptions?.find(
				(option) => option.id === "effort",
			);
			assert.equal(effort?.type, "select");
			assert.ok(effort.options.length > 1);
			const selected = await connection.setSessionConfigOption({
				sessionId: session.sessionId,
				configId: "model",
				value: qualified.value,
			});
			assert.equal(
				selected.configOptions.find((option) => option.id === "model")
					?.currentValue,
				qualified.value.slice(0, qualified.value.lastIndexOf("/")),
			);
			assert.equal(
				selected.configOptions.find((option) => option.id === "effort")
					?.currentValue,
				qualified.value.slice(qualified.value.lastIndexOf("/") + 1),
			);
		} finally {
			clearTimeout(timeout);
		}
	});
});

test("published Claude Agent ACP completes sequential prompts against LLMock", async () => {
	const mock = new LLMock({ port: 0, logLevel: "silent" });
	mock.addFixtures([
		{ match: { userMessage: "Reply with host-acp-first" }, response: { content: "host-acp-first" } },
		{ match: { userMessage: "Reply with host-acp-second" }, response: { content: "host-acp-second" } },
	]);
	const baseUrl = await mock.start();
	try {
		await withAdapter(
			async (connection) => {
				await connection.initialize({
					protocolVersion: PROTOCOL_VERSION,
					clientCapabilities: {},
					clientInfo: { name: "agentos-test", version: "0.0.1" },
				});
				const session = await connection.newSession({
					cwd: packageDir,
					mcpServers: [],
				});
				const first = await connection.prompt({
					sessionId: session.sessionId,
					prompt: [{ type: "text", text: "Reply with host-acp-first" }],
				});
				assert.equal(first.stopReason, "end_turn");
				const second = await connection.prompt({
					sessionId: session.sessionId,
					prompt: [{ type: "text", text: "Reply with host-acp-second" }],
				});
				assert.equal(second.stopReason, "end_turn");
				assert.ok(mock.getRequests().length >= 2);
			},
			{ ANTHROPIC_BASE_URL: baseUrl },
		);
	} finally {
		await mock.stop();
	}
});

test("published Claude Agent ACP prompts a second process while the first remains live", async () => {
	const mock = new LLMock({ port: 0, logLevel: "silent" });
	mock.addFixtures([
		{
			match: { userMessage: "Reply with second-live-process" },
			response: { content: "second-live-process" },
		},
	]);
	const baseUrl = await mock.start();
	try {
		await withAdapter(
			async (firstConnection) => {
				await firstConnection.initialize({
					protocolVersion: PROTOCOL_VERSION,
					clientCapabilities: {},
					clientInfo: { name: "agentos-test", version: "0.0.1" },
				});
				await firstConnection.newSession({ cwd: packageDir, mcpServers: [] });

				await withAdapter(
					async (secondConnection) => {
						await secondConnection.initialize({
							protocolVersion: PROTOCOL_VERSION,
							clientCapabilities: {},
							clientInfo: { name: "agentos-test", version: "0.0.1" },
						});
						const secondSession = await secondConnection.newSession({
							cwd: packageDir,
							mcpServers: [],
						});
						const result = await secondConnection.prompt({
							sessionId: secondSession.sessionId,
							prompt: [
								{ type: "text", text: "Reply with second-live-process" },
							],
						});
						assert.equal(result.stopReason, "end_turn");
					},
					{ ANTHROPIC_BASE_URL: baseUrl },
				);
			},
			{ ANTHROPIC_BASE_URL: baseUrl },
		);
	} finally {
		await mock.stop();
	}
});
