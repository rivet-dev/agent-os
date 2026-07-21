import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({
	endpoint: "http://localhost:6420",
});
const agent = client.vm.getOrCreate("my-agent");

// The agent invokes the binding itself as a shell command:
//   agentos-weather forecast --city Paris --days 3
await agent.sessions.open({
	agent: "claude",
	env: { ANTHROPIC_API_KEY: process.env.ANTHROPIC_API_KEY! },
});
await agent.sessions.prompt({
	content: [{ type: "text", text: "What's the weather in Paris?" }],
});
