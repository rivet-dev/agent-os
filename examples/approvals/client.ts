import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({
	endpoint: "http://localhost:6420",
});
const agent = client.vm.getOrCreate("my-agent");

// Permission requests use the ordinary durable session-event stream.
const conn = agent.connect();
conn.on("sessionEvent", (event) => {
	if (event.type !== "permission_request") return;
	const option = event.options.find(
		(candidate) => candidate.kind === "allow_once",
	);
	if (option) {
		agent.sessions
			.respondPermission({
				sessionId: event.sessionId,
				requestId: event.requestId,
				optionId: option.optionId,
			})
			.catch((error) => console.error("Permission response failed:", error));
	}
});

await agent.sessions.open({
	agent: "pi",
	// Required for permission_request events; the default is allow_all.
	permissionPolicy: "ask",
	env: { ANTHROPIC_API_KEY: process.env.ANTHROPIC_API_KEY! },
});
await agent.sessions.prompt({
	content: [{ type: "text", text: "Create /workspace/output.txt" }],
});
