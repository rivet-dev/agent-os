import type { WasmCommandPackage } from "@rivet-dev/agent-os-registry-types";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));

const pkg = {
	name: "http-get",
	aptName: "http-get",
	description: "Minimal HTTP GET fetch helper",
	source: "c" as const,
	commands: [{ name: "http_get", permissionTier: "full" as const }],
	get commandDir() {
		return resolve(__dirname, "..", "wasm");
	},
} satisfies WasmCommandPackage;

export default pkg;
