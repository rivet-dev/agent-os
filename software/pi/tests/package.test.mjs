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
	assert.equal(manifest.agent.env.PI_ACP_PI_COMMAND, "/opt/agentos/bin/pi");
	assert.equal(
		manifest.agent.env.PI_ACP_PI_ENTRYPOINT,
		"/opt/agentos/pkgs/pi/0.0.1/node_modules/@earendil-works/pi-coding-agent/dist/cli.js",
	);
	assert.equal(packageJson.bin["pi-acp"], "./dist/pi-acp/index.js");
	assert.equal(
		packageJson.bin.pi,
		"./node_modules/@earendil-works/pi-coding-agent/dist/cli.js",
	);
	assert.equal(
		packageJson.dependencies["@earendil-works/pi-coding-agent"],
		"0.80.10",
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
	assert.deepEqual(upstreamManifest.buildCommands, ["npm ci", "npm run build"]);
	assert.ok(adapterEntrypoint.startsWith("#!/usr/bin/env node"));
	assert.equal(adapterPackageJson.name, "pi-acp");
	assert.equal(adapterPackageJson.version, "0.0.31");
	assert.match(packagedMcpConfig, /export function loadMcpConfig/);
	assert.equal(packageJson.dependencies["@mariozechner/pi-coding-agent"], undefined);
});

test("packaged pinned Pi adapter initializes with persistent session capabilities", async (t) => {
	const child = spawn(
		new URL("../dist/package/bin/pi-acp", import.meta.url).pathname,
		[],
		{
			stdio: ["pipe", "pipe", "pipe"],
			env: {
				...process.env,
				PI_ACP_PI_COMMAND: "/opt/agentos/bin/pi",
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
});
