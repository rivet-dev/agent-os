import { setup as rivetkitSetup } from "rivetkit";

export type { Registry } from "rivetkit";

const AGENTOS_REGISTRY_MESSAGE_SIZE_DEFAULT = 512 * 1024 * 1024;

/**
 * RivetKit setup with the direct actor SQLite UDS enabled. The UDS is consumed
 * by the AgentOS sidecar; filesystem SQL never crosses the JavaScript layer.
 */
export const setup: typeof rivetkitSetup = ((
	input: Parameters<typeof rivetkitSetup>[0],
) =>
	rivetkitSetup({
		...input,
		maxIncomingMessageSize: AGENTOS_REGISTRY_MESSAGE_SIZE_DEFAULT,
		maxOutgoingMessageSize: AGENTOS_REGISTRY_MESSAGE_SIZE_DEFAULT,
		experimentalActorUds: true,
	} as Parameters<typeof rivetkitSetup>[0])) as typeof rivetkitSetup;

export * from "@rivet-dev/agentos-core";
export type {
	AgentOsActorConfigInput as AgentOSActorConfigInput,
	AgentOsActorConfigInput as AgentOSConfigInput,
} from "./actor.js";
export {
	type AgentOsActions,
	type AgentOsActorConfigInput,
	type AgentOsActorExtras,
	type AgentOsEventHooks,
	createAgentOS,
	createAgentOS as agentOS,
	createAgentOsActions,
	type VmFetchOptions,
	type VmFetchResponse,
	type VmFetchStreamChunk,
	type VmFetchStreamHead,
} from "./actor.js";
export type {
	ActorData,
	ActorLanguageSpawnOptions,
	ActorSpawnOptions,
	AgentOsEvents,
	ProcessExitPayload,
	ProcessOutputPayload,
	SerializableCronAction,
	SerializableCronEvent,
	SerializableCronJobInfo,
	SerializableCronJobOptions,
	ShellDataPayload,
	ShellExitPayload,
	VmBootedPayload,
	VmShutdownPayload,
} from "./types.js";
