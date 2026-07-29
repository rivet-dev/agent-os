import { Event, EventTarget } from "./dom-events.js";

const NativeAbortControllerGlobal = globalThis.AbortController;
const NativeAbortSignalGlobal = globalThis.AbortSignal;
const hasNativeAbortGlobals =
	typeof NativeAbortSignalGlobal === "function" &&
	NativeAbortSignalGlobal.name === "AbortSignal" &&
	typeof NativeAbortControllerGlobal === "function" &&
	NativeAbortControllerGlobal.name === "AbortController";
const MAX_ABORT_SIGNAL_ANY_INPUTS = 1024;
const abortSignalToken = {};
const abortSignalState = new WeakMap();
const abortControllerState = new WeakMap();

function withCode(error, code) {
	error.code = code;
	return error;
}

class FallbackAbortSignal extends EventTarget {
	constructor(token) {
		super();
		if (token !== abortSignalToken) {
			throw withCode(
				new TypeError("Illegal constructor"),
				"ERR_ILLEGAL_CONSTRUCTOR",
			);
		}
		abortSignalState.set(this, {
			aborted: false,
			reason: void 0,
			onabort: null,
		});
	}

	get aborted() {
		return abortSignalState.get(this).aborted;
	}

	get reason() {
		return abortSignalState.get(this).reason;
	}

	get onabort() {
		return abortSignalState.get(this).onabort;
	}

	set onabort(listener) {
		const state = abortSignalState.get(this);
		if (state.onabort) this.removeEventListener("abort", state.onabort);
		state.onabort = typeof listener === "function" ? listener : null;
		if (state.onabort) this.addEventListener("abort", state.onabort);
	}

	throwIfAborted() {
		const state = abortSignalState.get(this);
		if (state.aborted) throw state.reason;
	}
}

class FallbackAbortController {
	constructor() {
		abortControllerState.set(this, new FallbackAbortSignal(abortSignalToken));
	}

	get signal() {
		return abortControllerState.get(this);
	}

	abort(reason) {
		const signal = abortControllerState.get(this);
		const state = abortSignalState.get(signal);
		if (state.aborted) return;
		state.aborted = true;
		state.reason = createAbortSignalReason(reason);
		signal.dispatchEvent(new Event("abort"));
	}
}

var AbortSignal = hasNativeAbortGlobals
	? NativeAbortSignalGlobal
	: FallbackAbortSignal;
var AbortController = hasNativeAbortGlobals
	? NativeAbortControllerGlobal
	: FallbackAbortController;

function ensureNamedConstructor(ctor, expectedName) {
	if (typeof ctor !== "function") return;
	try {
		if (ctor.name !== expectedName) {
			Object.defineProperty(ctor, "name", {
				configurable: true,
				value: expectedName,
			});
		}
	} catch {}
}

ensureNamedConstructor(AbortSignal, "AbortSignal");
ensureNamedConstructor(AbortController, "AbortController");
try {
	const signalCtor = Object.getPrototypeOf(
		new AbortController().signal,
	)?.constructor;
	ensureNamedConstructor(signalCtor, "AbortSignal");
} catch {}
try {
	globalThis.AbortSignal = AbortSignal;
} catch {}
try {
	globalThis.AbortController = AbortController;
} catch {}

function createAbortSignalReason(reason) {
	if (reason !== void 0) return reason;
	if (typeof globalThis.DOMException === "function") {
		return new globalThis.DOMException(
			"This operation was aborted",
			"AbortError",
		);
	}
	const error = new Error("This operation was aborted");
	error.name = "AbortError";
	return error;
}

function createAbortSignalTimeoutReason() {
	if (typeof globalThis.DOMException === "function") {
		return new globalThis.DOMException(
			"The operation was aborted due to timeout",
			"TimeoutError",
		);
	}
	const error = new Error("The operation was aborted due to timeout");
	error.name = "TimeoutError";
	return error;
}

function createAbortedSignal(reason) {
	const controller = new AbortController();
	controller.abort(reason);
	return controller.signal;
}

function normalizeAbortSignalTimeout(delay) {
	if (typeof delay !== "number") {
		throw withCode(
			new TypeError(
				`The "delay" argument must be of type number. Received ${typeof delay}`,
			),
			"ERR_INVALID_ARG_TYPE",
		);
	}
	if (!Number.isInteger(delay) || delay < 0 || delay > 4294967295) {
		throw withCode(
			new RangeError(
				`The value of "delay" is out of range. It must be an integer >= 0 and <= 4294967295. Received ${String(delay)}`,
			),
			"ERR_OUT_OF_RANGE",
		);
	}
	return delay;
}

if (typeof AbortSignal.abort !== "function") {
	Object.defineProperty(AbortSignal, "abort", {
		configurable: true,
		writable: true,
		value(reason = void 0) {
			return createAbortedSignal(reason);
		},
	});
}

if (typeof AbortSignal.timeout !== "function") {
	Object.defineProperty(AbortSignal, "timeout", {
		configurable: true,
		writable: true,
		value(delay) {
			const timeout = normalizeAbortSignalTimeout(delay);
			const controller = new AbortController();
			const timer = setTimeout(() => {
				controller.abort(createAbortSignalTimeoutReason());
			}, timeout);
			if (typeof timer?.unref === "function") timer.unref();
			controller.signal.addEventListener("abort", () => clearTimeout(timer), {
				once: true,
			});
			return controller.signal;
		},
	});
}

if (typeof AbortSignal.any !== "function") {
	Object.defineProperty(AbortSignal, "any", {
		configurable: true,
		writable: true,
		value(signals) {
			if (!signals || typeof signals[Symbol.iterator] !== "function") {
				throw new TypeError('The "signals" argument must be an iterable');
			}
			const inputs = [];
			for (const signal of signals) {
				if (inputs.length >= MAX_ABORT_SIGNAL_ANY_INPUTS) {
					throw withCode(
						new RangeError(
							`AbortSignal.any input limit ${MAX_ABORT_SIGNAL_ANY_INPUTS} exceeded; this runtime limit cannot be raised by guest code`,
						),
						"ERR_ABORT_SIGNAL_LIMIT",
					);
				}
				if (!(signal instanceof AbortSignal)) {
					throw new TypeError(
						'The "signals" argument must contain AbortSignal instances',
					);
				}
				inputs.push(signal);
			}
			const controller = new AbortController();
			const listeners = [];
			const abortFromSignal = (signal) => {
				while (listeners.length > 0) {
					const [candidate, listener] = listeners.pop();
					candidate.removeEventListener("abort", listener);
				}
				controller.abort(signal.reason);
			};
			for (const signal of inputs) {
				if (signal.aborted) {
					abortFromSignal(signal);
					return controller.signal;
				}
				const onAbort = () => abortFromSignal(signal);
				listeners.push([signal, onAbort]);
				signal.addEventListener("abort", onAbort, { once: true });
			}
			return controller.signal;
		},
	});
}

export {
	AbortController,
	AbortSignal,
	createAbortedSignal,
	createAbortSignalReason,
	createAbortSignalTimeoutReason,
	ensureNamedConstructor,
	MAX_ABORT_SIGNAL_ANY_INPUTS,
	normalizeAbortSignalTimeout,
};
