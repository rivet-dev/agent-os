import { agentOS, setup } from "@rivet-dev/agentos";
import { createClient } from "@rivet-dev/agentos/client";
import { z } from "zod";
import pi from "@agentos-software/pi";

// The reviewer is its own isolated agent VM.
const reviewer = agentOS({ software: [pi] });

// The coder gets a `review` binding collection it can call itself: it copies a file from the
// coder's VM into the reviewer's VM and asks the reviewer to review it.
const coder = agentOS({
	software: [pi],
	bindings: [
		{
			name: "review",
			description: "Send a file to the reviewer agent and get back a review.",
			bindings: {
				submit: {
					description: "Submit a file path for review by the reviewer agent.",
					inputSchema: z.object({ path: z.string() }),
					execute: async ({ path }: { path: string }) => {
						const client = createClient<typeof registry>({
							endpoint: "http://localhost:6420",
						});
						const content = await client.coder
							.getOrCreate("feature-auth")
							.filesystem.readFile(path);
						const reviewerHandle = client.reviewer.getOrCreate("feature-auth");
						await reviewerHandle.filesystem.writeFile(path, content);
						await reviewerHandle.sessions.open({
							agent: "pi",
							env: { ANTHROPIC_API_KEY: process.env.ANTHROPIC_API_KEY! },
						});
						const result = await reviewerHandle.sessions.prompt({
							content: [
								{ type: "text", text: `Review ${path} for security issues` },
							],
						});
						return {
							review:
								result.message?.content
									.filter((block) => block.type === "text")
									.map((block) => block.text)
									.join("") ?? "",
						};
					},
				},
			},
		},
	],
});

export const registry = setup({ use: { coder, reviewer } });
registry.start();
