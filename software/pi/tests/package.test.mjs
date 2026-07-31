import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { readFile } from "node:fs/promises";
import test from "node:test";

test("Pi packages the commit-pinned rivet-dev ACP adapter and runtime closure", async () => {
	const manifest = JSON.parse(
		await readFile(new URL("../agentos-package.json", import.meta.url), "utf8"),
	);
	const packageJson = JSON.parse(
		await readFile(new URL("../package.json", import.meta.url), "utf8"),
	);
	const upstreamManifest = JSON.parse(
		await readFile(
			new URL("../dist/pi-acp-upstream.json", import.meta.url),
			"utf8",
		),
	);
	const adapterEntrypoint = await readFile(
		new URL("../dist/pi-acp/index.js", import.meta.url),
		"utf8",
	);
	const adapterPackageJson = JSON.parse(
		await readFile(
			new URL("../dist/pi-acp/package.json", import.meta.url),
			"utf8",
		),
	);
	const packagedMcpConfig = await readFile(
		new URL("../dist/package/node_modules/pi-mcp-adapter/config.ts", import.meta.url),
		"utf8",
	);

	assert.equal(manifest.agent.acpEntrypoint, "pi-acp");
	assert.equal(manifest.agent.runtime, undefined);
	assert.equal(manifest.agent.snapshot, undefined);
	assert.equal(manifest.agent.env.PI_ACP_PI_COMMAND, "/opt/agentos/bin/pi-agentos");
	assert.equal(manifest.agent.env.PI_ACP_PI_ENTRYPOINT, undefined);
	assert.equal(manifest.agent.env.PI_ACP_PI_EXTENSION, undefined);
	assert.equal(packageJson.bin["pi-acp"], "./dist/pi-acp/index.js");
	assert.equal(packageJson.bin["pi-agentos"], "./dist/pi-agentos.mjs");
	assert.equal(
		packageJson.bin.pi,
		"./node_modules/@earendil-works/pi-coding-agent/dist/cli.js",
	);
	assert.equal(
		packageJson.dependencies["@earendil-works/pi-coding-agent"],
		"0.83.0",
	);
	assert.equal(packageJson.dependencies["pi-acp"], undefined);
	assert.equal(packageJson.dependencies["pi-mcp-adapter"], "2.11.0");
	assert.equal(upstreamManifest.sourceRepository, "rivet-dev/pi-acp");
	assert.equal(
		upstreamManifest.sourceCommit,
		"87cb3ab06d9b7e781db9c9575755153b50b2ba90",
	);
	assert.equal(
		upstreamManifest.sourceTarballSha256,
		"85bc7e133d28e9d870ecad7aa3de9e6a17ffea142443a177d618597a56c72cd7",
	);
	assert.equal(upstreamManifest.sourcePackageVersion, "0.0.31");
	assert.equal(upstreamManifest.compatibilityPatches, undefined);
	assert.deepEqual(upstreamManifest.buildCommands, ["npm ci", "npm run build"]);
	assert.ok(adapterEntrypoint.startsWith("#!/usr/bin/env node"));
	assert.equal(adapterPackageJson.name, "pi-acp");
	assert.equal(adapterPackageJson.version, "0.0.31");
	assert.match(packagedMcpConfig, /export function loadMcpConfig/);
	assert.equal(packageJson.dependencies["@mariozechner/pi-coding-agent"], undefined);
});

test("Pi packages an AgentOS-owned external Codex auth extension", async (t) => {
	const extensionUrl = new URL(
		"../dist/package/node_modules/@agentos-software/pi/dist/extensions/codex-auth.mjs",
		import.meta.url,
	);
	const extension = await import(extensionUrl);
	const originalToken = process.env.OPENAI_CODEX_ACCESS_TOKEN;
	const originalAccountId = process.env.OPENAI_CODEX_ACCOUNT_ID;
	const accessToken = "external-codex-access-token";
	const accountId = "external-codex-account-id";
	let request;
	let registration;
	const captureFetch = async (input, init) => {
		request = { input, init };
		return new Response(null, { status: 204 });
	};
	process.env.OPENAI_CODEX_ACCESS_TOKEN = accessToken;
	process.env.OPENAI_CODEX_ACCOUNT_ID = accountId;
	t.after(() => {
		if (originalToken === undefined) delete process.env.OPENAI_CODEX_ACCESS_TOKEN;
		else process.env.OPENAI_CODEX_ACCESS_TOKEN = originalToken;
		if (originalAccountId === undefined) delete process.env.OPENAI_CODEX_ACCOUNT_ID;
		else process.env.OPENAI_CODEX_ACCOUNT_ID = originalAccountId;
		delete process.env.AGENTOS_CODEX_ACCOUNT_TOKEN;
	});

	extension.default({
		registerProvider(provider, config) {
			registration = { provider, config };
		},
	});

	assert.equal(registration.provider, "openai-codex");
	assert.equal(registration.config.api, "openai-codex-responses");
	assert.equal(registration.config.apiKey, "$AGENTOS_CODEX_ACCOUNT_TOKEN");
	assert.equal(typeof registration.config.streamSimple, "function");
	assert.equal(process.env.AGENTOS_CODEX_ACCOUNT_TOKEN, extension.createAccountToken(accountId));
	assert.doesNotMatch(process.env.AGENTOS_CODEX_ACCOUNT_TOKEN, /external-codex-access-token/);

	await extension.createCodexFetch(accessToken, accountId, captureFetch)(
		"https://chatgpt.com/backend-api/codex/responses",
		{
			headers: { authorization: "Bearer synthetic" },
		},
	);
	const headers = new Headers(request.init.headers);
	assert.equal(headers.get("authorization"), `Bearer ${accessToken}`);
	assert.equal(headers.get("chatgpt-account-id"), accountId);
	assert.doesNotMatch(
		await readFile(
			new URL(
				"../dist/package/node_modules/@earendil-works/pi-coding-agent/dist/core/http-dispatcher.js",
				import.meta.url,
			),
			"utf8",
		),
		/canInstallUndiciGlobals/,
	);
});

test("Codex auth resolves a fresh bound credential for every request", async () => {
	const extension = await import(
		new URL(
			"../dist/package/node_modules/@agentos-software/pi/dist/extensions/codex-auth.mjs",
			import.meta.url,
		)
	);
	let request;
	let resolution = 0;
	const dynamicFetch = extension.createDynamicCodexFetch(
		async () => ({
			accessToken: `access-token-${++resolution}`,
			accountId: `account-${resolution}`,
		}),
		async (input, init) => {
			request = { input, init };
			return new Response(null, { status: 204 });
		},
	);

	await dynamicFetch("https://chatgpt.com/backend-api/codex/responses");
	let headers = new Headers(request.init.headers);
	assert.equal(headers.get("authorization"), "Bearer access-token-1");
	assert.equal(headers.get("chatgpt-account-id"), "account-1");

	await dynamicFetch("https://chatgpt.com/backend-api/codex/responses");
	headers = new Headers(request.init.headers);
	assert.equal(headers.get("authorization"), "Bearer access-token-2");
	assert.equal(headers.get("chatgpt-account-id"), "account-2");
	assert.equal(resolution, 2);
	assert.deepEqual(
		extension.parseCodexCredential({
			ok: true,
			result: { accessToken: "bound-token", accountId: "bound-account" },
		}),
		{ accessToken: "bound-token", accountId: "bound-account" },
	);
	assert.throws(
		() => extension.parseCodexCredential('{"accessToken":"missing-account"}'),
		/invalid credential/,
	);
});

test("packaged pinned Pi adapter initializes with persistent session capabilities", async (t) => {
	const piCommand = new URL("../dist/package/bin/pi-agentos", import.meta.url).pathname;
	const child = spawn(
		new URL("../dist/package/bin/pi-acp", import.meta.url).pathname,
		[],
		{
			stdio: ["pipe", "pipe", "pipe"],
			env: {
				...process.env,
				PI_ACP_PI_COMMAND: piCommand,
				OPENAI_CODEX_ACCESS_TOKEN: "external-codex-access-token",
				OPENAI_CODEX_ACCOUNT_ID: "external-codex-account-id",
			},
		},
	);
	t.after(() => child.kill("SIGTERM"));

	let stderr = "";
	child.stderr.setEncoding("utf8");
	child.stderr.on("data", (chunk) => {
		stderr += chunk;
	});

	const response = await new Promise((resolve, reject) => {
		const timeout = setTimeout(
			() => reject(new Error(`initialize timed out: ${stderr}`)),
			5_000,
		);
		let buffer = "";
		child.stdout.setEncoding("utf8");
		child.stdout.on("data", (chunk) => {
			buffer += chunk;
			const lines = buffer.split("\n");
			buffer = lines.pop() ?? "";
			for (const line of lines) {
				if (!line.trim()) continue;
				const message = JSON.parse(line);
				if (message.id !== 1) continue;
				clearTimeout(timeout);
				resolve(message);
			}
		});
		child.once("error", (error) => {
			clearTimeout(timeout);
			reject(error);
		});
		child.once("exit", (code) => {
			clearTimeout(timeout);
			reject(new Error(`adapter exited with ${code}: ${stderr}`));
		});
		child.stdin.write(
			`${JSON.stringify({
				jsonrpc: "2.0",
				id: 1,
				method: "initialize",
				params: { protocolVersion: 1, clientCapabilities: {} },
			})}\n`,
		);
	});

	assert.equal(response.error, undefined);
	assert.equal(response.result.protocolVersion, 1);
	assert.deepEqual(
		response.result.agentCapabilities.sessionCapabilities.resume,
		{},
	);
	assert.deepEqual(
		response.result.agentCapabilities.sessionCapabilities.close,
		{},
	);

	const session = await new Promise((resolve, reject) => {
		const timeout = setTimeout(
			() => reject(new Error(`session/new timed out: ${stderr}`)),
			10_000,
		);
		let buffer = "";
		child.stdout.on("data", (chunk) => {
			buffer += chunk;
			const lines = buffer.split("\n");
			buffer = lines.pop() ?? "";
			for (const line of lines) {
				if (!line.trim()) continue;
				const message = JSON.parse(line);
				if (message.id !== 2) continue;
				clearTimeout(timeout);
				resolve(message);
			}
		});
		child.stdin.write(
			`${JSON.stringify({
				jsonrpc: "2.0",
				id: 2,
				method: "session/new",
				params: { cwd: new URL("..", import.meta.url).pathname, mcpServers: [] },
			})}\n`,
		);
	});

	assert.equal(session.error, undefined);
	assert.equal(typeof session.result.sessionId, "string");
	assert.ok(
		session.result.models.availableModels.some(
			(model) => model.modelId === "openai-codex/gpt-5.6-terra",
		),
	);
});
