import { defineSoftware } from "@rivet-dev/agent-os-core";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const packageDir = resolve(__dirname, "..");

const opencode = defineSoftware({
	name: "opencode",
	type: "agent" as const,
	packageDir,
	requires: ["@rivet-dev/agent-os-opencode"],
	agent: {
		id: "opencode",
		// OpenCode still speaks ACP natively, but Agent OS runs a source-built
		// Node ACP bundle entirely inside the VM rather than a host binary wrapper.
		acpAdapter: "@rivet-dev/agent-os-opencode",
		agentPackage: "@rivet-dev/agent-os-opencode",
		staticEnv: {
			OPENCODE_DISABLE_CONFIG_DEP_INSTALL: "1",
			OPENCODE_DISABLE_EMBEDDED_WEB_UI: "1",
		},
		prepareInstructions: async (kernel, _cwd, additionalInstructions, opts) => {
			const contextPaths = opts?.skipBase
				? []
				: [
						".github/copilot-instructions.md",
						".cursorrules",
						".cursor/rules/",
						"CLAUDE.md",
						"CLAUDE.local.md",
						"opencode.md",
						"opencode.local.md",
						"OpenCode.md",
						"OpenCode.local.md",
						"OPENCODE.md",
						"OPENCODE.local.md",
						"/etc/agentos/instructions.md",
					];
			if (additionalInstructions) {
				const additionalPath = "/tmp/agentos-additional-instructions.md";
				await kernel.writeFile(additionalPath, additionalInstructions);
				contextPaths.push(additionalPath);
			}
			if (opts?.toolReference) {
				const toolRefPath = "/tmp/agentos-tool-reference.md";
				await kernel.writeFile(toolRefPath, opts.toolReference);
				contextPaths.push(toolRefPath);
			}
			if (contextPaths.length === 0) return {};
			return {
				env: { OPENCODE_CONTEXTPATHS: JSON.stringify(contextPaths) },
			};
		},
	},
});

export default opencode;
