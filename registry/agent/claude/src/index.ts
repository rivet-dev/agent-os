import { defineSoftware } from "@rivet-dev/agent-os";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const packageDir = resolve(__dirname, "..");

const claude = defineSoftware({
	name: "claude",
	type: "agent" as const,
	packageDir,
	requires: ["@anthropic-ai/claude-agent-sdk"],
	agent: {
		id: "claude",
		acpAdapter: "@rivet-dev/agent-os-claude",
		agentPackage: "@anthropic-ai/claude-agent-sdk",
		staticEnv: {
			CLAUDE_AGENT_SDK_CLIENT_APP: "@rivet-dev/agent-os",
			CLAUDE_CODE_SIMPLE: "1",
			CLAUDE_CODE_FORCE_AGENT_OS_RIPGREP: "1",
			CLAUDE_CODE_DEFER_GROWTHBOOK_INIT: "1",
			CLAUDE_CODE_DISABLE_STREAM_JSON_HOOK_EVENTS: "1",
			CLAUDE_CODE_SHELL: "/bin/sh",
			CLAUDE_CODE_SKIP_INITIAL_MESSAGES: "1",
			CLAUDE_CODE_SKIP_SANDBOX_INIT: "1",
			CLAUDE_CODE_USE_PIPE_OUTPUT: "1",
			DISABLE_TELEMETRY: "1",
			SHELL: "/bin/sh",
			USE_BUILTIN_RIPGREP: "0",
		},
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

export default claude;
