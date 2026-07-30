import { describe, expect, it } from "vitest";
import {
	ANY_BUFFERED_EVENT_KEY,
	SidecarEventBuffer,
	SidecarEventBufferOverflow,
	fromGeneratedEventPayload,
	normalizeSidecarEventMatcher,
	sidecarEventBufferKeys,
	sidecarEventWaitAbortError,
	sidecarSelectorBufferKey,
	sidecarSelectorMatchesEvent,
	type LiveSidecarEventFrame,
} from "../src/event-buffer.js";
import * as protocol from "../src/generated-protocol.js";

const vmReadyEvent: LiveSidecarEventFrame = {
	ownership: {
		scope: "vm",
		connection_id: "conn",
		session_id: "session",
		vm_id: "vm",
	},
	payload: {
		type: "vm_lifecycle",
		state: "ready",
	},
};

describe("event buffer helpers", () => {
	it("reports bounded event buffer overflows", () => {
		const error = new SidecarEventBufferOverflow({
			capacity: 4,
			bufferedEvents: 4,
			eventType: "process_output",
		});

		expect(error).toBeInstanceOf(Error);
		expect(error.name).toBe("SidecarEventBufferOverflow");
		expect(error.capacity).toBe(4);
		expect(error.bufferedEvents).toBe(4);
		expect(error.eventType).toBe("process_output");
		expect(error.message).toContain(
			"sidecar event buffer overflow after 4 queued events",
		);
	});

	it("buffers, takes, and bounds unmatched events", () => {
		const buffer = new SidecarEventBuffer<LiveSidecarEventFrame>(1);
		expect(buffer.buffer(vmReadyEvent)).toBe(null);
		expect(buffer.size).toBe(1);

		const readyMatcher =
			normalizeSidecarEventMatcher<LiveSidecarEventFrame>({
				type: "vm_lifecycle",
				state: "ready",
			});
		expect(buffer.take(readyMatcher)).toBe(vmReadyEvent);
		expect(buffer.take(readyMatcher)).toBe(null);
		expect(buffer.size).toBe(0);

		expect(buffer.buffer(vmReadyEvent)).toBe(null);
		const overflow = buffer.buffer({
			...vmReadyEvent,
			payload: { type: "vm_lifecycle", state: "failed" },
		});
		expect(overflow).toBeInstanceOf(SidecarEventBufferOverflow);
		expect(overflow).toMatchObject({
			capacity: 1,
			bufferedEvents: 1,
			eventType: "vm_lifecycle",
		});
	});

	it("normalizes event wait abort reasons", () => {
		const error = new Error("stop");
		expect(sidecarEventWaitAbortError(error)).toBe(error);
		expect(sidecarEventWaitAbortError("stop").message).toBe("stop");
		expect(sidecarEventWaitAbortError(undefined).message).toBe(
			"sidecar event wait aborted",
		);
	});

	it("matches event selectors by ownership and payload fields", () => {
		expect(
			sidecarSelectorMatchesEvent(
				{
					type: "vm_lifecycle",
					ownership: vmReadyEvent.ownership,
					state: "ready",
				},
				vmReadyEvent,
			),
		).toBe(true);
		expect(
			sidecarSelectorMatchesEvent(
				{
					type: "vm_lifecycle",
					ownership: vmReadyEvent.ownership,
					state: "failed",
				},
				vmReadyEvent,
			),
		).toBe(false);
	});

	it("uses exact keys for direct buffered-event lookup", () => {
		expect(
			sidecarSelectorBufferKey({
				type: "vm_lifecycle",
				ownership: vmReadyEvent.ownership,
				state: "ready",
			}),
		).toBe("type:vm_lifecycle|ownership:vm:conn:session:vm|state:ready");
		expect(sidecarSelectorBufferKey({ any: true })).toBe(
			ANY_BUFFERED_EVENT_KEY,
		);
	});

	it("declines direct lookup when structured detail filtering requires scan", () => {
		expect(
			sidecarSelectorBufferKey({
				type: "structured",
				name: "tool",
				detail: { ok: "true" },
			}),
		).toBe(null);
	});

	it("indexes process output by type, ownership, process, and channel", () => {
		const event: LiveSidecarEventFrame = {
			ownership: vmReadyEvent.ownership,
			payload: {
				type: "process_output",
				process_id: "proc",
				channel: "stderr",
				chunk: new Uint8Array([1, 2, 3]),
			},
		};
		expect(sidecarEventBufferKeys(event)).toEqual([
			"*",
			"type:process_output",
			"type:process_output|ownership:vm:conn:session:vm",
			"type:process_output|process:proc",
			"type:process_output|channel:stderr",
			"type:process_output|process:proc|channel:stderr",
			"type:process_output|ownership:vm:conn:session:vm|process:proc",
			"type:process_output|ownership:vm:conn:session:vm|channel:stderr",
			"type:process_output|ownership:vm:conn:session:vm|process:proc|channel:stderr",
		]);
	});

	it("normalizes selector and function matchers", () => {
		const selectorMatcher = normalizeSidecarEventMatcher<LiveSidecarEventFrame>({
			type: "vm_lifecycle",
			state: "ready",
		});
		expect(selectorMatcher.matches(vmReadyEvent)).toBe(true);
		expect(selectorMatcher.bufferKey).toBe("type:vm_lifecycle|state:ready");

		const functionMatcher =
			normalizeSidecarEventMatcher<LiveSidecarEventFrame>(
				(event) => event.payload.type === "vm_lifecycle",
			);
		expect(functionMatcher.matches(vmReadyEvent)).toBe(true);
		expect(functionMatcher.bufferKey).toBe(null);
	});

	it("maps generated event payloads to live payloads", () => {
		expect(
			fromGeneratedEventPayload({
				tag: "VmLifecycleEvent",
				val: { state: protocol.VmLifecycleState.Ready },
			}),
		).toEqual({
			type: "vm_lifecycle",
			state: "ready",
		});

		expect(
			fromGeneratedEventPayload({
				tag: "ProcessOutputEvent",
				val: {
					processId: "proc",
					channel: protocol.StreamChannel.Stdout,
					chunk: new Uint8Array([1, 2, 3]).buffer,
				},
			}),
		).toMatchObject({
			type: "process_output",
			process_id: "proc",
			channel: "stdout",
			chunk: Buffer.from([1, 2, 3]),
		});

		expect(
			fromGeneratedEventPayload({
				tag: "StructuredEvent",
				val: {
					name: "tool",
					detail: new Map([["ok", "true"]]),
				},
			}),
		).toEqual({
			type: "structured",
			name: "tool",
			detail: { ok: "true" },
		});
	});
});
