import { AgentOs } from "@rivet-dev/agentos";

const runtime = await AgentOs.create();

try {
	// timeoutMs is a wall-clock deadline for the whole operation. It expires
	// into a result rather than throwing, and never replaces the VM watchdogs.
	const result = await runtime.python.execute("import time; time.sleep(10)", {
		timeoutMs: 2_000,
	});
	console.log(result.outcome); // "timed_out"
} finally {
	await runtime.dispose();
}
