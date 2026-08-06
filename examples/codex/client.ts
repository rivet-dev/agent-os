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
	await agent.process.exec(
		"npm install -g @modelcontextprotocol/server-filesystem",
	);

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

// docs:start agent-plugins
// ── Agent Plugins ─────────────────────────────────────────────────
//
// Install a portable Agent Plugin into Codex's cache before opening the
// session. Other compatible clients use their own installation locations.
async function withAgentPlugin() {
	const pluginRoot =
		"/home/agentos/.codex/plugins/cache/local/release-workflow/1.0.0";
	await agent.filesystem.mkdir(`${pluginRoot}/.codex-plugin`);
	await agent.filesystem.mkdir(`${pluginRoot}/skills/release-notes`);

	await agent.filesystem.writeFile(
		`${pluginRoot}/.codex-plugin/plugin.json`,
		JSON.stringify({
			$schema: "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json",
			name: "release-workflow",
			version: "1.0.0",
			description: "Project release workflow",
		}),
	);
	await agent.filesystem.writeFile(
		`${pluginRoot}/skills/release-notes/SKILL.md`,
		`---
name: release-notes
description: Write release notes for this project.
---

Summarize user-visible changes under Added, Changed, and Fixed.
`,
	);

	await agent.filesystem.writeFile(
		"/home/agentos/.codex/config.toml",
		`[features]
plugins = true

[plugins."release-workflow@local"]
enabled = true
`,
	);

	await agent.sessions.open({
		agent: "codex",
		env: { OPENAI_API_KEY: process.env.OPENAI_API_KEY! },
	});
	await agent.sessions.prompt({
		content: [
			{
				type: "text",
				text: "Use $release-workflow:release-notes for the current changes.",
			},
		],
	});
}
// docs:end agent-plugins

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
	await agent.process.exec(
		"npm install -g @modelcontextprotocol/server-filesystem",
	);

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

export { quickStart, withAgentPlugin, withMcp, withSkill, withSkillAndMcp };
