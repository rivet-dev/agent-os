import { AgentOs } from "@rivet-dev/agentos-core";

const runtime = await AgentOs.create();

try {
	await runtime.python.executeFile("/workspace/report.py", {
		args: ["--json"],
	});
	await runtime.python.executeModule("http.server", { args: ["8000"] }); // python -m

	// Inline Python supports top-level await, `async for`, and `async with`.
	await runtime.createContext("async");
	const result = await runtime.python.evaluate<string>("await fetch_data()", {
		contextId: "async",
	});
	console.log(result.outcome === "succeeded" ? result.value : result.error);
} finally {
	await runtime.dispose();
}
