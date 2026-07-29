import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({
	endpoint: "http://localhost:6420",
});
const agent = client.vm.getOrCreate("my-agent");

// Create two sessions in the same VM
const coderSessionId = "coder";
await agent.sessions.open({
	sessionId: coderSessionId,
	agent: "pi",
	env: { ANTHROPIC_API_KEY: process.env.ANTHROPIC_API_KEY! },
});
const reviewerSessionId = "reviewer";
await agent.sessions.open({
	sessionId: reviewerSessionId,
	agent: "pi",
	env: { ANTHROPIC_API_KEY: process.env.ANTHROPIC_API_KEY! },
});

// Coder writes code
await agent.sessions.prompt({
	sessionId: coderSessionId,
	content: [{ type: "text", text: "Write a REST API at /workspace/api.ts" }],
});

// Reviewer reads and reviews the same file
await agent.sessions.prompt({
	sessionId: reviewerSessionId,
	content: [{ type: "text", text: "Review /workspace/api.ts for issues" }],
});

// Unload each adapter independently while retaining both histories.
await agent.sessions.unload({ sessionId: coderSessionId });
await agent.sessions.unload({ sessionId: reviewerSessionId });
