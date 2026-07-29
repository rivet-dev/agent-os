import { existsSync } from "node:fs";
import { createRequire } from "node:module";

interface SidecarBinaryModule {
	getSidecarPath(): string;
}

/**
 * Resolves the published AgentOS runtime sidecar binary for Node.js clients.
 */
export function resolvePublishedSidecarBinary(): string {
	const override =
		process.env.AGENTOS_NATIVE_SIDECAR_BIN ??
		process.env.AGENTOS_SIDECAR_BIN;
	if (override) {
		if (!existsSync(override)) {
			throw new Error(
				`AgentOS native sidecar override is set to ${override} but the file does not exist`,
			);
		}
		return override;
	}

	const require = createRequire(import.meta.url);
	let mod: SidecarBinaryModule;
	try {
		mod = require("@rivet-dev/agentos-runtime-sidecar") as SidecarBinaryModule;
	} catch (error) {
		throw new Error(
			"failed to resolve the AgentOS runtime sidecar binary: the @rivet-dev/agentos-runtime-sidecar " +
				"package is not installed. Install it, or set AGENTOS_NATIVE_SIDECAR_BIN to a local " +
				`agentos-native-sidecar binary. (${(error as Error).message})`,
		);
	}
	return mod.getSidecarPath();
}
