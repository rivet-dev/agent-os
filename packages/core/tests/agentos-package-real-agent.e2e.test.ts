import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { afterAll, beforeAll, describe, expect, test } from "vitest";
import { AgentOs } from "../src/index.js";

/**
 * REAL production-agent end-to-end proof: pack the published
 * `@agentos-software/pi` ACP adapter with the toolchain (flat + `--prune-native`),
 * project it as an `/opt/agentos` package, and launch a session via
 * `/opt/agentos/bin/pi-acp` — exercising the whole package model on a
 * real ESM agent with dynamic imports.
 *
 * Gated behind `AGENTOS_TEST_REAL_PI=1` because it runs a real `npm install`
 * (network + ~30s) — it is not part of the default suite. Run with:
 *   AGENTOS_TEST_REAL_PI=1 pnpm --dir packages/core exec vitest run \
 *     tests/agentos-package-real-agent.e2e.test.ts
 */
const ENABLED = process.env.AGENTOS_TEST_REAL_PI === "1";
const here = dirname(fileURLToPath(import.meta.url));
// Toolchain now lives in the sibling agentos repo (see its package README).
const TOOLCHAIN_CLI = resolve(
	here,
	"../../../../agentos/packages/agentos-toolchain/dist/cli.js",
);

/** Real published agent adapters, packed flat with native addons pruned. */
const AGENTS = [
	// `agentType` is the package's projected agent name. `openSession` asks the
	// sidecar to resolve it under /opt/agentos/pkgs/<name>/<version>.
	// `pkg` is only the npm package name used to pack the adapter.
	{ pkg: "@agentos-software/pi", agentType: "pi", acpEntrypoint: "pi-acp" },
	// claude is intentionally NOT here: this block packs from the PUBLISHED npm
	// package, and `@agentos-software/claude-code` does not yet ship
	// `agentos-package.json`, so `pack` falls back to the unscoped npm name
	// "claude-code" instead of the friendly id "claude". Re-enable once that
	// package is republished with `agentos-package.json` in its `files`.
	// See the deferred-skip test below and
	// ~/.agents/todo/agentos-agent-resolution-followups.md.
];

describe.skipIf(!ENABLED).each(AGENTS)(
	"real agent package end-to-end ($pkg)",
	({ pkg, agentType, acpEntrypoint }) => {
		let vm: AgentOs;
		let outRoot: string;
		let packageDir: string;

		beforeAll(async () => {
			outRoot = mkdtempSync(join(tmpdir(), "agentos-real-agent-"));
			// --prune-native drops unreachable native addons from the flat closure
			// while keeping dynamically-imported modules that --bundle would miss.
			const stdout = execFileSync(
				"node",
				[
					TOOLCHAIN_CLI,
					"pack",
					pkg,
					"--agent",
					acpEntrypoint,
					"--prune-native",
					"--out",
					outRoot,
				],
				{ encoding: "utf8" },
			);
			const match = stdout.match(/→\s+(\S+)/);
			if (!match) throw new Error(`could not parse pack output: ${stdout}`);
			packageDir = match[1];

			vm = await AgentOs.create({
				defaultSoftware: false,
				software: [packageDir],
			});
		}, 120_000);

		afterAll(async () => {
			await vm?.dispose();
			if (outRoot) rmSync(outRoot, { recursive: true, force: true });
		});

		test("lists the real agent as installed", async () => {
			const entry = (await vm.listAgents()).find((a) => a.id === agentType);
			expect(entry?.installed).toBe(true);
			expect(entry?.adapterEntrypoint).toBe(
				`/opt/agentos/bin/${acpEntrypoint}`,
			);
		});

		test("openSession launches the real adapter for a caller-owned session", async () => {
			const sessionId = `real-${agentType}`;
			await expect(
				vm.openSession({ sessionId, agent: agentType }),
			).resolves.toBeUndefined();
			await vm.unloadSession({ sessionId });
		}, 60_000);
	},
);

// Deferred: block-1 claude packs from the PUBLISHED `@agentos-software/claude-code`,
// which does not yet ship `agentos-package.json`, so its manifest name resolves to
// "claude-code" instead of "claude". Re-enable in the AGENTS list above once that
// package is republished with `agentos-package.json` in `files`.
describe.skipIf(!ENABLED)(
	"real agent package end-to-end ('@agentos-software/claude-code')",
	() => {
		test.skip("deferred until @agentos-software/claude-code republishes with agentos-package.json (name → 'claude')", () => {});
	},
);

/**
 * Default software is coreutils-only ([common]) — it ships NO agents by design
 * (see default-software.ts). This guards that decision: a bare `AgentOs.create()`
 * projects no agent, and asking for one by id fails with the clear "unknown agent
 * type" error rather than silently resolving. Agents must be passed via `software:`.
 */
describe.skipIf(!ENABLED)("default software ships no agents", () => {
	let vm: AgentOs;

	beforeAll(async () => {
		vm = await AgentOs.create({}); // default software — coreutils only, no agents
	}, 120_000);

	afterAll(async () => {
		await vm?.dispose();
	});

	test("lists no built-in agents", async () => {
		const ids = (await vm.listAgents()).map((a) => a.id);
		expect(ids).not.toContain("pi");
		expect(ids).not.toContain("claude");
	});

	test("openSession({ agent: 'pi' }) fails with unknown agent type", async () => {
		await expect(vm.openSession({ agent: "pi" })).rejects.toThrow(
			/unknown agent type/i,
		);
	});
});
