import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { Readable, Writable } from "node:stream";
import test from "node:test";
import { fileURLToPath } from "node:url";
import {
	ClientSideConnection,
	PROTOCOL_VERSION,
	ndJsonStream,
} from "@agentclientprotocol/sdk";
import codex from "../dist/index.js";

const packageDir = join(dirname(fileURLToPath(import.meta.url)), "..");
const workspaceDir = join(packageDir, "..", "..");
const adapterPath = join(packageDir, "dist", "adapter.js");

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

async function withAdapter(codexHome, run) {
	const child = spawn(process.execPath, [adapterPath], {
		cwd: packageDir,
		env: { ...process.env, CODEX_HOME: codexHome },
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
		const initialization = await connection.initialize({
			protocolVersion: PROTOCOL_VERSION,
			clientCapabilities: {},
			clientInfo: { name: "agentos-test", version: "0.0.1" },
		});
		return await run(connection, initialization);
	} finally {
		if (child.exitCode === null && child.signalCode === null) child.kill("SIGTERM");
		await Promise.race([
			once(child, "exit"),
			new Promise((resolve) => setTimeout(resolve, 2_000)),
		]);
		assert.equal(
			child.exitCode === 0 || child.signalCode === "SIGTERM",
			true,
			`Codex ACP exited unexpectedly: ${stderr}`,
		);
	}
}

test("codex package exposes the session-turn ACP adapter and WASI agent", () => {
	const pkg = JSON.parse(readFileSync(join(packageDir, "package.json"), "utf8"));
	const manifest = JSON.parse(
		readFileSync(join(packageDir, "agentos-package.json"), "utf8"),
	);

	assert.equal(pkg.dependencies["@agentclientprotocol/sdk"], "1.2.1");
	assert.equal(pkg.bin["codex-acp"], "./dist/adapter.js");
	assert.equal(pkg.bin.codex, "./dist/codex.wasm");
	assert.equal(pkg.bin["codex-exec"], "./dist/codex-exec.wasm");
	assert.equal(manifest.name, "codex");
	assert.equal(manifest.agent.acpEntrypoint, "codex-acp");
	assert.equal(
		manifest.agent.env.CODEX_EXEC_COMMAND,
		"/opt/agentos/bin/codex-exec",
	);
	assert.equal(typeof codex.packagePath, "string");
});

test("reproducible Codex build links the AgentOS WASI libc", () => {
	const buildScript = readFileSync(
		join(workspaceDir, "toolchain", "scripts", "clone-and-build-codex-wasi.sh"),
		"utf8",
	);
	const makefile = readFileSync(
		join(workspaceDir, "toolchain", "Makefile"),
		"utf8",
	);

	assert.match(buildScript, /CODEX_PATCH_DIR="\$TOOLCHAIN_DIR\/std-patches\/codex"/);
	assert.match(buildScript, /AGENTOS_WASI_LIBDIR="\$TOOLCHAIN_DIR\/c\/sysroot\/lib\/wasm32-wasi"/);
	assert.match(buildScript, /-C link-self-contained=no/);
	assert.match(buildScript, /-C link-arg=\$AGENTOS_WASI_LIBDIR\/libc\.a/);
	assert.match(
		makefile,
		/codex: c\/vendor\/wasi-sdk\/bin\/clang c\/sysroot\/lib\/wasm32-wasi\/libc\.a/,
	);
});

test("Codex WASI uses the AgentOS-compatible standard shell tool", () => {
	const patch = readFileSync(
		join(
			workspaceDir,
			"toolchain",
			"std-patches",
			"codex",
			"0008-wasi-disable-unified-exec.patch",
		),
		"utf8",
	);
	const sandboxPatch = readFileSync(
		join(
			workspaceDir,
			"toolchain",
			"std-patches",
			"codex",
			"0006-session-turn-workspace-write.patch",
		),
		"utf8",
	);

	assert.match(patch, /"features\.unified_exec"\.to_string\(\)/);
	assert.match(patch, /toml::Value::Boolean\(false\)/);
	assert.match(sandboxPatch, /workspace-write/);
});

test("Codex WASI receives the selected reasoning effort", () => {
	const patch = readFileSync(
		join(
			workspaceDir,
			"toolchain",
			"std-patches",
			"codex",
			"0009-session-turn-reasoning-effort.patch",
		),
		"utf8",
	);

	assert.match(patch, /start\["reasoning_effort"\]/);
	assert.match(patch, /"model_reasoning_effort"\.to_string\(\)/);
	const effortPatch = readFileSync(
		join(
			workspaceDir,
			"toolchain",
			"std-patches",
			"codex",
			"0010-reasoning-effort-max-ultra.patch",
		),
		"utf8",
	);
	assert.match(effortPatch, /Max,/);
	assert.match(effortPatch, /Ultra,/);
});

test("Codex WASI consumes canonical message items without legacy duplication", () => {
	const patch = readFileSync(
		join(
			workspaceDir,
			"toolchain",
			"std-patches",
			"codex",
			"0011-session-turn-canonical-message-events.patch",
		),
		"utf8",
	);

	assert.match(patch, /EventMsg::AgentMessageContentDelta/);
	assert.match(patch, /EventMsg::ItemCompleted/);
	assert.match(patch, /MessagePhase::FinalAnswer/);
	assert.doesNotMatch(patch, /^\+\s*EventMsg::AgentMessage\(/m);
});

test("Codex ACP rejects unsupported rich media instead of silently dropping it", async () => {
	const codexHome = mkdtempSync(join(tmpdir(), "agentos-codex-media-"));
	try {
		await withAdapter(codexHome, async (connection, initialization) => {
			assert.deepEqual(initialization.agentCapabilities.promptCapabilities, {
				image: false,
				audio: false,
				embeddedContext: false,
			});
			const session = await connection.newSession({
				cwd: packageDir,
				mcpServers: [],
			});
			const unsupported = [
				{
					type: "image",
					data: "iVBORw0KGgo=",
					mimeType: "image/png",
				},
				{
					type: "audio",
					data: "UklGRg==",
					mimeType: "audio/wav",
				},
				{
					type: "resource",
					resource: {
						uri: "file:///workspace/context.txt",
						mimeType: "text/plain",
						text: "context",
					},
				},
				{
					type: "resource_link",
					uri: "https://example.test/reference.txt",
					name: "reference.txt",
				},
			];
			for (const content of unsupported) {
				await assert.rejects(
					connection.prompt({
						sessionId: session.sessionId,
						prompt: [content],
					}),
					new RegExp(`${content.type} prompt content`),
				);
			}
			await connection.closeSession({ sessionId: session.sessionId });
		});
	} finally {
		rmSync(codexHome, { recursive: true, force: true });
	}
});

test("Codex ACP resumes a session after its adapter process restarts", async () => {
	const codexHome = mkdtempSync(join(tmpdir(), "agentos-codex-home-"));
	try {
		const sessionId = await withAdapter(codexHome, async (connection) => {
			const session = await connection.newSession({
				cwd: packageDir,
				mcpServers: [],
			});
			const model = session.configOptions?.find((option) => option.id === "model");
			assert.equal(model?.type, "select");
			assert.deepEqual(
					model.options
						.map((option) => option.value)
						.filter((value) => !value.includes("/")),
				[
					"gpt-5.6-sol",
					"gpt-5.6-terra",
					"gpt-5.6-luna",
					"gpt-5.5",
					"gpt-5.3-codex-spark",
				],
				);
				assert.deepEqual(
					model.options
						.filter((option) =>
							option.value.startsWith("gpt-5.6-luna/"),
						)
						.map((option) => option.value),
					[
						"gpt-5.6-luna/low",
						"gpt-5.6-luna/medium",
						"gpt-5.6-luna/high",
						"gpt-5.6-luna/xhigh",
						"gpt-5.6-luna/max",
					],
				);
			const effort = session.configOptions?.find(
				(option) => option.id === "reasoning_effort",
			);
			assert.equal(effort?.currentValue, "low");
			assert.deepEqual(
				effort.options.map((option) => option.value),
				["low", "medium", "high", "xhigh", "max", "ultra"],
			);
			const selected = await connection.setSessionConfigOption({
				sessionId: session.sessionId,
				configId: "model",
				value: "gpt-5.6-luna/max",
			});
			assert.equal(
				selected.configOptions.find((option) => option.id === "model")
					?.currentValue,
				"gpt-5.6-luna",
			);
			assert.deepEqual(
				selected.configOptions
					.find((option) => option.id === "reasoning_effort")
					?.options.map((option) => option.value),
				["low", "medium", "high", "xhigh", "max"],
			);
			await connection.setSessionConfigOption({
				sessionId: session.sessionId,
				configId: "reasoning_effort",
				value: "high",
			});
			await connection.setSessionMode({
				sessionId: session.sessionId,
				modeId: "plan",
			});
			return session.sessionId;
		});

		await withAdapter(codexHome, async (connection) => {
			const resumed = await connection.resumeSession({
				sessionId,
				cwd: packageDir,
				mcpServers: [],
			});
			assert.equal(resumed.modes?.currentModeId, "plan");
			assert.equal(
				resumed.configOptions?.find(
					(option) => option.id === "reasoning_effort",
				)?.currentValue,
				"high",
			);
			await connection.closeSession({ sessionId });
		});

		await assert.rejects(
			withAdapter(codexHome, (connection) =>
				connection.resumeSession({
					sessionId,
					cwd: packageDir,
					mcpServers: [],
				}),
			),
			/unknown session/,
		);
	} finally {
		rmSync(codexHome, { recursive: true, force: true });
	}
});
