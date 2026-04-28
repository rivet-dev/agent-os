/**
 * Provides registry software packages for tests.
 *
 * Each registry package exports a descriptor with a `commandDir` getter
 * that resolves to the package's wasm/ directory. Pass these directly
 * to AgentOs.create({ software: [...] }).
 *
 * When a C-backed registry package is missing its built command artifact, this
 * helper builds the command on demand into `registry/native/c/build` and uses
 * that directory as a fallback command source.
 */

import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import codex from "@rivet-dev/agent-os-codex";
import coreutils from "@rivet-dev/agent-os-coreutils";
import curl from "@rivet-dev/agent-os-curl";
import diffutils from "@rivet-dev/agent-os-diffutils";
import fd from "@rivet-dev/agent-os-fd";
import file from "@rivet-dev/agent-os-file";
import findutils from "@rivet-dev/agent-os-findutils";
import gawk from "@rivet-dev/agent-os-gawk";
import grep from "@rivet-dev/agent-os-grep";
import gzip from "@rivet-dev/agent-os-gzip";
import jq from "@rivet-dev/agent-os-jq";
import ripgrep from "@rivet-dev/agent-os-ripgrep";
import sed from "@rivet-dev/agent-os-sed";
import tar from "@rivet-dev/agent-os-tar";
import tree from "@rivet-dev/agent-os-tree";
import yq from "@rivet-dev/agent-os-yq";

const __dirname = dirname(fileURLToPath(import.meta.url));
const FALLBACK_COMMAND_DIR = resolve(
	__dirname,
	"../../../../registry/native/target/wasm32-wasip1/release/commands",
);
const C_BUILD_COMMAND_DIR = resolve(__dirname, "../../../../registry/native/c/build");
const C_BUILD_ROOT = resolve(__dirname, "../../../../registry/native/c");
const C_PATCHED_SYSROOT_TARGET = "sysroot/lib/wasm32-wasi/libc.a";
const C_BUILD_TARGETS = new Map<string, string>([
	["duckdb", "build/duckdb"],
	["http_get", "build/http_get"],
	["sqlite3", "build/sqlite3_cli"],
	["wget", "build/wget"],
	["zip", "build/zip"],
	["unzip", "build/unzip"],
]);
const attemptedCBuilds = new Set<string>();

type CommandPackageLike = {
	commandDir: string;
	commands?: Array<{ name?: string }>;
};

function declaredCommandNames(pkg: CommandPackageLike): string[] {
	return (pkg.commands ?? [])
		.map((command) => command.name)
		.filter((name): name is string => typeof name === "string" && name.length > 0);
}

function hasUsableCommandDir(dir: string, commands: string[]): boolean {
	if (!existsSync(dir)) {
		return false;
	}
	if (commands.length === 0) {
		return true;
	}
	return commands.every((command) => existsSync(resolve(dir, command)));
}

function ensureFallbackCommandArtifacts(commands: string[]): string | false {
	const buildTargets = [
		...new Set(
			commands.flatMap((command) => {
				if (
					hasUsableCommandDir(FALLBACK_COMMAND_DIR, [command]) ||
					hasUsableCommandDir(C_BUILD_COMMAND_DIR, [command]) ||
					attemptedCBuilds.has(command)
				) {
					return [];
				}

				const buildTarget = C_BUILD_TARGETS.get(command);
				if (!buildTarget) {
					return [];
				}

				attemptedCBuilds.add(command);
				return [buildTarget];
			}),
		),
	];

	if (buildTargets.length === 0) {
		return false;
	}

	const sysrootResult = spawnSync("make", ["sysroot"], {
		cwd: C_BUILD_ROOT,
		encoding: "utf8",
	});
	if (sysrootResult.status !== 0) {
		const output = [sysrootResult.stderr, sysrootResult.stdout]
			.filter((value) => typeof value === "string" && value.trim().length > 0)
			.join("\n")
			.trim();
		if (output.length === 0) {
			return "Failed to build registry command artifacts via make sysroot";
		}
		return `Failed to build registry command artifacts via make sysroot:\n${output}`;
	}

	const buildResult = spawnSync("make", ["-o", C_PATCHED_SYSROOT_TARGET, ...buildTargets], {
		cwd: C_BUILD_ROOT,
		encoding: "utf8",
	});
	if (buildResult.status === 0) {
		return false;
	}

	const output = [buildResult.stderr, buildResult.stdout]
		.filter((value) => typeof value === "string" && value.trim().length > 0)
		.join("\n")
		.trim();
	if (output.length === 0) {
		return `Failed to build registry command artifacts via make ${buildTargets.join(" ")}`;
	}
	return `Failed to build registry command artifacts via make ${buildTargets.join(" ")}:\n${output}`;
}

export function withFallbackCommandDir<
	T extends CommandPackageLike,
>(pkg: T): T {
	const commands = declaredCommandNames(pkg);
	if (hasUsableCommandDir(pkg.commandDir, commands)) {
		return pkg;
	}

	ensureFallbackCommandArtifacts(commands);

	for (const fallbackDir of [FALLBACK_COMMAND_DIR, C_BUILD_COMMAND_DIR]) {
		if (!hasUsableCommandDir(fallbackDir, commands)) {
			continue;
		}
		return {
			...pkg,
			get commandDir() {
				return fallbackDir;
			},
		};
	}

	return pkg;
}

export function commandPackageSkipReason(...packages: CommandPackageLike[]): string | false {
	const buildErrors = packages
		.map((pkg) => ensureFallbackCommandArtifacts(declaredCommandNames(pkg)))
		.filter((error): error is string => typeof error === "string");

	const unavailable = packages.flatMap((pkg) => {
		const commands = declaredCommandNames(pkg);
		return [pkg.commandDir, FALLBACK_COMMAND_DIR, C_BUILD_COMMAND_DIR].some((dir) =>
			hasUsableCommandDir(dir, commands),
		)
			? []
			: commands;
	});
	if (unavailable.length === 0) {
		return false;
	}

	if (buildErrors.length > 0) {
		return buildErrors.join("\n\n");
	}

	return `Registry command artifacts not available for: ${unavailable.join(", ")}`;
}

/** All standard registry software packages. */
export const REGISTRY_SOFTWARE = [
	coreutils,
	sed,
	grep,
	gawk,
	findutils,
	diffutils,
	tar,
	gzip,
	jq,
	ripgrep,
	fd,
	tree,
	file,
	yq,
	codex,
	curl,
].map(withFallbackCommandDir);

/** True if registry wasm binaries are available through copied or locally built artifacts. */
export const hasRegistryCommands =
	hasUsableCommandDir(coreutils.commandDir, declaredCommandNames(coreutils)) ||
	hasUsableCommandDir(FALLBACK_COMMAND_DIR, declaredCommandNames(coreutils));

/** Skip reason for tests that need registry commands. */
export const registrySkipReason = hasRegistryCommands
	? false
	: "Registry WASM binaries not available (run: make -C registry/native && make -C registry copy-wasm build)";
