/**
 * Kernel timer table with per-process ownership and budget enforcement.
 *
 * Tracks active timers (setTimeout/setInterval) per-process. Actual
 * scheduling is delegated to the host via callbacks — the kernel only
 * manages ownership, limits, and cleanup.
 */

import { KernelError } from "./types.js";

export interface KernelTimer {
	readonly id: number;
	readonly pid: number;
	readonly delayMs: number;
	readonly repeat: boolean;
	/** Host-side handle returned by the scheduling function (for cancellation). */
	hostHandle: ReturnType<typeof setTimeout> | number | undefined;
	/** User callback to invoke when the timer fires. */
	callback: () => void;
	/** True once the timer has been cleared. */
	cleared: boolean;
}

export interface TimerTableOptions {
	/** Default per-process timer limit. 0 = unlimited. */
	defaultMaxTimers?: number;
}

export class TimerTable {
	private timers: Map<number, KernelTimer> = new Map();
	private nextTimerId = 1;
	private defaultMaxTimers: number;
	/** Per-process limit overrides. */
	private processLimits: Map<number, number> = new Map();

	constructor(options?: TimerTableOptions) {
		this.defaultMaxTimers = options?.defaultMaxTimers ?? 0;
	}

	/**
	 * Create a timer owned by `pid`.
	 * Returns the kernel timer ID. The caller must schedule the actual
	 * timeout on the host and set `timer.hostHandle`.
	 */
	createTimer(
		pid: number,
		delayMs: number,
		repeat: boolean,
		callback: () => void,
	): number {
		// Enforce per-process limit
		const limit = this.getLimit(pid);
		if (limit > 0) {
			const count = this.countForProcess(pid);
			if (count >= limit) {
				throw new KernelError("EAGAIN", "timer limit exceeded");
			}
		}

		const id = this.nextTimerId++;
		const timer: KernelTimer = {
			id,
			pid,
			delayMs,
			repeat,
			hostHandle: undefined,
			callback,
			cleared: false,
		};
		this.timers.set(id, timer);
		return id;
	}

	/** Get a timer by ID. Returns null if not found. */
	get(timerId: number): KernelTimer | null {
		return this.timers.get(timerId) ?? null;
	}

	/** Clear (cancel) a timer. The caller should also cancel the host-side handle. */
	clearTimer(timerId: number, pid?: number): void {
		const timer = this.timers.get(timerId);
		if (!timer) return; // Clearing a non-existent timer is a no-op (matches POSIX)

		// Cross-process isolation: if pid is provided, only the owning process can clear
		if (pid !== undefined && timer.pid !== pid) {
			throw new KernelError("EACCES", `timer ${timerId} not owned by pid ${pid}`);
		}

		timer.cleared = true;
		this.timers.delete(timerId);
	}

	/** Set per-process timer limit. */
	setLimit(pid: number, maxTimers: number): void {
		this.processLimits.set(pid, maxTimers);
	}

	/** Get the active timer count for a process. */
	countForProcess(pid: number): number {
		let count = 0;
		for (const timer of this.timers.values()) {
			if (timer.pid === pid) count++;
		}
		return count;
	}

	/** Get all active timers for a process. */
	getActiveTimers(pid: number): KernelTimer[] {
		const result: KernelTimer[] = [];
		for (const timer of this.timers.values()) {
			if (timer.pid === pid) result.push(timer);
		}
		return result;
	}

	/** Clear all timers owned by a process. Called on process exit. */
	clearAllForProcess(pid: number): void {
		for (const [id, timer] of this.timers) {
			if (timer.pid === pid) {
				timer.cleared = true;
				this.timers.delete(id);
			}
		}
		this.processLimits.delete(pid);
	}

	/** Dispose all timers. Called on kernel shutdown. */
	disposeAll(): void {
		for (const timer of this.timers.values()) {
			timer.cleared = true;
		}
		this.timers.clear();
		this.processLimits.clear();
	}

	/** Number of active timers across all processes. */
	get size(): number {
		return this.timers.size;
	}

	private getLimit(pid: number): number {
		return this.processLimits.get(pid) ?? this.defaultMaxTimers;
	}
}
