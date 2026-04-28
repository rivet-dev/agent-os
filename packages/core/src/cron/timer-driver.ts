import type { LongTimeout } from "long-timeout";
import {
	clearTimeout as clearLongTimeout,
	setTimeout as longSetTimeout,
} from "long-timeout";
import {
	resolveSchedule,
	validateScheduleForRegistration,
} from "./parse-schedule.js";
import type {
	ScheduleDriver,
	ScheduleEntry,
	ScheduleHandle,
} from "./schedule-driver.js";

/**
 * Default ScheduleDriver that uses in-process timers. For cron expressions
 * it parses via croner and sets a single timeout for the next fire time,
 * rescheduling after each fire. For ISO 8601 one-shot timestamps it fires
 * once and removes the entry.
 *
 * Uses long-timeout to support delays exceeding setTimeout's 2^31ms limit.
 */
export class TimerScheduleDriver implements ScheduleDriver {
	private timers = new Map<string, LongTimeout>();
	private entries = new Map<string, ScheduleEntry>();

	schedule(entry: ScheduleEntry): ScheduleHandle {
		const resolved = validateScheduleForRegistration(entry.schedule);
		this.entries.set(entry.id, entry);
		this.scheduleNext(entry, resolved);
		return { id: entry.id };
	}

	cancel(handle: ScheduleHandle): void {
		const timer = this.timers.get(handle.id);
		if (timer) {
			clearLongTimeout(timer);
			this.timers.delete(handle.id);
		}
		this.entries.delete(handle.id);
	}

	dispose(): void {
		for (const timer of this.timers.values()) {
			clearLongTimeout(timer);
		}
		this.timers.clear();
		this.entries.clear();
	}

	private scheduleNext(
		entry: ScheduleEntry,
		resolved = resolveSchedule(entry.schedule),
	): void {
		const { parsed, nextRun: next } = resolved;
		const isCron = parsed.kind === "cron";

		if (!next) {
			this.timers.delete(entry.id);
			this.entries.delete(entry.id);
			return;
		}

		const delay = Math.max(0, next.getTime() - Date.now());

		const timer = longSetTimeout(async () => {
			this.timers.delete(entry.id);
			try {
				await entry.callback();
			} catch {
				// The driver is fire-and-forget; error handling is the caller's responsibility.
			}
			if (isCron && this.entries.has(entry.id)) {
				this.scheduleNext(entry);
			} else {
				this.entries.delete(entry.id);
			}
		}, delay);

		this.timers.set(entry.id, timer);
	}
}
