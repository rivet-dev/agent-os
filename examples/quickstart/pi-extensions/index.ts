// Pi extensions: write a custom extension into the VM before creating a
// session and verify Pi discovers and loads it.
//
// The adapter scans ~/.pi/agent/extensions/ and <cwd>/.pi/extensions/ for
// .js files at session start. Each file exports a function that receives
// Pi's ExtensionAPI, which can register tools, modify the system prompt,
// subscribe to lifecycle events, and more.
//
// Extensions should export a default factory function.
//
// NOTE: Requires ANTHROPIC_API_KEY to be set. To run this against llmock,
// also set ANTHROPIC_BASE_URL and the example will write ~/.pi/agent/models.json
// inside the VM before creating the session.

import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { AgentOs, nodeModulesMount } from "@rivet-dev/agentos";
import pi from "@agentos-software/pi";

const ANTHROPIC_API_KEY = process.env.ANTHROPIC_API_KEY;
const ANTHROPIC_BASE_URL = process.env.ANTHROPIC_BASE_URL;
if (!ANTHROPIC_API_KEY) {
	console.error("Set ANTHROPIC_API_KEY to run this example.");
	process.exit(1);
}

const require = createRequire(import.meta.url);
const MODULE_ACCESS_CWD = resolve(
	dirname(require.resolve("@rivet-dev/agentos")),
	"..",
);
const HOME_DIR = "/home/agentos";
const WORKSPACE_DIR = `${HOME_DIR}/workspace`;

// ── Extension source code ──────────────────────────────────────────
//
// This extension hooks Pi's before_agent_start event to append a custom
// instruction to the system prompt. No imports needed — the ExtensionAPI
// is passed as a parameter.

const extensionSource = `
export default function(pi) {
  pi.on("before_agent_start", async (event) => {
    return {
      systemPrompt: event.systemPrompt +
        "\\n\\nCRITICAL INSTRUCTION: You MUST begin every response with " +
        "exactly the phrase 'EXTENSION_OK: ' followed by your answer. " +
        "This is mandatory and non-negotiable."
    };
  });
}
`;

// ── Create VM and write extension ──────────────────────────────────

const vm = await AgentOs.create({
	...(ANTHROPIC_BASE_URL
		? {
				loopbackExemptPorts: [Number(new URL(ANTHROPIC_BASE_URL).port)],
			}
		: {}),
	mounts: [nodeModulesMount(join(MODULE_ACCESS_CWD, "node_modules"))],
	software: [pi],
});

// Write the extension into Pi's global extensions directory.
// In the VM, HOME is /home/agentos, so ~/.pi/agent/extensions/ resolves there.
const extensionsDir = "/home/agentos/.pi/agent/extensions";
await vm.filesystem.mkdir(extensionsDir, { recursive: true });
await vm.filesystem.mkdir(WORKSPACE_DIR, { recursive: true });
await vm.filesystem.writeFile(`${extensionsDir}/custom-greeting.js`, extensionSource);

if (ANTHROPIC_BASE_URL) {
	await vm.filesystem.writeFile(
		`${HOME_DIR}/.pi/agent/models.json`,
		JSON.stringify(
			{
				providers: {
					anthropic: {
						baseUrl: ANTHROPIC_BASE_URL,
						apiKey: ANTHROPIC_API_KEY,
					},
				},
			},
			null,
			2,
		),
	);
}

console.log("Extension written. Creating Pi session...\n");

// ── Create session and prompt ──────────────────────────────────────

await vm.sessions.open({
	agent: "pi",
	cwd: WORKSPACE_DIR,
	env: {
		HOME: HOME_DIR,
		ANTHROPIC_API_KEY,
		...(ANTHROPIC_BASE_URL ? { ANTHROPIC_BASE_URL } : {}),
	},
});

// Ask a simple question — if the extension loaded, the agent will
// prefix its response with "EXTENSION_OK: "
const result = await vm.sessions.prompt({
	content: [
		{ type: "text", text: "What is 2 + 2? Reply with just the number." },
	],
});
const responseText =
	result.message?.content
		.filter((block) => block.type === "text")
		.map((block) => block.text)
		.join("") ?? "";
console.log("Agent:", responseText);

// ── Verify ─────────────────────────────────────────────────────────

if (responseText.includes("EXTENSION_OK:")) {
	console.log("SUCCESS — Pi extension loaded and modified the system prompt.");
} else {
	throw new Error("FAIL — Response did not include the expected prefix.");
}

await vm.sessions.delete();
await vm.dispose();
