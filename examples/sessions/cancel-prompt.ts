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

// Start a long-running prompt
const promptPromise = agent.sessions.prompt({
	content: [
		{
			type: "text",
			text: "Refactor the entire codebase to use TypeScript strict mode",
		},
	],
});

// Cancellation is cooperative and does not delete durable history.
setTimeout(async () => {
	await agent.sessions.cancelPrompt();
}, 10_000);

const response = await promptPromise;
console.log(response.message?.content ?? []);
