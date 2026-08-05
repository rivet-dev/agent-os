import { existsSync } from "node:fs";
import { createRequire } from "node:module";

interface SidecarBinaryModule {
	getSidecarPath(): string;
}

/**
 * Resolves the published agentOS sidecar binary for Node.js clients.
 */
export function resolvePublishedSidecarBinary(): string {
	const override = process.env.AGENTOS_SIDECAR_BIN;
	if (override) {
		if (!existsSync(override)) {
			throw new Error(
				`AgentOS sidecar override is set to ${override} but the file does not exist`,
			);
		}
		return override;
	}

	const require = createRequire(import.meta.url);
	let mod: SidecarBinaryModule;
	try {
		mod = require("@rivet-dev/agentos-sidecar") as SidecarBinaryModule;
	} catch (error) {
		throw new Error(
			"failed to resolve the AgentOS sidecar binary: the @rivet-dev/agentos-sidecar " +
				"package is not installed. Install it, or set AGENTOS_SIDECAR_BIN to a local " +
				`agentos-sidecar binary. (${(error as Error).message})`,
		);
	}
	return mod.getSidecarPath();
}
