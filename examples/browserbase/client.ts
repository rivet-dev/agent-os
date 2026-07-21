import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({
	endpoint: "http://localhost:6420",
});
const agent = client.vm.getOrCreate("my-agent");

// ── 1. Run the browse CLI yourself ──────────────────────────────────────────
// Drive `browse` directly through the VM's process API. `browse cloud fetch`
// retrieves a page through the Browserbase cloud and returns it as JSON with the
// page rendered as markdown. `browse` reads its credentials from the command
// environment, which we pass through `exec`.
const env = {
	BROWSERBASE_API_KEY: process.env.BROWSERBASE_API_KEY!,
	BROWSERBASE_PROJECT_ID: process.env.BROWSERBASE_PROJECT_ID!,
};

const { stdout } = await agent.process.exec("browse cloud fetch https://example.com", {
	env,
});

const page = JSON.parse(stdout) as { statusCode: number; content: string };
console.log(`fetched status ${page.statusCode}`);
console.log(page.content);

// ── 2. Let an agent use the browse CLI ──────────────────────────────────────
// The server mounts `skills/` into Claude Code's skills directory, so the agent
// discovers Browserbase's `browse` CLI skill and reaches for it on its own. The
// Browserbase credentials go into the session environment, so every command the
// agent runs — including `browse` — inherits them.
await agent.sessions.open({
	agent: "claude",
	env: {
		BROWSERBASE_API_KEY: process.env.BROWSERBASE_API_KEY!,
		BROWSERBASE_PROJECT_ID: process.env.BROWSERBASE_PROJECT_ID!,
		ANTHROPIC_API_KEY: process.env.ANTHROPIC_API_KEY!,
	},
});

// Note: no mention of `browse` — the mounted skill tells the agent to use it.
const response = await agent.sessions.prompt({
	content: [
		{
			type: "text",
			text: "What is the page at https://example.com about? Answer in one sentence.",
		},
	],
});
console.log(response.message?.content ?? []);

await agent.sessions.delete();
