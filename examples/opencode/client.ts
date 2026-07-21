import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({
	endpoint: "http://localhost:6420",
});
const agent = client.vm.getOrCreate("my-agent");

// ── Quick start ───────────────────────────────────────────────────
async function quickStart() {
	// docs:start quickstart
	await agent.sessions.open({
		agent: "opencode",
		env: { ANTHROPIC_API_KEY: process.env.ANTHROPIC_API_KEY! },
	});

	const result = await agent.sessions.prompt({
		content: [
			{ type: "text", text: "What files are in the current directory?" },
		],
	});
	console.log(result.message?.content ?? []);
	// docs:end quickstart
}

// ── Skills ────────────────────────────────────────────────────────
//
// Write a SKILL.md into the agent's skills directory before creating the
// session and the agent discovers it automatically.
async function withSkill() {
	// docs:start skills
	const skill = `---
name: commit-style
description: How to write commit messages in this project.
---

Write commit messages in the imperative mood and keep the subject under 50 characters.
`;

	// Write the skill before creating the session
	await agent.filesystem.mkdir("/home/agentos/.config/opencode/skills/commit-style");
	await agent.filesystem.writeFile(
		"/home/agentos/.config/opencode/skills/commit-style/SKILL.md",
		skill,
	);

	// OpenCode discovers the skill automatically
	await agent.sessions.open({
		agent: "opencode",
		env: { ANTHROPIC_API_KEY: process.env.ANTHROPIC_API_KEY! },
	});
	// docs:end skills
}

// ── MCP servers ───────────────────────────────────────────────────
//
// OpenCode reads MCP servers from its own config file. Write an
// `opencode.json` into the VM before creating the session — local
// child-process servers and remote URLs are both supported.
async function withMcp() {
	// Pre-install the MCP server so `npx` is silent — first-run install output
	// would otherwise corrupt the MCP stdio handshake ("Connection closed").
	await agent.process.exec("npm install -g @modelcontextprotocol/server-filesystem");

	// docs:start mcp
	const config = {
		mcp: {
			filesystem: {
				type: "local",
				command: [
					"npx",
					"-y",
					"@modelcontextprotocol/server-filesystem",
					"/home/agentos",
				],
				enabled: true,
			},
			example: {
				type: "remote",
				url: "https://mcp.example.com/sse",
				headers: { Authorization: "Bearer my-token" },
				enabled: true,
			},
		},
	};

	await agent.filesystem.writeFile(
		"/home/agentos/.config/opencode/opencode.json",
		JSON.stringify(config, null, 2),
	);

	await agent.sessions.open({
		agent: "opencode",
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

	await agent.filesystem.mkdir("/home/agentos/.config/opencode/skills/commit-style");
	await agent.filesystem.writeFile(
		"/home/agentos/.config/opencode/skills/commit-style/SKILL.md",
		skill,
	);

	// Pre-install the MCP server so `npx` is silent — first-run install output
	// would otherwise corrupt the MCP stdio handshake ("Connection closed").
	await agent.process.exec("npm install -g @modelcontextprotocol/server-filesystem");

	const config = {
		mcp: {
			filesystem: {
				type: "local",
				command: [
					"npx",
					"-y",
					"@modelcontextprotocol/server-filesystem",
					"/home/agentos",
				],
				enabled: true,
			},
		},
	};

	await agent.filesystem.writeFile(
		"/home/agentos/.config/opencode/opencode.json",
		JSON.stringify(config, null, 2),
	);

	await agent.sessions.open({
		agent: "opencode",
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

export { quickStart, withSkill, withMcp, withSkillAndMcp };
