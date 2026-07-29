import type { AgentOsSandboxProvider } from "@rivet-dev/agentos-core";
import type { DockerProviderOptions } from "sandbox-agent/docker";
import { docker as sandboxAgentDocker } from "sandbox-agent/docker";
import { sandboxAgentProvider } from "./provider.js";

export type { DockerProviderOptions };

/** Start a local Docker sandbox for each AgentOS VM. */
export function docker(
	options?: DockerProviderOptions,
): AgentOsSandboxProvider {
	return sandboxAgentProvider(sandboxAgentDocker(options));
}
