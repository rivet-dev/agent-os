import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({
	endpoint: "http://localhost:6420",
});
const agent = client.vm.getOrCreate("my-agent");

// ── Quick start ───────────────────────────────────────────────────
// docs:start quick-start
async function quickStart() {
	await agent.sessions.open({
		agent: "pi",
		env: { ANTHROPIC_API_KEY: process.env.ANTHROPIC_API_KEY! },
	});

	const result = await agent.sessions.prompt({
		content: [
			{ type: "text", text: "What files are in the current directory?" },
		],
	});
	console.log(result.message?.content ?? []);
}
// docs:end quick-start

// ── Skills ────────────────────────────────────────────────────────
//
// Write a SKILL.md into the agent's skills directory before creating the
// session and the agent discovers it automatically.
// docs:start skill
async function withSkill() {
	const skill = `---
name: commit-style
description: How to write commit messages in this project.
---

Write commit messages in the imperative mood and keep the subject under 50 characters.
`;

	await agent.filesystem.mkdir("/home/agentos/.pi/agent/skills/commit-style");
	await agent.filesystem.writeFile(
		"/home/agentos/.pi/agent/skills/commit-style/SKILL.md",
		skill,
	);

	await agent.sessions.open({
		agent: "pi",
		env: { ANTHROPIC_API_KEY: process.env.ANTHROPIC_API_KEY! },
	});
}
// docs:end skill

// ── MCP servers ───────────────────────────────────────────────────
//
// Pi discovers MCP servers from its own config file. Write an `.mcp.json`
// into the agent's config directory before creating the session — local
// child-process servers and remote URLs are both supported.
async function withMcp() {
	// Pre-install the MCP server so `npx` is silent — first-run install output
	// would otherwise corrupt the MCP stdio handshake ("Connection closed").
	await agent.process.exec("npm install -g @modelcontextprotocol/server-filesystem");

	// docs:start mcp
	const mcpConfig = JSON.stringify({
		mcpServers: {
			filesystem: {
				command: "npx",
				args: [
					"-y",
					"@modelcontextprotocol/server-filesystem",
					"/home/agentos",
				],
			},
			remote: {
				url: "https://mcp.example.com/sse",
				headers: { Authorization: "Bearer my-token" },
			},
		},
	});

	await agent.filesystem.mkdir("/home/agentos/.pi/agent");
	await agent.filesystem.writeFile("/home/agentos/.pi/agent/.mcp.json", mcpConfig);

	await agent.sessions.open({
		agent: "pi",
		env: { ANTHROPIC_API_KEY: process.env.ANTHROPIC_API_KEY! },
	});
	// docs:end mcp
}

// ── Skills + MCP together ─────────────────────────────────────────
async function withSkillAndMcp() {
	const skill = `---
name: commit-style
description: How to write commit messages in this project.
---

Write commit messages in the imperative mood and keep the subject under 50 characters.
`;

	await agent.filesystem.mkdir("/home/agentos/.pi/agent/skills/commit-style");
	await agent.filesystem.writeFile(
		"/home/agentos/.pi/agent/skills/commit-style/SKILL.md",
		skill,
	);

	// Pre-install the MCP server so `npx` is silent — first-run install output
	// would otherwise corrupt the MCP stdio handshake ("Connection closed").
	await agent.process.exec("npm install -g @modelcontextprotocol/server-filesystem");

	const mcpConfig = JSON.stringify({
		mcpServers: {
			filesystem: {
				command: "npx",
				args: [
					"-y",
					"@modelcontextprotocol/server-filesystem",
					"/home/agentos",
				],
			},
		},
	});
	await agent.filesystem.mkdir("/home/agentos/.pi/agent");
	await agent.filesystem.writeFile("/home/agentos/.pi/agent/.mcp.json", mcpConfig);

	await agent.sessions.open({
		agent: "pi",
		env: { ANTHROPIC_API_KEY: process.env.ANTHROPIC_API_KEY! },
	});

	const result = await agent.sessions.prompt({
		content: [
			{
				type: "text",
				text: "Stage everything and write a commit message following the project skill.",
			},
		],
	});
	console.log(result.message?.content ?? []);
}

// ── Extensions ────────────────────────────────────────────────────
//
// Write a `.js` extension into the agent's extensions directory before
// creating the session and the agent discovers it automatically.
async function withExtension() {
	// docs:start extension
	const extensionCode = `
export default function(pi) {
  // Modify the system prompt before each agent turn
  pi.on("before_agent_start", async (event) => {
    return {
      systemPrompt: event.systemPrompt +
        "\\n\\nAlways respond in formal English."
    };
  });
}
`;

	// Write the extension before creating the session
	await agent.filesystem.mkdir("/home/agentos/.pi/agent/extensions");
	await agent.filesystem.writeFile(
		"/home/agentos/.pi/agent/extensions/formal.js",
		extensionCode,
	);

	// Pi discovers the extension automatically
	await agent.sessions.open({
		agent: "pi",
		env: { ANTHROPIC_API_KEY: process.env.ANTHROPIC_API_KEY! },
	});
	// docs:end extension
}

export { quickStart, withSkill, withMcp, withSkillAndMcp, withExtension };
