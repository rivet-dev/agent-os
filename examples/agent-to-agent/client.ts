import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({
	endpoint: "http://localhost:6420",
});
const writerAgent = client.writer.getOrCreate("my-project");

await writerAgent.sessions.open({
	agent: "claude",
	env: { ANTHROPIC_API_KEY: process.env.ANTHROPIC_API_KEY! },
});

// The writer calls the `review` binding collection, which bridges to the reviewer VM.
await writerAgent.sessions.prompt({
	content: [
		{
			type: "text",
			text: "Write a small REST API, then send it to the review agent for review.",
		},
	],
});
