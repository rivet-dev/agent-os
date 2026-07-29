import type { AgentOsSandboxProvider } from "@rivet-dev/agentos-core";
import type { DaytonaProviderOptions } from "sandbox-agent/daytona";
import { daytona as sandboxAgentDaytona } from "sandbox-agent/daytona";
import { sandboxAgentProvider } from "./provider.js";

export type { DaytonaProviderOptions };

/** Start a Daytona sandbox for each AgentOS VM. */
export function daytona(
	options?: DaytonaProviderOptions,
): AgentOsSandboxProvider {
	return sandboxAgentProvider(sandboxAgentDaytona(options));
}
