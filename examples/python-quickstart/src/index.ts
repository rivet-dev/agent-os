import { AgentOs } from "@rivet-dev/agentos";

const runtime = await AgentOs.create();

try {
	const result = await runtime.python.evaluate<number>("21 * 2");
	console.log(result.outcome === "succeeded" ? result.value : result.error); // 42
} finally {
	await runtime.dispose();
}
