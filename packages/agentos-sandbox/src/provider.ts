import type {
	AgentOsSandboxClient,
	AgentOsSandboxProvider,
} from "@rivet-dev/agentos-core";
import {
	SandboxAgent,
	type SandboxProvider as SandboxAgentBackend,
	type SandboxAgentStartOptions,
} from "sandbox-agent";

export type SandboxAgentProviderOptions = Omit<
	SandboxAgentStartOptions,
	"sandbox" | "sandboxId"
>;

/** Adapt any sandbox-agent backend into a per-VM AgentOS sandbox provider. */
export function sandboxAgentProvider(
	backend: SandboxAgentBackend,
	options: SandboxAgentProviderOptions = {},
): AgentOsSandboxProvider {
	return {
		async start(): Promise<AgentOsSandboxClient> {
			const client = await SandboxAgent.start({ ...options, sandbox: backend });
			return new Proxy(client, {
				get(target, property) {
					if (property === "dispose") {
						return target.destroySandbox.bind(target);
					}
					const value = Reflect.get(target, property, target);
					return typeof value === "function" ? value.bind(target) : value;
				},
			}) as AgentOsSandboxClient;
		},
	};
}
