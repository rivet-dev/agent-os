import { AgentOs } from "@rivet-dev/agentos";

const runtime = await AgentOs.create();

try {
	const result = await runtime.javascript.evaluate<number>(
		"inputs.items.reduce((total, item) => total + item.price, 0)",
		{ inputs: { items: [{ price: 10 }, { price: 32 }] } },
	);
	console.log(result.outcome === "succeeded" ? result.value : result.error); // 42
} finally {
	await runtime.dispose();
}
