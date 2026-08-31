import { AgentOs } from "@rivet-dev/agentos-core";

const rt = await AgentOs.create();

try {
	const { stdout, stderr, exitCode } = await rt.javascript.execute(
		`
			console.log("hello from the VM");
			console.error("oops from the VM");
			process.exit(3);
		`,
		{ output: { capture: "all" } },
	);

	console.log("exitCode:", exitCode);
	console.log("stdout:", JSON.stringify(stdout));
	console.log("stderr:", JSON.stringify(stderr));
} finally {
	await rt.dispose();
}
