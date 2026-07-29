import { AgentOs } from "@rivet-dev/agentos";

const runtime = await AgentOs.create();

try {
	// timeoutMs is a wall-clock deadline. It expires into a result rather than
	// throwing; a denied operation gets the normal POSIX error.
	const result = await runtime.process.exec("sleep 10", { timeoutMs: 1_000 });
	console.log(result.outcome); // "timed_out"
} finally {
	await runtime.dispose();
}
