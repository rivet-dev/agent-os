import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server-minimal";

const client = createClient<typeof registry>({
	endpoint: "http://localhost:6420",
});
const agent = client.vm.getOrCreate("my-agent");

// The Browserbase credentials go into the session environment, so every
// command the agent runs — including `browse` — inherits them.
await agent.sessions.open({
	agent: "pi",
	env: {
		BROWSERBASE_API_KEY: process.env.BROWSERBASE_API_KEY!,
		BROWSERBASE_PROJECT_ID: process.env.BROWSERBASE_PROJECT_ID!,
		ANTHROPIC_API_KEY: process.env.ANTHROPIC_API_KEY!,
	},
});

const response = await agent.sessions.prompt({
	content: [
		{
			type: "text",
			text: "Run `browse cloud fetch https://example.com` and tell me in one sentence what the page is about.",
		},
	],
});
console.log(response.message?.content ?? []);

await agent.sessions.delete();
