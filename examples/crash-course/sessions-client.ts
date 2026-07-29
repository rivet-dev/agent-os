import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({
	endpoint: "http://localhost:6420",
});
const agent = client.vm.getOrCreate("my-agent");

// Stream events as they arrive
const conn = agent.connect();
conn.on("sessionEvent", (event) => {
	console.log(event);
});

// Create a session
await agent.sessions.open({
	agent: "pi",
	env: { ANTHROPIC_API_KEY: process.env.ANTHROPIC_API_KEY! },
});

// Send a prompt and wait for the response
const response = await agent.sessions.prompt({
	content: [{ type: "text", text: "List all files in the home directory" }],
});
console.log(response.message?.content ?? []);
