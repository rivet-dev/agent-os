import type { WasmCommandPackage } from "@rivet-dev/agent-os-registry-types";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));

const pkg = {
	name: "gzip",
	aptName: "gzip",
	description: "GNU gzip compression (gzip, gunzip, zcat)",
	source: "rust" as const,
	commands: [
		{ name: "gzip", permissionTier: "read-write" as const },
		{ name: "gunzip", permissionTier: "read-write" as const, aliasOf: "gzip" },
		{ name: "zcat", permissionTier: "read-only" as const, aliasOf: "gzip" },
	],
	get commandDir() {
		return resolve(__dirname, "..", "wasm");
	},
} satisfies WasmCommandPackage;

export default pkg;
