// Converged sync-bridge router.
//
// The migration seam between the legacy in-process TS-kernel sync-bridge
// servicer (`runtime-driver.ts` `handleSyncBridgeOperation`) and the converged
// wasm-kernel handler (`ConvergedSyncBridgeHandler`). Operations the converged
// handler services (fs.* / net.* / dns.*) go to the wasm sidecar; everything not
// yet converged (module.* / child_process.* / dgram.* / process.signal_state)
// falls back to the legacy servicer. As each family is converged the fallback
// shrinks; when it is empty the legacy servicer is deleted (spec slice 5).

import type { ConvergedSyncResponse } from "./converged-fs-bridge.js";
import type { ConvergedSyncBridgeHandler } from "./converged-sync-bridge-handler.js";

/** Services a sync-bridge operation not yet handled by the converged path. */
export type LegacySyncBridgeServicer = (
	operation: string,
	args: readonly unknown[],
) => Promise<ConvergedSyncResponse>;

/**
 * An async converged servicer (e.g. module resolution) that routes its
 * operations to the kernel but cannot be synchronous because it walks the
 * filesystem. Tried after the sync handler, before the legacy fallback.
 */
export interface AsyncConvergedServicer {
	handles(operation: string): boolean;
	handle(
		operation: string,
		args: readonly unknown[],
	): Promise<ConvergedSyncResponse>;
}

export interface ConvergedSyncBridgeRouterOptions {
	handler: ConvergedSyncBridgeHandler;
	legacy: LegacySyncBridgeServicer;
	asyncServicers?: readonly AsyncConvergedServicer[];
}

export class ConvergedSyncBridgeRouter {
	private readonly handler: ConvergedSyncBridgeHandler;
	private readonly legacy: LegacySyncBridgeServicer;
	private readonly asyncServicers: readonly AsyncConvergedServicer[];

	constructor(options: ConvergedSyncBridgeRouterOptions) {
		this.handler = options.handler;
		this.legacy = options.legacy;
		this.asyncServicers = options.asyncServicers ?? [];
	}

	/**
	 * Route one sync-bridge operation: sync converged handler (fs/net/dns) first,
	 * then any async converged servicers (module resolution), then the legacy
	 * fallback for families not yet converged. Returns a promise either way so
	 * callers have a single await point.
	 */
	async route(
		operation: string,
		args: readonly unknown[],
	): Promise<ConvergedSyncResponse> {
		if (this.handler.handles(operation)) {
			return this.handler.handle(operation, args);
		}
		for (const servicer of this.asyncServicers) {
			if (servicer.handles(operation)) {
				return servicer.handle(operation, args);
			}
		}
		return this.legacy(operation, args);
	}

	/** True once every sync-bridge operation routes to a converged servicer. */
	static isFullyConverged(
		handler: ConvergedSyncBridgeHandler,
		operations: readonly string[],
		asyncServicers: readonly AsyncConvergedServicer[] = [],
	): boolean {
		return operations.every(
			(operation) =>
				handler.handles(operation) ||
				asyncServicers.some((servicer) => servicer.handles(operation)),
		);
	}
}
