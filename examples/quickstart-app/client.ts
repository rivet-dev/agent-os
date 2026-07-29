import { createClient } from "@rivet-dev/agentos/client";
import type { registry } from "./server";

const client = createClient<typeof registry>({
	endpoint: "http://localhost:6420",
});
const handle = client.vm.getOrCreate("my-agent");

// Subscribe to streaming events. The payload is inferred from the event schema.
const conn = handle.connect();
conn.on("sessionEvent", (event) => {
	console.log(event);
});

// Open a durable session and send a prompt.
await handle.sessions.open({
	agent: "pi",
	env: { ANTHROPIC_API_KEY: process.env.ANTHROPIC_API_KEY! },
});
await handle.sessions.prompt({
	content: [
		{ type: "text", text: "Write a hello world script to /workspace/hello.js" },
	],
});

// Read the file the agent created
const content = await handle.filesystem.readFile("/workspace/hello.js");
console.log(new TextDecoder().decode(content));
