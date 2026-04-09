import type { WasmCommandPackage } from "@rivet-dev/agent-os-registry-types";
import { existsSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const COMMAND_DIR = resolve(__dirname, "..", "wasm");
const FALLBACK_COMMAND_DIR = resolve(
	__dirname,
	"..",
	"..",
	"..",
	"native/target/wasm32-wasip1/release/commands",
);

const pkg = {
	name: "curl",
	aptName: "curl",
	description: "curl-compatible HTTP client",
	source: "rust" as const,
	commands: [{ name: "curl", permissionTier: "full" as const }],
	get commandDir() {
		return existsSync(COMMAND_DIR) || !existsSync(FALLBACK_COMMAND_DIR)
			? COMMAND_DIR
			: FALLBACK_COMMAND_DIR;
	},
} satisfies WasmCommandPackage;

export default pkg;
