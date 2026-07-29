import { AgentOs } from "@rivet-dev/agentos";

const runtime = await AgentOs.create();

try {
	const shell = await runtime.process.spawn("sh", [], {
		output: { retainEvents: true }, // buffer output so it can be replayed
	});

	await runtime.process.writeStdin(shell.pid, "echo from the background\n");
	await runtime.process.resizePty(shell.pid, { cols: 120, rows: 40 });
	await runtime.process.closeStdin(shell.pid);

	const exit = await runtime.process.wait(shell.pid);
	const replay = await runtime.process.readOutput(shell.pid);
	console.log(exit.exitCode, replay.events.length); // 0 1
} finally {
	await runtime.dispose();
}
