#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import { cpSync, mkdirSync, realpathSync, rmSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const packageDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const stagedPackageDir = resolve(packageDir, "dist", "package");
const codingAgentDir = resolve(
	packageDir,
	"node_modules",
	"@earendil-works",
	"pi-coding-agent",
);
const piAiDir = realpathSync(
	resolve(dirname(realpathSync(codingAgentDir)), "pi-ai"),
);
const codexExtension = resolve(packageDir, "extensions", "codex-auth.mjs");

const patchedRuntimeFiles = [
	{
		source: resolve(piAiDir, "dist", "api", "openai-codex-responses.js"),
		targets: [
			"@earendil-works/pi-coding-agent/node_modules/@earendil-works/pi-ai/dist/api/openai-codex-responses.js",
		],
	},
	{
		source: resolve(codingAgentDir, "dist", "core", "http-dispatcher.js"),
		targets: ["@earendil-works/pi-coding-agent/dist/core/http-dispatcher.js"],
	},
];

for (const file of patchedRuntimeFiles) {
	for (const target of file.targets) {
		cpSync(file.source, resolve(stagedPackageDir, "node_modules", target));
	}
}

const stagedExtension = resolve(stagedPackageDir, "extensions", "codex-auth.mjs");
mkdirSync(dirname(stagedExtension), { recursive: true });
cpSync(codexExtension, stagedExtension);

function run(command, args) {
	const result = spawnSync(command, args, { stdio: "inherit" });
	if (result.status !== 0) {
		throw new Error(
			`Command failed (${result.status ?? "unknown"}): ${command} ${args.join(" ")}`,
		);
	}
}

const packageTar = resolve(packageDir, "dist", "package.tar");
const packageAospkg = resolve(packageDir, "dist", "package.aospkg");
rmSync(packageTar, { force: true });
rmSync(packageAospkg, { force: true });
run("tar", ["-cf", packageTar, "-C", stagedPackageDir, "."]);
run("agentos-toolchain", ["pack-aospkg", packageTar, packageAospkg]);
