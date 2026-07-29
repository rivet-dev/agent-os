import { AgentOs } from "@rivet-dev/agentos";

const runtime = await AgentOs.create();

try {
	const sum = await runtime.javascript.evaluate<number>("1 + 2");
	console.log(sum.outcome === "succeeded" ? sum.value : sum.error); // 3

	// More than one statement goes in a function, so the code has somewhere to
	// return from.
	const report = await runtime.javascript.evaluate<{ total: number }>(`
		(() => {
			const values = [1, 2, 3];
			return { total: values.reduce((a, b) => a + b, 0) };
		})()
	`);
	console.log(report.outcome === "succeeded" ? report.value : report.error); // { total: 6 }
} finally {
	await runtime.dispose();
}
