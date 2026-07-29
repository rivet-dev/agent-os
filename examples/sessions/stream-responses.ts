import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({
	endpoint: "http://localhost:6420",
});
const agent = client.vm.getOrCreate("my-agent");
const conn = agent.connect();

// Subscribe to session events before sending the prompt
conn.on("sessionEvent", (event) => {
	console.log(`[${event.sessionId}]`, event.durability, event);
});

await agent.sessions.open({
	agent: "pi",
	env: { ANTHROPIC_API_KEY: process.env.ANTHROPIC_API_KEY! },
});
await agent.sessions.prompt({
	content: [{ type: "text", text: "Explain how async/await works" }],
});
