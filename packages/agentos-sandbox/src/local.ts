import type { AgentOsSandboxProvider } from "@rivet-dev/agentos-core";
import type { LocalProviderOptions } from "sandbox-agent/local";
import { local as sandboxAgentLocal } from "sandbox-agent/local";
import { sandboxAgentProvider } from "./provider.js";

export type { LocalProviderOptions };

/** Start a local Sandbox Agent process for each AgentOS VM. */
export function local(options?: LocalProviderOptions): AgentOsSandboxProvider {
	return sandboxAgentProvider(sandboxAgentLocal(options));
}
