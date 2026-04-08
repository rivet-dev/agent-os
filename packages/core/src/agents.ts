// Agent configurations for ACP-compatible coding agents

import type { Kernel } from "./runtime-compat.js";

const INSTRUCTIONS_PATH = "/etc/agentos/instructions.md";

/**
 * Read OS instructions from /etc/agentos/instructions.md inside the VM,
 * optionally appending session-level additional instructions and tool reference.
 * When skipBase is true, the OS base file is not read (used for tool-docs-only injection).
 */
async function readVmInstructions(
	kernel: Kernel,
	additionalInstructions?: string,
	toolReference?: string,
	skipBase?: boolean,
): Promise<string> {
	const parts: string[] = [];
	if (!skipBase) {
		const data = await kernel.readFile(INSTRUCTIONS_PATH);
		parts.push(new TextDecoder().decode(data));
	}
	if (additionalInstructions) parts.push(additionalInstructions);
	if (toolReference) parts.push(toolReference);
	if (parts.length === 0) return "";
	// Append a horizontal rule so agents can distinguish the injected
	// system prompt from whatever the host appends after it.
	parts.push("---");
	return parts.join("\n\n");
}

/** Options passed alongside additionalInstructions in prepareInstructions. */
export interface PrepareInstructionsOptions {
	/** Auto-generated tool reference markdown to append to the prompt. */
	toolReference?: string;
	/** When true, skip reading the base OS instructions file. */
	skipBase?: boolean;
}

export interface AgentConfig {
	/** npm package name for the ACP adapter (spawned inside the VM) */
	acpAdapter: string;
	/** npm package name for the underlying agent */
	agentPackage: string;
	/**
	 * Absolute host path to the software package directory that registered this
	 * agent config. Package-provided agent adapters should resolve their nested
	 * dependencies relative to this directory before falling back to the caller's
	 * moduleAccessCwd.
	 */
	declaringPackageDir?: string;
	/** Additional CLI args prepended when launching the ACP adapter. */
	launchArgs?: string[];
	/**
	 * Default env vars to pass when spawning the adapter. These are merged
	 * UNDER prepareInstructions env and user env (lowest priority).
	 * Typically set by package descriptors for computed paths (e.g. PI_ACP_PI_COMMAND).
	 */
	defaultEnv?: Record<string, string>;
	/**
	 * Prepare agent-specific spawn overrides for OS instruction injection.
	 * Reads /etc/agentos/instructions.md from the VM filesystem (written at boot)
	 * and returns extra CLI args and env vars to merge into the spawn call.
	 *
	 * IMPORTANT: Must extend (not replace) the user's existing config.
	 * User-provided env vars and args always take priority — callers merge as:
	 *   env: { ...prepareInstructions().env, ...userEnv }
	 */
	prepareInstructions?(
		kernel: Kernel,
		cwd: string,
		additionalInstructions?: string,
		options?: PrepareInstructionsOptions,
	): Promise<{ args?: string[]; env?: Record<string, string> }>;
}

export type AgentType = "pi" | "pi-cli" | "opencode" | "claude";

export const AGENT_CONFIGS: Record<AgentType, AgentConfig> = {
	pi: {
		acpAdapter: "@rivet-dev/agent-os-pi",
		agentPackage: "@mariozechner/pi-coding-agent",
		// OS instructions injection keeps the Pi session self-contained inside the VM.
		prepareInstructions: async (kernel, _cwd, additionalInstructions, opts) => {
			const instructions = await readVmInstructions(
				kernel,
				additionalInstructions,
				opts?.toolReference,
				opts?.skipBase,
			);
			if (!instructions) return {};
			return { args: ["--append-system-prompt", instructions] };
		},
	},
	"pi-cli": {
		acpAdapter: "pi-acp",
		agentPackage: "@mariozechner/pi-coding-agent",
		// Full CLI-based ACP adapter: spawns pi --mode rpc as a subprocess.
		// Higher memory overhead but provides full CLI feature set.
		prepareInstructions: async (kernel, _cwd, additionalInstructions, opts) => {
			const instructions = await readVmInstructions(
				kernel,
				additionalInstructions,
				opts?.toolReference,
				opts?.skipBase,
			);
			if (!instructions) return {};
			return { args: ["--append-system-prompt", instructions] };
		},
	},
	opencode: {
		// OpenCode speaks ACP natively. Agent OS runs a source-built ACP bundle
		// from @rivet-dev/agent-os-opencode entirely inside the VM.
		acpAdapter: "@rivet-dev/agent-os-opencode",
		agentPackage: "@rivet-dev/agent-os-opencode",
		defaultEnv: {
			OPENCODE_DISABLE_CONFIG_DEP_INSTALL: "1",
			OPENCODE_DISABLE_EMBEDDED_WEB_UI: "1",
		},
		// OS instructions injection: OPENCODE_CONTEXTPATHS env var with absolute
		// path to /etc/agentos/instructions.md. No cwd file writes needed; the
		// file is already on disk from VM boot. /etc/agentos/ is read-only, so we
		// only write additional session-level prompt fragments into /tmp/.
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
						INSTRUCTIONS_PATH,
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
	claude: {
		acpAdapter: "@rivet-dev/agent-os-claude",
		agentPackage: "@anthropic-ai/claude-agent-sdk",
		defaultEnv: {
			CLAUDE_AGENT_SDK_CLIENT_APP: "@rivet-dev/agent-os",
			CLAUDE_CODE_SIMPLE: "1",
			CLAUDE_CODE_FORCE_AGENT_OS_RIPGREP: "1",
			CLAUDE_CODE_DEFER_GROWTHBOOK_INIT: "1",
			CLAUDE_CODE_DISABLE_STREAM_JSON_HOOK_EVENTS: "1",
			CLAUDE_CODE_SHELL: "/bin/bash",
			CLAUDE_CODE_SKIP_INITIAL_MESSAGES: "1",
			CLAUDE_CODE_SKIP_SANDBOX_INIT: "1",
			CLAUDE_CODE_USE_PIPE_OUTPUT: "1",
			DISABLE_TELEMETRY: "1",
			SHELL: "/bin/bash",
			USE_BUILTIN_RIPGREP: "0",
		},
		prepareInstructions: async (kernel, _cwd, additionalInstructions, opts) => {
			const instructions = await readVmInstructions(
				kernel,
				additionalInstructions,
				opts?.toolReference,
				opts?.skipBase,
			);
			if (!instructions) return {};
			return { args: ["--append-system-prompt", instructions] };
		},
	},
};

// ── Agents not yet in AGENT_CONFIGS ─────────────────────────────────────
//
// Claude Code (@anthropic-ai/claude-code)
//   Cannot run in VM: native ripgrep dep, complex async startup, no TTY.
//   Speaks ACP natively (cli.js, no separate adapter).
//   Injection approach: reads /etc/agentos/instructions.md from VM,
//   passes via --append-system-prompt <text> CLI flag.
//   Zero filesystem writes. User's CLAUDE.md still loads normally.
//   Config when runnable:
//     acpAdapter: "@anthropic-ai/claude-code"
//     agentPackage: "@anthropic-ai/claude-code"
//     prepareInstructions: async (kernel, _cwd, additionalInstructions) => {
//       const instructions = await readVmInstructions(kernel, additionalInstructions);
//       return { args: ["--append-system-prompt", instructions] };
//     }
//
// Codex (@openai/codex)
//   Not yet investigated for VM compatibility (Rust binary).
//   Injection approach: reads /etc/agentos/instructions.md from VM,
//   passes via -c developer_instructions="..." CLI flag.
//   Injected as additive developer role message — does not replace built-in
//   system instructions. User's AGENTS.md still loads normally.
//   Zero filesystem writes.
//   Config when runnable:
//     acpAdapter: "@openai/codex" (or dedicated ACP adapter TBD)
//     agentPackage: "@openai/codex"
//     prepareInstructions: async (kernel, _cwd, additionalInstructions) => {
//       const instructions = await readVmInstructions(kernel, additionalInstructions);
//       return { args: ["-c", `developer_instructions=${instructions}`] };
//     }
