/**
 * Provides registry software packages for tests.
 *
 * Each registry package exports a descriptor with a `commandDir` getter
 * that resolves to the package's wasm/ directory. Pass these directly
 * to AgentOs.create({ software: [...] }).
 *
 * Requires: `cd ~/agent-os-registry && make copy-wasm && make build`
 */

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

function withFallbackCommandDir<
	T extends {
		commandDir: string;
	},
>(pkg: T): T {
	if (existsSync(pkg.commandDir) || !existsSync(FALLBACK_COMMAND_DIR)) {
		return pkg;
	}

	return {
		...pkg,
		get commandDir() {
			return FALLBACK_COMMAND_DIR;
		},
	};
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
	existsSync(coreutils.commandDir) || existsSync(FALLBACK_COMMAND_DIR);

/** Skip reason for tests that need registry commands. */
export const registrySkipReason = hasRegistryCommands
	? false
	: "Registry WASM binaries not available (run: make -C registry/native && make -C registry copy-wasm build)";
