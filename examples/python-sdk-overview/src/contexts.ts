import { AgentOs } from "@rivet-dev/agentos-core";

const runtime = await AgentOs.create();

try {
	await runtime.createContext("analysis");

	await runtime.python.execute("answer = 40", { contextId: "analysis" });

	const result = await runtime.python.evaluate<number>("answer + 2", {
		contextId: "analysis",
	});
	console.log(result.outcome === "succeeded" ? result.value : result.error); // 42

	// Delete an idle context when you are done with it. `contexts.reset()`
	// is the other option: it keeps the id and drops only the interpreter state.
	await runtime.contexts.delete("analysis");
} finally {
	await runtime.dispose();
}
