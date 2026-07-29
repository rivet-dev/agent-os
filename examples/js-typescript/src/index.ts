import { AgentOs } from "@rivet-dev/agentos";

const runtime = await AgentOs.create();

try {
	// Type checking validates an agent's generated code before it executes.
	// `filePath` labels the source for diagnostics; it is never read from disk.
	const checked = await runtime.typescript.check(
		`const total: number = "not a number";`,
		{ filePath: "example.ts" },
	);

	// Feed these diagnostics back to the agent so it can fix the code it
	// generated and try again, instead of running code you know is broken.
	for (const diagnostic of checked.diagnostics) {
		// error TS2322: Type 'string' is not assignable to type 'number'.
		console.log(
			`${diagnostic.category} TS${diagnostic.code}: ${diagnostic.message}`,
		);
	}

	// Executing TypeScript only transpiles it, so check first if it matters.
	if (checked.diagnostics.length === 0) {
		await runtime.typescript.execute(`const total: number = 42;`);
	}
} finally {
	await runtime.dispose();
}
