// Create an agent session and send a prompt using a coding agent.
//
// NOTE: This example requires an API key for the chosen agent and a working
// agent runtime. It may not complete in all environments.

import claude from "@agentos-software/claude-code";
import type { SoftwareInput } from "@rivet-dev/agentos";
import { AgentOs } from "@rivet-dev/agentos";
import opencode from "@agentos-software/opencode";
import pi from "@agentos-software/pi";

const ANTHROPIC_API_KEY = process.env.ANTHROPIC_API_KEY;

const software: SoftwareInput[] = [claude, opencode, pi];

const vm = await AgentOs.create({
	software,
});

// Change the agent here: "claude", "opencode", or "pi"
const agent = "pi";

const env: Record<string, string> = {};
if (ANTHROPIC_API_KEY) env.ANTHROPIC_API_KEY = ANTHROPIC_API_KEY;

await vm.sessions.open({ agent, env });

// Listen for session events (streamed text, tool use, etc.)
vm.onSessionEvent((event) => {
	console.log("Event:", JSON.stringify(event, null, 2));
});

// Send a prompt and wait for the response
const result = await vm.sessions.prompt({
	content: [
		{ type: "text", text: "What is 2 + 2? Reply with just the number." },
	],
});
console.log("Response:", result.message?.content ?? []);

// Close the session
await vm.sessions.delete();
await vm.dispose();
