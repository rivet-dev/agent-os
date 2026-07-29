const MAX_EVENT_TYPES = 256;
const MAX_EVENT_LISTENERS = 1024;
const eventState = new WeakMap();
const customEventDetail = new WeakMap();
const targetState = new WeakMap();

function toDomString(value) {
	return `${value}`;
}

function withCode(error, code) {
	error.code = code;
	return error;
}

function normalizeAddEventListenerOptions(options) {
	if (typeof options === "boolean") {
		return { capture: options, once: false, passive: false };
	}
	if (options == null) {
		return { capture: false, once: false, passive: false };
	}
	const normalized = Object(options);
	return {
		capture: Boolean(normalized.capture),
		once: Boolean(normalized.once),
		passive: Boolean(normalized.passive),
		signal: normalized.signal,
	};
}

function normalizeRemoveEventListenerOptions(options) {
	if (typeof options === "boolean") return options;
	if (options == null) return false;
	return Boolean(Object(options).capture);
}

function isAbortSignalLike(value) {
	return (
		typeof value === "object" &&
		value !== null &&
		"aborted" in value &&
		typeof value.addEventListener === "function" &&
		typeof value.removeEventListener === "function"
	);
}

function reportListenerError(error) {
	queueMicrotask(() => {
		throw error;
	});
}

class PatchedEvent {
	static NONE = 0;
	static CAPTURING_PHASE = 1;
	static AT_TARGET = 2;
	static BUBBLING_PHASE = 3;

	constructor(type, init) {
		if (arguments.length === 0) {
			throw new TypeError("The event type must be provided");
		}
		const normalizedInit = init == null ? {} : Object(init);
		eventState.set(this, {
			type: toDomString(type),
			bubbles: Boolean(normalizedInit.bubbles),
			cancelable: Boolean(normalizedInit.cancelable),
			composed: Boolean(normalizedInit.composed),
			defaultPrevented: false,
			target: null,
			currentTarget: null,
			eventPhase: 0,
			timeStamp: Date.now(),
			inPassiveListener: false,
			propagationStopped: false,
			immediatePropagationStopped: false,
			dispatching: false,
		});
	}

	get NONE() {
		return 0;
	}
	get CAPTURING_PHASE() {
		return 1;
	}
	get AT_TARGET() {
		return 2;
	}
	get BUBBLING_PHASE() {
		return 3;
	}
	get type() {
		return eventState.get(this).type;
	}
	get bubbles() {
		return eventState.get(this).bubbles;
	}
	get cancelable() {
		return eventState.get(this).cancelable;
	}
	get composed() {
		return eventState.get(this).composed;
	}
	get defaultPrevented() {
		return eventState.get(this).defaultPrevented;
	}
	get target() {
		return eventState.get(this).target;
	}
	get currentTarget() {
		return eventState.get(this).currentTarget;
	}
	get eventPhase() {
		return eventState.get(this).eventPhase;
	}
	get timeStamp() {
		return eventState.get(this).timeStamp;
	}
	get isTrusted() {
		return false;
	}
	get srcElement() {
		return eventState.get(this).target;
	}
	get returnValue() {
		return !eventState.get(this).defaultPrevented;
	}
	set returnValue(value) {
		if (!value) this.preventDefault();
	}
	get cancelBubble() {
		return eventState.get(this).propagationStopped;
	}
	set cancelBubble(value) {
		if (value) this.stopPropagation();
	}

	get [Symbol.toStringTag]() {
		return "Event";
	}

	preventDefault() {
		const state = eventState.get(this);
		if (state.cancelable && !state.inPassiveListener) {
			state.defaultPrevented = true;
		}
	}

	stopPropagation() {
		eventState.get(this).propagationStopped = true;
	}

	stopImmediatePropagation() {
		const state = eventState.get(this);
		state.propagationStopped = true;
		state.immediatePropagationStopped = true;
	}

	composedPath() {
		const target = eventState.get(this).target;
		return target ? [target] : [];
	}
}

class PatchedCustomEvent extends PatchedEvent {
	constructor(type, init) {
		super(type, init);
		const normalizedInit = init == null ? null : Object(init);
		customEventDetail.set(
			this,
			normalizedInit && "detail" in normalizedInit
				? normalizedInit.detail
				: null,
		);
	}

	get detail() {
		return customEventDetail.get(this);
	}

	get [Symbol.toStringTag]() {
		return "CustomEvent";
	}
}

class PatchedEventTarget {
	constructor() {
		targetState.set(this, { listeners: new Map(), listenerCount: 0 });
	}

	addEventListener(type, listener, options) {
		const eventType = toDomString(type);
		const normalized = normalizeAddEventListenerOptions(options);
		if (normalized.signal !== void 0 && !isAbortSignalLike(normalized.signal)) {
			throw new TypeError(
				'The "signal" option must be an instance of AbortSignal.',
			);
		}
		if (listener == null) return;
		if (
			typeof listener !== "function" &&
			(typeof listener !== "object" || listener === null)
		) {
			return;
		}
		if (normalized.signal?.aborted) return;

		const state = targetState.get(this);
		const records = state.listeners.get(eventType) ?? [];
		if (
			records.some(
				(record) =>
					record.listener === listener && record.capture === normalized.capture,
			)
		) {
			return;
		}
		if (
			!state.listeners.has(eventType) &&
			state.listeners.size >= MAX_EVENT_TYPES
		) {
			throw withCode(
				new RangeError(
					`EventTarget event type limit ${MAX_EVENT_TYPES} exceeded; this runtime limit cannot be raised by guest code`,
				),
				"ERR_EVENT_TYPE_LIMIT",
			);
		}
		if (state.listenerCount >= MAX_EVENT_LISTENERS) {
			throw withCode(
				new RangeError(
					`EventTarget listener limit ${MAX_EVENT_LISTENERS} exceeded; this runtime limit cannot be raised by guest code`,
				),
				"ERR_EVENT_LISTENER_LIMIT",
			);
		}

		const record = {
			listener,
			capture: normalized.capture,
			once: normalized.once,
			passive: normalized.passive,
			kind: typeof listener === "function" ? "function" : "object",
			signal: normalized.signal,
		};
		if (normalized.signal) {
			record.abortListener = () => {
				this.removeEventListener(eventType, listener, normalized.capture);
			};
			normalized.signal.addEventListener("abort", record.abortListener, {
				once: true,
			});
		}
		records.push(record);
		state.listeners.set(eventType, records);
		state.listenerCount += 1;
	}

	removeEventListener(type, listener, options) {
		const eventType = toDomString(type);
		if (listener == null) return;
		const capture = normalizeRemoveEventListenerOptions(options);
		const state = targetState.get(this);
		const records = state.listeners.get(eventType);
		if (!records) return;
		const nextRecords = [];
		for (const record of records) {
			const match = record.listener === listener && record.capture === capture;
			if (match) {
				state.listenerCount -= 1;
				if (record.signal && record.abortListener) {
					record.signal.removeEventListener("abort", record.abortListener);
				}
			} else {
				nextRecords.push(record);
			}
		}
		if (nextRecords.length === 0) state.listeners.delete(eventType);
		else state.listeners.set(eventType, nextRecords);
	}

	dispatchEvent(event) {
		if (!(event instanceof PatchedEvent)) {
			throw new TypeError("Argument 1 must be an Event");
		}
		const state = eventState.get(event);
		if (state.dispatching) {
			throw new DOMException(
				"The event is already being dispatched",
				"InvalidStateError",
			);
		}
		const listeners = targetState.get(this).listeners;
		const records = (listeners.get(state.type) ?? []).slice();
		state.target = this;
		state.currentTarget = this;
		state.eventPhase = PatchedEvent.AT_TARGET;
		state.dispatching = true;
		try {
			for (const record of records) {
				if (!listeners.get(state.type)?.includes(record)) continue;
				if (record.once) {
					this.removeEventListener(state.type, record.listener, record.capture);
				}
				state.inPassiveListener = record.passive;
				try {
					if (record.kind === "function") {
						record.listener.call(this, event);
					} else {
						const handleEvent = record.listener.handleEvent;
						if (typeof handleEvent === "function") {
							handleEvent.call(record.listener, event);
						}
					}
				} catch (error) {
					reportListenerError(error);
				} finally {
					state.inPassiveListener = false;
				}
				if (state.immediatePropagationStopped) break;
			}
		} finally {
			state.currentTarget = null;
			state.eventPhase = PatchedEvent.NONE;
			state.dispatching = false;
		}
		return !state.defaultPrevented;
	}
}

var Event = PatchedEvent;
var CustomEvent = PatchedCustomEvent;
var EventTarget = PatchedEventTarget;

export {
	CustomEvent,
	Event,
	EventTarget,
	isAbortSignalLike,
	MAX_EVENT_LISTENERS,
	MAX_EVENT_TYPES,
	normalizeAddEventListenerOptions,
	normalizeRemoveEventListenerOptions,
	PatchedCustomEvent,
	PatchedEvent,
	PatchedEventTarget,
	toDomString,
};
