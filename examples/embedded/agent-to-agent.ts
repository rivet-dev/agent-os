import pi from "@agentos-software/pi";
import { AgentOs, type Bindings } from "@rivet-dev/agentos-core";
import { z } from "zod";

const apiKey = process.env.ANTHROPIC_API_KEY;
if (!apiKey) {
	throw new Error("Set ANTHROPIC_API_KEY before running this example.");
}

const reviewer = await AgentOs.create({ software: [pi] });
await reviewer.sessions.open({
	agent: "pi",
	env: { ANTHROPIC_API_KEY: apiKey },
});

const review: Bindings = {
	name: "review",
	description: "Ask the reviewer agent for feedback",
	bindings: {
		draft: {
			description: "Review a draft",
			inputSchema: z.object({ draft: z.string() }),
			execute: async ({ draft }: { draft: string }) => {
				const response = await reviewer.sessions.prompt({
					content: [{ type: "text", text: `Review this draft:\n\n${draft}` }],
				});
				const feedback =
					response.message?.content
						.filter((block) => block.type === "text")
						.map((block) => block.text)
						.join("") ?? "";
				return { feedback };
			},
		},
	},
};

const writer = await AgentOs.create({ software: [pi], bindings: [review] });

try {
	await writer.sessions.open({
		agent: "pi",
		env: { ANTHROPIC_API_KEY: apiKey },
	});
	await writer.sessions.prompt({
		content: [
			{
				type: "text",
				text: "Draft a release note, then ask the review binding for feedback.",
			},
		],
	});
} finally {
	await writer.dispose();
	await reviewer.dispose();
}
