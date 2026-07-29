#!/usr/bin/env node
import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { installOpenCodeNodeCompatibility } from "./node-compat.js";

process.env.OPENCODE_DISABLE_CONFIG_DEP_INSTALL ??= "1";
process.env.OPENCODE_DISABLE_EMBEDDED_WEB_UI ??= "1";
process.env.OPENCODE_DISABLE_MODELS_FETCH ??= "1";
process.env.OPENCODE_MODELS_PATH ??= fileURLToPath(
	new URL("./opencode-acp/models.json", import.meta.url),
);
installOpenCodeNodeCompatibility();

const agentOsPrompt = process.env.ACP_APPEND_SYSTEM_PROMPT;
if (agentOsPrompt && !process.env.OPENCODE_CONTEXTPATHS) {
	const promptPath = "/tmp/agentos-system-prompt.md";
	writeFileSync(promptPath, agentOsPrompt);
	process.env.OPENCODE_CONTEXTPATHS = JSON.stringify([
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
		promptPath,
	]);
}

// @ts-expect-error Generated at build time by scripts/build-opencode-acp.mjs.
const { AcpCommand } = (await import("./opencode-acp/acp.js")) as {
	AcpCommand: {
		handler(args: {
			port: number;
			hostname: string;
			mdns: boolean;
			"mdns-domain": string;
			cors: string[];
			cwd: string;
		}): Promise<void>;
	};
};

await AcpCommand.handler({
	port: 0,
	hostname: "127.0.0.1",
	mdns: false,
	"mdns-domain": "opencode.local",
	cors: [],
	cwd: process.cwd(),
});

export {};
