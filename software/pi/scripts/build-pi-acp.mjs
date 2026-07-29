#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
	cpSync,
	existsSync,
	mkdirSync,
	readFileSync,
	renameSync,
	rmSync,
	writeFileSync,
} from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const SOURCE_REPOSITORY = "rivet-dev/pi-acp";
const SOURCE_COMMIT = "87cb3ab06d9b7e781db9c9575755153b50b2ba90";
const SOURCE_TARBALL_SHA256 =
	"85bc7e133d28e9d870ecad7aa3de9e6a17ffea142443a177d618597a56c72cd7";
const SOURCE_TARBALL_URL = `https://github.com/${SOURCE_REPOSITORY}/archive/${SOURCE_COMMIT}.tar.gz`;

const packageDir = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const cacheDir = resolve(packageDir, "node_modules", ".cache", "pi-acp-build");
const tarballPath = resolve(cacheDir, `pi-acp-${SOURCE_COMMIT}.tar.gz`);
const sourceRoot = resolve(cacheDir, `pi-acp-${SOURCE_COMMIT}`);
const outputDir = resolve(packageDir, "dist", "pi-acp");
const manifestPath = resolve(packageDir, "dist", "pi-acp-upstream.json");

function sha256(path) {
	return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function run(command, args, options = {}) {
	const result = spawnSync(command, args, { stdio: "inherit", ...options });
	if (result.status !== 0) {
		throw new Error(
			`Command failed (${result.status ?? "unknown"}): ${command} ${args.join(" ")}`,
		);
	}
}

async function ensureTarball() {
	mkdirSync(cacheDir, { recursive: true });
	if (
		existsSync(tarballPath) &&
		sha256(tarballPath) === SOURCE_TARBALL_SHA256
	) {
		return;
	}

	const temporaryPath = `${tarballPath}.download`;
	rmSync(temporaryPath, { force: true });
	const response = await fetch(SOURCE_TARBALL_URL);
	if (!response.ok) {
		throw new Error(
			`Failed to download ${SOURCE_TARBALL_URL}: ${response.status} ${response.statusText}`,
		);
	}
	writeFileSync(temporaryPath, Buffer.from(await response.arrayBuffer()));
	const actual = sha256(temporaryPath);
	if (actual !== SOURCE_TARBALL_SHA256) {
		throw new Error(
			`pi-acp tarball checksum mismatch: expected ${SOURCE_TARBALL_SHA256}, received ${actual}`,
		);
	}
	renameSync(temporaryPath, tarballPath);
}

await ensureTarball();
rmSync(sourceRoot, { recursive: true, force: true });
mkdirSync(sourceRoot, { recursive: true });
run("tar", ["-xzf", tarballPath, "--strip-components=1", "-C", sourceRoot]);

const sourcePackage = JSON.parse(
	readFileSync(resolve(sourceRoot, "package.json"), "utf8"),
);
if (sourcePackage.name !== "pi-acp" || sourcePackage.version !== "0.0.31") {
	throw new Error(
		`Expected pi-acp@0.0.31 source package, received ${sourcePackage.name}@${sourcePackage.version}`,
	);
}
const expectedDependencies = {
	"@agentclientprotocol/sdk": "1.2.1",
	"pi-mcp-adapter": "2.11.0",
	zod: "^3.25.0",
};
for (const [name, version] of Object.entries(expectedDependencies)) {
	if (sourcePackage.dependencies?.[name] !== version) {
		throw new Error(
			`Pinned pi-acp source must depend on ${name}@${version}`,
		);
	}
}

run("npm", ["ci"], { cwd: sourceRoot });
run("npm", ["run", "build"], { cwd: sourceRoot });

const sourceEntrypoint = resolve(sourceRoot, "dist", "index.js");
if (
	!existsSync(sourceEntrypoint) ||
	!readFileSync(sourceEntrypoint, "utf8").startsWith("#!")
) {
	throw new Error("pi-acp build did not produce an executable dist/index.js");
}

rmSync(outputDir, { recursive: true, force: true });
mkdirSync(outputDir, { recursive: true });
cpSync(sourceEntrypoint, resolve(outputDir, "index.js"));
cpSync(resolve(sourceRoot, "package.json"), resolve(outputDir, "package.json"));
const sourceMap = `${sourceEntrypoint}.map`;
if (existsSync(sourceMap)) cpSync(sourceMap, resolve(outputDir, "index.js.map"));

mkdirSync(dirname(manifestPath), { recursive: true });
writeFileSync(
	manifestPath,
	`${JSON.stringify(
		{
			sourceRepository: SOURCE_REPOSITORY,
			sourceCommit: SOURCE_COMMIT,
			sourceTarballSha256: SOURCE_TARBALL_SHA256,
			sourcePackageVersion: sourcePackage.version,
			buildCommands: ["npm ci", "npm run build"],
			entrypoint: "./pi-acp/index.js",
			entrypointSha256: sha256(sourceEntrypoint),
			runtimeDependencies: {
				"@agentclientprotocol/sdk": "1.2.1",
				"pi-mcp-adapter": "2.11.0",
				zod: "3.25.76",
			},
		},
		null,
		2,
	)}\n`,
);

process.stdout.write(`Built pi-acp from ${SOURCE_REPOSITORY}@${SOURCE_COMMIT}\n`);
