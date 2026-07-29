#!/usr/bin/env node

import { createHash } from "node:crypto";
import {
	existsSync,
	mkdirSync,
	readdirSync,
	readFileSync,
	renameSync,
	rmSync,
	statSync,
	writeFileSync,
} from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { homedir } from "node:os";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const SOURCE_REPOSITORY = "anomalyco/opencode";
const SOURCE_VERSION = "1.17.20";
const SOURCE_COMMIT = "4473fc3c9055046183990a965d68df3db7ea6f62";
const SOURCE_TARBALL_SHA256 =
	"2a07505ed5f76e84d42f6752594bbbd25ecfe98a82b546b880edb917cfcd239a";
const ACP_ENTRYPOINT = "packages/opencode/src/cli/cmd/acp.ts";
const ACP_ENTRYPOINT_SHA256 =
	"104d1dfe498154fca38c5971deec8330be4793cedf122e36c5a38589089e1b23";
const SOURCE_TARBALL_URL = `https://github.com/${SOURCE_REPOSITORY}/archive/refs/tags/v${SOURCE_VERSION}.tar.gz`;

const packageDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const distDir = resolve(packageDir, "dist");
const bundleDir = resolve(distDir, "opencode-acp");
const cacheDir = resolve(
	process.env.AGENTOS_TOOLCHAIN_CACHE_DIR ??
		resolve(process.env.XDG_CACHE_HOME ?? resolve(homedir(), ".cache"), "agentos"),
	"opencode-build",
);
const tarballPath = resolve(cacheDir, `opencode-v${SOURCE_VERSION}.tar.gz`);
const sourceRoot = resolve(cacheDir, `opencode-${SOURCE_VERSION}`);
const sourceHashPath = `${sourceRoot}.sha256`;
const dependenciesReadyPath = `${sourceRoot}.dependencies-ready`;
const manifestPath = resolve(distDir, "opencode-acp.manifest.json");
const bunBin = resolve(packageDir, "node_modules", ".bin", process.platform === "win32" ? "bun.cmd" : "bun");
const compatibilityModule = resolve(packageDir, "../../packages/node-pty/src/index.ts");

function sha256(data) {
	return createHash("sha256").update(data).digest("hex");
}

function hashFile(path) {
	return sha256(readFileSync(path));
}

function hashSourceTree(root) {
	const hash = createHash("sha256");
	const visit = (directory) => {
		for (const entry of readdirSync(directory, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
			if (entry.name === "node_modules" || entry.name === ".git") continue;
			const path = join(directory, entry.name);
			if (entry.isDirectory()) {
				visit(path);
				continue;
			}
			if (!entry.isFile()) continue;
			hash.update(relative(root, path));
			hash.update("\0");
			hash.update(readFileSync(path));
			hash.update("\0");
		}
	};
	visit(root);
	return hash.digest("hex");
}

function run(command, args, options = {}) {
	const result = spawnSync(command, args, {
		stdio: "inherit",
		...options,
		env: {
			...process.env,
			PATH: `${dirname(bunBin)}:${process.env.PATH ?? ""}`,
			...options.env,
		},
	});
	if (result.status !== 0) {
		const spawnError = result.error ? `: ${result.error.message}` : "";
		throw new Error(
			`Command failed (${result.status ?? "unknown"}): ${command} ${args.join(" ")}${spawnError}`,
		);
	}
}

async function ensureTarball() {
	if (existsSync(tarballPath) && hashFile(tarballPath) === SOURCE_TARBALL_SHA256) return;
	const temporaryPath = `${tarballPath}.download`;
	rmSync(temporaryPath, { force: true });
	const response = await fetch(SOURCE_TARBALL_URL);
	if (!response.ok) {
		throw new Error(`Failed to download ${SOURCE_TARBALL_URL}: ${response.status} ${response.statusText}`);
	}
	writeFileSync(temporaryPath, Buffer.from(await response.arrayBuffer()));
	const actual = hashFile(temporaryPath);
	if (actual !== SOURCE_TARBALL_SHA256) {
		throw new Error(`OpenCode tarball checksum mismatch: expected ${SOURCE_TARBALL_SHA256}, received ${actual}`);
	}
	renameSync(temporaryPath, tarballPath);
}

function extractPristineSource() {
	rmSync(sourceRoot, { recursive: true, force: true });
	rmSync(dependenciesReadyPath, { force: true });
	mkdirSync(sourceRoot, { recursive: true });
	run("tar", ["-xzf", tarballPath, "--strip-components=1", "-C", sourceRoot]);
	const actualEntrypointHash = hashFile(resolve(sourceRoot, ACP_ENTRYPOINT));
	if (actualEntrypointHash !== ACP_ENTRYPOINT_SHA256) {
		throw new Error(`OpenCode ACP entrypoint checksum mismatch: expected ${ACP_ENTRYPOINT_SHA256}, received ${actualEntrypointHash}`);
	}
	const sourceHash = hashSourceTree(sourceRoot);
	writeFileSync(sourceHashPath, `${sourceHash}\n`);
	return sourceHash;
}

function ensurePristineSource() {
	if (!existsSync(sourceRoot) || !existsSync(sourceHashPath)) return extractPristineSource();
	const expected = readFileSync(sourceHashPath, "utf8").trim();
	const actual = hashSourceTree(sourceRoot);
	return actual === expected ? actual : extractPristineSource();
}

function outputHashes(root) {
	const result = {};
	const visit = (directory) => {
		for (const entry of readdirSync(directory, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
			const path = join(directory, entry.name);
			if (entry.isDirectory()) visit(path);
			else if (entry.isFile()) result[relative(root, path)] = hashFile(path);
		}
	};
	visit(root);
	return result;
}

mkdirSync(cacheDir, { recursive: true });
await ensureTarball();
const sourceTreeSha256Before = ensurePristineSource();

const dependenciesReady =
	existsSync(dependenciesReadyPath) &&
	existsSync(resolve(sourceRoot, "packages", "opencode", "node_modules", "@ai-sdk", "xai", "package.json"));
if (!dependenciesReady) {
	// A failed native install can leave Bun with a partial workspace that a later
	// install incorrectly treats as complete. Rebuild the disposable dependency
	// tree from the verified lockfile whenever the ready marker is absent.
	rmSync(resolve(sourceRoot, "node_modules"), { recursive: true, force: true });
	run(bunBin, ["install", "--frozen-lockfile"], { cwd: sourceRoot });
	writeFileSync(dependenciesReadyPath, `${SOURCE_TARBALL_SHA256}\n`);
}

rmSync(bundleDir, { recursive: true, force: true });
mkdirSync(bundleDir, { recursive: true });
run(
	bunBin,
	[
		resolve(packageDir, "scripts", "build-upstream-node.ts"),
		sourceRoot,
		bundleDir,
		compatibilityModule,
	],
	{ cwd: sourceRoot, env: { OPENCODE_CHANNEL: "latest" } },
);

const sourceTreeSha256After = hashSourceTree(sourceRoot);
if (sourceTreeSha256After !== sourceTreeSha256Before) {
	throw new Error(
		`The upstream OpenCode source changed during the build (${sourceTreeSha256Before} -> ${sourceTreeSha256After})`,
	);
}

const outputs = outputHashes(bundleDir);
if (!outputs["acp.js"] || statSync(resolve(bundleDir, "acp.js")).size === 0) {
	throw new Error("OpenCode ACP build did not produce acp.js");
}
mkdirSync(distDir, { recursive: true });
writeFileSync(
	manifestPath,
	`${JSON.stringify(
		{
			builder: "upstream-node-acp",
			sourceRepository: SOURCE_REPOSITORY,
			sourceVersion: SOURCE_VERSION,
			sourceCommit: SOURCE_COMMIT,
			sourceTarballSha256: SOURCE_TARBALL_SHA256,
			entrypoint: ACP_ENTRYPOINT,
			entrypointSha256: ACP_ENTRYPOINT_SHA256,
			bundleSplitting: true,
			sourceTreeSha256Before,
			sourceTreeSha256After,
			semanticSourceModifications: true,
			sourceTreeModified: false,
			nodeCompatibilitySubstitutions: [
				{
					define: "Bun.hash=globalThis.__agentOSOpenCodeHashFast",
					upstreamExpression: "Bun.hash(base).toString(16)",
					nodeSemantics: "Hash.fast(base)",
					reason: "Remote skill cache naming is the remaining Bun-only expression in the native ACP dependency graph",
					removalBlocker: "Remove after a released OpenCode source replaces Bun.hash(base) with Hash.fast(base)",
				},
			],
			acpCompatibilitySubstitutions: [
				{
					upstreamBehavior: "Issue the event stream and all initial control-plane requests concurrently over the same loopback origin",
					nodeSemantics: "Keep the event stream independent and serialize short control-plane requests over the real HTTP transport",
					reason: "Embedded HTTP transports do not reliably drain a burst of same-process loopback connections",
					removalBlocker: "Remove after the AgentOS HTTP bridge reliably supports concurrent same-process loopback requests",
				},
				{
					upstreamBehavior: "Pre-apply approved edits through ACP fs/write_text_file before replying to OpenCode",
					nodeSemantics: "Let OpenCode's approved edit tool write directly inside the guest",
					reason: "The redundant ACP host write can re-enter and deadlock synchronous actor transports",
					removalBlocker: "Remove after OpenCode gates the pre-write on the advertised fs.writeTextFile capability or removes it",
				},
				{
					upstreamBehavior: "Rely exclusively on the asynchronous global event stream for completed prompt content and tool parts",
					nodeSemantics: "Reconcile the completed turn from authoritative session messages and suppress late duplicate text deltas",
					reason: "Fast providers can complete before message metadata is queryable, causing the global event bridge to drop text or tool updates",
					removalBlocker: "Remove after OpenCode orders part metadata before deltas or reconciles completed prompt turns upstream",
				},
				{
					upstreamBehavior: "Collapse SDK failures from session/load into an opaque OpenCode service failure",
					nodeSemantics: "Return the original SDK error name/message and distinguish session metadata from message-history loading",
					reason: "AgentOS callers need the underlying failure stage and cause to diagnose persisted-session resume failures",
					removalBlocker: "Remove after OpenCode preserves actionable SDK failure details in its ACP error response or stderr",
				},
				{
					upstreamBehavior: "Expose effort choices only for the currently selected model",
					nodeSemantics: "Also expose variant-qualified model choices so one ACP session describes every model's native reasoning levels",
					reason: "AgentOS model discovery must build an accurate per-model OpenCode-compatible catalog without opening one session per model",
					removalBlocker: "Remove after ACP provides a model metadata operation with per-model configuration options",
				},
			],
			compatibilityModules: {
				"@lydell/node-pty": "@rivet-dev/agentos-node-pty",
			},
			outputs,
		},
		null,
		2,
	)}\n`,
);
