import { existsSync } from "node:fs";
import { resolve } from "node:path";
import { afterEach, describe, expect, test } from "vitest";
import { AgentOs, type Permissions } from "../src/index.js";

const MODULE_ACCESS_CWD = resolve(import.meta.dirname, "..");
const BROWSER_BASE_API_KEY = process.env.BROWSER_BASE_API_KEY ?? "";
const BROWSER_BASE_PROJECT_ID = process.env.BROWSER_BASE_PROJECT_ID ?? "";
const HAS_BROWSERBASE_CREDENTIALS = Boolean(
	BROWSER_BASE_API_KEY && BROWSER_BASE_PROJECT_ID,
);

if (!HAS_BROWSERBASE_CREDENTIALS) {
	console.warn(
		"Skipping Browserbase e2e: source ~/misc/env.txt so BROWSER_BASE_API_KEY and BROWSER_BASE_PROJECT_ID are available.",
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

const BROWSE_PATH = "/root/node_modules/@browserbasehq/browse-cli/dist/index.js";
const CLI_PATH = "/root/node_modules/@browserbasehq/cli/dist/main.js";
const JSON_OUTPUT_TIMEOUT_MS = 60_000;
const SESSION_SCRIPT_PATH = "/tmp/browserbase-session.mjs";

function testIf(
	condition: boolean,
	...args: Parameters<typeof test>
): void {
	if (condition) {
		// @ts-expect-error forwarded test() arguments stay runtime-compatible.
		test(...args);
		return;
	}
	const [name] = args;
	test(String(name), () => {});
}

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
					reject(new Error(`${label} timed out after ${JSON_OUTPUT_TIMEOUT_MS}ms`));
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

const SESSION_SCRIPT = String.raw`
const mode = process.argv[2];

async function request(path, init) {
  const response = await fetch("https://api.browserbase.com" + path, {
    ...init,
    headers: {
      "x-bb-api-key": process.env.BROWSERBASE_API_KEY,
      "content-type": "application/json",
      ...(init?.headers ?? {}),
    },
    signal: AbortSignal.timeout(30_000),
  });

  if (!response.ok) {
    throw new Error(
      "Browserbase API " +
        path +
        " failed with " +
        response.status +
        ": " +
        (await response.text()),
    );
  }

  return response.json();
}

if (mode === "create") {
  const created = await request("/v1/sessions", {
    method: "POST",
    body: JSON.stringify({
      projectId: process.env.BROWSERBASE_PROJECT_ID,
      browserSettings: {
        viewport: { width: 1288, height: 711 },
      },
      userMetadata: {
        agent_os_browserbase_e2e: "true",
      },
    }),
  });
  console.log(
    JSON.stringify({
      connectUrl: created.connectUrl ?? null,
      id: created.id ?? null,
      status: created.status ?? null,
    }),
  );
} else if (mode === "release") {
  const sessionId = process.argv[3];
  if (!sessionId) {
    throw new Error("missing session id for release");
  }
  const released = await request("/v1/sessions/" + sessionId, {
    method: "POST",
    body: JSON.stringify({ status: "REQUEST_RELEASE" }),
  });
  console.log(
    JSON.stringify({
      id: released.id ?? sessionId,
      status: released.status ?? "REQUEST_RELEASE",
    }),
  );
} else {
  throw new Error("unknown mode: " + String(mode));
}
`;

describe("Browserbase e2e", () => {
	let vm: AgentOs | null = null;

	afterEach(async () => {
		if (vm) {
			await vm.dispose();
			vm = null;
		}
	});

	const browserbaseTest = (...args: Parameters<typeof test>) =>
		testIf(HAS_BROWSERBASE_CREDENTIALS, ...args);

	browserbaseTest(
		"runs Browserbase browser automation inside the VM with restricted guest egress",
		async () => {
			const screenshotPath = `/tmp/browserbase-e2e-${Date.now()}.png`;
			const browseEnv = {
				BROWSERBASE_API_KEY: BROWSER_BASE_API_KEY,
				BROWSERBASE_PROJECT_ID: BROWSER_BASE_PROJECT_ID,
			};

			vm = await AgentOs.create({
				moduleAccessCwd: MODULE_ACCESS_CWD,
				permissions: BROWSERBASE_PERMISSIONS,
			});
			await vm.writeFile("/tmp/browserbase-e2e.mjs", GUEST_SCRIPT);
			await vm.writeFile(SESSION_SCRIPT_PATH, SESSION_SCRIPT);

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

			const created = await runVmNodeJsonCommand<{
				connectUrl?: string;
				id?: string;
				status?: string;
			}>(
				vm,
				SESSION_SCRIPT_PATH,
				["create"],
				"Browserbase session create",
				browseEnv,
			);
			expect(created.id).toBeTruthy();
			expect(created.connectUrl).toMatch(/^wss?:\/\//);

			try {
				await runVmNodeCommand(
					vm,
					BROWSE_PATH,
					[
						"--ws",
						created.connectUrl!,
						"open",
						"https://example.com",
						"--json",
					],
					"browse open via direct websocket",
					browseEnv,
				);
				await runVmNodeCommand(
					vm,
					BROWSE_PATH,
					[
						"--ws",
						created.connectUrl!,
						"screenshot",
						screenshotPath,
						"--json",
					],
					"browse screenshot via direct websocket",
					browseEnv,
				);
			} finally {
				if (created.id) {
					await runVmNodeCommand(
						vm,
						SESSION_SCRIPT_PATH,
						["release", created.id],
						"Browserbase session release",
						browseEnv,
					).catch(() => {});
				}
			}

			expect(existsSync(screenshotPath)).toBe(false);

			const screenshotBytes = await vm.readFile(screenshotPath);
			expect(screenshotBytes.byteLength).toBeGreaterThanOrEqual(1024);
			expect(Array.from(screenshotBytes.slice(0, 8))).toEqual([
				0x89,
				0x50,
				0x4e,
				0x47,
				0x0d,
				0x0a,
				0x1a,
				0x0a,
			]);
		},
		90_000,
	);
});
