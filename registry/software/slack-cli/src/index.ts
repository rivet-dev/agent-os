import type { WasmCommandPackage } from "@rivet-dev/agent-os-registry-types";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));

const pkg = {
	name: "slack-cli",
	aptName: "slack-cli",
	description: "Slack CLI for building apps on the Slack Platform",
	source: "go" as const,
	commands: [{ name: "slack", permissionTier: "full" as const }],
	get commandDir() {
		return resolve(__dirname, "..", "wasm");
	},
} satisfies WasmCommandPackage;

export default pkg;
