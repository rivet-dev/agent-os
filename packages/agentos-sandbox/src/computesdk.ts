import type { AgentOsSandboxProvider } from "@rivet-dev/agentos-core";
import type { ComputeSdkProviderOptions } from "sandbox-agent/computesdk";
import { computesdk as sandboxAgentComputeSdk } from "sandbox-agent/computesdk";
import { sandboxAgentProvider } from "./provider.js";

export type { ComputeSdkProviderOptions };

/** Start a ComputeSDK sandbox for each AgentOS VM. */
export function computesdk(
	options?: ComputeSdkProviderOptions,
): AgentOsSandboxProvider {
	return sandboxAgentProvider(sandboxAgentComputeSdk(options));
}
