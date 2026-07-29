import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, resolve as resolvePath } from "node:path";

const require = createRequire(import.meta.url);
const packageDir = resolvePath(import.meta.dirname, "..");
const sdkPath = require.resolve("@anthropic-ai/claude-agent-sdk");
const sdkPackage = JSON.parse(
	readFileSync(resolvePath(dirname(sdkPath), "package.json"), "utf8"),
);
const upstreamCliPath = resolvePath(dirname(sdkPath), "cli.js");
const stagedCliPath = resolvePath(packageDir, "dist", "claude-cli.mjs");
const manifestPath = resolvePath(
	packageDir,
	"dist",
	"claude-cli-upstream.json",
);

test("stages the newest JavaScript Claude CLI without source patches", () => {
	const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));

	assert.equal(sdkPackage.version, "0.2.112");
	assert.equal(sdkPackage.claudeCodeVersion, "2.1.112");
	assert.equal(manifest.sdkVersion, sdkPackage.version);
	assert.equal(manifest.claudeCodeVersion, sdkPackage.claudeCodeVersion);
	assert.deepEqual(manifest.patches, []);
	assert.equal(manifest.entry, "./claude-cli.mjs");
	assert.deepEqual(
		readFileSync(stagedCliPath),
		readFileSync(upstreamCliPath),
		"AgentOS must package the upstream CLI byte-for-byte until a focused runtime test proves a patch is required",
	);
});
