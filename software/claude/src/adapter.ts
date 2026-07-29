#!/usr/bin/env node

/**
 * AgentOS launcher for the last JavaScript-based Claude Agent ACP release.
 *
 * The upstream adapter delegates to an SDK that supports effort controls, but
 * that adapter version does not publish them through ACP configOptions. Keep
 * the extension here until AgentOS can consume a newer VM-compatible release.
 */

import {
	ClaudeAcpAgent,
	runAcp,
} from "@agentclientprotocol/claude-agent-acp/dist/acp-agent.js";
import {
	applyEnvironmentSettings,
	loadManagedSettings,
} from "@agentclientprotocol/claude-agent-acp/dist/utils.js";

type EffortLevel = "low" | "medium" | "high" | "xhigh" | "max";
type ModelInfo = {
	value: string;
	displayName: string;
	supportedEffortLevels?: EffortLevel[];
	supportsEffort?: boolean;
};
type UpstreamSession = {
	query: {
		applyFlagSettings(settings: { effortLevel: EffortLevel }): Promise<void>;
		supportedModels(): Promise<ModelInfo[]>;
	};
	settingsManager: {
		getSettings(): { effortLevel?: Exclude<EffortLevel, "max"> };
	};
	configOptions: Array<Record<string, unknown>>;
};

const EFFORT_CONFIG_ID = "effort";
const EFFORT_LEVELS = ["low", "medium", "high", "xhigh", "max"] as const;

function effortsForModel(model: ModelInfo | undefined): EffortLevel[] {
	return model?.supportedEffortLevels ??
		(model?.supportsEffort ? [...EFFORT_LEVELS] : []);
}

function effortOption(currentValue: EffortLevel): Record<string, unknown> {
	return {
		id: EFFORT_CONFIG_ID,
		name: "Reasoning effort",
		description: "Controls how much thinking Claude applies",
		category: "thought_level",
		type: "select",
		currentValue,
		options: EFFORT_LEVELS.map((value) => ({
			value,
			name:
				value === "xhigh"
					? "Extra high"
					: `${value[0].toUpperCase()}${value.slice(1)}`,
		})),
	};
}

type UpstreamAgent = {
	sessions: Record<string, UpstreamSession>;
};

function upstreamSession(
	agent: UpstreamAgent,
	sessionId: string,
): UpstreamSession {
	const session = agent.sessions[sessionId];
	if (!session) throw new Error("Session not found");
	return session;
}

async function addEffortOption<
	T extends { configOptions?: Array<Record<string, unknown>> },
>(
	agent: UpstreamAgent,
	sessionId: string,
	response: T,
	currentOverride?: EffortLevel,
): Promise<T> {
	const session = upstreamSession(agent, sessionId);
	const models = await session.query.supportedModels();
	const modelOption = (response.configOptions ?? []).find(
		(option) => option.id === "model" && Array.isArray(option.options),
	);
	const currentModel =
		modelOption && typeof modelOption.currentValue === "string"
			? modelOption.currentValue
			: undefined;
	const selectedModel = models.find((model) => model.value === currentModel);
	const supported = effortsForModel(selectedModel);
	const configured = session.settingsManager.getSettings().effortLevel;
	const currentValue = supported.includes(currentOverride as EffortLevel)
		? currentOverride
		: supported.includes(configured as EffortLevel)
		? (configured as EffortLevel)
		: supported.includes("high")
			? "high"
			: supported[0];
	const withExpandedModels = (response.configOptions ?? []).map((option) => {
		if (option !== modelOption || !Array.isArray(option.options)) return option;
		const baseOptions = option.options.filter(
			(value): value is Record<string, unknown> =>
				!!value && typeof value === "object" && !Array.isArray(value),
		);
		return {
			...option,
			options: baseOptions.flatMap((base) => {
				if (typeof base.value !== "string" || typeof base.name !== "string") {
					return [base];
				}
				const info = models.find((model) => model.value === base.value);
				const efforts = effortsForModel(info);
				return [
					base,
					...efforts.map((effort) => ({
						...base,
						value: `${base.value}/${effort}`,
						name: `${base.name} (${effort === "xhigh" ? "Extra high" : effort === "max" ? "Max" : `${effort[0].toUpperCase()}${effort.slice(1)}`})`,
					})),
				];
			}),
		};
	});
	const configOptions = [
		...withExpandedModels.filter(
			(option) => option.id !== EFFORT_CONFIG_ID,
		),
		...(currentValue ? [effortOption(currentValue)] : []),
	];
	session.configOptions = configOptions;
	return { ...response, configOptions };
}

type AgentPrototype = {
	newSession(params: Record<string, unknown>): Promise<Record<string, unknown>>;
	unstable_resumeSession(
		params: Record<string, unknown>,
	): Promise<Record<string, unknown>>;
	setSessionConfigOption(
		params: Record<string, unknown>,
	): Promise<Record<string, unknown>>;
};

const prototype = ClaudeAcpAgent.prototype as unknown as AgentPrototype;
const upstreamNewSession = prototype.newSession;
const upstreamResumeSession = prototype.unstable_resumeSession;
const upstreamSetConfigOption = prototype.setSessionConfigOption;

prototype.newSession = async function (params) {
	const response = await upstreamNewSession.call(this, params);
	return await addEffortOption(
		this as unknown as UpstreamAgent,
		response.sessionId as string,
		response,
	);
};

prototype.unstable_resumeSession = async function (params) {
	const response = await upstreamResumeSession.call(this, params);
	return await addEffortOption(
		this as unknown as UpstreamAgent,
		params.sessionId as string,
		response,
	);
};

prototype.setSessionConfigOption = async function (params) {
	if (params.configId === "model" && typeof params.value === "string") {
		const requestedValue = params.value;
		const session = upstreamSession(
			this as unknown as UpstreamAgent,
			params.sessionId as string,
		);
		const models = await session.query.supportedModels();
		const selected = models
				.filter((model) => requestedValue === model.value || requestedValue.startsWith(`${model.value}/`))
			.sort((left, right) => right.value.length - left.value.length)[0];
		const effort = selected
			? requestedValue.slice(selected.value.length + 1)
			: "";
		const response = await upstreamSetConfigOption.call(this, {
			...params,
			value: selected?.value ?? requestedValue,
		});
		if (effort) {
			if (!effortsForModel(selected).includes(effort as EffortLevel)) {
				throw new Error(`Invalid effort level for ${selected?.value}: ${effort}`);
			}
			await session.query.applyFlagSettings({ effortLevel: effort as EffortLevel });
		}
		return await addEffortOption(
			this as unknown as UpstreamAgent,
			params.sessionId as string,
			response,
			effort ? (effort as EffortLevel) : undefined,
		);
	}
	if (params.configId !== EFFORT_CONFIG_ID) {
		return await upstreamSetConfigOption.call(this, params);
	}
	if (
		typeof params.value !== "string" ||
		!EFFORT_LEVELS.includes(params.value as EffortLevel)
	) {
		throw new Error(`Invalid effort level: ${params.value}`);
	}
	const session = upstreamSession(
		this as unknown as UpstreamAgent,
		params.sessionId as string,
	);
	const value = params.value as EffortLevel;
	const modelOption = session.configOptions.find((option) => option.id === "model");
	const currentModel =
		modelOption && typeof modelOption.currentValue === "string"
			? modelOption.currentValue
			: undefined;
	const model = (await session.query.supportedModels()).find(
		(candidate) => candidate.value === currentModel,
	);
	if (!effortsForModel(model).includes(value)) {
		throw new Error(`Invalid effort level for ${currentModel}: ${value}`);
	}
	await session.query.applyFlagSettings({ effortLevel: value });
	session.configOptions = session.configOptions.map((option) =>
		option.id === EFFORT_CONFIG_ID
			? { ...option, currentValue: value }
			: option,
	);
	return { configOptions: session.configOptions };
};

const managedSettings = loadManagedSettings();
if (managedSettings) applyEnvironmentSettings(managedSettings);

console.log = console.error;
console.info = console.error;
console.warn = console.error;
console.debug = console.error;

const { connection, agent } = runAcp();

async function shutdown(): Promise<void> {
	await agent?.dispose().catch((error) => {
		console.error("Error during cleanup:", error);
	});
	process.exit(0);
}

connection.closed.then(shutdown);
process.on("SIGTERM", shutdown);
process.on("SIGINT", shutdown);
process.stdin.resume();
