import type { AgentOsSandboxProvider } from "@rivet-dev/agentos-core";
import type {
	CloudflareProviderOptions,
	CloudflareSandboxClient,
} from "sandbox-agent/cloudflare";
import { cloudflare as sandboxAgentCloudflare } from "sandbox-agent/cloudflare";
import { sandboxAgentProvider } from "./provider.js";

export type { CloudflareProviderOptions, CloudflareSandboxClient };

/** Start a Cloudflare sandbox for each AgentOS VM. */
export function cloudflare(
	options: CloudflareProviderOptions,
): AgentOsSandboxProvider {
	return sandboxAgentProvider(sandboxAgentCloudflare(options));
}
