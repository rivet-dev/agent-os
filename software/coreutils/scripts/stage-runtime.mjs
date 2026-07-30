import { existsSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { stage } from "@rivet-dev/agentos-toolchain";

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const repositoryRoot = resolve(packageRoot, "../..");
const localBuild = resolve(
	repositoryRoot,
	"toolchain/target/wasm32-wasip1/release/commands",
);
const validatedArtifact = resolve(
	repositoryRoot,
	"packages/core/commands",
);
const commandsDir = process.env.AGENTOS_SOFTWARE_COMMANDS_DIR
	? resolve(repositoryRoot, process.env.AGENTOS_SOFTWARE_COMMANDS_DIR)
	: existsSync(localBuild)
		? localBuild
		: validatedArtifact;

stage({
	packageDir: packageRoot,
	commandsDir,
	ifMissing: "error",
});
