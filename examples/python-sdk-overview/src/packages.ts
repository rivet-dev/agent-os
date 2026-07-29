import { AgentOs } from "@rivet-dev/agentos";

const runtime = await AgentOs.create();

try {
	const installed = await runtime.python.install(["requests==2.32.4"], {
		upgrade: true,
	});
	console.log(installed.outcome); // "succeeded"

	await runtime.python.install({ requirementsFile: "requirements.txt" });
} finally {
	await runtime.dispose();
}
