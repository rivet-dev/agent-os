import { createHash } from "node:crypto";

/**
 * Node equivalent of OpenCode's existing `Hash.fast(base)` helper.
 *
 * Upstream remote-skill discovery still spells this call as
 * `Bun.hash(base).toString(16)`. The Node bundle substitutes only that property
 * reference with this function; returning the SHA-1 hex string means the
 * existing `.toString(16)` remains a no-op, exactly as it would after the
 * intended upstream `Hash.fast(base)` change.
 */
export function openCodeHashFast(value: string): string {
	return createHash("sha1").update(value).digest("hex");
}

declare global {
	var __agentOSOpenCodeHashFast: typeof openCodeHashFast | undefined;
}

export function installOpenCodeNodeCompatibility(): void {
	globalThis.__agentOSOpenCodeHashFast ??= openCodeHashFast;
}
