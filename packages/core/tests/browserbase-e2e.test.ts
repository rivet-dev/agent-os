import { resolve } from "node:path";
import { afterEach, describe, expect, test } from "vitest";
import { AgentOs, type Permissions } from "../src/index.js";
import { moduleAccessMounts } from "./helpers/node-modules-mount.js";

const MODULE_ACCESS_CWD = resolve(import.meta.dirname, "..");
const BROWSER_BASE_API_KEY = process.env.BROWSER_BASE_API_KEY ?? "";
const BROWSER_BASE_PROJECT_ID = process.env.BROWSER_BASE_PROJECT_ID ?? "";
const HAS_BROWSERBASE_CREDENTIALS = Boolean(
	BROWSER_BASE_API_KEY && BROWSER_BASE_PROJECT_ID,
);
const BROWSERBASE_E2E_ENABLED = process.env.AGENTOS_BROWSERBASE_E2E === "1";

if (BROWSERBASE_E2E_ENABLED && !HAS_BROWSERBASE_CREDENTIALS) {
	throw new Error(
		"Browserbase e2e requires BROWSER_BASE_API_KEY and BROWSER_BASE_PROJECT_ID when AGENTOS_BROWSERBASE_E2E=1.",
	);
}

if (!BROWSERBASE_E2E_ENABLED) {
	console.warn(
		"Skipping Browserbase e2e: set AGENTOS_BROWSERBASE_E2E=1 and provide Browserbase credentials to opt in.",
	);
}

const BROWSERBASE_PERMISSIONS: Permissions = {
	fs: "allow",
	childProcess: "allow",
	env: "allow",
	network: {
		default: "deny",
		rules: [
			{
				mode: "allow",
				patterns: ["dns://*.browserbase.com", "tcp://*.browserbase.com:*"],
			},
		],
	},
};

const BROWSE_PATH =
	"/root/node_modules/@browserbasehq/browse-cli/dist/index.js";
const CLI_PATH = "/root/node_modules/@browserbasehq/cli/dist/main.js";
const JSON_OUTPUT_TIMEOUT_MS = 60_000;
const BROWSE_COMMAND_SCRIPT_PATH = "/tmp/browserbase-browse-command.mjs";
const SCREENSHOT_PATH = "/tmp/browserbase-e2e.png";
const EXAMPLE_URL_PATTERN = /^https:\/\/example\.com\/?$/;

async function runVmNodeCommand(
	vm: AgentOs,
	scriptPath: string,
	args: string[],
	label: string,
	env: Record<string, string>,
) {
	let stdout = "";
	let stderr = "";
	const { pid } = vm.spawn("node", [scriptPath, ...args], {
		env,
		onStdout: (data: Uint8Array) => {
			stdout += new TextDecoder().decode(data);
		},
		onStderr: (data: Uint8Array) => {
			stderr += new TextDecoder().decode(data);
		},
	});

	let timeoutHandle: ReturnType<typeof setTimeout> | undefined;
	try {
		const exitCode = await Promise.race([
			vm.waitProcess(pid),
			new Promise<never>((_, reject) => {
				timeoutHandle = setTimeout(() => {
					try {
						vm.killProcess(pid);
					} catch {}
					reject(
						new Error(`${label} timed out after ${JSON_OUTPUT_TIMEOUT_MS}ms`),
					);
				}, JSON_OUTPUT_TIMEOUT_MS);
			}),
		]);
		if (timeoutHandle) {
			clearTimeout(timeoutHandle);
		}
		expect(exitCode, `stdout:\n${stdout}\nstderr:\n${stderr}`).toBe(0);
		return {
			stderr: stderr.trim(),
			stdout: stdout.trim(),
		};
	} catch (error) {
		if (timeoutHandle) {
			clearTimeout(timeoutHandle);
		}
		throw new Error(
			[
				error instanceof Error ? error.message : String(error),
				`stdout:\n${stdout}`,
				`stderr:\n${stderr}`,
				`processes:\n${JSON.stringify(vm.allProcesses(), null, 2)}`,
			].join("\n\n"),
		);
	}
}

async function runVmNodeJsonCommand<T>(
	vm: AgentOs,
	scriptPath: string,
	args: string[],
	label: string,
	env: Record<string, string>,
): Promise<T> {
	const result = await runVmNodeCommand(vm, scriptPath, args, label, env);
	try {
		return JSON.parse(result.stdout) as T;
	} catch (error) {
		throw new Error(
			[
				`${label} did not emit valid JSON`,
				error instanceof Error ? error.message : String(error),
				`stdout:\n${result.stdout}`,
				`stderr:\n${result.stderr}`,
			].join("\n\n"),
		);
	}
}

const GUEST_SCRIPT = String.raw`
import { existsSync } from "node:fs";

const CLI_PATH = "/root/node_modules/@browserbasehq/cli/dist/main.js";
const BROWSE_PATH = "/root/node_modules/@browserbasehq/browse-cli/dist/index.js";

function ensure(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

async function assertDirectGuestEgressIsDenied() {
  try {
    await fetch("https://example.com", { signal: AbortSignal.timeout(10_000) });
    throw new Error("direct guest fetch to example.com unexpectedly succeeded");
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    ensure(
      /(EACCES|ERR_ACCESS_DENIED|blocked outbound network access|fetch failed)/.test(
        message,
      ),
      "unexpected direct egress failure: " + message,
    );
    return message;
  }
}

ensure(existsSync(CLI_PATH), "Browserbase CLI is not projected into the VM");
ensure(existsSync(BROWSE_PATH), "Browserbase browse CLI is not projected into the VM");

const blockedFetchMessage = await assertDirectGuestEgressIsDenied();

console.log(
  "BROWSERBASE_GUEST_CHECKS:" +
    JSON.stringify({
      blockedFetchMessage,
      envAliased: Boolean(
        process.env.BROWSERBASE_API_KEY && process.env.BROWSERBASE_PROJECT_ID,
      ),
      cliProjected: existsSync(CLI_PATH),
      browseProjected: existsSync(BROWSE_PATH),
    }),
);
`;

const BROWSE_COMMAND_SCRIPT = String.raw`
import { spawnSync } from "node:child_process";

const BROWSE_PATH = "/root/node_modules/@browserbasehq/browse-cli/dist/index.js";
const commandArgs = process.argv.slice(2);

const result = spawnSync(process.execPath, [
  BROWSE_PATH,
  "--json",
  ...commandArgs,
], {
  encoding: "utf8",
  env: process.env,
  timeout: 60_000,
});

if (result.error) {
  throw result.error;
}
if (result.stdout) {
  process.stdout.write(result.stdout);
}
if (result.stderr) {
  process.stderr.write(result.stderr);
}
process.exit(result.status ?? 1);
`;

describe("Browserbase e2e", () => {
	let vm: AgentOs | null = null;

	afterEach(async () => {
		if (vm) {
			await vm.dispose();
			vm = null;
		}
	});

	const browserbaseTest =
		BROWSERBASE_E2E_ENABLED && HAS_BROWSERBASE_CREDENTIALS ? test : test.skip;

	browserbaseTest(
		"runs Browserbase browser automation inside the VM with restricted guest egress",
		async () => {
			const browseSession = `browserbase-e2e-${Date.now()}`;
			const browseEnv = {
				BROWSERBASE_API_KEY: BROWSER_BASE_API_KEY,
				BROWSERBASE_PROJECT_ID: BROWSER_BASE_PROJECT_ID,
				BROWSE_SESSION: browseSession,
				BROWSERBASE_CONFIG_DIR: "/tmp/browserbase-e2e-debug",
				BROWSERBASE_FLOW_LOGS: "1",
				BROWSERBASE_CDP_CONNECT_MAX_MS: "5000",
				BROWSERBASE_SESSION_CREATE_MAX_MS: "10000",
				STAGEHAND_FIRST_TOP_LEVEL_PAGE_TIMEOUT_MS: "2000",
			};

			vm = await AgentOs.create({
				mounts: moduleAccessMounts(MODULE_ACCESS_CWD),
				permissions: BROWSERBASE_PERMISSIONS,
			});
			await vm.writeFile("/tmp/browserbase-e2e.mjs", GUEST_SCRIPT);
			await vm.writeFile(BROWSE_COMMAND_SCRIPT_PATH, BROWSE_COMMAND_SCRIPT);

			let stdout = "";
			let stderr = "";

			const { pid } = vm.spawn("node", ["/tmp/browserbase-e2e.mjs"], {
				env: browseEnv,
				onStdout: (data: Uint8Array) => {
					stdout += new TextDecoder().decode(data);
				},
				onStderr: (data: Uint8Array) => {
					stderr += new TextDecoder().decode(data);
				},
			});

			const exitCode = await vm.waitProcess(pid);
			expect(exitCode, `stdout:\n${stdout}\nstderr:\n${stderr}`).toBe(0);

			const checksLine = stdout
				.split("\n")
				.find((line) => line.startsWith("BROWSERBASE_GUEST_CHECKS:"));
			expect(checksLine, `stdout:\n${stdout}\nstderr:\n${stderr}`).toBeTruthy();

			const checks = JSON.parse(
				checksLine!.slice("BROWSERBASE_GUEST_CHECKS:".length),
			) as {
				blockedFetchMessage: string;
				envAliased: boolean;
				cliProjected: boolean;
				browseProjected: boolean;
			};

			expect(checks.blockedFetchMessage).toMatch(
				/(EACCES|ERR_ACCESS_DENIED|blocked outbound network access|fetch failed)/,
			);
			expect(checks.envAliased).toBe(true);
			expect(checks.cliProjected).toBe(true);
			expect(checks.browseProjected).toBe(true);

			try {
				const opened = await runVmNodeJsonCommand<{
					url?: string;
				}>(
					vm,
					BROWSE_COMMAND_SCRIPT_PATH,
					["open", "https://example.com"],
					"browse open via direct websocket launcher",
					browseEnv,
				);
				expect(opened.url).toMatch(EXAMPLE_URL_PATTERN);

				const screenshot = await runVmNodeJsonCommand<{
					saved?: string;
				}>(
					vm,
					BROWSE_COMMAND_SCRIPT_PATH,
					["screenshot", SCREENSHOT_PATH],
					"browse screenshot path via direct websocket launcher",
					browseEnv,
				);

				expect(screenshot.saved).toBe(SCREENSHOT_PATH);
				const screenshotBytes = await vm.readFile(SCREENSHOT_PATH);
				expect(screenshotBytes.byteLength).toBeGreaterThanOrEqual(1024);
				expect(Array.from(screenshotBytes.slice(0, 8))).toEqual([
					0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a,
				]);
			} finally {
				await runVmNodeCommand(
					vm,
					BROWSE_COMMAND_SCRIPT_PATH,
					["stop"],
					"browse stop launcher",
					browseEnv,
				).catch(() => {});
			}
		},
		90_000,
	);
});
