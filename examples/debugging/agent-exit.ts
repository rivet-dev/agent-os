import pi from "@agentos-software/pi";
import { AgentOs } from "@rivet-dev/agentos-core";

const agentOs = await AgentOs.create({
	software: [pi],
	onAgentExit(event) {
		// A later explicit prompt restores the durable session through resume,
		// load, or bounded transcript continuation.
		console.warn(
			`agent exited (code ${event.exitCode}), restart=${event.restart}`,
		);
	},
});

await agentOs.dispose();
