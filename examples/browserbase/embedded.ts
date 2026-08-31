import browserbase from "@agentos-software/browserbase";
import pi from "@agentos-software/pi";
import { AgentOs } from "@rivet-dev/agentos-core";

const { ANTHROPIC_API_KEY, BROWSERBASE_API_KEY, BROWSERBASE_PROJECT_ID } =
	process.env;
if (!ANTHROPIC_API_KEY || !BROWSERBASE_API_KEY || !BROWSERBASE_PROJECT_ID) {
	throw new Error("Set the Anthropic and Browserbase environment variables.");
}

const vm = await AgentOs.create({ software: [pi, browserbase] });

try {
	await vm.sessions.open({
		agent: "pi",
		env: {
			ANTHROPIC_API_KEY,
			BROWSERBASE_API_KEY,
			BROWSERBASE_PROJECT_ID,
		},
	});
	await vm.sessions.prompt({
		content: [
			{
				type: "text",
				text: "Use browse to summarize https://example.com.",
			},
		],
	});
} finally {
	await vm.dispose();
}
