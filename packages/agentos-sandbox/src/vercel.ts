import type { AgentOsSandboxProvider } from "@rivet-dev/agentos-core";
import type { VercelProviderOptions } from "sandbox-agent/vercel";
import { vercel as sandboxAgentVercel } from "sandbox-agent/vercel";
import { sandboxAgentProvider } from "./provider.js";

export type { VercelProviderOptions };

/** Start a Vercel Sandbox for each AgentOS VM. */
export function vercel(
	options?: VercelProviderOptions,
): AgentOsSandboxProvider {
	return sandboxAgentProvider(sandboxAgentVercel(options));
}
