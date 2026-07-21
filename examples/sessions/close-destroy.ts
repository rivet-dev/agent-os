import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({
	endpoint: "http://localhost:6420",
});
const agent = client.vm.getOrCreate("my-agent");

await agent.sessions.open({
	agent: "pi",
	env: { ANTHROPIC_API_KEY: process.env.ANTHROPIC_API_KEY! },
});

// Release only the adapter. SQLite history remains and the next prompt restores it.
await agent.sessions.unload();

// Permanent deletion requires an explicit public session ID.
await agent.sessions.delete();
