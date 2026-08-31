import pi from "@agentos-software/pi";
import { AgentOs } from "@rivet-dev/agentos-core";

const apiKey = process.env.ANTHROPIC_API_KEY;
if (!apiKey) {
	throw new Error("Set ANTHROPIC_API_KEY before running this example.");
}

const vm = await AgentOs.create({ software: [pi] });

try {
	await vm.sessions.open({
		agent: "pi",
		env: { ANTHROPIC_API_KEY: apiKey },
	});
} finally {
	await vm.dispose();
}
