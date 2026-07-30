import { describe, expect, it } from "vitest";
import {
	applyProcessSignalStateUpdate,
	defaultSignalExitCode,
	parseProcessSignalStateArgs,
	signalNumberForEvent,
	type BrowserSignalRegistration,
} from "../../src/signals.js";

describe("browser signal helpers", () => {
	it("normalizes signal names and default exit codes", () => {
		expect(signalNumberForEvent("SIGTERM")).toBe(15);
		expect(signalNumberForEvent("term")).toBe(15);
		expect(signalNumberForEvent("SIGPOLL")).toBe(29);
		expect(signalNumberForEvent("NOPE")).toBeNull();
		expect(defaultSignalExitCode(15)).toBe(143);
		expect(defaultSignalExitCode(0)).toBeNull();
	});

	it("parses and applies process signal state registrations", () => {
		const { signal, registration } = parseProcessSignalStateArgs([
			15,
			"user",
			"[2,9]",
			1,
		]);
		expect(signal).toBe(15);
		expect(registration).toEqual({ action: "user", mask: [2, 9], flags: 1 });

		const states = new Map<string, Map<number, BrowserSignalRegistration>>();
		applyProcessSignalStateUpdate(states, "exec-1", signal, registration);
		expect(states.get("exec-1")?.get(15)).toEqual(registration);

		applyProcessSignalStateUpdate(states, "exec-1", 15, {
			action: "default",
			mask: [],
			flags: 0,
		});
		expect(states.has("exec-1")).toBe(false);
	});

	it("rejects unknown process signal state values", () => {
		expect(() => parseProcessSignalStateArgs([32, "user", "[]", 0])).toThrow(
			"process.signal_state signal must be a valid POSIX signal",
		);
		expect(() =>
			parseProcessSignalStateArgs([15, "user", "[32]", 0]),
		).toThrow("process.signal_state mask entries must be a valid POSIX signal");
	});
});
