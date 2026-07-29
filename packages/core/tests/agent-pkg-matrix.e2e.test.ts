import { execFileSync, spawnSync } from "node:child_process";
import { cpSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { afterAll, beforeAll, describe, expect, it } from "vitest";

/**
 * AGENT × PACKAGE-MANAGER e2e MATRIX (real API, skipped by default).
 *
 * For every package manager (npm/pnpm/yarn/bun) × every agent
 * (pi/claude/opencode/codex) this installs the PUBLISHED packages into an
 * isolated temp project and asserts a real user can: install → create a session
 * → prompt → stream tokens LIVE. It is the regression net for the exact issues
 * that bit us shipping the preview:
 *
 *  - retired/stale model ids (Anthropic 404 → empty turn),
 *  - opencode needing a config file (model + provider baseURL ending in /v1) + cwd,
 *  - permission keys being fs/network/childProcess/process/env (not filesystem/…),
 *  - gap-based streaming detection (opencode bursts chunks but still delivers live),
 *  - ACP bootstrap flakiness (retry before failing).
 *
 * SKIPPED BY DEFAULT: it does real network installs and hits a real LLM API. Run
 * it deliberately:
 *
 *   AGENTOS_MATRIX_E2E=1 ANTHROPIC_API_KEY=sk-... \
 *     pnpm --dir packages/core exec vitest run tests/agent-pkg-matrix.e2e.test.ts
 *
 * Env knobs:
 *   AGENTOS_MATRIX_E2E=1        required to enable (also gated out in vitest.config.ts)
 *   ANTHROPIC_API_KEY          required for pi/claude/opencode
 *   OPENAI_API_KEY             required for codex
 *   AGENTOS_MATRIX_CORE        @rivet-dev/agentos-core version/tag (default "latest")
 *   AGENTOS_MATRIX_AGENTS      @agentos-software/* version/tag (default "latest")
 *   AGENTOS_MATRIX_MODEL       opencode model id (default a current Haiku)
 *   AGENTOS_MATRIX_PMS         comma list to restrict package managers
 *   AGENTOS_MATRIX_AGENTS_LIST comma list to restrict agents
 */

const ENABLED = process.env.AGENTOS_MATRIX_E2E === "1";
const CORE_VERSION = process.env.AGENTOS_MATRIX_CORE || "latest";
const AGENTS_VERSION = process.env.AGENTOS_MATRIX_AGENTS || "latest";
const CELL = resolve(import.meta.dirname, "fixtures/agent-matrix-cell.mjs");
const CELL_TIMEOUT_MS = 240_000;

const AGENT_PKGS: Record<string, string> = {
	pi: "@agentos-software/pi",
	claude: "@agentos-software/claude-code",
	opencode: "@agentos-software/opencode",
	codex: "@agentos-software/codex",
};

function commandAvailable(cmd: string): boolean {
	try {
		const r = spawnSync(cmd, ["--version"], { stdio: "ignore" });
		return r.status === 0;
	} catch {
		return false;
	}
}

function installArgs(pm: string, pkgs: string[]): Array<[string, string[]]> {
	switch (pm) {
		case "npm":
			return [
				["npm", ["init", "-y"]],
				["npm", ["install", "--no-audit", "--no-fund", ...pkgs]],
			];
		case "pnpm":
			return [
				["pnpm", ["init"]],
				["pnpm", ["add", ...pkgs]],
			];
		case "yarn":
			return [
				["yarn", ["init", "-y"]],
				["yarn", ["add", ...pkgs]],
			];
		case "bun":
			return [
				["bun", ["init", "-y"]],
				["bun", ["add", ...pkgs]],
			];
		default:
			throw new Error(`unknown package manager ${pm}`);
	}
}

const ALL_PMS = (process.env.AGENTOS_MATRIX_PMS || "npm,pnpm,yarn,bun")
	.split(",")
	.map((s) => s.trim())
	.filter(Boolean);
const ALL_AGENTS = (
	process.env.AGENTOS_MATRIX_AGENTS_LIST || "pi,claude,opencode,codex"
)
	.split(",")
	.map((s) => s.trim())
	.filter(Boolean);

const availablePms = ALL_PMS.filter(commandAvailable);

describe.skipIf(!ENABLED)(
	"agent × package-manager e2e matrix (real API)",
	() => {
		const tmpDirs: string[] = [];

		beforeAll(() => {
			const anthropicAgents = ALL_AGENTS.filter((agent) => agent !== "codex");
			if (anthropicAgents.length > 0 && !process.env.ANTHROPIC_API_KEY) {
				throw new Error(
					`AGENTOS_MATRIX_E2E requires ANTHROPIC_API_KEY for ${anthropicAgents.join(", ")}`,
				);
			}
			if (ALL_AGENTS.includes("codex") && !process.env.OPENAI_API_KEY) {
				throw new Error("AGENTOS_MATRIX_E2E requires OPENAI_API_KEY for codex");
			}
			const skipped = ALL_PMS.filter((p) => !availablePms.includes(p));
			if (skipped.length) {
				// eslint-disable-next-line no-console
				console.warn(
					`[matrix] package managers not on PATH, skipping: ${skipped.join(", ")}`,
				);
			}
			// eslint-disable-next-line no-console
			console.log(
				`[matrix] core=${CORE_VERSION} agents=${AGENTS_VERSION} pms=[${availablePms.join(",")}] agents=[${ALL_AGENTS.join(",")}]`,
			);
		});

		afterAll(() => {
			for (const d of tmpDirs) {
				try {
					rmSync(d, { recursive: true, force: true });
				} catch (error) {
					console.warn(`[matrix] failed to remove ${d}: ${String(error)}`);
				}
			}
		});

		for (const pm of availablePms) {
			for (const agent of ALL_AGENTS) {
				// OpenCode's ACP bootstrap and real LLM APIs can fail transiently;
				// retry the complete install/run cell, while persistent failures stay red.
				it(`${pm} + ${agent}: install → session → live token streaming`, {
					timeout: CELL_TIMEOUT_MS + 200_000,
					retry: 2,
				}, async () => {
					const dir = mkdtempSync(
						join(tmpdir(), `agentos-matrix-${pm}-${agent}-`),
					);
					tmpDirs.push(dir);
					// yarn 1.x global cache contends under repeated runs; isolate it.
					const cacheDir = join(dir, ".pm-cache");
					const childEnv = {
						...process.env,
						AGENT: agent,
						YARN_CACHE_FOLDER: cacheDir,
						npm_config_cache: cacheDir,
					};

					const pkgs = [
						`@rivet-dev/agentos-core@${CORE_VERSION}`,
						`${AGENT_PKGS[agent]}@${AGENTS_VERSION}`,
					];
					for (const [cmd, args] of installArgs(pm, pkgs)) {
						execFileSync(cmd, args, {
							cwd: dir,
							env: childEnv,
							stdio: "pipe",
							timeout: 180_000,
						});
					}

					cpSync(CELL, join(dir, "agent-matrix-cell.mjs"));

					const run = spawnSync("node", ["agent-matrix-cell.mjs"], {
						cwd: dir,
						env: childEnv,
						encoding: "utf8",
						timeout: CELL_TIMEOUT_MS,
					});

					const line = (run.stdout || "")
						.split("\n")
						.find((l) => l.startsWith("E2E_RESULT_JSON:"));
					if (!line) {
						throw new Error(
							`no E2E_RESULT_JSON from ${pm}/${agent}.\nstdout:\n${run.stdout}\nstderr:\n${(run.stderr || "").slice(-2000)}`,
						);
					}
					const result = JSON.parse(line.slice("E2E_RESULT_JSON:".length));

					// eslint-disable-next-line no-console
					console.log(
						`[matrix] ${pm}/${agent}:`,
						JSON.stringify(result.metrics),
					);

					expect(
						result.ok,
						`prompt produced output (err: ${result.error})`,
					).toBe(true);
					expect(
						result.streaming,
						`tokens streamed live (metrics: ${JSON.stringify(result.metrics)})`,
					).toBe(true);
				});
			}
		}
	},
);
