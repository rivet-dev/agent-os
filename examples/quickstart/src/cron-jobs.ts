import { AgentOs } from "@rivet-dev/agent-os";

const os = await AgentOs.create();
const { sessionId } = await os.createSession("pi");

// Schedule a recurring task
setInterval(async () => {
  await os.prompt(sessionId, "Check for dependency updates and open PRs");
}, 6 * 60 * 60 * 1000); // Every 6 hours
