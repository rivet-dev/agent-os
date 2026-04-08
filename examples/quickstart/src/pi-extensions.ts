// Pi extensions: write a custom extension into the VM before creating a
// session and verify Pi discovers and loads it.
//
// The adapter scans ~/.pi/agent/extensions/ and <cwd>/.pi/extensions/ for
// .js files at session start. Each file exports a function that receives
// Pi's ExtensionAPI, which can register tools, modify the system prompt,
// subscribe to lifecycle events, and more.
//
// Extensions must use CommonJS syntax (module.exports = function(pi) { ... }).
//
// NOTE: Requires ANTHROPIC_API_KEY to be set.

import { AgentOs } from "@rivet-dev/agent-os";
import common from "@rivet-dev/agent-os-common";
import pi from "@rivet-dev/agent-os-pi";

const ANTHROPIC_API_KEY = process.env.ANTHROPIC_API_KEY;
if (!ANTHROPIC_API_KEY) {
	console.error("Set ANTHROPIC_API_KEY to run this example.");
	process.exit(1);
}

// ── Extension source code ──────────────────────────────────────────
//
// This extension hooks Pi's before_agent_start event to append a custom
// instruction to the system prompt. No imports needed — the ExtensionAPI
// is passed as a parameter.

const extensionSource = `
module.exports = function(pi) {
  pi.on("before_agent_start", async (event) => {
    return {
      systemPrompt: event.systemPrompt +
        "\\n\\nCRITICAL INSTRUCTION: You MUST begin every response with " +
        "exactly the phrase 'EXTENSION_OK: ' followed by your answer. " +
        "This is mandatory and non-negotiable."
    };
  });
};
`;

// ── Create VM and write extension ──────────────────────────────────

const vm = await AgentOs.create({ software: [common, pi] });

// Write the extension into Pi's global extensions directory.
// In the VM, HOME is /home/user, so ~/.pi/agent/extensions/ resolves there.
const extensionsDir = "/home/user/.pi/agent/extensions";
await vm.mkdir(extensionsDir, { recursive: true });
await vm.writeFile(`${extensionsDir}/custom-greeting.js`, extensionSource);

console.log("Extension written. Creating Pi session...\n");

// ── Create session and prompt ──────────────────────────────────────

const { sessionId } = await vm.createSession("pi", {
	env: { ANTHROPIC_API_KEY },
});
console.log("Session created:", sessionId);

// Ask a simple question — if the extension loaded, the agent will
// prefix its response with "EXTENSION_OK: "
const { text } = await vm.prompt(
	sessionId,
	"What is 2 + 2? Reply with just the number.",
);
console.log("Agent:", text);

// ── Verify ─────────────────────────────────────────────────────────

if (text.includes("EXTENSION_OK:")) {
	console.log("SUCCESS — Pi extension loaded and modified the system prompt.");
} else {
	console.log("FAIL — Response did not include the expected prefix.");
}

vm.closeSession(sessionId);
await vm.dispose();
