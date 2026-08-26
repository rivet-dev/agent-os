#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
	copyFileSync,
	existsSync,
	mkdirSync,
	mkdtempSync,
	readFileSync,
	renameSync,
	rmSync,
	statSync,
	writeFileSync,
} from "node:fs";
import { homedir } from "node:os";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const SOURCE_REPOSITORY = "vercel-labs/fx";
const SOURCE_COMMIT = "8a61ffa670d80729a2ff1e04d5605d8effae6170";
const SOURCE_TARBALL_SHA256 = "baf8655a3a37bcb3dfb7d773050a01db3c4f36d670bcaf148a0fc2f940bbc8ac";
const SOURCE_TARBALL_URL = `https://github.com/${SOURCE_REPOSITORY}/archive/${SOURCE_COMMIT}.tar.gz`;
const SOURCE_DOWNLOAD_TIMEOUT_MS = 60_000;
const REQUIRED_ZIG_VERSION = "0.16.0";
const BUILD_COMMAND = ["build", "fx-core-wasm", "-Dwasm-surface=core", "-Doptimize=ReleaseSmall"];

const packageDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const distDir = resolve(packageDir, "dist");
const patchPath = resolve(packageDir, "patches", "agentos.patch");
const cacheDir = resolve(
	process.env.AGENTOS_TOOLCHAIN_CACHE_DIR ??
		resolve(process.env.XDG_CACHE_HOME ?? resolve(homedir(), ".cache"), "agentos"),
	"fx-build",
);
const tarballPath = resolve(cacheDir, `${SOURCE_COMMIT}.tar.gz`);

function sha256(data) {
	return createHash("sha256").update(data).digest("hex");
}

function hashFile(path) {
	return sha256(readFileSync(path));
}

function run(command, args, options = {}) {
	const result = spawnSync(command, args, { stdio: "inherit", ...options });
	if (result.status !== 0) {
		const detail = result.error ? `: ${result.error.message}` : "";
		throw new Error(`Command failed (${result.status ?? "unknown"}): ${command} ${args.join(" ")}${detail}`);
	}
}

function assertZigVersion() {
	const result = spawnSync("zig", ["version"], { encoding: "utf8" });
	if (result.status !== 0) {
		const detail = result.error ? `: ${result.error.message}` : "";
		throw new Error(`Zig ${REQUIRED_ZIG_VERSION} is required${detail}`);
	}
	const actual = result.stdout.trim();
	if (actual !== REQUIRED_ZIG_VERSION) {
		throw new Error(`Zig ${REQUIRED_ZIG_VERSION} is required; found ${actual || "unknown"}`);
	}
}

async function ensureTarball() {
	if (existsSync(tarballPath) && hashFile(tarballPath) === SOURCE_TARBALL_SHA256) return;
	const downloadRoot = mkdtempSync(resolve(cacheDir, "fx-download-"));
	const temporaryPath = resolve(downloadRoot, `${SOURCE_COMMIT}.tar.gz`);
	try {
		const response = await fetch(SOURCE_TARBALL_URL, {
			signal: AbortSignal.timeout(SOURCE_DOWNLOAD_TIMEOUT_MS),
		});
		if (!response.ok) {
			throw new Error(`Failed to download ${SOURCE_TARBALL_URL}: ${response.status} ${response.statusText}`);
		}
		writeFileSync(temporaryPath, Buffer.from(await response.arrayBuffer()));
		const actual = hashFile(temporaryPath);
		if (actual !== SOURCE_TARBALL_SHA256) {
			throw new Error(`FX tarball checksum mismatch: expected ${SOURCE_TARBALL_SHA256}, received ${actual}`);
		}
		renameSync(temporaryPath, tarballPath);
	} finally {
		rmSync(downloadRoot, { recursive: true, force: true });
	}
}

mkdirSync(cacheDir, { recursive: true });
assertZigVersion();
await ensureTarball();
const buildRoot = mkdtempSync(resolve(cacheDir, `${SOURCE_COMMIT}-`));
const sourceRoot = resolve(buildRoot, "source");
try {
	mkdirSync(sourceRoot);
	run("tar", ["-xzf", tarballPath, "--strip-components=1", "-C", sourceRoot]);
	run("git", ["apply", "--check", patchPath], { cwd: sourceRoot });
	run("git", ["apply", patchPath], { cwd: sourceRoot });
	run("zig", BUILD_COMMAND, { cwd: sourceRoot });

	const wasmPath = resolve(sourceRoot, "zig-out", "bin", "fx-core.wasm");
	if (!existsSync(wasmPath) || statSync(wasmPath).size === 0) {
		throw new Error("FX build did not produce zig-out/bin/fx-core.wasm");
	}
	mkdirSync(distDir, { recursive: true });
	copyFileSync(wasmPath, resolve(distDir, "fx-core.wasm"));
	copyFileSync(resolve(sourceRoot, "sdk", "fx-sdk.js"), resolve(distDir, "fx-sdk.js"));
	copyFileSync(resolve(sourceRoot, "LICENSE"), resolve(distDir, "LICENSE.fx"));
	writeFileSync(
		resolve(distDir, "fx-upstream.json"),
		`${JSON.stringify(
			{
				sourceRepository: SOURCE_REPOSITORY,
				sourceCommit: SOURCE_COMMIT,
				sourceTarballSha256: SOURCE_TARBALL_SHA256,
				patchSha256: hashFile(patchPath),
				zigVersion: REQUIRED_ZIG_VERSION,
				buildCommand: ["zig", ...BUILD_COMMAND],
				wasmSha256: hashFile(wasmPath),
			},
			null,
			2,
		)}\n`,
	);
} finally {
	rmSync(buildRoot, { recursive: true, force: true });
}
