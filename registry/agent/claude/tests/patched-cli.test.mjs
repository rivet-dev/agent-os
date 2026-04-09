import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, resolve as resolvePath } from "node:path";
import { tmpdir } from "node:os";

const require = createRequire(import.meta.url);
const sdkPath = require.resolve("@anthropic-ai/claude-agent-sdk");
const packageDir = resolvePath(import.meta.dirname, "..");
const cliManifestPath = resolvePath(packageDir, "dist", "claude-cli-patched.json");
const sdkManifestPath = resolvePath(packageDir, "dist", "claude-sdk-patched.json");
const { resolveClaudeCliPath, resolveClaudeSdkPath } = await import(
	resolvePath(packageDir, "dist", "patched-cli.js")
);

function readManifest(manifestPath) {
	return JSON.parse(readFileSync(manifestPath, "utf-8"));
}

test("build writes patched Claude CLI and SDK manifests to dist", () => {
	const cliManifest = readManifest(cliManifestPath);
	const sdkManifest = readManifest(sdkManifestPath);

	assert.equal(typeof cliManifest.entry, "string");
	assert.equal(typeof sdkManifest.entry, "string");
	assert.equal(
		resolveClaudeCliPath({ packageDir, sdkPath }),
		resolvePath(packageDir, "dist", cliManifest.entry.replace(/^\.\//, "")),
	);
	assert.equal(
		resolveClaudeSdkPath({ packageDir, sdkPath }),
		resolvePath(packageDir, "dist", sdkManifest.entry.replace(/^\.\//, "")),
	);
});

test("patched-path helpers fall back to the upstream SDK when manifests are missing", () => {
	const tempDir = mkdtempSync(resolvePath(tmpdir(), "agent-os-claude-test-"));
	try {
		assert.equal(
			resolveClaudeCliPath({ packageDir: tempDir, sdkPath }),
			resolvePath(dirname(sdkPath), "cli.js"),
		);
		assert.equal(resolveClaudeSdkPath({ packageDir: tempDir, sdkPath }), sdkPath);
	} finally {
		rmSync(tempDir, { recursive: true, force: true });
	}
});

test("patched-path helpers resolve custom manifest entries from dist", () => {
	const tempDir = mkdtempSync(resolvePath(tmpdir(), "agent-os-claude-test-"));
	try {
		const distDir = resolvePath(tempDir, "dist");
		mkdirSync(distDir, { recursive: true });
		writeFileSync(
			resolvePath(distDir, "claude-cli-patched.json"),
			JSON.stringify({ entry: "./custom-cli.mjs" }),
			"utf-8",
		);
		writeFileSync(
			resolvePath(distDir, "claude-sdk-patched.json"),
			JSON.stringify({ entry: "./custom-sdk.mjs" }),
			"utf-8",
		);

		assert.equal(
			resolveClaudeCliPath({ packageDir: tempDir, sdkPath }),
			resolvePath(distDir, "custom-cli.mjs"),
		);
		assert.equal(
			resolveClaudeSdkPath({ packageDir: tempDir, sdkPath }),
			resolvePath(distDir, "custom-sdk.mjs"),
		);
	} finally {
		rmSync(tempDir, { recursive: true, force: true });
	}
});
