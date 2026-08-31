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

	await vm.sessions.prompt({
		content: [
			{
				type: "text",
				text: "Create /home/agentos/hello-world.js that prints hello world.",
			},
		],
	});

	const script = await vm.filesystem.readFile("/home/agentos/hello-world.js");
	console.log(new TextDecoder().decode(script));
} finally {
	await vm.dispose();
}
