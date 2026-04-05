import { defineSoftware } from "@rivet-dev/agent-os";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const packageDir = resolve(__dirname, "..");

const pi = defineSoftware({
	name: "pi",
	type: "agent" as const,
	packageDir,
	requires: ["@rivet-dev/agent-os-pi", "@mariozechner/pi-coding-agent"],
	agent: {
		id: "pi",
		acpAdapter: "@rivet-dev/agent-os-pi",
		agentPackage: "@mariozechner/pi-coding-agent",
		prepareInstructions: async (kernel, _cwd, additionalInstructions, opts) => {
			const parts: string[] = [];
			if (!opts?.skipBase) {
				const data = await kernel.readFile("/etc/agentos/instructions.md");
				parts.push(new TextDecoder().decode(data));
			}
			if (additionalInstructions) parts.push(additionalInstructions);
			if (opts?.toolReference) parts.push(opts.toolReference);
			parts.push("---");
			const instructions = parts.join("\n\n");
			if (!instructions) return {};
			return { args: ["--append-system-prompt", instructions] };
		},
	},
});

export default pi;
