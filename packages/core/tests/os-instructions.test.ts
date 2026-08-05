import * as fs from "node:fs";
import { resolve } from "node:path";
import { afterEach, beforeEach, describe, expect, test } from "vitest";
import { AgentOs } from "../src/agent-os.js";
import {
	createProjectedAgentPackage,
	type ProjectedAgentPackage,
} from "./helpers/projected-agent-package.js";

const OS_INSTRUCTIONS_FIXTURE = resolve(
	import.meta.dirname,
	// The sidecar crate embeds this prompt; it lives next to the Rust source so
	// `cargo publish` can package it. This test only sanity-checks its contents.
	"../../../crates/sidecar/src/AGENTOS_SYSTEM_PROMPT.md",
);

// ── base prompt fixture sanity ─────────────────────────────────────────
//
// The base prompt is no longer baked into a guest file. The sidecar embeds this fixture and
// injects it at session start. This block only verifies the fixture itself is non-empty so the
// injection has real content to assemble.

describe("base system prompt fixture", () => {
	test("ships a non-empty base prompt", () => {
		const base = fs.readFileSync(OS_INSTRUCTIONS_FIXTURE, "utf-8");
		expect(base).toBeTruthy();
		expect(base.length).toBeGreaterThan(0);
		expect(base).toContain("# agentOS");
	});
});

// ── openSession integration tests ────────────────────────────────────

/**
 * Mock ACP adapter that responds to initialize/session/new.
 * Echoes process.env in agentInfo for env var verification.
 */
const MOCK_ACP_ADAPTER = `
let buffer = '';
process.stdin.resume();
process.stdin.on('data', (chunk) => {
  const str = chunk instanceof Uint8Array ? new TextDecoder().decode(chunk) : String(chunk);
  buffer += str;

  while (true) {
    const idx = buffer.indexOf('\\n');
    if (idx === -1) break;
    const line = buffer.substring(0, idx);
    buffer = buffer.substring(idx + 1);
    if (!line.trim()) continue;

    try {
      const msg = JSON.parse(line);
      if (msg.id === undefined) continue;

      let result;
      switch (msg.method) {
        case 'initialize':
          result = {
            protocolVersion: 1,
            agentInfo: {
              name: 'mock-adapter',
              version: '1.0',
              contextPaths: process.env.OPENCODE_CONTEXTPATHS || null,
              systemPrompt: process.env.ACP_APPEND_SYSTEM_PROMPT || null,
              argv: process.argv.slice(2),
            },
          };
          break;
        case 'session/new':
          result = { sessionId: 'mock-session-1' };
          break;
        case 'session/cancel':
          result = {};
          break;
        default:
          process.stdout.write(JSON.stringify({
            jsonrpc: '2.0', id: msg.id,
            error: { code: -32601, message: 'Method not found' },
          }) + '\\n');
          continue;
      }

      process.stdout.write(JSON.stringify({
        jsonrpc: '2.0', id: msg.id, result,
      }) + '\\n');
    } catch (e) {}
  }
});
`;

describe("openSession OS instructions integration", () => {
	let vm: AgentOs;
	let agentPackages: ProjectedAgentPackage[];

	beforeEach(async () => {
		agentPackages = ["pi", "opencode"].map((name) =>
			createProjectedAgentPackage({ name, adapterScript: MOCK_ACP_ADAPTER }),
		);
		vm = await AgentOs.create({
			defaultSoftware: false,
			software: agentPackages.map((agentPackage) => agentPackage.software),
		});
	});

	afterEach(async () => {
		await vm.dispose();
		for (const agentPackage of agentPackages) {
			agentPackage.cleanup();
		}
	});

	test("openSession passes the assembled prompt through the adapter-neutral contract", async () => {
		const sessionId = "assembled-prompt";
		await vm.openSession({ sessionId, agent: "pi" });
		const agentInfo = (await vm.getSessionAgentInfo({ sessionId })) as {
			systemPrompt?: string;
		};
		const instructionsArg = agentInfo.systemPrompt ?? "";
		expect(instructionsArg).toBeTruthy();
		expect(instructionsArg.length).toBeGreaterThan(0);
		// The sidecar injects the embedded base prompt, not a guest-read file.
		expect(instructionsArg).toContain("# agentOS");

		vm.unloadSession({ sessionId });
	});

	test("openSession leaves OpenCode compatibility translation to its launcher", async () => {
		const sessionId = "opencode-compat";
		await vm.openSession({ sessionId, agent: "opencode" });

		const agentInfo = (await vm.getSessionAgentInfo({ sessionId })) as {
			contextPaths?: string;
			systemPrompt?: string;
			argv?: string[];
		};
		expect(agentInfo.argv ?? []).not.toContain("acp");
		expect(agentInfo.systemPrompt).toContain("# agentOS");
		expect(agentInfo.contextPaths).toBeNull();

		// No .agent-os/ directory created in cwd
		const agentOsDirExists = await vm.exists("/home/agentos/.agent-os");
		expect(agentOsDirExists).toBe(false);

		vm.unloadSession({ sessionId });
	});

	test("openSession with skipOsInstructions:true does not inject args or env", async () => {
		const sessionId = "skip-os-instructions";
		await vm.openSession({
			sessionId,
			agent: "pi",
			skipOsInstructions: true,
		});
		const agentInfo = (await vm.getSessionAgentInfo({ sessionId })) as {
			systemPrompt?: string;
		};
		expect(agentInfo.systemPrompt).toBeNull();

		vm.unloadSession({ sessionId });
	});

	test("openSession with skipOsInstructions:true still forwards additionalInstructions", async () => {
		const additionalText = "CUSTOM_MARKER: skip base, keep extras.";

		const sessionId = "skip-base-keep-extra";
		await vm.openSession({
			sessionId,
			agent: "pi",
			skipOsInstructions: true,
			additionalInstructions: additionalText,
		});
		const agentInfo = (await vm.getSessionAgentInfo({ sessionId })) as {
			systemPrompt?: string;
		};
		const instructionsArg = agentInfo.systemPrompt ?? "";
		expect(instructionsArg).toContain(additionalText);
		expect(instructionsArg).not.toContain("# agentOS");

		vm.unloadSession({ sessionId });
	});

	test("user-provided env vars override instruction env vars", async () => {
		const userContextPaths = '["my-custom-paths.md"]';
		const sessionId = "user-env-override";
		await vm.openSession({
			sessionId,
			agent: "opencode",
			env: { OPENCODE_CONTEXTPATHS: userContextPaths },
		});

		const agentInfo = (await vm.getSessionAgentInfo({ sessionId })) as {
			contextPaths?: string;
		};
		expect(agentInfo.contextPaths).toBe(userContextPaths);

		vm.unloadSession({ sessionId });
	});

	test("additionalInstructions content appears in injected text", async () => {
		const additionalText = "CUSTOM_MARKER: Always use pnpm for this project.";

		const sessionId = "additional-instructions";
		await vm.openSession({
			sessionId,
			agent: "pi",
			additionalInstructions: additionalText,
		});
		const agentInfo = (await vm.getSessionAgentInfo({ sessionId })) as {
			systemPrompt?: string;
		};
		const instructionsArg = agentInfo.systemPrompt ?? "";
		expect(instructionsArg).toContain(additionalText);

		vm.unloadSession({ sessionId });
	});
});
