import { AgentOs } from "@rivet-dev/agentos";

const runtime = await AgentOs.create();

try {
	const result = await runtime.javascript.execute(
		`console.log("hello from agentOS")`,
		{ output: { capture: "all" } },
	);
	console.log(result.outcome === "succeeded" ? result.stdout : result.error); // "hello from agentOS\n"
} finally {
	await runtime.dispose();
}
