import { resolve } from "node:path";
import pi from "@rivet-dev/agent-os-pi";
import { describe, expect, test } from "vitest";
import { AgentOs } from "../src/agent-os.js";

const MODULE_ACCESS_CWD = resolve(
	import.meta.dirname,
	"../../../examples/quickstart",
);
const SDK_PATH = "/root/node_modules/@mariozechner/pi-coding-agent/dist/index.js";
const PROBE_TIMEOUT_MS = 5_000;
const PROBE_ENV = {
	HOME: "/home/user",
	PI_OFFLINE: "1",
	PI_SKIP_VERSION_CHECK: "1",
};

type ProbeFailureKind =
	| "module_resolution"
	| "missing_polyfill"
	| "runtime_error"
	| "timeout";

interface ProbeFailure {
	kind: ProbeFailureKind;
	step: string;
	target: string;
	code?: string;
	message: string;
	detail?: string;
	stack?: string[];
}

interface ProbeStep {
	name: string;
	target: string;
	status: "ok" | "failed";
	summary?: string;
}

interface ProbeReport {
	packageName: string;
	startedAt: string;
	probeTimeoutMs: number;
	steps: ProbeStep[];
	failures: ProbeFailure[];
}

interface ProbeCase {
	name: string;
	target: string;
	timeoutMs?: number;
	script: string;
	classifySuccess?: (stdout: string) => ProbeFailure | null;
}

const MINIMAL_RESOURCE_LOADER_SOURCE = `
function createMinimalResourceLoader(sdk, cwd) {
	const runtime =
		typeof sdk.createExtensionRuntime === "function"
			? sdk.createExtensionRuntime()
			: {};
	return {
		async reload() {},
		getExtensions() {
			return {
				extensions: [],
				errors: [],
				runtime,
			};
		},
		getSkills() {
			return { skills: [], diagnostics: [] };
		},
		getPrompts() {
			return { prompts: [], diagnostics: [] };
		},
		getThemes() {
			return { themes: [], diagnostics: [] };
		},
		getAgentsFiles() {
			return { agentsFiles: [] };
		},
		getSystemPrompt() {
			return "Pi SDK V8 boot probe";
		},
		getAppendSystemPrompt() {
			return [];
		},
		getPathMetadata() {
			return new Map();
		},
		extendResources() {},
	};
}
`;

function classifyErrorText(
	step: string,
	target: string,
	stderr: string,
): ProbeFailure {
	const lines = stderr
		.split("\n")
		.map((line) => line.trim())
		.filter(Boolean);
	const headline = lines[0] ?? "Probe failed";
	const lowered = stderr.toLowerCase();

	let kind: ProbeFailureKind = "runtime_error";
	if (
		lowered.includes("module not found") ||
		lowered.includes("cannot find module") ||
		lowered.includes("cannot find package") ||
		lowered.includes("package path not exported")
	) {
		kind = "module_resolution";
	} else if (
		lowered.includes("err_access_denied") ||
		lowered.includes("not implemented") ||
		lowered.includes("is not a function") ||
		lowered.includes("unsupported builtin")
	) {
		kind = "missing_polyfill";
	}

	const codeMatch = stderr.match(/\b([A-Z][A-Z0-9_]+)\b/);
	return {
		kind,
		step,
		target,
		code: codeMatch?.[1],
		message: headline,
		detail: stderr.trim() || undefined,
		stack: lines.slice(0, 8),
	};
}

function buildProbeCases(): ProbeCase[] {
	return [
		{
			name: "projected-sdk-path",
			target: SDK_PATH,
			script: `
import fs from "node:fs";
console.log(JSON.stringify({ exists: fs.existsSync(${JSON.stringify(SDK_PATH)}) }));
`,
			classifySuccess: (stdout) => {
				const parsed = JSON.parse(stdout) as { exists?: boolean };
				return parsed.exists
					? null
					: {
							kind: "module_resolution",
							step: "projected-sdk-path",
							target: SDK_PATH,
							message: "Projected SDK entrypoint is missing from /root/node_modules",
							detail: stdout.trim(),
						};
			},
		},
		{
			name: "sdk-bare-import",
			target: "@mariozechner/pi-coding-agent",
			script: `
const mod = await import("@mariozechner/pi-coding-agent");
console.log(JSON.stringify({ exportCount: Object.keys(mod).length }));
`,
		},
		{
			name: "sdk-path-import",
			target: SDK_PATH,
			script: `
const mod = await import(${JSON.stringify(SDK_PATH)});
console.log(JSON.stringify({ exportCount: Object.keys(mod).length }));
`,
		},
		{
			name: "sdk-create-coding-tools",
			target: "createCodingTools()",
			script: `
const sdk = await import(${JSON.stringify(SDK_PATH)});
const tools = sdk.createCodingTools("/home/user/workspace");
console.log(JSON.stringify({ toolCount: tools.length }));
`,
		},
		{
			name: "sdk-create-agent-session",
			target: "createAgentSession()",
			timeoutMs: 8_000,
			script: `
${MINIMAL_RESOURCE_LOADER_SOURCE}
const sdk = await import(${JSON.stringify(SDK_PATH)});
const cwd = "/home/user/workspace";
const created = await sdk.createAgentSession({
	cwd,
	sessionManager: sdk.SessionManager.inMemory(cwd),
	resourceLoader: createMinimalResourceLoader(sdk, cwd),
	tools: sdk.createCodingTools(cwd),
});
await created.session.abort().catch(() => {});
console.log(
	JSON.stringify({
		sessionId: created.session.sessionId,
		thinkingLevel: created.session.thinkingLevel,
	}),
);
`,
		},
		{
			name: "dependency-import",
			target: "@anthropic-ai/sdk",
			script: `
const mod = await import("@anthropic-ai/sdk");
console.log(JSON.stringify({ exportCount: Object.keys(mod).length }));
`,
		},
		{
			name: "dependency-import",
			target: "@mariozechner/jiti",
			script: `
const mod = await import("@mariozechner/jiti");
console.log(JSON.stringify({ exportCount: Object.keys(mod).length }));
`,
		},
		{
			name: "dependency-import",
			target: "zod",
			script: `
const mod = await import("zod");
console.log(JSON.stringify({ exportCount: Object.keys(mod).length }));
`,
		},
		{
			name: "builtin-import",
			target: "node:child_process",
			script: `
const mod = await import("node:child_process");
const missingMethods = ["spawn", "spawnSync"].filter((name) => typeof mod[name] === "undefined");
console.log(JSON.stringify({ missingMethods }));
`,
			classifySuccess: (stdout) => {
				const parsed = JSON.parse(stdout) as { missingMethods?: string[] };
				if (!parsed.missingMethods || parsed.missingMethods.length === 0) {
					return null;
				}
				return {
					kind: "missing_polyfill",
					step: "builtin-import",
					target: "node:child_process",
					message: `Missing child_process exports: ${parsed.missingMethods.join(", ")}`,
					detail: stdout.trim(),
				};
			},
		},
		{
			name: "builtin-import",
			target: "node:fs/promises",
			script: `
const mod = await import("node:fs/promises");
const missingMethods = ["access", "open", "readFile", "stat"].filter(
	(name) => typeof mod[name] === "undefined",
);
console.log(JSON.stringify({ missingMethods }));
`,
			classifySuccess: (stdout) => {
				const parsed = JSON.parse(stdout) as { missingMethods?: string[] };
				if (!parsed.missingMethods || parsed.missingMethods.length === 0) {
					return null;
				}
				return {
					kind: "missing_polyfill",
					step: "builtin-import",
					target: "node:fs/promises",
					message: `Missing fs/promises exports: ${parsed.missingMethods.join(", ")}`,
					detail: stdout.trim(),
				};
			},
		},
		{
			name: "builtin-import",
			target: "node:module",
			script: `
const mod = await import("node:module");
const missingMethods = ["createRequire"].filter((name) => typeof mod[name] === "undefined");
console.log(JSON.stringify({ missingMethods }));
`,
			classifySuccess: (stdout) => {
				const parsed = JSON.parse(stdout) as { missingMethods?: string[] };
				if (!parsed.missingMethods || parsed.missingMethods.length === 0) {
					return null;
				}
				return {
					kind: "missing_polyfill",
					step: "builtin-import",
					target: "node:module",
					message: `Missing module exports: ${parsed.missingMethods.join(", ")}`,
					detail: stdout.trim(),
				};
			},
		},
	];
}

async function runProbeCase(
	probe: ProbeCase,
	index: number,
): Promise<{ step: ProbeStep; failure: ProbeFailure | null }> {
	const vm = await AgentOs.create({
		moduleAccessCwd: MODULE_ACCESS_CWD,
		software: [pi],
	});
	let stdout = "";
	let stderr = "";
	try {
		await vm.mkdir("/home/user/workspace", { recursive: true });
		const probePath = `/tmp/pi-sdk-boot-probe-${index}.mjs`;
		await vm.writeFile(probePath, probe.script);

		const { pid } = vm.spawn("node", [probePath], {
			env: PROBE_ENV,
			onStdout: (data: Uint8Array) => {
				stdout += new TextDecoder().decode(data);
			},
			onStderr: (data: Uint8Array) => {
				stderr += new TextDecoder().decode(data);
			},
		});

		const exitCode = await Promise.race([
			vm.waitProcess(pid),
			new Promise<number>((_, reject) => {
				setTimeout(() => {
					reject(
						new Error(
							`probe timed out after ${probe.timeoutMs ?? PROBE_TIMEOUT_MS}ms`,
						),
					);
				}, probe.timeoutMs ?? PROBE_TIMEOUT_MS);
			}),
		]);

		if (exitCode !== 0) {
			const failure = classifyErrorText(probe.name, probe.target, stderr);
			return {
				step: { name: probe.name, target: probe.target, status: "failed" },
				failure,
			};
		}

		const successFailure = probe.classifySuccess?.(stdout.trim()) ?? null;
		return {
			step: {
				name: probe.name,
				target: probe.target,
				status: successFailure ? "failed" : "ok",
				summary: stdout.trim() || undefined,
			},
			failure: successFailure,
		};
	} catch (error) {
		return {
			step: { name: probe.name, target: probe.target, status: "failed" },
			failure: {
				kind: "timeout",
				step: probe.name,
				target: probe.target,
				message:
					error instanceof Error ? error.message : `Probe timed out: ${String(error)}`,
				detail: stderr.trim() || undefined,
			},
		};
	} finally {
		await vm.dispose();
	}
}

describe("Pi SDK V8 boot probe", () => {
	test(
		"reports Pi SDK boot gaps inside the VM without requiring a successful boot",
		async () => {
			const report: ProbeReport = {
				packageName: "@mariozechner/pi-coding-agent",
				startedAt: new Date().toISOString(),
				probeTimeoutMs: PROBE_TIMEOUT_MS,
				steps: [],
				failures: [],
			};

			const probes = buildProbeCases();
			for (const [index, probe] of probes.entries()) {
				const result = await runProbeCase(probe, index);
				report.steps.push(result.step);
				if (result.failure) {
					report.failures.push(result.failure);
				}
			}

			console.log(`PI_BOOT_PROBE_REPORT ${JSON.stringify(report)}`);

			expect(report.steps.length).toBe(probes.length);
			expect(report.steps.some((step) => step.name === "sdk-path-import")).toBe(
				true,
			);
			expect(
				report.steps.some((step) => step.name === "sdk-create-agent-session"),
			).toBe(true);
			expect(Array.isArray(report.failures)).toBe(true);
		},
		90_000,
	);
});
