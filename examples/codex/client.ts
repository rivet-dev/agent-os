// docs:start quickstart
import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({
	endpoint: "http://localhost:6420",
});
const agent = client.vm.getOrCreate("my-agent");

// ── Quick start ───────────────────────────────────────────────────
async function quickStart() {
	await agent.sessions.open({
		agent: "codex",
		env: { OPENAI_API_KEY: process.env.OPENAI_API_KEY! },
	});

	const result = await agent.sessions.prompt({
		content: [
			{ type: "text", text: "What files are in the current directory?" },
		],
	});
	console.log(result.message?.content ?? []);
}
// docs:end quickstart

// docs:start skills
// ── Skills ────────────────────────────────────────────────────────
//
// Write a SKILL.md into the agent's skills directory before creating the
// session and the agent discovers it automatically.
async function withSkill() {
	const skill = `---
name: commit-style
description: How to write commit messages in this project.
---

Write commit messages in the imperative mood and keep the subject under 50 characters.
`;

	await agent.filesystem.mkdir("/home/agentos/.codex/skills/commit-style");
	await agent.filesystem.writeFile(
		"/home/agentos/.codex/skills/commit-style/SKILL.md",
		skill,
	);

	await agent.sessions.open({
		agent: "codex",
		env: { OPENAI_API_KEY: process.env.OPENAI_API_KEY! },
	});
}
// docs:end skills

// docs:start mcp
// ── MCP servers ───────────────────────────────────────────────────
//
// Codex reads MCP servers from its own config file. Write a `config.toml`
// into the VM before creating the session — local child-process servers and
// remote URLs are both supported.
async function withMcp() {
	// Pre-install the MCP server so `npx` is silent — first-run install output
	// would otherwise corrupt the MCP stdio handshake ("Connection closed").
	await agent.process.exec("npm install -g @modelcontextprotocol/server-filesystem");

	const config = `[mcp_servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/home/agentos"]

[mcp_servers.example]
url = "https://mcp.example.com/sse"
http_headers = { Authorization = "Bearer my-token" }
`;

	await agent.filesystem.writeFile("/home/agentos/.codex/config.toml", config);

	await agent.sessions.open({
		agent: "codex",
		env: { OPENAI_API_KEY: process.env.OPENAI_API_KEY! },
	});
}
// docs:end mcp

// ── Skills + MCP together ─────────────────────────────────────────
async function withSkillAndMcp() {
	const skill = `---
name: commit-style
description: How to write commit messages in this project.
---

Write commit messages in the imperative mood and keep the subject under 50 characters.
`;

	await agent.filesystem.mkdir("/home/agentos/.codex/skills/commit-style");
	await agent.filesystem.writeFile(
		"/home/agentos/.codex/skills/commit-style/SKILL.md",
		skill,
	);

	// Pre-install the MCP server so `npx` is silent — first-run install output
	// would otherwise corrupt the MCP stdio handshake ("Connection closed").
	await agent.process.exec("npm install -g @modelcontextprotocol/server-filesystem");

	const config = `[mcp_servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/home/agentos"]
`;

	await agent.filesystem.writeFile("/home/agentos/.codex/config.toml", config);

	await agent.sessions.open({
		agent: "codex",
		env: { OPENAI_API_KEY: process.env.OPENAI_API_KEY! },
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

export { quickStart, withSkill, withMcp, withSkillAndMcp };
