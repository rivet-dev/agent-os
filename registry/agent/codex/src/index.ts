import { defineSoftware } from "@rivet-dev/agent-os";
import codexSoftware from "@rivet-dev/agent-os-codex";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const packageDir = resolve(__dirname, "..");

const codexAgent = defineSoftware({
	name: "codex",
	type: "agent" as const,
	packageDir,
	requires: ["@rivet-dev/agent-os-codex-agent"],
	agent: {
		id: "codex",
		acpAdapter: "@rivet-dev/agent-os-codex-agent",
		agentPackage: "@rivet-dev/agent-os-codex",
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
			return {
				args: ["--append-developer-instructions", instructions],
			};
		},
	},
});

const codex = [codexSoftware, codexAgent] as const;

export default codex;
