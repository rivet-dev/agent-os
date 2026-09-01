import { describe, expect, test } from "vitest";
import { AgentOs } from "../src/index.js";

// odw-0a6. Every AgentOs created from one sidecar handle listens on the SAME
// native process, and execution ids are only unique inside a VM (they used to
// be minted from a per-VM counter, so K VMs fanning out minted the identical id
// in the same millisecond). Dispatching on the id alone therefore delivered
// VM B's stdout and exit into VM A. `_handleSidecarEvent` must drop foreign
// vm-scoped frames BEFORE the mappers run, since the mappers discard
// `ownership` entirely.
//
// Built on the prototype rather than `AgentOs.create()`: the guard is pure
// routing over the instance's own maps, and a real VM would drag in the whole
// guest-software toolchain for nothing.
const OWN_VM_ID = "vm-A";
const FOREIGN_VM_ID = "vm-B";
const COLLIDING_EXECUTION_ID = "operation-1-1";

function ownership(vmId: string) {
	return {
		scope: "vm" as const,
		connection_id: "conn-1",
		session_id: "session-1",
		vm_id: vmId,
	};
}

function outputFrame(vmId: string) {
	return {
		ownership: ownership(vmId),
		payload: {
			type: "execution_output" as const,
			event: {
				executionId: COLLIDING_EXECUTION_ID,
				generation: 1n,
				processId: null,
				sequence: 0n,
				channel: "Stdout",
				chunk: new TextEncoder().encode("secret").buffer,
				timestampMs: 0n,
			},
		},
	};
}

function completedFrame(vmId: string) {
	return {
		ownership: ownership(vmId),
		payload: {
			type: "execution_completed" as const,
			event: {
				executionId: COLLIDING_EXECUTION_ID,
				generation: 1n,
				outcome: "Succeeded",
				exitCode: 0,
				error: null,
			},
		},
	};
}

interface EventRouterProbe {
	_sidecarVm: { vmId: string };
	_languageProcessIds: Map<string, number>;
	_languageProcesses: Map<number, unknown>;
	_executionOutputHandlers: Map<string, Set<(event: unknown) => void>>;
	_executionCompletedHandlers: Map<string, Set<(event: unknown) => void>>;
	_handleSidecarEvent(event: unknown): void;
}

function eventRouter(): {
	probe: EventRouterProbe;
	output: unknown[];
	completed: unknown[];
} {
	const probe = Object.create(AgentOs.prototype) as EventRouterProbe;
	probe._sidecarVm = { vmId: OWN_VM_ID };
	probe._languageProcessIds = new Map();
	probe._languageProcesses = new Map();
	const output: unknown[] = [];
	const completed: unknown[] = [];
	probe._executionOutputHandlers = new Map([
		["*", new Set([(event: unknown) => void output.push(event)])],
	]);
	probe._executionCompletedHandlers = new Map([
		["*", new Set([(event: unknown) => void completed.push(event)])],
	]);
	return { probe, output, completed };
}

describe("cross-VM execution event isolation", () => {
	test("an execution event owned by another VM is never dispatched here", () => {
		const { probe, output, completed } = eventRouter();

		probe._handleSidecarEvent(outputFrame(FOREIGN_VM_ID));
		probe._handleSidecarEvent(completedFrame(FOREIGN_VM_ID));

		expect(output).toEqual([]);
		expect(completed).toEqual([]);
	});

	test("the same execution id owned by this VM is still dispatched", () => {
		const { probe, output, completed } = eventRouter();

		probe._handleSidecarEvent(outputFrame(OWN_VM_ID));
		probe._handleSidecarEvent(completedFrame(OWN_VM_ID));

		expect(output).toMatchObject([
			{ executionId: COLLIDING_EXECUTION_ID, channel: "stdout" },
		]);
		expect(completed).toMatchObject([
			{ executionId: COLLIDING_EXECUTION_ID, outcome: "succeeded" },
		]);
	});
});
