import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { resolve } from "node:path";
import { Readable, Writable } from "node:stream";
import test from "node:test";
import { ClientSideConnection, PROTOCOL_VERSION, ndJsonStream } from "@agentclientprotocol/sdk";

const packageDir = resolve(import.meta.dirname, "..");
const adapterPath = resolve(packageDir, "dist", "adapter.js");

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

test("native upstream OpenCode ACP initializes and creates a session over stdio", async () => {
	const child = spawn(process.execPath, [adapterPath], {
		cwd: packageDir,
		env: {
			...process.env,
			OPENCODE_DISABLE_AUTOUPDATE: "1",
			OPENCODE_DISABLE_FILEWATCHER: "1",
			OPENCODE_DISABLE_LSP_DOWNLOAD: "1",
			XDG_DATA_HOME: resolve(packageDir, "node_modules/.cache/opencode-test/data"),
			XDG_CACHE_HOME: resolve(packageDir, "node_modules/.cache/opencode-test/cache"),
			XDG_CONFIG_HOME: resolve(packageDir, "node_modules/.cache/opencode-test/config"),
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
		ndJsonStream(Writable.toWeb(child.stdin), Readable.toWeb(child.stdout)),
	);

	try {
		let initializeTimeout;
		const timeout = new Promise((_, reject) => {
			initializeTimeout = setTimeout(
				() => reject(new Error(`initialize timed out: ${stderr}`)),
				20_000,
			);
			initializeTimeout.unref();
		});
		const result = await Promise.race([
			connection.initialize({
				protocolVersion: PROTOCOL_VERSION,
				clientCapabilities: {},
				clientInfo: { name: "agentos-test", version: "0.0.1" },
			}),
			timeout,
		]);
		clearTimeout(initializeTimeout);
		assert.equal(result.protocolVersion, PROTOCOL_VERSION);
		assert.equal(result.agentInfo?.name, "OpenCode");
		assert.deepEqual(result.agentCapabilities?.sessionCapabilities?.list, {});
		assert.deepEqual(result.agentCapabilities?.sessionCapabilities?.resume, {});
		assert.deepEqual(result.agentCapabilities?.sessionCapabilities?.close, {});

		let sessionTimeout;
		let session;
		try {
			session = await Promise.race([
				connection.newSession({ cwd: packageDir, mcpServers: [] }),
				new Promise((_, reject) => {
					sessionTimeout = setTimeout(
						() => reject(new Error(`session/new timed out: ${stderr}`)),
						20_000,
					);
					sessionTimeout.unref();
				}),
			]);
		} catch (error) {
			throw new Error(`session/new failed: ${error?.stack ?? error}\n${stderr}`, {
				cause: error,
			});
		}
		clearTimeout(sessionTimeout);
		assert.equal(typeof session.sessionId, "string");
		assert.notEqual(session.sessionId.length, 0);
		const model = session.configOptions?.find((option) => option.id === "model");
		assert.equal(model?.type, "select");
		const modelValues = new Set(model.options.map((option) => option.value));
		const qualified = model.options.find((option) => {
			const separator = option.value.lastIndexOf("/");
			return separator > 0 && modelValues.has(option.value.slice(0, separator));
		});
		assert.ok(
			qualified,
			"OpenCode ACP should expose variant-qualified model values for per-model discovery",
		);
		const selected = await connection.setSessionConfigOption({
			sessionId: session.sessionId,
			configId: "model",
			value: qualified.value,
		});
		assert.equal(
			selected.configOptions.find((option) => option.id === "model")
				?.currentValue,
			qualified.value,
		);
		assert.ok(
			selected.configOptions.some(
				(option) => option.category === "thought_level",
			),
		);
	} finally {
		if (child.exitCode === null && child.signalCode === null) child.kill("SIGTERM");
		await Promise.race([once(child, "exit"), new Promise((resolve) => setTimeout(resolve, 2_000))]);
		assert.equal(
			child.exitCode === 0 || child.signalCode === "SIGTERM",
			true,
			`OpenCode ACP exited unexpectedly: ${stderr}`,
		);
	}
});
