import { AgentOs } from "@rivet-dev/agent-os";

const os = await AgentOs.create();
const { sessionId } = await os.createSession("pi");

os.onSessionEvent(sessionId, (event) => {
	console.log(event);
});

await os.prompt(sessionId, "Write a JavaScript function that calculates pi");
