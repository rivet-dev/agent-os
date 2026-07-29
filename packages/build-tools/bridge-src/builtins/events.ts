import eventsStdlibModule, * as eventsStdlibModuleNs from "node:events";
import { exposeCustomGlobal } from "../global-exposure.js";
import { Event } from "../polyfills/index.js";
import { process2 } from "./process.js";

const EventEmitter =
	eventsStdlibModuleNs.EventEmitter ??
	eventsStdlibModule?.EventEmitter ??
	eventsStdlibModule;

if (typeof EventEmitter !== "function") {
	throw new TypeError(
		"The upstream events module does not export EventEmitter",
	);
}

const eventsErrorMonitor =
	eventsStdlibModuleNs.errorMonitor ?? Symbol("events.errorMonitor");

// events@3 routes listener-leak warnings to console.warn because browsers do not
// expose process.emitWarning. Keep its warning construction and threshold logic,
// but deliver the warning through the agentOS process object.
function routeMaxListenersWarning(method) {
	return function routedEventEmitterListener(...args) {
		const originalWarn = globalThis.console?.warn;
		if (typeof originalWarn !== "function") {
			return Reflect.apply(method, this, args);
		}
		globalThis.console.warn = (warning, ...warningArgs) => {
			if (
				warning?.name === "MaxListenersExceededWarning" &&
				typeof process2?.emitWarning === "function"
			) {
				const emitterName =
					warning.emitter?.constructor === EventEmitter
						? "EventEmitter"
						: warning.emitter?.constructor?.name || "EventEmitter";
				const maxListeners =
					typeof warning.emitter?.getMaxListeners === "function"
						? warning.emitter.getMaxListeners()
						: EventEmitter.defaultMaxListeners;
				warning.message =
					`Possible EventEmitter memory leak detected. ${warning.count} ` +
					`${String(warning.type)} listeners added to [${emitterName}]. ` +
					`MaxListeners is ${maxListeners}. Use emitter.setMaxListeners() ` +
					"to increase limit";
				process2.emitWarning(warning);
				return;
			}
			Reflect.apply(originalWarn, globalThis.console, [
				warning,
				...warningArgs,
			]);
		};
		try {
			return Reflect.apply(method, this, args);
		} finally {
			globalThis.console.warn = originalWarn;
		}
	};
}

if (!EventEmitter.__agentOSEventsWarningPatched) {
	const addListener = routeMaxListenersWarning(
		EventEmitter.prototype.addListener,
	);
	const prependListener = routeMaxListenersWarning(
		EventEmitter.prototype.prependListener,
	);
	EventEmitter.prototype.addListener = addListener;
	EventEmitter.prototype.on = addListener;
	EventEmitter.prototype.prependListener = prependListener;
	Object.defineProperty(EventEmitter, "__agentOSEventsWarningPatched", {
		value: true,
	});
}

const once = eventsStdlibModuleNs.once ?? EventEmitter.once;
const eventTargetMaxListeners = new WeakMap();

function isEventTargetLike(emitter) {
	return (
		emitter !== null &&
		(typeof emitter === "object" || typeof emitter === "function") &&
		typeof emitter.addEventListener === "function" &&
		typeof emitter.removeEventListener === "function" &&
		typeof emitter.dispatchEvent === "function"
	);
}

function getEventListeners(emitter, eventName) {
	if (typeof emitter?.listeners === "function") {
		return emitter.listeners(eventName);
	}
	throw new TypeError("The emitter argument must expose listeners()");
}

function getMaxListeners(emitter) {
	if (typeof emitter?.getMaxListeners === "function") {
		return emitter.getMaxListeners();
	}
	if (isEventTargetLike(emitter)) {
		return (
			eventTargetMaxListeners.get(emitter) ?? EventEmitter.defaultMaxListeners
		);
	}
	throw new TypeError("The emitter argument must expose getMaxListeners()");
}

function setMaxListeners(count, ...emitters) {
	if (emitters.length === 0) {
		EventEmitter.defaultMaxListeners = count;
		return;
	}
	const validationEmitter = new EventEmitter();
	validationEmitter.setMaxListeners(count);
	for (const emitter of emitters) {
		if (typeof emitter?.setMaxListeners === "function") {
			emitter.setMaxListeners(count);
		} else if (isEventTargetLike(emitter)) {
			eventTargetMaxListeners.set(emitter, count);
		} else {
			throw new TypeError(
				'The "eventTargets" argument must be an instance of EventEmitter or EventTarget.',
			);
		}
	}
}

function addAbortListener(signal, listener) {
	if (!signal || typeof signal.addEventListener !== "function") {
		throw new TypeError("AbortSignal is required");
	}
	if (typeof listener !== "function") {
		throw new TypeError("listener must be a function");
	}
	const wrapped = () =>
		listener(
			typeof Event === "function" ? new Event("abort") : { type: "abort" },
		);
	if (signal.aborted) {
		queueMicrotask(wrapped);
	} else {
		signal.addEventListener("abort", wrapped, { once: true });
	}
	const dispose = () => signal.removeEventListener("abort", wrapped);
	return {
		dispose,
		[Symbol.dispose]: dispose,
	};
}

// events@3 predates the async-iterator helper added to Node. This is adapter
// surface only; listener storage and dispatch remain owned by upstream events.
function on(emitter, eventName, options) {
	const signal = options?.signal;
	if (signal?.aborted) {
		throw signal.reason ?? new Error("The operation was aborted");
	}
	const queue = [];
	const pending = [];
	let error = null;
	let finished = false;
	const removeListener = (name, listener) =>
		(emitter.off ?? emitter.removeListener).call(emitter, name, listener);
	const cleanup = () => {
		removeListener(eventName, handleEvent);
		removeListener("error", handleError);
		signal?.removeEventListener?.("abort", handleAbort);
	};
	const iterator = {
		next() {
			if (queue.length > 0) {
				return Promise.resolve({ value: queue.shift(), done: false });
			}
			if (error !== null) {
				const currentError = error;
				error = null;
				cleanup();
				return Promise.reject(currentError);
			}
			if (finished) {
				return Promise.resolve({ value: undefined, done: true });
			}
			return new Promise((resolve, reject) => {
				pending.push({ resolve, reject });
			});
		},
		return() {
			finished = true;
			cleanup();
			for (const waiter of pending.splice(0)) {
				waiter.resolve({ value: undefined, done: true });
			}
			return Promise.resolve({ value: undefined, done: true });
		},
		throw(thrownError) {
			error = thrownError;
			cleanup();
			return Promise.reject(thrownError);
		},
		[Symbol.asyncIterator]() {
			return this;
		},
	};
	function handleEvent(...args) {
		const waiter = pending.shift();
		if (waiter) {
			waiter.resolve({ value: args, done: false });
		} else {
			queue.push(args);
		}
	}
	function handleError(thrownError) {
		const waiter = pending.shift();
		if (waiter) {
			cleanup();
			waiter.reject(thrownError);
		} else {
			error = thrownError;
		}
	}
	function handleAbort() {
		void iterator.return();
	}
	emitter.on(eventName, handleEvent);
	emitter.on("error", handleError);
	signal?.addEventListener?.("abort", handleAbort, { once: true });
	return iterator;
}

Object.assign(EventEmitter, {
	EventEmitter,
	addAbortListener,
	errorMonitor: eventsErrorMonitor,
	getEventListeners,
	getMaxListeners,
	on,
	once,
	setMaxListeners,
});

const eventsModule = EventEmitter;
exposeCustomGlobal("_eventsModule", eventsModule);

export {
	addAbortListener,
	EventEmitter,
	eventsErrorMonitor,
	eventsModule,
	getEventListeners,
	getMaxListeners,
	on,
	once,
	setMaxListeners,
};
