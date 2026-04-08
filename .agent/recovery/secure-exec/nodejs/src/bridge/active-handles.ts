import { exposeCustomGlobal } from "@secure-exec/core/internal/shared/global-exposure";
import { bridgeDispatchSync } from "./dispatch.js";

/**
 * Active Handles: Mechanism to keep the sandbox alive for async operations.
 *
 * The V8 isolate doesn't have an event loop, so async callbacks (like child process
 * events) would never fire because the sandbox exits immediately after synchronous
 * code finishes. This module tracks active handles and provides a promise that
 * resolves when all handles complete.
 *
 * See: docs-internal/node/ACTIVE_HANDLES.md
 */

const HANDLE_DISPATCH = {
	register: "kernelHandleRegister",
	unregister: "kernelHandleUnregister",
	list: "kernelHandleList",
} as const;

// Resolvers waiting for all handles to complete
let _waitResolvers: Array<() => void> = [];

// No polling timer needed. Handle drain is detected event-driven via
// _unregisterHandle which resolves all waiters when remaining === 0.

/**
 * Register an active handle that keeps the sandbox alive.
 * Throws if the handle cap (_maxHandles) would be exceeded.
 * @param id Unique identifier for the handle
 * @param description Human-readable description for debugging
 */
export function _registerHandle(id: string, description: string): void {
	try {
		bridgeDispatchSync<void>(HANDLE_DISPATCH.register, id, description);
	} catch (error) {
		if (error instanceof Error && error.message.includes("EAGAIN")) {
			throw new Error(
				"ERR_RESOURCE_BUDGET_EXCEEDED: maximum active handles exceeded",
			);
		}
		throw error;
	}
}

/**
 * Unregister a handle. If no handles remain, resolves all waiters.
 * @param id The handle identifier to unregister
 */
export function _unregisterHandle(id: string): void {
	const remaining = bridgeDispatchSync<number>(HANDLE_DISPATCH.unregister, id);
	if (remaining === 0 && _waitResolvers.length > 0) {
		const resolvers = _waitResolvers;
		_waitResolvers = [];
		resolvers.forEach((r) => r());
	}
}

/**
 * Wait for all active handles and pending timers to complete.
 * Returns immediately if no handles are active and no timers are pending.
 *
 * Timers (setTimeout/setInterval) are tracked separately via _getPendingTimerCount
 * and _waitForTimerDrain exposed from the process bridge module. This ensures CJS
 * scripts that create timers don't exit before the timers fire.
 */
export function _waitForActiveHandles(): Promise<void> {
	const getPendingTimerCount = (globalThis as Record<string, unknown>)
		._getPendingTimerCount as (() => number) | undefined;
	const waitForTimerDrain = (globalThis as Record<string, unknown>)
		._waitForTimerDrain as (() => Promise<void>) | undefined;

	const hasHandles = _getActiveHandles().length > 0;
	const hasTimers =
		typeof getPendingTimerCount === "function" && getPendingTimerCount() > 0;

	if (!hasHandles && !hasTimers) {
		return Promise.resolve();
	}

	const promises: Promise<void>[] = [];

	if (hasHandles) {
		// Instead of polling with setTimeout (which uses IPC and starves user code),
		// register a resolver that fires when _unregisterHandle reduces handles to 0.
		// The _unregisterHandle function already calls _notifyHandleChange.
		promises.push(
			new Promise((resolve) => {
				let settled = false;
				const complete = () => {
					if (settled) return;
					settled = true;
					resolve();
				};
				_waitResolvers.push(complete);
				// Check immediately in case handles already drained
				if (_getActiveHandles().length === 0) {
					complete();
				}
			}),
		);
	}

	if (hasTimers && typeof waitForTimerDrain === "function") {
		promises.push(waitForTimerDrain());
	}

	return Promise.all(promises).then(() => {});
}

/**
 * Get list of currently active handles (for debugging).
 * Returns array of [id, description] tuples.
 */
export function _getActiveHandles(): Array<[string, string]> {
	return bridgeDispatchSync<Array<[string, string]>>(HANDLE_DISPATCH.list);
}

// Install on globalThis for use by other bridge modules and exec().
// Lock bridge internals so sandbox code cannot replace lifecycle hooks.
exposeCustomGlobal("_registerHandle", _registerHandle);
exposeCustomGlobal("_unregisterHandle", _unregisterHandle);
exposeCustomGlobal("_waitForActiveHandles", _waitForActiveHandles);
exposeCustomGlobal("_getActiveHandles", _getActiveHandles);
