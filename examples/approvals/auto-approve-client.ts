import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({
	endpoint: "http://localhost:6420",
});
const agent = client.vm.getOrCreate("my-agent");

// allow_all selects an adapter-supplied allow option without a client round-trip.
await agent.sessions.open({
	agent: "pi",
	permissionPolicy: "allow_all",
	env: { ANTHROPIC_API_KEY: process.env.ANTHROPIC_API_KEY! },
});
await agent.sessions.prompt({
	content: [{ type: "text", text: "Write files as needed" }],
});
