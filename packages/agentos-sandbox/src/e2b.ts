import type { AgentOsSandboxProvider } from "@rivet-dev/agentos-core";
import type { E2BProviderOptions } from "sandbox-agent/e2b";
import { e2b as sandboxAgentE2b } from "sandbox-agent/e2b";
import { sandboxAgentProvider } from "./provider.js";

export type { E2BProviderOptions };

/** Start an E2B sandbox for each AgentOS VM. */
export function e2b(options?: E2BProviderOptions): AgentOsSandboxProvider {
	return sandboxAgentProvider(sandboxAgentE2b(options));
}
