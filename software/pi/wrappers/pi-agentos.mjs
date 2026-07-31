#!/usr/bin/env node

import { fileURLToPath } from "node:url";
import { readFileSync } from "node:fs";
import { isAbsolute, resolve } from "node:path";
import {
	createAgentSessionFromServices,
	createAgentSessionRuntime,
	createAgentSessionServices,
	getAgentDir,
	parseArgs,
	runRpcMode,
	SessionManager,
	SettingsManager,
} from "@earendil-works/pi-coding-agent";

process.title = "pi-agentos-rpc";
process.env.PI_CODING_AGENT = "true";

let configuredCwd = "";
if (process.env.AGENTOS_PI_CWD_FILE) {
	try {
		configuredCwd = readFileSync(process.env.AGENTOS_PI_CWD_FILE, "utf8").trim();
	} catch (error) {
		if (error?.code !== "ENOENT") throw error;
	}
}
const launchCwd = isAbsolute(configuredCwd) ? configuredCwd : process.cwd();
process.chdir(launchCwd);
process.env.PWD = launchCwd;
const agentDir = getAgentDir();
const parsed = parseArgs(process.argv.slice(2));
const errors = parsed.diagnostics.filter((diagnostic) => diagnostic.type === "error");
if (parsed.mode && parsed.mode !== "rpc") {
	errors.push({ type: "error", message: "pi-agentos only supports RPC mode" });
}
if (errors.length > 0) throw new Error(errors.map((diagnostic) => diagnostic.message).join("\n"));

const extensionPath = fileURLToPath(new URL("./extensions/codex-auth.mjs", import.meta.url));
const extensionPaths = [extensionPath, ...(parsed.extensions ?? [])].map((path) =>
	isAbsolute(path) ? path : resolve(launchCwd, path),
);
const startupSettings = SettingsManager.create(launchCwd, agentDir, { projectTrusted: false });
const sessionDir = parsed.sessionDir ?? process.env.PI_CODING_AGENT_SESSION_DIR ?? startupSettings.getSessionDir();
const sessionManager = parsed.noSession
	? SessionManager.inMemory(launchCwd)
	: parsed.session
		? SessionManager.open(
				isAbsolute(parsed.session) ? parsed.session : resolve(launchCwd, parsed.session),
				sessionDir,
			)
		: SessionManager.create(launchCwd, sessionDir);

const createRuntime = async ({ cwd, agentDir: runtimeAgentDir, sessionManager, sessionStartEvent }) => {
	const settingsManager = SettingsManager.create(cwd, runtimeAgentDir, { projectTrusted: false });
	const services = await createAgentSessionServices({
		cwd,
		agentDir: runtimeAgentDir,
		settingsManager,
		extensionFlagValues: parsed.unknownFlags,
		resourceLoaderOptions: {
			additionalExtensionPaths: extensionPaths,
			noThemes: parsed.noThemes ?? true,
			noExtensions: parsed.noExtensions,
			noSkills: parsed.noSkills,
			noPromptTemplates: parsed.noPromptTemplates,
			noContextFiles: parsed.noContextFiles,
			additionalSkillPaths: parsed.skills,
			additionalPromptTemplatePaths: parsed.promptTemplates,
			additionalThemePaths: parsed.themes,
			systemPrompt: parsed.systemPrompt,
			appendSystemPrompt: parsed.appendSystemPrompt,
		},
	});
	const diagnostics = [
		...services.diagnostics,
		...services.resourceLoader.getExtensions().errors.map(({ path, error }) => ({
			type: "error",
			message: `Failed to load extension "${path}": ${error}`,
		})),
	];
	const runtimeErrors = diagnostics.filter((diagnostic) => diagnostic.type === "error");
	if (runtimeErrors.length > 0) {
		throw new Error(runtimeErrors.map((diagnostic) => diagnostic.message).join("\n"));
	}

	return {
		...(await createAgentSessionFromServices({
			services,
			sessionManager,
			sessionStartEvent,
			tools: parsed.tools,
			excludeTools: parsed.excludeTools,
			noTools: parsed.noTools ? "all" : parsed.noBuiltinTools ? "builtin" : undefined,
		})),
		services,
		diagnostics,
	};
};

const runtime = await createAgentSessionRuntime(createRuntime, {
	cwd: sessionManager.getCwd(),
	agentDir,
	sessionManager,
});
await runRpcMode(runtime);
