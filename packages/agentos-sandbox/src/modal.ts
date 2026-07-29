import type { AgentOsSandboxProvider } from "@rivet-dev/agentos-core";
import type { ModalProviderOptions } from "sandbox-agent/modal";
import { modal as sandboxAgentModal } from "sandbox-agent/modal";
import { sandboxAgentProvider } from "./provider.js";

export type { ModalProviderOptions };

/** Start a Modal sandbox for each AgentOS VM. */
export function modal(options?: ModalProviderOptions): AgentOsSandboxProvider {
	return sandboxAgentProvider(sandboxAgentModal(options));
}
