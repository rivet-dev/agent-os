import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({
	endpoint: "http://localhost:6420",
});
const agent = client.vm.getOrCreate("my-agent");

// Select exact adapter-supplied options from the generic session-event stream.
const conn = agent.connect();
conn.on("sessionEvent", (event) => {
	if (event.type !== "permission_request") return;
	const toolCall = JSON.stringify(event.toolCall).toLowerCase();
	const desiredKind = toolCall.includes("read") ? "allow_once" : "reject_once";
	const option = event.options.find(
		(candidate) => candidate.kind === desiredKind,
	);
	if (!option) return;
	agent.sessions
		.respondPermission({
			sessionId: event.sessionId,
			requestId: event.requestId,
			optionId: option.optionId,
		})
		.catch((error) => console.error("Permission response failed:", error));
});

await agent.sessions.open({
	agent: "claude",
	// Required for permission_request events; the default is allow_all.
	permissionPolicy: "ask",
	env: { ANTHROPIC_API_KEY: process.env.ANTHROPIC_API_KEY! },
});
await agent.sessions.prompt({
	content: [{ type: "text", text: "Read config.json and update it" }],
});
