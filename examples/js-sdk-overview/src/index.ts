import { AgentOs } from "@rivet-dev/agentos-core";

const runtime = await AgentOs.create();

try {
	const result = await runtime.javascript.execute(
		`
		export const message = await Promise.resolve("hello from agentOS");
		console.log(message);
		`,
		{ output: { capture: "all" } },
	);
	console.log(result.outcome === "succeeded" ? result.stdout : result.error); // "hello from agentOS\n"
} finally {
	await runtime.dispose();
}
