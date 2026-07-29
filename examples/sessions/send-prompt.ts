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
const response = await agent.sessions.prompt({
	content: [
		{
			type: "text",
			text: "Create a TypeScript function that checks if a number is prime",
		},
	],
});
console.log(response.message?.content ?? []);
