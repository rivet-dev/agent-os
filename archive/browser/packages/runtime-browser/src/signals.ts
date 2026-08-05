export const PROCESS_SIGNAL_NUMBERS: Record<string, number> = {
	SIGHUP: 1,
	SIGINT: 2,
	SIGQUIT: 3,
	SIGILL: 4,
	SIGTRAP: 5,
	SIGABRT: 6,
	SIGIOT: 6,
	SIGBUS: 7,
	SIGFPE: 8,
	SIGKILL: 9,
	SIGUSR1: 10,
	SIGSEGV: 11,
	SIGUSR2: 12,
	SIGPIPE: 13,
	SIGALRM: 14,
	SIGTERM: 15,
	SIGSTKFLT: 16,
	SIGCHLD: 17,
	SIGCONT: 18,
	SIGSTOP: 19,
	SIGTSTP: 20,
	SIGTTIN: 21,
	SIGTTOU: 22,
	SIGURG: 23,
	SIGXCPU: 24,
	SIGXFSZ: 25,
	SIGVTALRM: 26,
	SIGPROF: 27,
	SIGWINCH: 28,
	SIGIO: 29,
	SIGPOLL: 29,
	SIGPWR: 30,
	SIGSYS: 31,
};

export type BrowserSignalRegistration = {
	action: "default" | "ignore" | "user";
	mask: number[];
	flags: number;
};

const VALID_PROCESS_SIGNALS = new Set([0, ...Object.values(PROCESS_SIGNAL_NUMBERS)]);

export function signalNumberForEvent(event: string): number | null {
	const upper = event.trim().toUpperCase();
	const signalName = upper.startsWith("SIG") ? upper : `SIG${upper}`;
	return PROCESS_SIGNAL_NUMBERS[signalName] ?? null;
}

export function defaultSignalExitCode(signal: number): number | null {
	return signal > 0 ? 128 + signal : null;
}

export function parseProcessSignalStateArgs(args: unknown[]): {
	signal: number;
	registration: BrowserSignalRegistration;
} {
	const signal = parseSignalNumber(args[0], "process.signal_state signal");
	const action = String(args[1] ?? "default").toLowerCase();
	if (action !== "default" && action !== "ignore" && action !== "user") {
		throw new Error(`unsupported process.signal_state action ${action}`);
	}
	const maskValue = typeof args[2] === "string" ? JSON.parse(args[2]) : args[2];
	if (!Array.isArray(maskValue)) {
		throw new Error("process.signal_state mask must be an array");
	}
	const mask = maskValue.map((value) =>
		parseSignalNumber(value, "process.signal_state mask entries"),
	);
	const flags = Number(args[3] ?? 0);
	if (!Number.isInteger(flags) || flags < 0) {
		throw new Error("process.signal_state flags must be a non-negative integer");
	}
	return {
		signal,
		registration: { action, mask, flags },
	};
}

export function applyProcessSignalStateUpdate(
	states: Map<string, Map<number, BrowserSignalRegistration>>,
	executionId: string,
	signal: number,
	registration: BrowserSignalRegistration,
): void {
	if (
		registration.action === "default" &&
		registration.mask.length === 0 &&
		registration.flags === 0
	) {
		const handlers = states.get(executionId);
		handlers?.delete(signal);
		if (handlers?.size === 0) {
			states.delete(executionId);
		}
		return;
	}
	let handlers = states.get(executionId);
	if (!handlers) {
		handlers = new Map();
		states.set(executionId, handlers);
	}
	handlers.set(signal, registration);
}

function parseSignalNumber(value: unknown, label: string): number {
	const signal = Number(value);
	if (!Number.isInteger(signal) || !VALID_PROCESS_SIGNALS.has(signal)) {
		throw new Error(`${label} must be a valid POSIX signal`);
	}
	return signal;
}
