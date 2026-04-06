import { AgentOs } from "@rivet-dev/agent-os";
import pi from "@rivet-dev/agent-os-pi";

const os = await AgentOs.create({ software: [pi] });
const { sessionId } = await os.createSession("pi");

os.onSessionEvent(sessionId, (event) => {
	console.log(event);
});

await os.prompt(sessionId, "Write a JavaScript function that calculates pi");
