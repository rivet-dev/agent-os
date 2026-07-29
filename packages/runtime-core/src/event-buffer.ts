import { fromGeneratedExtEnvelope, type LiveExtEnvelope } from "./ext.js";
import type * as protocol from "./generated-protocol.js";
import {
	ownershipMatchesSelector,
	ownershipSelectorKey,
	type LiveOwnershipScope,
} from "./ownership.js";
import {
	fromGeneratedStreamChannel,
	fromGeneratedVmLifecycleState,
} from "./protocol-maps.js";

export const ANY_BUFFERED_EVENT_KEY = "*";

export type { LiveOwnershipScope } from "./ownership.js";

export type LiveSidecarEventPayload =
	| {
			type: "vm_lifecycle";
			state: "creating" | "ready" | "disposing" | "disposed" | "failed";
	  }
	| {
			type: "process_output";
			process_id: string;
			channel: "stdout" | "stderr";
			chunk: Uint8Array;
	  }
	| {
			type: "process_exited";
			process_id: string;
			exit_code: number;
	  }
	| {
			type: "structured";
			name: string;
			detail: Record<string, string>;
	  }
	| { type: "execution_output"; event: protocol.ExecutionOutputEvent }
	| { type: "execution_completed"; event: protocol.ExecutionCompletedEvent }
	| {
			type: "ext";
			envelope: LiveExtEnvelope;
	  };

export interface LiveSidecarEventFrame {
	ownership: LiveOwnershipScope;
	payload: LiveSidecarEventPayload;
}

export type LiveSidecarEventSelector =
	| {
			any: true;
	  }
	| {
			type: "vm_lifecycle";
			ownership?: LiveOwnershipScope;
			state?: Extract<
				LiveSidecarEventPayload,
				{ type: "vm_lifecycle" }
			>["state"];
	  }
	| {
			type: "process_output";
			ownership?: LiveOwnershipScope;
			processId?: string;
			channel?: Extract<
				LiveSidecarEventPayload,
				{ type: "process_output" }
			>["channel"];
	  }
	| {
			type: "process_exited";
			ownership?: LiveOwnershipScope;
			processId?: string;
	  }
	| {
			type: "structured";
			ownership?: LiveOwnershipScope;
			name?: string;
			detail?: Record<string, string>;
	  }
	| {
			type: "execution_output";
			ownership?: LiveOwnershipScope;
			executionId?: string;
	  }
	| {
			type: "execution_completed";
			ownership?: LiveOwnershipScope;
			executionId?: string;
	  };

export type LiveSidecarBufferedEventRecord<
	TEvent extends LiveSidecarEventFrame,
> = {
	event: TEvent;
	keys: readonly string[];
};

export type LiveSidecarEventWaitMatcher<TEvent extends LiveSidecarEventFrame> =
	{
		matches: (event: TEvent) => boolean;
		bufferKey: string | null;
	};

export class SidecarEventBufferOverflow extends Error {
	readonly capacity: number;
	readonly bufferedEvents: number;
	readonly eventType: LiveSidecarEventPayload["type"];

	constructor(options: {
		capacity: number;
		bufferedEvents: number;
		eventType: LiveSidecarEventPayload["type"];
	}) {
		super(
			`sidecar event buffer overflow after ${options.bufferedEvents} queued events (capacity ${options.capacity}) while buffering ${options.eventType}`,
		);
		this.name = "SidecarEventBufferOverflow";
		this.capacity = options.capacity;
		this.bufferedEvents = options.bufferedEvents;
		this.eventType = options.eventType;
	}
}

export class SidecarEventBuffer<TEvent extends LiveSidecarEventFrame> {
	private readonly bufferedEvents = new Map<
		number,
		LiveSidecarBufferedEventRecord<TEvent>
	>();
	private readonly bufferedEventQueues = new Map<string, Set<number>>();
	private nextBufferedEventId = 1;

	constructor(private readonly capacity: number) {}

	get size(): number {
		return this.bufferedEvents.size;
	}

	buffer(event: TEvent): SidecarEventBufferOverflow | null {
		if (this.bufferedEvents.size >= this.capacity) {
			return new SidecarEventBufferOverflow({
				capacity: this.capacity,
				bufferedEvents: this.bufferedEvents.size,
				eventType: event.payload.type,
			});
		}
		const eventId = this.nextBufferedEventId++;
		const keys = sidecarEventBufferKeys(event);
		this.bufferedEvents.set(eventId, {
			event,
			keys,
		});
		for (const key of keys) {
			const queue = this.bufferedEventQueues.get(key);
			if (queue) {
				queue.add(eventId);
				continue;
			}
			this.bufferedEventQueues.set(key, new Set([eventId]));
		}
		return null;
	}

	take(matcher: LiveSidecarEventWaitMatcher<TEvent>): TEvent | null {
		if (matcher.bufferKey !== null) {
			return this.takeFromKey(matcher.bufferKey);
		}
		const queue = this.bufferedEventQueues.get(ANY_BUFFERED_EVENT_KEY);
		if (!queue) {
			return null;
		}
		for (const eventId of queue) {
			const record = this.bufferedEvents.get(eventId);
			if (!record) {
				continue;
			}
			if (!matcher.matches(record.event)) {
				continue;
			}
			return this.remove(eventId);
		}
		return null;
	}

	private takeFromKey(key: string): TEvent | null {
		const queue = this.bufferedEventQueues.get(key);
		if (!queue) {
			return null;
		}
		for (const eventId of queue) {
			const record = this.bufferedEvents.get(eventId);
			if (!record) {
				queue.delete(eventId);
				continue;
			}
			return this.remove(eventId);
		}
		return null;
	}

	private remove(eventId: number): TEvent | null {
		const record = this.bufferedEvents.get(eventId);
		if (!record) {
			return null;
		}
		this.bufferedEvents.delete(eventId);
		for (const key of record.keys) {
			const queue = this.bufferedEventQueues.get(key);
			if (!queue) {
				continue;
			}
			queue.delete(eventId);
			if (queue.size === 0) {
				this.bufferedEventQueues.delete(key);
			}
		}
		return record.event;
	}
}

function buildBufferKey(
	type: LiveSidecarEventPayload["type"],
	options?: {
		ownership?: LiveOwnershipScope;
		state?: string;
		processId?: string;
		executionId?: string;
		channel?: string;
		name?: string;
	},
): string {
	const parts = [`type:${type}`];
	if (options?.ownership) {
		parts.push(`ownership:${ownershipSelectorKey(options.ownership)}`);
	}
	if (options?.state) {
		parts.push(`state:${options.state}`);
	}
	if (options?.processId) {
		parts.push(`process:${options.processId}`);
	}
	if (options?.executionId) {
		parts.push(`execution:${options.executionId}`);
	}
	if (options?.channel) {
		parts.push(`channel:${options.channel}`);
	}
	if (options?.name) {
		parts.push(`name:${options.name}`);
	}
	return parts.join("|");
}

export function sidecarSelectorMatchesEvent<
	TEvent extends LiveSidecarEventFrame,
>(selector: LiveSidecarEventSelector, event: TEvent): boolean {
	if ("any" in selector) {
		return true;
	}
	if (event.payload.type !== selector.type) {
		return false;
	}
	if (!ownershipMatchesSelector(selector.ownership, event.ownership)) {
		return false;
	}
	switch (selector.type) {
		case "vm_lifecycle": {
			const payload = event.payload as Extract<
				LiveSidecarEventPayload,
				{ type: "vm_lifecycle" }
			>;
			return selector.state === undefined || payload.state === selector.state;
		}
		case "process_output": {
			const payload = event.payload as Extract<
				LiveSidecarEventPayload,
				{ type: "process_output" }
			>;
			return (
				(selector.processId === undefined ||
					payload.process_id === selector.processId) &&
				(selector.channel === undefined || payload.channel === selector.channel)
			);
		}
		case "process_exited": {
			const payload = event.payload as Extract<
				LiveSidecarEventPayload,
				{ type: "process_exited" }
			>;
			return (
				selector.processId === undefined ||
				payload.process_id === selector.processId
			);
		}
		case "structured": {
			const payload = event.payload as Extract<
				LiveSidecarEventPayload,
				{ type: "structured" }
			>;
			if (selector.name !== undefined && payload.name !== selector.name) {
				return false;
			}
			if (!selector.detail) {
				return true;
			}
			for (const [key, value] of Object.entries(selector.detail)) {
				if (payload.detail[key] !== value) {
					return false;
				}
			}
			return true;
		}
		case "execution_output":
		case "execution_completed": {
			const payload = event.payload as Extract<
				LiveSidecarEventPayload,
				{ type: "execution_output" | "execution_completed" }
			>;
			return (
				selector.executionId === undefined ||
				payload.event.executionId === selector.executionId
			);
		}
	}
}

export function sidecarSelectorBufferKey(
	selector: LiveSidecarEventSelector,
): string | null {
	if ("any" in selector) {
		return ANY_BUFFERED_EVENT_KEY;
	}
	switch (selector.type) {
		case "vm_lifecycle":
			return buildBufferKey(selector.type, {
				ownership: selector.ownership,
				state: selector.state,
			});
		case "process_output":
			return buildBufferKey(selector.type, {
				ownership: selector.ownership,
				processId: selector.processId,
				channel: selector.channel,
			});
		case "process_exited":
			return buildBufferKey(selector.type, {
				ownership: selector.ownership,
				processId: selector.processId,
			});
		case "structured":
			if (selector.detail) {
				return null;
			}
			return buildBufferKey(selector.type, {
				ownership: selector.ownership,
				name: selector.name,
			});
		case "execution_output":
		case "execution_completed":
			return buildBufferKey(selector.type, {
				ownership: selector.ownership,
				executionId: selector.executionId,
			});
	}
}

export function normalizeSidecarEventMatcher<
	TEvent extends LiveSidecarEventFrame,
>(
	selector: LiveSidecarEventSelector | ((event: TEvent) => boolean),
): LiveSidecarEventWaitMatcher<TEvent> {
	if (typeof selector === "function") {
		return {
			matches: selector,
			bufferKey: null,
		};
	}
	return {
		matches: (event) => sidecarSelectorMatchesEvent(selector, event),
		bufferKey: sidecarSelectorBufferKey(selector),
	};
}

export function fromGeneratedEventPayload(
	payload: protocol.EventPayload,
): LiveSidecarEventPayload {
	switch (payload.tag) {
		case "VmLifecycleEvent":
			return {
				type: "vm_lifecycle",
				state: fromGeneratedVmLifecycleState(payload.val.state),
			};
		case "ProcessOutputEvent":
			return {
				type: "process_output",
				process_id: payload.val.processId,
				channel: fromGeneratedStreamChannel(payload.val.channel),
				chunk: Buffer.from(payload.val.chunk),
			};
		case "ProcessExitedEvent":
			return {
				type: "process_exited",
				process_id: payload.val.processId,
				exit_code: payload.val.exitCode,
			};
		case "StructuredEvent":
			return {
				type: "structured",
				name: payload.val.name,
				detail: Object.fromEntries(payload.val.detail),
			};
		case "ExtEnvelope":
			return {
				type: "ext",
				envelope: fromGeneratedExtEnvelope(payload.val),
			};
		case "ExecutionOutputEvent":
			return { type: "execution_output", event: payload.val };
		case "ExecutionCompletedEvent":
			return { type: "execution_completed", event: payload.val };
	}
}

export function sidecarEventWaitAbortError(reason: unknown): Error {
	return reason instanceof Error
		? reason
		: new Error(reason ? String(reason) : "sidecar event wait aborted");
}

export function sidecarEventBufferKeys<TEvent extends LiveSidecarEventFrame>(
	event: TEvent,
): string[] {
	const owner = event.ownership;
	const keys = new Set<string>([
		ANY_BUFFERED_EVENT_KEY,
		buildBufferKey(event.payload.type),
		buildBufferKey(event.payload.type, { ownership: owner }),
	]);
	switch (event.payload.type) {
		case "vm_lifecycle":
			keys.add(
				buildBufferKey(event.payload.type, {
					state: event.payload.state,
				}),
			);
			keys.add(
				buildBufferKey(event.payload.type, {
					ownership: owner,
					state: event.payload.state,
				}),
			);
			break;
		case "process_output":
			keys.add(
				buildBufferKey(event.payload.type, {
					processId: event.payload.process_id,
				}),
			);
			keys.add(
				buildBufferKey(event.payload.type, {
					channel: event.payload.channel,
				}),
			);
			keys.add(
				buildBufferKey(event.payload.type, {
					processId: event.payload.process_id,
					channel: event.payload.channel,
				}),
			);
			keys.add(
				buildBufferKey(event.payload.type, {
					ownership: owner,
					processId: event.payload.process_id,
				}),
			);
			keys.add(
				buildBufferKey(event.payload.type, {
					ownership: owner,
					channel: event.payload.channel,
				}),
			);
			keys.add(
				buildBufferKey(event.payload.type, {
					ownership: owner,
					processId: event.payload.process_id,
					channel: event.payload.channel,
				}),
			);
			break;
		case "process_exited":
			keys.add(
				buildBufferKey(event.payload.type, {
					processId: event.payload.process_id,
				}),
			);
			keys.add(
				buildBufferKey(event.payload.type, {
					ownership: owner,
					processId: event.payload.process_id,
				}),
			);
			break;
		case "structured":
			keys.add(
				buildBufferKey(event.payload.type, {
					name: event.payload.name,
				}),
			);
			keys.add(
				buildBufferKey(event.payload.type, {
					ownership: owner,
					name: event.payload.name,
				}),
			);
			break;
		case "execution_output":
		case "execution_completed":
			keys.add(
				buildBufferKey(event.payload.type, {
					executionId: event.payload.event.executionId,
				}),
			);
			keys.add(
				buildBufferKey(event.payload.type, {
					ownership: owner,
					executionId: event.payload.event.executionId,
				}),
			);
			break;
		case "ext":
			break;
	}
	return [...keys];
}
