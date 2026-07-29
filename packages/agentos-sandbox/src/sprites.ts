import type { AgentOsSandboxProvider } from "@rivet-dev/agentos-core";
import type {
	SpritesClientOverrides,
	SpritesCreateOverrides,
	SpritesProviderOptions,
} from "sandbox-agent/sprites";
import { sprites as sandboxAgentSprites } from "sandbox-agent/sprites";
import { sandboxAgentProvider } from "./provider.js";

export type {
	SpritesClientOverrides,
	SpritesCreateOverrides,
	SpritesProviderOptions,
};

/** Start a Fly.io Sprite for each AgentOS VM. */
export function sprites(
	options?: SpritesProviderOptions,
): AgentOsSandboxProvider {
	return sandboxAgentProvider(sandboxAgentSprites(options));
}
