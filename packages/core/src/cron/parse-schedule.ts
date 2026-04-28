import { Cron } from "croner";

export type ParsedSchedule =
	| {
			kind: "date";
			date: Date;
	  }
	| {
			kind: "cron";
			cron: Cron;
	  };

const ONE_SHOT_SCHEDULE_PATTERN =
	/^\d{4}-\d{2}-\d{2}(?:[T ]\d{2}:\d{2}(?::\d{2}(?:\.\d{1,3})?)?(?:Z|[+-]\d{2}:\d{2})?)?$/;

export class InvalidScheduleError extends Error {
	readonly schedule: string;

	constructor(schedule: string) {
		super(
			`Invalid schedule "${schedule}". Expected a cron expression or an ISO-like one-shot timestamp.`,
		);
		this.name = "InvalidScheduleError";
		this.schedule = schedule;
	}
}

export class PastScheduleError extends Error {
	readonly schedule: string;

	constructor(schedule: string) {
		super(`One-shot schedule "${schedule}" is already in the past.`);
		this.name = "PastScheduleError";
		this.schedule = schedule;
	}
}

function looksLikeOneShotSchedule(schedule: string): boolean {
	return ONE_SHOT_SCHEDULE_PATTERN.test(schedule);
}

export function parseSchedule(schedule: string): ParsedSchedule {
	const normalizedSchedule = schedule.trim();
	if (looksLikeOneShotSchedule(normalizedSchedule)) {
		const parsedTime = Date.parse(normalizedSchedule);
		if (!Number.isFinite(parsedTime)) {
			throw new InvalidScheduleError(schedule);
		}

		return {
			kind: "date",
			date: new Date(parsedTime),
		};
	}

	try {
		return {
			kind: "cron",
			cron: new Cron(normalizedSchedule),
		};
	} catch {
		throw new InvalidScheduleError(schedule);
	}
}

export function resolveSchedule(
	schedule: string,
	now: Date = new Date(),
): {
	parsed: ParsedSchedule;
	nextRun?: Date;
} {
	const parsed = parseSchedule(schedule);
	const nextRun =
		parsed.kind === "cron"
			? (parsed.cron.nextRun() ?? undefined)
			: parsed.date.getTime() > now.getTime()
				? parsed.date
				: undefined;

	return { parsed, nextRun };
}

export function validateScheduleForRegistration(
	schedule: string,
	now: Date = new Date(),
): {
	parsed: ParsedSchedule;
	nextRun?: Date;
} {
	const resolved = resolveSchedule(schedule, now);
	if (resolved.parsed.kind === "date" && !resolved.nextRun) {
		throw new PastScheduleError(schedule);
	}

	return resolved;
}
