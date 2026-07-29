import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

// Client A: creates the session and sends prompts
const clientA = createClient<typeof registry>({
	endpoint: "http://localhost:6420",
});
const agentA = clientA.vm.getOrCreate("shared-agent");
const connA = agentA.connect();
connA.on("sessionEvent", (event) => {
	console.log("[A]", event);
});

await agentA.sessions.open({
	agent: "pi",
	env: { ANTHROPIC_API_KEY: process.env.ANTHROPIC_API_KEY! },
});
await agentA.sessions.prompt({
	content: [{ type: "text", text: "Build a REST API" }],
});

// Client B: observes the same session (separate process)
const clientB = createClient<typeof registry>({
	endpoint: "http://localhost:6420",
});
const connB = clientB.vm.getOrCreate("shared-agent").connect();
connB.on("sessionEvent", (event) => {
	console.log("[B]", event);
});
// Client B sees the same events as Client A
