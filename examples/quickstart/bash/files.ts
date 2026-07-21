import { AgentOs } from "@rivet-dev/agentos";

const runtime = await AgentOs.create();

try {
	// Files written by the shell are the same files agents, JavaScript, and
	// Python see — one persistent VM filesystem.
	await runtime.process.exec("printf 'a\\nb\\nc\\n' > /workspace/out.txt");

	const lines = await runtime.process.exec("cat /workspace/out.txt | wc -l", {
		output: { capture: "all" },
	});
	console.log(lines.stdout?.trim()); // "3"
} finally {
	await runtime.dispose();
}
