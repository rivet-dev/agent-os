import { agentOS, setup } from "@rivet-dev/agentos";
import { createClient } from "@rivet-dev/agentos/client";
import { z } from "zod";

// The reviewer is its own isolated agent VM.
const reviewer = agentOS({});

// Bridge the writer to the reviewer. The VMs share no filesystem, so the writer
// sends the full file contents; the bridge writes them into the reviewer's VM
// and asks the reviewer to review. Runs on the host.
async function reviewCode(code: string): Promise<string> {
	const client = createClient<typeof registry>({
		endpoint: "http://localhost:6420",
	});
	const reviewerHandle = client.reviewer.getOrCreate("my-project");

	// Write the submitted contents into the reviewer's VM.
	await reviewerHandle.filesystem.writeFile("/home/agentos/review.ts", code);

	// Ask the reviewer to review.
	await reviewerHandle.sessions.open({
		agent: "claude",
		env: { ANTHROPIC_API_KEY: process.env.ANTHROPIC_API_KEY! },
	});
	const result = await reviewerHandle.sessions.prompt({
		content: [
			{
				type: "text",
				text: "Review the code at /home/agentos/review.ts and list any issues.",
			},
		],
	});
	await reviewerHandle.sessions.delete();

	return (
		result.message?.content
			.filter((block) => block.type === "text")
			.map((block) => block.text)
			.join("") ?? ""
	);
}

// The writer agent gets a `review` binding collection. When the writer runs
// `agentos-review submit`, the bridge above executes on the host.
const writer = agentOS({
	bindings: [
		{
			name: "review",
			description: "Send code to the reviewer agent and get back a review.",
			bindings: {
				submit: {
					description:
						"Submit the full contents of a file to the reviewer agent for review. Returns the reviewer's feedback as text.",
					inputSchema: z.object({
						code: z.string().describe("The full source code to review."),
					}),
					execute: async (input: { code: string }) => ({
						review: await reviewCode(input.code),
					}),
				},
			},
		},
	],
});

export const registry = setup({ use: { writer, reviewer } });

registry.start();
