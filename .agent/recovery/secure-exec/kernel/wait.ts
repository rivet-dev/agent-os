/**
 * Unified blocking I/O wait system.
 *
 * Provides WaitHandle and WaitQueue primitives for all kernel subsystems
 * (pipes, sockets, flock, poll) to share the same wait/wake mechanism.
 * Promise-based — no Atomics.
 */

/**
 * A single wait/wake handle. Callers await wait(), producers call wake().
 * Each handle resolves exactly once (either by wake or timeout).
 */
export class WaitHandle {
	private resolve: (() => void) | null = null;
	private timer: ReturnType<typeof setTimeout> | null = null;
	private settled = false;
	readonly promise: Promise<void>;
	/** True if the handle resolved via timeout rather than wake(). */
	timedOut = false;

	constructor(timeoutMs?: number) {
		this.promise = new Promise<void>((resolve) => {
			this.resolve = resolve;
		});

		if (timeoutMs !== undefined && timeoutMs >= 0) {
			this.timer = setTimeout(() => {
				if (!this.settled) {
					this.timedOut = true;
					this.settled = true;
					this.resolve!();
					this.resolve = null;
				}
			}, timeoutMs);
		}
	}

	/** Suspend until woken or timed out. */
	wait(): Promise<void> {
		return this.promise;
	}

	/** Wake this handle. No-op if already settled. */
	wake(): void {
		if (this.settled) return;
		this.settled = true;
		if (this.timer !== null) {
			clearTimeout(this.timer);
			this.timer = null;
		}
		this.resolve!();
		this.resolve = null;
	}

	/** Whether this handle has already been resolved. */
	get isSettled(): boolean {
		return this.settled;
	}
}

/**
 * A FIFO queue of WaitHandles. Subsystems enqueue waiters and producers
 * wake them one-at-a-time or all-at-once.
 */
export class WaitQueue {
	private waiters: WaitHandle[] = [];

	/** Create and enqueue a new WaitHandle. */
	enqueue(timeoutMs?: number): WaitHandle {
		const handle = new WaitHandle(timeoutMs);
		this.waiters.push(handle);
		return handle;
	}

	/** Remove a waiter from the queue without waking it. */
	remove(handle: WaitHandle): void {
		const index = this.waiters.indexOf(handle);
		if (index >= 0) {
			this.waiters.splice(index, 1);
		}
	}

	/** Wake exactly one waiter (FIFO order). Returns true if a waiter was woken. */
	wakeOne(): boolean {
		while (this.waiters.length > 0) {
			const handle = this.waiters.shift()!;
			if (!handle.isSettled) {
				handle.wake();
				return true;
			}
			// Skip already-settled handles (timed out)
		}
		return false;
	}

	/** Wake all enqueued waiters. Returns the number woken. */
	wakeAll(): number {
		let count = 0;
		for (const handle of this.waiters) {
			if (!handle.isSettled) {
				handle.wake();
				count++;
			}
		}
		this.waiters.length = 0;
		return count;
	}

	/** Number of pending (unsettled) waiters. */
	get pending(): number {
		// Compact settled handles while counting
		let count = 0;
		for (const handle of this.waiters) {
			if (!handle.isSettled) count++;
		}
		return count;
	}

	/** Remove all waiters without waking them. */
	clear(): void {
		this.waiters.length = 0;
	}
}
