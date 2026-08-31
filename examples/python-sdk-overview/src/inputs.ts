import { AgentOs } from "@rivet-dev/agentos-core";

const runtime = await AgentOs.create();

try {
	const result = await runtime.python.evaluate<number>(
		"sum(item['price'] for item in inputs['items'])",
		{ inputs: { items: [{ price: 10 }, { price: 32 }] } },
	);
	console.log(result.outcome === "succeeded" ? result.value : result.error); // 42
} finally {
	await runtime.dispose();
}
