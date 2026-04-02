import type { WasmCommandPackage } from "@rivet-dev/agent-os-registry-types";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));

const pkg = {
	name: "git",
	aptName: "git",
	description: "git version control",
	source: "rust" as const,
	commands: [
		{ name: "git", permissionTier: "full" as const },
		{ name: "git-remote-http", permissionTier: "full" as const },
		{ name: "git-remote-https", permissionTier: "full" as const },
	],
	get commandDir() {
		return resolve(__dirname, "..", "wasm");
	},
} satisfies WasmCommandPackage;

export default pkg;
