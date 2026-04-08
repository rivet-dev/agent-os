// Create an agent session and send a prompt using a coding agent.
//
// NOTE: This example requires an API key for the chosen agent and a working
// agent runtime. It may not complete in all environments.

import type { SoftwareInput } from "@rivet-dev/agent-os";
import { AgentOs } from "@rivet-dev/agent-os";
import claude from "@rivet-dev/agent-os-claude";
import codex from "@rivet-dev/agent-os-codex-agent";
import common from "@rivet-dev/agent-os-common";
import opencode from "@rivet-dev/agent-os-opencode";
import pi from "@rivet-dev/agent-os-pi";

const ANTHROPIC_API_KEY = process.env.ANTHROPIC_API_KEY;
const OPENAI_API_KEY = process.env.OPENAI_API_KEY;

const software: SoftwareInput[] = [common, claude, [...codex], opencode, pi];

const vm = await AgentOs.create({
	software,
});

// Change the agent here: "claude", "codex", "opencode", or "pi"
const agent = "claude";

const env: Record<string, string> = {};
if (ANTHROPIC_API_KEY) env.ANTHROPIC_API_KEY = ANTHROPIC_API_KEY;
if (OPENAI_API_KEY) env.OPENAI_API_KEY = OPENAI_API_KEY;

const { sessionId } = await vm.createSession(agent, { env });
console.log("Session ID:", sessionId);

// Listen for session events (streamed text, tool use, etc.)
vm.onSessionEvent(sessionId, (event) => {
	console.log("Event:", JSON.stringify(event, null, 2));
});

// Send a prompt and wait for the response
const { text } = await vm.prompt(
	sessionId,
	"What is 2 + 2? Reply with just the number.",
);
console.log("Response:", text);

// Close the session
vm.closeSession(sessionId);
await vm.dispose();
