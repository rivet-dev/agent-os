import { getIsolateRuntimeSource } from "../generated/isolate-runtime.js";

/**
 * Get the isolate-side script that installs the global `require()` function,
 * `_requireFrom()`, and require helpers (for example `require.resolve` and
 * `require.cache` wiring to the pre-initialized `_moduleCache`).
 */
export function getRequireSetupCode(): string {
	return getIsolateRuntimeSource("requireSetup");
}
