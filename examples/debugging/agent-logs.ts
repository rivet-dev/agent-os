// Capture the coding agent's stderr at the VM level to diagnose tool calls,
// model errors, and crashes mid-turn.

import { AgentOs } from "@rivet-dev/agentos";
import pi from "@agentos-software/pi";

const agentOs = await AgentOs.create({
  software: [pi],
  onAgentStderr(event) {
    // event: { sessionId, agentType, processId, pid, chunk: Uint8Array }
    process.stderr.write(`[agent:${event.agentType}] `);
    process.stderr.write(event.chunk);
  },
});

await agentOs.dispose();
