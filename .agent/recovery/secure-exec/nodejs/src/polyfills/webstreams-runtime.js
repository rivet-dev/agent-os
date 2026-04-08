import * as ponyfill from "web-streams-polyfill/dist/ponyfill.js";
import { getJsTransferState } from "./js-transferable.js";

const SHARED_KEY = "__secureExecWebStreams";
const inspectSymbol = Symbol.for("nodejs.util.inspect.custom");

function defineHidden(target, key, value) {
	Object.defineProperty(target, key, {
		value,
		configurable: true,
		enumerable: false,
		writable: false,
	});
}

function defineTag(proto, name) {
	if (!proto) return;
	const descriptor = Object.getOwnPropertyDescriptor(proto, Symbol.toStringTag);
	if (
		descriptor?.value === name &&
		descriptor.configurable === true &&
		descriptor.enumerable === false &&
		descriptor.writable === false
	) {
		return;
	}
	Object.defineProperty(proto, Symbol.toStringTag, {
		configurable: true,
		enumerable: false,
		value: name,
		writable: false,
	});
}

function createCodeError(name, code, message) {
	const error = new Error(message);
	error.name = name;
	error.code = code;
	return error;
}

function createInvalidArgType(message) {
	return createCodeError("TypeError", "ERR_INVALID_ARG_TYPE", message);
}

function createInvalidArgValue(message) {
	return createCodeError("TypeError", "ERR_INVALID_ARG_VALUE", message);
}

function createInvalidState(message) {
	return createCodeError("TypeError", "ERR_INVALID_STATE", message);
}

function createInvalidThis(message) {
	return createCodeError("TypeError", "ERR_INVALID_THIS", message);
}

function createIllegalConstructor(message) {
	return createCodeError("TypeError", "ERR_ILLEGAL_CONSTRUCTOR", message);
}

function createAbortError(message) {
	return createCodeError("AbortError", "ABORT_ERR", message || "The operation was aborted");
}

const normalizedPromiseMap = new WeakMap();

function normalizeInvalidStateError(error) {
	if (!(error instanceof Error) || error.code === "ERR_INVALID_STATE") {
		return error;
	}
	if (error.name !== "TypeError") {
		return error;
	}
	const message = String(error.message || "");
	if (
		!/state|locked|already has a reader|already has a writer|already been locked|cannot be used on a locked|released|invalidated|cannot close|cannot enqueue|cannot respond|closed or draining/.test(
			message.toLowerCase(),
		)
	) {
		return error;
	}
	error.code = "ERR_INVALID_STATE";
	return error;
}

function withInvalidStateNormalization(result) {
	if (result && typeof result.then === "function") {
		let normalized = normalizedPromiseMap.get(result);
		if (!normalized) {
			normalized = result.catch((error) => {
				throw normalizeInvalidStateError(error);
			});
			normalizedPromiseMap.set(result, normalized);
		}
		return normalized;
	}
	return result;
}

function getRuntimeRequire() {
	return typeof globalThis.require === "function" ? globalThis.require : null;
}

function toBuffer(chunk) {
	if (typeof Buffer !== "undefined" && Buffer.isBuffer(chunk)) {
		return chunk;
	}
	if (chunk instanceof Uint8Array) {
		return Buffer.from(chunk.buffer, chunk.byteOffset, chunk.byteLength);
	}
	if (chunk instanceof ArrayBuffer) {
		return Buffer.from(chunk);
	}
	if (typeof chunk === "string") {
		return Buffer.from(chunk);
	}
	return Buffer.from(String(chunk));
}

function ensureInspect(proto, name, formatter) {
	if (!proto || proto[inspectSymbol]) return;
	defineHidden(proto, inspectSymbol, function inspect(depth) {
		if (typeof depth === "number" && depth <= 0) {
			return `${name} [Object]`;
		}
		return formatter.call(this, depth);
	});
}

function ensureClassBrand(proto, ctor, name) {
	defineTag(proto, name);
	defineTag(ctor?.prototype, name);
}

function copyObjectLike(source) {
	const target = Object.create(Object.getPrototypeOf(source));
	Object.defineProperties(target, Object.getOwnPropertyDescriptors(source));
	return target;
}

export function getWebStreamsState() {
	if (globalThis[SHARED_KEY]) {
		return globalThis[SHARED_KEY];
	}

	const transferState = getJsTransferState();
	const kState = Symbol("kState");
	const kDecorated = Symbol("kDecorated");
	const streamStateMap = new WeakMap();
	const promiseStateMap = new WeakMap();

	function isObject(value) {
		return (typeof value === "object" || typeof value === "function") && value !== null;
	}

	function installPromiseTracking() {
		if (globalThis.__secureExecPromiseTrackingInstalled) {
			return;
		}
		const NativePromise = globalThis.Promise;
		if (typeof NativePromise !== "function") {
			return;
		}

		function trackPromise(promise, initialState = "pending") {
			if (!(promise instanceof NativePromise) || promiseStateMap.has(promise)) {
				return promise;
			}
			const record = { state: initialState };
			promiseStateMap.set(promise, record);
			const trackerSource =
				promise.constructor === NativePromise
					? promise
					: NativePromise.resolve(promise);
			NativePromise.prototype.then.call(
				trackerSource,
				() => {
					if (record.state === "pending") {
						record.state = "fulfilled";
					}
				},
				() => {
					if (record.state === "pending") {
						record.state = "rejected";
					}
				},
			);
			return promise;
		}

		function TrackedPromise(executor) {
			if (!(this instanceof TrackedPromise)) {
				throw new TypeError("Promise constructor cannot be invoked without 'new'");
			}
			return trackPromise(
				Reflect.construct(
					NativePromise,
					[executor],
					new.target ?? TrackedPromise,
				),
			);
		}

		Object.setPrototypeOf(TrackedPromise, NativePromise);
		TrackedPromise.prototype = NativePromise.prototype;
		Object.defineProperty(TrackedPromise, "name", { value: "Promise" });
		TrackedPromise.resolve = function resolve(value) {
			const initialState =
				value instanceof NativePromise
					? (promiseStateMap.get(value)?.state ?? "pending")
					: "fulfilled";
			return trackPromise(
				NativePromise.resolve.call(this, value),
				initialState,
			);
		};
		TrackedPromise.reject = function reject(reason) {
			return trackPromise(NativePromise.reject.call(this, reason), "rejected");
		};
		for (const key of ["all", "allSettled", "any", "race"]) {
			if (typeof NativePromise[key] === "function") {
				TrackedPromise[key] = function trackedStatic(iterable) {
					return trackPromise(NativePromise[key].call(this, iterable));
				};
			}
		}
		if (typeof NativePromise.withResolvers === "function") {
			TrackedPromise.withResolvers = function withResolvers() {
				const resolvers = NativePromise.withResolvers.call(this);
				trackPromise(resolvers.promise);
				return resolvers;
			};
		}
		globalThis.Promise = TrackedPromise;
		globalThis.__secureExecPromiseTrackingInstalled = true;
	}

	function getPromiseState(promise) {
		if (!(promise instanceof Promise)) return false;
		const tracked = promiseStateMap.get(promise);
		if (tracked) {
			return tracked.state === "pending";
		}
		const runtimeRequire = getRuntimeRequire();
		const inspect = runtimeRequire?.("util")?.inspect;
		return typeof inspect === "function" && inspect(promise).includes("<pending>");
	}

	installPromiseTracking();

	function setState(target, state) {
		if (!isObject(target)) return;
		streamStateMap.set(target, state);
		defineHidden(target, kState, state);
	}

	function syncReadableController(stream, streamState) {
		if (streamState.controller || !isObject(stream)) return;
		const controller = stream._readableStreamController;
		if (controller) {
			streamState.controller = controller;
		}
	}

	function syncReadableReader(stream, streamState) {
		if (streamState.reader || !isObject(stream)) return;
		const reader = stream._reader;
		if (reader) {
			streamState.reader = decorateReader(reader, stream);
		}
	}

	function syncWritableController(stream, streamState) {
		if (streamState.controller || !isObject(stream)) return;
		const controller = stream._writableStreamController;
		if (controller) {
			streamState.controller = controller;
		}
	}

	function clearReadableControllerAlgorithms(streamState) {
		const controllerState = streamState?.controller?.[kState];
		if (!controllerState) {
			return;
		}
		controllerState.pullAlgorithm = undefined;
		controllerState.cancelAlgorithm = undefined;
		controllerState.sizeAlgorithm = undefined;
	}

	function decorateReader(reader, stream) {
		if (!isObject(reader) || reader[kState]) return reader;
		const state = {
			stream,
			readRequests: [],
		};
		setState(reader, state);
		const originalRead = reader.read;
		reader.read = function read(...args) {
			const streamState = stream?.[kState];
			if (streamState) {
				streamState.disturbed = true;
			}
			const promise = originalRead.apply(this, args);
			state.readRequests.push(promise);
			return Promise.resolve(promise)
				.then((result) => {
					if (result?.done && streamState?.closeRequested) {
						streamState.state = "closed";
					}
					return result;
				})
				.finally(() => {
					const index = state.readRequests.indexOf(promise);
					if (index !== -1) {
						state.readRequests.splice(index, 1);
					}
				});
		};
		const originalReleaseLock = reader.releaseLock;
		reader.releaseLock = function releaseLock() {
			const streamState = stream?.[kState];
			if (streamState?.reader === this) {
				streamState.reader = undefined;
			}
			return originalReleaseLock.call(this);
		};
		return reader;
	}

	function decorateWritableWriter(writer, stream) {
		if (!isObject(writer) || writer[kState]) return writer;
		setState(writer, { stream });
		return writer;
	}

	function decorateReadableController(controller, streamState, source) {
		if (!isObject(controller) || controller[kState]) return controller;
		const controllerState = {
			streamState,
			pendingPullIntos: [],
			pullAlgorithm: typeof source?.pull === "function" ? source.pull : undefined,
			cancelAlgorithm: typeof source?.cancel === "function" ? source.cancel : undefined,
			sizeAlgorithm: undefined,
		};
		setState(controller, controllerState);
		streamState.controller = controller;
		if (typeof controller.close === "function") {
			const originalClose = controller.close;
			controller.close = function close() {
				streamState.closeRequested = true;
				return originalClose.call(this);
			};
		}
		if (typeof controller.error === "function") {
			const originalError = controller.error;
			controller.error = function error(reason) {
				streamState.state = "errored";
				streamState.storedError = reason;
				return originalError.call(this, reason);
			};
		}
		return controller;
	}

	function decorateWritableController(controller, streamState) {
		if (!isObject(controller) || controller[kState]) return controller;
		const controllerState = {
			streamState,
		};
		setState(controller, controllerState);
		streamState.controller = controller;
		if (typeof controller.error === "function") {
			const originalError = controller.error;
			controller.error = function error(reason) {
				streamState.state = "errored";
				streamState.storedError = reason;
				return originalError.call(this, reason);
			};
		}
		return controller;
	}

	function decorateTransformController(controller) {
		if (!isObject(controller) || controller[kState]) return controller;
		setState(controller, {});
		return controller;
	}

	function isReadableStreamInstance(value) {
		return value instanceof ponyfill.ReadableStream;
	}

	function isWritableStreamInstance(value) {
		return value instanceof ponyfill.WritableStream;
	}

	function isTransformStreamInstance(value) {
		return value instanceof ponyfill.TransformStream;
	}

	function wrapIllegalConstructor(ctor, name) {
		function IllegalConstructor() {
			throw createIllegalConstructor("Illegal constructor");
		}
		Object.setPrototypeOf(IllegalConstructor, ctor);
		IllegalConstructor.prototype = ctor.prototype;
		Object.defineProperty(IllegalConstructor, "name", { value: name });
		return IllegalConstructor;
	}

	function patchGetterBrand(proto, key, brandCheck, errorFactory) {
		const descriptor = Object.getOwnPropertyDescriptor(proto, key);
		if (!descriptor?.get || descriptor.get._secureExecPatched) {
			return;
		}
		const originalGet = descriptor.get;
		const patchedGet = function patchedGet() {
			if (!brandCheck(this)) {
				throw errorFactory();
			}
			return originalGet.call(this);
		};
		patchedGet._secureExecPatched = true;
		Object.defineProperty(proto, key, {
			configurable: descriptor.configurable !== false,
			enumerable: descriptor.enumerable === true,
			get: patchedGet,
			set: descriptor.set,
		});
	}

	function patchMethodBrand(proto, key, brandCheck, errorFactory, wrapper) {
		const original = proto?.[key];
		if (typeof original !== "function" || original._secureExecPatched) {
			return;
		}
		const patched = function patchedMethod(...args) {
			if (!brandCheck(this)) {
				throw errorFactory();
			}
			if (typeof wrapper === "function") {
				return wrapper.call(this, original, args);
			}
			return original.apply(this, args);
		};
		patched._secureExecPatched = true;
		Object.defineProperty(proto, key, {
			value: patched,
			configurable: true,
			enumerable: false,
			writable: true,
		});
	}

	function patchAsyncMethodBrand(proto, key, brandCheck, errorFactory, wrapper) {
		const original = proto?.[key];
		if (typeof original !== "function" || original._secureExecPatched) {
			return;
		}
		const patched = function patchedMethod(...args) {
			if (!brandCheck(this)) {
				return Promise.reject(errorFactory());
			}
			if (typeof wrapper === "function") {
				return wrapper.call(this, original, args);
			}
			return original.apply(this, args);
		};
		patched._secureExecPatched = true;
		Object.defineProperty(proto, key, {
			value: patched,
			configurable: true,
			enumerable: false,
			writable: true,
		});
	}

	function patchRejectedGetterBrand(proto, key, brandCheck, errorFactory, wrapper) {
		const descriptor = Object.getOwnPropertyDescriptor(proto, key);
		if (!descriptor?.get || descriptor.get._secureExecPatched) {
			return;
		}
		const originalGet = descriptor.get;
		const patchedGet = function patchedGet() {
			if (!brandCheck(this)) {
				return Promise.reject(errorFactory());
			}
			if (typeof wrapper === "function") {
				return wrapper.call(this, originalGet);
			}
			return originalGet.call(this);
		};
		patchedGet._secureExecPatched = true;
		Object.defineProperty(proto, key, {
			configurable: descriptor.configurable !== false,
			enumerable: descriptor.enumerable === true,
			get: patchedGet,
			set: descriptor.set,
		});
	}

	function createPrivateMemberError() {
		return new TypeError("Cannot read private member from an object whose class did not declare it");
	}

	function wrapReadableSource(source, streamState) {
		if (source == null) return { start(controller) { decorateReadableController(controller, streamState, source); } };
		if (typeof source !== "object") {
			throw createInvalidArgType('The "source" argument must be of type object.');
		}
		const wrapped = copyObjectLike(source);
		Object.defineProperties(wrapped, {
			start: {
				configurable: true,
				enumerable: true,
				writable: true,
				value(controller) {
				decorateReadableController(controller, streamState, source);
				return source.start?.call(source, controller);
			},
			},
			pull: {
				configurable: true,
				enumerable: true,
				writable: true,
				value(controller) {
				decorateReadableController(controller, streamState, source);
				return source.pull?.call(source, controller);
			},
			},
			cancel: {
				configurable: true,
				enumerable: true,
				writable: true,
				value(reason) {
				return source.cancel?.call(source, reason);
			},
			},
		});
		return wrapped;
	}

	function wrapWritableSink(sink, streamState) {
		if (sink == null) return { start(controller) { decorateWritableController(controller, streamState); } };
		if (typeof sink !== "object") {
			throw createInvalidArgType('The "sink" argument must be of type object.');
		}
		const wrapped = copyObjectLike(sink);
		Object.defineProperties(wrapped, {
			start: {
				configurable: true,
				enumerable: true,
				writable: true,
				value(controller) {
				decorateWritableController(controller, streamState);
				return sink.start?.call(sink, controller);
			},
			},
			write: {
				configurable: true,
				enumerable: true,
				writable: true,
				value(chunk, controller) {
				return sink.write?.call(sink, chunk, controller);
			},
			},
			close: {
				configurable: true,
				enumerable: true,
				writable: true,
				value() {
				streamState.state = "closed";
				return sink.close?.call(sink);
			},
			},
			abort: {
				configurable: true,
				enumerable: true,
				writable: true,
				value(reason) {
				streamState.state = "errored";
				streamState.storedError = reason;
				return sink.abort?.call(sink, reason);
			},
			},
		});
		return wrapped;
	}

	function wrapTransformer(transformer) {
		if (transformer == null) return { start(controller) { decorateTransformController(controller); } };
		if (typeof transformer !== "object") {
			throw createInvalidArgType('The "transformer" argument must be of type object.');
		}
		const wrapped = copyObjectLike(transformer);
		Object.defineProperties(wrapped, {
			start: {
				configurable: true,
				enumerable: true,
				writable: true,
				value(controller) {
				decorateTransformController(controller);
				return transformer.start?.call(transformer, controller);
			},
			},
			transform: {
				configurable: true,
				enumerable: true,
				writable: true,
				value(chunk, controller) {
				decorateTransformController(controller);
				return transformer.transform?.call(transformer, chunk, controller);
			},
			},
			flush: {
				configurable: true,
				enumerable: true,
				writable: true,
				value(controller) {
				decorateTransformController(controller);
				return transformer.flush?.call(transformer, controller);
			},
			},
		});
		return wrapped;
	}

	function decorateReadableStream(stream) {
		if (!isObject(stream) || stream[kDecorated]) return stream;
		defineHidden(stream, kDecorated, true);
		const state =
			stream[kState] ??
			{
				state: "readable",
				controller: undefined,
				reader: undefined,
				storedError: undefined,
				disturbed: false,
				closeRequested: false,
			};
		if (typeof state.disturbed !== "boolean") {
			state.disturbed = false;
		}
		if (typeof state.closeRequested !== "boolean") {
			state.closeRequested = false;
		}
		setState(stream, state);
		syncReadableController(stream, state);
		const originalGetReader = stream.getReader;
		stream.getReader = function getReader(options) {
			let reader;
			try {
				reader = originalGetReader.call(this, options);
			} catch (error) {
				throw normalizeInvalidStateError(error);
			}
			reader = decorateReader(reader, this);
			state.reader = reader;
			return reader;
		};
		const originalCancel = stream.cancel;
		stream.cancel = function cancel(reason) {
			let result;
			try {
				result = originalCancel.call(this, reason);
			} catch (error) {
				const normalized = normalizeInvalidStateError(error);
				return Promise.reject(normalized);
			}
			state.state = "closed";
			clearReadableControllerAlgorithms(state);
			return Promise.resolve(result).then((value) => {
				return value;
			}, (error) => {
				const normalized = normalizeInvalidStateError(error);
				throw normalized;
			});
		};
		transferState.defineTransferHooks(stream, (value) => value instanceof ReadableStream);
		return stream;
	}

	function decorateWritableStream(stream) {
		if (!isObject(stream) || stream[kDecorated]) return stream;
		defineHidden(stream, kDecorated, true);
		const state =
			stream[kState] ??
			{
				state: "writable",
				controller: undefined,
				writer: undefined,
				storedError: undefined,
			};
		setState(stream, state);
		syncWritableController(stream, state);
		const originalGetWriter = stream.getWriter;
		stream.getWriter = function getWriter() {
			let writer;
			try {
				writer = originalGetWriter.call(this);
			} catch (error) {
				throw normalizeInvalidStateError(error);
			}
			writer = decorateWritableWriter(writer, this);
			state.writer = writer;
			return writer;
		};
		const originalAbort = stream.abort;
		stream.abort = function abort(reason) {
			let result;
			try {
				result = originalAbort.call(this, reason);
			} catch (error) {
				throw normalizeInvalidStateError(error);
			}
			return Promise.resolve(result).then((value) => {
				state.state = "errored";
				state.storedError = reason;
				return value;
			}, (error) => {
				throw normalizeInvalidStateError(error);
			});
		};
		const originalClose = stream.close;
		if (typeof originalClose === "function") {
			stream.close = function close() {
				let result;
				try {
					result = originalClose.call(this);
				} catch (error) {
					throw normalizeInvalidStateError(error);
				}
				return Promise.resolve(result).then((value) => {
					state.state = "closed";
					return value;
				}, (error) => {
					throw normalizeInvalidStateError(error);
				});
			};
		}
		transferState.defineTransferHooks(stream, (value) => value instanceof WritableStream);
		return stream;
	}

	function decorateTransformStream(stream) {
		if (!isObject(stream) || stream[kDecorated]) return stream;
		defineHidden(stream, kDecorated, true);
		setState(stream, {});
		transferState.defineTransferHooks(stream, (value) => value instanceof TransformStream);
		return stream;
	}

	function ReadableStream(source, strategy) {
		if (typeof source !== "undefined" && (source === null || typeof source !== "object")) {
			throw createInvalidArgType('The "source" argument must be of type object.');
		}
		if (strategy != null && typeof strategy !== "object") {
			throw createInvalidArgType('The "strategy" argument must be of type object.');
		}
		if (
			strategy &&
			typeof strategy.size !== "undefined" &&
			typeof strategy.size !== "function"
		) {
			throw createInvalidArgType('The "strategy.size" argument must be of type function.');
		}
		if (
			strategy &&
			typeof strategy.highWaterMark !== "undefined" &&
			(typeof strategy.highWaterMark !== "number" ||
				Number.isNaN(strategy.highWaterMark) ||
				strategy.highWaterMark < 0)
		) {
			throw createInvalidArgValue('The property \'strategy.highWaterMark\' is invalid.');
		}
		const streamState = {
			state: "readable",
			controller: undefined,
			reader: undefined,
			storedError: undefined,
			disturbed: false,
			closeRequested: false,
		};
		const stream = new ponyfill.ReadableStream(wrapReadableSource(source, streamState), strategy);
		setState(stream, streamState);
		syncReadableController(stream, streamState);
		decorateReadableStream(stream);
		return stream;
	}
	ReadableStream.prototype = ponyfill.ReadableStream.prototype;
	Object.setPrototypeOf(ReadableStream, ponyfill.ReadableStream);
	if (typeof ponyfill.ReadableStream.from === "function") {
		ReadableStream.from = function from(iterable) {
			const isIterable =
				iterable != null &&
				(typeof iterable[Symbol.iterator] === "function" ||
					typeof iterable[Symbol.asyncIterator] === "function");
			if (!isIterable) {
				throw createCodeError("TypeError", "ERR_ARG_NOT_ITERABLE", "The provided value is not iterable");
			}
			return decorateReadableStream(ponyfill.ReadableStream.from(iterable));
		};
	}

	function WritableStream(sink, strategy) {
		if (typeof sink !== "undefined" && (sink === null || typeof sink !== "object")) {
			throw createInvalidArgType('The "sink" argument must be of type object.');
		}
		if (strategy != null && typeof strategy !== "object") {
			throw createInvalidArgType('The "strategy" argument must be of type object.');
		}
		if (
			sink &&
			typeof sink.type !== "undefined" &&
			sink.type !== undefined
		) {
			throw createInvalidArgValue('The property \'sink.type\' is invalid.');
		}
		if (
			strategy &&
			typeof strategy.size !== "undefined" &&
			typeof strategy.size !== "function"
		) {
			throw createInvalidArgType('The "strategy.size" argument must be of type function.');
		}
		if (
			strategy &&
			typeof strategy.highWaterMark !== "undefined" &&
			(typeof strategy.highWaterMark !== "number" ||
				Number.isNaN(strategy.highWaterMark) ||
				strategy.highWaterMark < 0)
		) {
			throw createInvalidArgValue('The property \'strategy.highWaterMark\' is invalid.');
		}
		const streamState = {
			state: "writable",
			controller: undefined,
			writer: undefined,
			storedError: undefined,
		};
		const stream = new ponyfill.WritableStream(wrapWritableSink(sink, streamState), strategy);
		setState(stream, streamState);
		syncWritableController(stream, streamState);
		decorateWritableStream(stream);
		return stream;
	}
	WritableStream.prototype = ponyfill.WritableStream.prototype;
	Object.setPrototypeOf(WritableStream, ponyfill.WritableStream);

	function TransformStream(transformer, writableStrategy, readableStrategy) {
		if (
			typeof transformer !== "undefined" &&
			(transformer === null || typeof transformer !== "object")
		) {
			throw createInvalidArgType('The "transformer" argument must be of type object.');
		}
		if (writableStrategy != null && typeof writableStrategy !== "object") {
			throw createInvalidArgType('The "writableStrategy" argument must be of type object.');
		}
		if (readableStrategy != null && typeof readableStrategy !== "object") {
			throw createInvalidArgType('The "readableStrategy" argument must be of type object.');
		}
		if (
			transformer &&
			typeof transformer.readableType !== "undefined" &&
			transformer.readableType !== undefined
		) {
			throw createInvalidArgValue('The property \'transformer.readableType\' is invalid.');
		}
		if (
			transformer &&
			typeof transformer.writableType !== "undefined" &&
			transformer.writableType !== undefined
		) {
			throw createInvalidArgValue('The property \'transformer.writableType\' is invalid.');
		}
		const stream = new ponyfill.TransformStream(
			wrapTransformer(transformer),
			writableStrategy,
			readableStrategy,
		);
		decorateTransformStream(stream);
		return stream;
	}
	TransformStream.prototype = ponyfill.TransformStream.prototype;
	Object.setPrototypeOf(TransformStream, ponyfill.TransformStream);

	const ReadableStreamDefaultReader = ponyfill.ReadableStreamDefaultReader;
	const ReadableStreamBYOBReader = ponyfill.ReadableStreamBYOBReader;
	const ReadableStreamBYOBRequest = wrapIllegalConstructor(
		ponyfill.ReadableStreamBYOBRequest,
		"ReadableStreamBYOBRequest",
	);
	const ReadableByteStreamController = wrapIllegalConstructor(
		ponyfill.ReadableByteStreamController,
		"ReadableByteStreamController",
	);
	const ReadableStreamDefaultController = wrapIllegalConstructor(
		ponyfill.ReadableStreamDefaultController,
		"ReadableStreamDefaultController",
	);
	const WritableStreamDefaultWriter = ponyfill.WritableStreamDefaultWriter;
	const WritableStreamDefaultController = ponyfill.WritableStreamDefaultController;
	const TransformStreamDefaultController = wrapIllegalConstructor(
		ponyfill.TransformStreamDefaultController,
		"TransformStreamDefaultController",
	);
	const ByteLengthQueuingStrategy = ponyfill.ByteLengthQueuingStrategy;
	const CountQueuingStrategy = ponyfill.CountQueuingStrategy;

	patchGetterBrand(
		ByteLengthQueuingStrategy.prototype,
		"highWaterMark",
		(value) => value instanceof ByteLengthQueuingStrategy,
		createPrivateMemberError,
	);
	patchGetterBrand(
		ByteLengthQueuingStrategy.prototype,
		"size",
		(value) => value instanceof ByteLengthQueuingStrategy,
		createPrivateMemberError,
	);
	patchGetterBrand(
		CountQueuingStrategy.prototype,
		"highWaterMark",
		(value) => value instanceof CountQueuingStrategy,
		createPrivateMemberError,
	);
	patchGetterBrand(
		CountQueuingStrategy.prototype,
		"size",
		(value) => value instanceof CountQueuingStrategy,
		createPrivateMemberError,
	);
	patchGetterBrand(ReadableStream.prototype, "locked", isReadableStreamInstance, () => createInvalidThis("Invalid this"));
	patchAsyncMethodBrand(
		ReadableStream.prototype,
		"cancel",
		isReadableStreamInstance,
		() => createInvalidThis("Invalid this"),
		function cancelWrapper(original, args) {
			try {
				return withInvalidStateNormalization(original.apply(this, args));
			} catch (error) {
				return Promise.reject(normalizeInvalidStateError(error));
			}
		},
	);
	patchMethodBrand(
		ReadableStream.prototype,
		"getReader",
		isReadableStreamInstance,
		() => createInvalidThis("Invalid this"),
		function getReaderWrapper(original, [options]) {
			if (typeof options !== "undefined" && (typeof options !== "object" || options === null)) {
				throw createInvalidArgType('The "options" argument must be of type object.');
			}
			const mode = options?.mode;
			if (
				typeof options !== "undefined" &&
				options !== null &&
				typeof mode !== "undefined" &&
				mode !== "byob"
			) {
				throw createInvalidArgValue('The property \'options.mode\' is invalid.');
			}
			try {
				return original.call(this, options);
			} catch (error) {
				throw normalizeInvalidStateError(error);
			}
		},
	);
	patchAsyncMethodBrand(
		ReadableStream.prototype,
		"pipeThrough",
		isReadableStreamInstance,
		() => createInvalidThis("Invalid this"),
		function pipeThroughWrapper(original, args) {
			try {
				return withInvalidStateNormalization(original.apply(this, args));
			} catch (error) {
				throw normalizeInvalidStateError(error);
			}
		},
	);
	patchAsyncMethodBrand(
		ReadableStream.prototype,
		"pipeTo",
		isReadableStreamInstance,
		() => createInvalidThis("Invalid this"),
		function pipeToWrapper(original, args) {
			try {
				return withInvalidStateNormalization(original.apply(this, args));
			} catch (error) {
				throw normalizeInvalidStateError(error);
			}
		},
	);
	patchMethodBrand(
		ReadableStream.prototype,
		"tee",
		isReadableStreamInstance,
		() => createInvalidThis("Invalid this"),
		function teeWrapper(original, args) {
			try {
				return original.apply(this, args);
			} catch (error) {
				throw normalizeInvalidStateError(error);
			}
		},
	);
	patchMethodBrand(
		ReadableStream.prototype,
		"values",
		isReadableStreamInstance,
		() => createInvalidThis("Invalid this"),
		function valuesWrapper(original, [options]) {
			if (typeof options !== "undefined" && (options === null || typeof options !== "object")) {
				throw createInvalidArgType('The "options" argument must be of type object.');
			}
			const stream = this;
			const preventCancel = Boolean(options?.preventCancel);
			const reader = stream.getReader();
			stream[kState].reader = reader;
			return {
				async next() {
					const result = await reader.read();
					if (result.done && stream.locked) {
						reader.releaseLock();
					}
					return result;
				},
				async return(value) {
					if (preventCancel) {
						reader.releaseLock();
						return { done: true, value };
					}
					try {
						await reader.cancel(value);
						return { done: true, value };
					} finally {
						if (stream.locked) {
							reader.releaseLock();
						}
					}
				},
				[Symbol.asyncIterator]() {
					return this;
				},
			};
		},
	);
	Object.defineProperty(ReadableStream.prototype, Symbol.asyncIterator, {
		value: function asyncIterator(options) {
			return this.values(options);
		},
		configurable: true,
		enumerable: false,
		writable: true,
	});
	patchRejectedGetterBrand(
		ReadableStreamDefaultReader.prototype,
		"closed",
		(value) => value instanceof ReadableStreamDefaultReader,
		() => createInvalidThis("Invalid this"),
		function closedGetterWrapper(originalGet) {
			return withInvalidStateNormalization(originalGet.call(this));
		},
	);
	patchAsyncMethodBrand(
		ReadableStreamDefaultReader.prototype,
		"read",
		(value) => value instanceof ReadableStreamDefaultReader,
		() => createInvalidThis("Invalid this"),
		function readerReadWrapper(original, args) {
			try {
				return withInvalidStateNormalization(original.apply(this, args));
			} catch (error) {
				throw normalizeInvalidStateError(error);
			}
		},
	);
	patchAsyncMethodBrand(
		ReadableStreamDefaultReader.prototype,
		"cancel",
		(value) => value instanceof ReadableStreamDefaultReader,
		() => createInvalidThis("Invalid this"),
		function readerCancelWrapper(original, args) {
			try {
				return withInvalidStateNormalization(original.apply(this, args)).then((value) => {
					const streamState = this[kState]?.stream?.[kState];
					if (streamState) {
						streamState.state = "closed";
						clearReadableControllerAlgorithms(streamState);
					}
					return value;
				});
			} catch (error) {
				return Promise.reject(normalizeInvalidStateError(error));
			}
		},
	);
	patchMethodBrand(ReadableStreamDefaultReader.prototype, "releaseLock", (value) => value instanceof ReadableStreamDefaultReader, () => createInvalidThis("Invalid this"));
	patchRejectedGetterBrand(
		ReadableStreamBYOBReader.prototype,
		"closed",
		(value) => value instanceof ReadableStreamBYOBReader,
		() => createInvalidThis("Invalid this"),
		function closedGetterWrapper(originalGet) {
			return withInvalidStateNormalization(originalGet.call(this));
		},
	);
	patchAsyncMethodBrand(
		ReadableStreamBYOBReader.prototype,
		"read",
		(value) => value instanceof ReadableStreamBYOBReader,
		() => createInvalidThis("Invalid this"),
		function byobReadWrapper(original, [view, ...rest]) {
			if (!ArrayBuffer.isView(view)) {
				throw createInvalidArgType('The "view" argument must be an instance of ArrayBufferView.');
			}
			const bufferView =
				typeof Buffer !== "undefined" && Buffer.isBuffer(view)
					? new Uint8Array(view.buffer, view.byteOffset, view.byteLength)
					: view;
			try {
				return withInvalidStateNormalization(
					original.call(this, bufferView, ...rest).then((result) => {
						if (
							typeof Buffer !== "undefined" &&
							Buffer.isBuffer(view) &&
							result &&
							ArrayBuffer.isView(result.value) &&
							!Buffer.isBuffer(result.value)
						) {
							return {
								done: result.done,
								value: Buffer.from(
									result.value.buffer,
									result.value.byteOffset,
									result.value.byteLength,
								),
							};
						}
						return result;
					}),
				);
			} catch (error) {
				throw normalizeInvalidStateError(error);
			}
		},
	);
	patchAsyncMethodBrand(
		ReadableStreamBYOBReader.prototype,
		"cancel",
		(value) => value instanceof ReadableStreamBYOBReader,
		() => createInvalidThis("Invalid this"),
		function byobCancelWrapper(original, args) {
			try {
				return withInvalidStateNormalization(original.apply(this, args)).then((value) => {
					const streamState = this[kState]?.stream?.[kState];
					if (streamState) {
						streamState.state = "closed";
						clearReadableControllerAlgorithms(streamState);
					}
					return value;
				});
			} catch (error) {
				return Promise.reject(normalizeInvalidStateError(error));
			}
		},
	);
	patchMethodBrand(ReadableStreamBYOBReader.prototype, "releaseLock", (value) => value instanceof ReadableStreamBYOBReader, () => createInvalidThis("Invalid this"));
	patchGetterBrand(ReadableStreamBYOBRequest.prototype, "view", (value) => value instanceof ponyfill.ReadableStreamBYOBRequest, () => createInvalidThis("Invalid this"));
	patchMethodBrand(
		ReadableStreamBYOBRequest.prototype,
		"respond",
		(value) => value instanceof ponyfill.ReadableStreamBYOBRequest,
		() => createInvalidThis("Invalid this"),
		function respondWrapper(original, args) {
			try {
				return original.apply(this, args);
			} catch (error) {
				throw normalizeInvalidStateError(error);
			}
		},
	);
	patchMethodBrand(
		ReadableStreamBYOBRequest.prototype,
		"respondWithNewView",
		(value) => value instanceof ponyfill.ReadableStreamBYOBRequest,
		() => createInvalidThis("Invalid this"),
		function respondWithNewViewWrapper(original, [view]) {
			if (!ArrayBuffer.isView(view)) {
				throw createInvalidArgType('The "view" argument must be an instance of ArrayBufferView.');
			}
			try {
				return original.call(this, view);
			} catch (error) {
				throw normalizeInvalidStateError(error);
			}
		},
	);
	patchGetterBrand(ReadableByteStreamController.prototype, "byobRequest", (value) => value instanceof ponyfill.ReadableByteStreamController, () => createInvalidThis("Invalid this"));
	patchGetterBrand(ReadableByteStreamController.prototype, "desiredSize", (value) => value instanceof ponyfill.ReadableByteStreamController, () => createInvalidThis("Invalid this"));
	patchMethodBrand(
		ReadableByteStreamController.prototype,
		"enqueue",
		(value) => value instanceof ponyfill.ReadableByteStreamController,
		() => createInvalidThis("Invalid this"),
		function enqueueWrapper(original, [chunk]) {
			if (!ArrayBuffer.isView(chunk)) {
				throw createInvalidArgType('The "chunk" argument must be an instance of ArrayBufferView.');
			}
			try {
				return original.call(this, chunk);
			} catch (error) {
				throw normalizeInvalidStateError(error);
			}
		},
	);
	patchMethodBrand(
		ReadableByteStreamController.prototype,
		"close",
		(value) => value instanceof ponyfill.ReadableByteStreamController,
		() => createInvalidThis("Invalid this"),
		function controllerCloseWrapper(original, args) {
			try {
				return original.apply(this, args);
			} catch (error) {
				throw normalizeInvalidStateError(error);
			}
		},
	);
	patchMethodBrand(ReadableByteStreamController.prototype, "error", (value) => value instanceof ponyfill.ReadableByteStreamController, () => createInvalidThis("Invalid this"));
	patchMethodBrand(
		ReadableByteStreamController.prototype,
		"respond",
		(value) => value instanceof ponyfill.ReadableByteStreamController,
		() => createInvalidThis("Invalid this"),
		function controllerRespondWrapper(original, args) {
			try {
				return original.apply(this, args);
			} catch (error) {
				throw normalizeInvalidStateError(error);
			}
		},
	);
	patchMethodBrand(
		ReadableByteStreamController.prototype,
		"respondWithNewView",
		(value) => value instanceof ponyfill.ReadableByteStreamController,
		() => createInvalidThis("Invalid this"),
		function controllerRespondWithNewViewWrapper(original, args) {
			try {
				return original.apply(this, args);
			} catch (error) {
				throw normalizeInvalidStateError(error);
			}
		},
	);
	patchMethodBrand(
		ReadableStreamDefaultController.prototype,
		"enqueue",
		(value) => value instanceof ponyfill.ReadableStreamDefaultController,
		() => createInvalidThis("Invalid this"),
		function defaultControllerEnqueueWrapper(original, args) {
			try {
				return original.apply(this, args);
			} catch (error) {
				throw normalizeInvalidStateError(error);
			}
		},
	);
	patchMethodBrand(
		ReadableStreamDefaultController.prototype,
		"close",
		(value) => value instanceof ponyfill.ReadableStreamDefaultController,
		() => createInvalidThis("Invalid this"),
		function defaultControllerCloseWrapper(original, args) {
			try {
				return original.apply(this, args);
			} catch (error) {
				throw normalizeInvalidStateError(error);
			}
		},
	);
	patchMethodBrand(
		ReadableStreamDefaultController.prototype,
		"error",
		(value) => value instanceof ponyfill.ReadableStreamDefaultController,
		() => createInvalidThis("Invalid this"),
		function defaultControllerErrorWrapper(original, args) {
			try {
				return original.apply(this, args);
			} catch (error) {
				throw normalizeInvalidStateError(error);
			}
		},
	);
	patchGetterBrand(WritableStream.prototype, "locked", isWritableStreamInstance, () => createInvalidThis("Invalid this"));
	patchAsyncMethodBrand(
		WritableStream.prototype,
		"abort",
		isWritableStreamInstance,
		() => createInvalidThis("Invalid this"),
		function writableAbortWrapper(original, args) {
			try {
				return withInvalidStateNormalization(original.apply(this, args));
			} catch (error) {
				throw normalizeInvalidStateError(error);
			}
		},
	);
	patchAsyncMethodBrand(
		WritableStream.prototype,
		"close",
		isWritableStreamInstance,
		() => createInvalidThis("Invalid this"),
		function writableCloseWrapper(original, args) {
			try {
				return withInvalidStateNormalization(original.apply(this, args));
			} catch (error) {
				throw normalizeInvalidStateError(error);
			}
		},
	);
	patchMethodBrand(
		WritableStream.prototype,
		"getWriter",
		isWritableStreamInstance,
		() => createInvalidThis("Invalid this"),
		function writableGetWriterWrapper(original, args) {
			try {
				return original.apply(this, args);
			} catch (error) {
				throw normalizeInvalidStateError(error);
			}
		},
	);
	patchRejectedGetterBrand(WritableStreamDefaultWriter.prototype, "closed", (value) => value instanceof WritableStreamDefaultWriter, () => createInvalidThis("Invalid this"));
	patchRejectedGetterBrand(WritableStreamDefaultWriter.prototype, "ready", (value) => value instanceof WritableStreamDefaultWriter, () => createInvalidThis("Invalid this"));
	patchGetterBrand(WritableStreamDefaultWriter.prototype, "desiredSize", (value) => value instanceof WritableStreamDefaultWriter, () => createInvalidThis("Invalid this"));
	patchAsyncMethodBrand(
		WritableStreamDefaultWriter.prototype,
		"abort",
		(value) => value instanceof WritableStreamDefaultWriter,
		() => createInvalidThis("Invalid this"),
	);
	patchAsyncMethodBrand(
		WritableStreamDefaultWriter.prototype,
		"close",
		(value) => value instanceof WritableStreamDefaultWriter,
		() => createInvalidThis("Invalid this"),
	);
	patchAsyncMethodBrand(
		WritableStreamDefaultWriter.prototype,
		"write",
		(value) => value instanceof WritableStreamDefaultWriter,
		() => createInvalidThis("Invalid this"),
	);
	patchMethodBrand(WritableStreamDefaultWriter.prototype, "releaseLock", (value) => value instanceof WritableStreamDefaultWriter, () => createInvalidThis("Invalid this"));
	patchGetterBrand(WritableStreamDefaultController.prototype, "signal", (value) => value instanceof WritableStreamDefaultController, () => createInvalidThis("Invalid this"));
	patchMethodBrand(WritableStreamDefaultController.prototype, "error", (value) => value instanceof WritableStreamDefaultController, () => createInvalidThis("Invalid this"));
	patchGetterBrand(TransformStream.prototype, "readable", isTransformStreamInstance, () => createInvalidThis("Invalid this"));
	patchGetterBrand(TransformStream.prototype, "writable", isTransformStreamInstance, () => createInvalidThis("Invalid this"));
	patchGetterBrand(TransformStreamDefaultController.prototype, "desiredSize", (value) => value instanceof ponyfill.TransformStreamDefaultController, () => createInvalidThis("Invalid this"));
	patchMethodBrand(TransformStreamDefaultController.prototype, "enqueue", (value) => value instanceof ponyfill.TransformStreamDefaultController, () => createInvalidThis("Invalid this"));
	patchMethodBrand(TransformStreamDefaultController.prototype, "error", (value) => value instanceof ponyfill.TransformStreamDefaultController, () => createInvalidThis("Invalid this"));
	patchMethodBrand(TransformStreamDefaultController.prototype, "terminate", (value) => value instanceof ponyfill.TransformStreamDefaultController, () => createInvalidThis("Invalid this"));

	class TextEncoderStream {
		constructor() {
			const encoder = new TextEncoder();
			const stream = new TransformStream({
				transform(chunk, controller) {
					controller.enqueue(encoder.encode(String(chunk)));
				},
			});
			this._stream = stream;
		}

		get encoding() {
			if (!(this instanceof TextEncoderStream)) {
				throw new TypeError("Cannot read private member");
			}
			return "utf-8";
		}

		get readable() {
			if (!(this instanceof TextEncoderStream)) {
				throw new TypeError("Cannot read private member");
			}
			return this._stream.readable;
		}

		get writable() {
			if (!(this instanceof TextEncoderStream)) {
				throw new TypeError("Cannot read private member");
			}
			return this._stream.writable;
		}
	}

	class TextDecoderStream {
		constructor(label, options) {
			if (options != null && (typeof options !== "object" || Array.isArray(options))) {
				throw createInvalidArgType('The "options" argument must be of type object.');
			}
			const decoder = new TextDecoder(label, options);
			const stream = new TransformStream({
				transform(chunk, controller) {
					controller.enqueue(decoder.decode(chunk, { stream: true }));
				},
				flush(controller) {
					const tail = decoder.decode();
					if (tail) {
						controller.enqueue(tail);
					}
				},
			});
			this._stream = stream;
			this._decoder = decoder;
		}

		get encoding() {
			if (!(this instanceof TextDecoderStream)) {
				throw new TypeError("Cannot read private member");
			}
			return this._decoder.encoding;
		}

		get fatal() {
			if (!(this instanceof TextDecoderStream)) {
				throw new TypeError("Cannot read private member");
			}
			return this._decoder.fatal;
		}

		get ignoreBOM() {
			if (!(this instanceof TextDecoderStream)) {
				throw new TypeError("Cannot read private member");
			}
			return this._decoder.ignoreBOM;
		}

		get readable() {
			if (!(this instanceof TextDecoderStream)) {
				throw new TypeError("Cannot read private member");
			}
			return this._stream.readable;
		}

		get writable() {
			if (!(this instanceof TextDecoderStream)) {
				throw new TypeError("Cannot read private member");
			}
			return this._stream.writable;
		}
	}

	function getCompressionFormat(format) {
		if (format !== "gzip" && format !== "deflate" && format !== "deflate-raw") {
			throw createInvalidArgValue(`The argument 'format' is invalid. Received ${format}`);
		}
		return format;
	}

	function createCompressionTransform(format, mode) {
		const runtimeRequire = getRuntimeRequire();
		if (!runtimeRequire) {
			throw new Error("require is not available in sandbox");
		}
		const zlib = runtimeRequire("zlib");
		const engine =
			mode === "compress"
				? format === "gzip"
					? zlib.createGzip()
					: format === "deflate"
						? zlib.createDeflate()
						: zlib.createDeflateRaw()
				: format === "gzip"
					? zlib.createGunzip()
					: format === "deflate"
						? zlib.createInflate()
						: zlib.createInflateRaw();

		return new TransformStream({
			start(controller) {
				engine.on("data", (chunk) => controller.enqueue(new Uint8Array(chunk)));
				engine.on("end", () => controller.terminate());
				engine.on("error", (error) => controller.error(error));
			},
			transform(chunk) {
				return new Promise((resolve, reject) => {
					engine.write(toBuffer(chunk), (error) => {
						if (error) reject(error);
						else resolve();
					});
				});
			},
			flush() {
				return new Promise((resolve, reject) => {
					engine.end((error) => {
						if (error) reject(error);
						else resolve();
					});
				});
			},
		});
	}

	class CompressionStream {
		constructor(format) {
			this._format = getCompressionFormat(format);
			this._stream = createCompressionTransform(this._format, "compress");
		}

		get readable() {
			if (!(this instanceof CompressionStream)) {
				throw new TypeError("Cannot read private member");
			}
			return this._stream.readable;
		}

		get writable() {
			if (!(this instanceof CompressionStream)) {
				throw new TypeError("Cannot read private member");
			}
			return this._stream.writable;
		}
	}

	class DecompressionStream {
		constructor(format) {
			this._format = getCompressionFormat(format);
			this._stream = createCompressionTransform(this._format, "decompress");
		}

		get readable() {
			if (!(this instanceof DecompressionStream)) {
				throw new TypeError("Cannot read private member");
			}
			return this._stream.readable;
		}

		get writable() {
			if (!(this instanceof DecompressionStream)) {
				throw new TypeError("Cannot read private member");
			}
			return this._stream.writable;
		}
	}

	function isReadableStream(value) {
		return isObject(value) && typeof value.getReader === "function" && value instanceof ReadableStream;
	}

	function isWritableStream(value) {
		return isObject(value) && typeof value.getWriter === "function" && value instanceof WritableStream;
	}

	function isTransformStream(value) {
		return isObject(value) && value instanceof TransformStream;
	}

	function newReadableStreamFromStreamReadable(readable) {
		if (!readable || typeof readable.on !== "function") {
			throw createInvalidArgType('The "readable" argument must be a stream.Readable.');
		}
		let canceled = false;
		let streamRef;
		const stream = new ReadableStream({
			start(controller) {
				if (readable.destroyed) {
					const existingError =
						readable.errored ??
						readable._readableState?.errored ??
						null;
					const state = streamRef?.[kState];
					if (existingError) {
						if (state) {
							state.state = "errored";
							state.storedError = existingError;
						}
						controller.error(existingError);
						return;
					}
					if (state) {
						state.state = "closed";
					}
					controller.close();
					return;
				}
				readable.pause?.();
				readable.on("data", (chunk) => controller.enqueue(chunk));
				readable.on("end", () => {
					const state = streamRef?.[kState];
					if (state) state.state = "closed";
					controller.close();
				});
				readable.on("error", (error) => {
					if (canceled) {
						return;
					}
					const state = streamRef?.[kState];
					if (state) {
						state.state = "errored";
						state.storedError = error;
					}
					controller.error(error);
				});
				readable.on("close", () => {
					if (canceled) {
						return;
					}
					const state = streamRef?.[kState];
					if (state?.state === "readable") {
						const error = createAbortError();
						state.state = "errored";
						state.storedError = error;
						controller.error(error);
					}
				});
			},
			cancel() {
				canceled = true;
				const state = stream[kState];
				if (state) {
					state.state = "closed";
				}
				readable.pause?.();
				if (!readable.destroyed) {
					readable.destroy(createAbortError());
				}
			},
		});
		streamRef = stream;
		return stream;
	}

	function newStreamReadableFromReadableStream(readableStream, options) {
		if (!isReadableStream(readableStream)) {
			throw createInvalidArgType('The "readableStream" argument must be a ReadableStream.');
		}
		const runtimeRequire = getRuntimeRequire();
		const { Readable } = runtimeRequire("stream");
		const reader = readableStream.getReader();
		const streamReadable = new Readable({
			...(options || {}),
			read() {
				reader.read().then(({ value, done }) => {
					if (done) {
						this.push(null);
						return;
					}
					if (options?.objectMode) {
						this.push(value);
						return;
					}
					if (typeof value === "string") {
						this.push(options?.encoding ? value : Buffer.from(value));
						return;
					}
					const buffer = toBuffer(value);
					this.push(options?.encoding ? buffer.toString(options.encoding) : buffer);
				}, (error) => {
					this._fromWebStreamErrored = true;
					this.destroy(error);
				});
			},
			destroy(error, callback) {
				if (!this._fromWebStreamErrored) {
					Promise.resolve(reader.cancel(error)).finally(() => callback(error));
					return;
				}
				callback(error);
			},
		});
		return streamReadable;
	}

	function newWritableStreamFromStreamWritable(writable) {
		if (!writable || typeof writable.write !== "function") {
			throw createInvalidArgType('The "writable" argument must be a stream.Writable.');
		}
		return new WritableStream({
			write(chunk) {
				const useObjectMode =
					writable.writableObjectMode === true ||
					writable._writableState?.objectMode === true;
				const normalizedChunk =
					useObjectMode || typeof chunk === "string" || (typeof Buffer !== "undefined" && Buffer.isBuffer(chunk))
						? chunk
						: toBuffer(chunk);
				return new Promise((resolve, reject) => {
					writable.write(normalizedChunk, (error) => {
						if (error) reject(error);
						else resolve();
					});
				});
			},
			close() {
				return new Promise((resolve, reject) => {
					let settled = false;
					const cleanup = () => {
						writable.off?.("error", onError);
						writable.off?.("close", onClose);
						writable.off?.("finish", onFinish);
					};
					const finishResolve = () => {
						if (settled) return;
						settled = true;
						cleanup();
						resolve();
					};
					const onError = (error) => {
						if (settled) return;
						settled = true;
						cleanup();
						reject(error);
					};
					const onClose = () => {
						finishResolve();
					};
					const onFinish = () => {
						process.nextTick(() => {
							if (settled) return;
							if (writable.destroyed || writable.closed) {
								finishResolve();
								return;
							}
							if (typeof writable.destroy === "function") {
								writable.destroy();
							} else {
								finishResolve();
							}
						});
					};
					writable.once?.("error", onError);
					writable.once?.("close", onClose);
					writable.once?.("finish", onFinish);
					writable.end();
				});
			},
			abort(reason) {
				return new Promise((resolve, reject) => {
					let settled = false;
					const cleanup = () => {
						writable.off?.("error", onError);
						writable.off?.("close", onClose);
					};
					const onError = (error) => {
						if (settled) return;
						settled = true;
						cleanup();
						if (error && error !== reason) {
							reject(error);
							return;
						}
						resolve();
					};
					const onClose = () => {
						if (settled) return;
						settled = true;
						cleanup();
						resolve();
					};
					writable.once?.("error", onError);
					writable.once?.("close", onClose);
					writable.destroy(reason);
				});
			},
		});
	}

	function newStreamWritableFromWritableStream(writableStream, options) {
		if (!isWritableStream(writableStream)) {
			throw createInvalidArgType('The "writableStream" argument must be a WritableStream.');
		}
		const runtimeRequire = getRuntimeRequire();
		const { Writable } = runtimeRequire("stream");
		const writer = writableStream.getWriter();
		return new Writable({
			...(options || {}),
			write(chunk, _encoding, callback) {
				const normalizedChunk =
					options?.objectMode || (options?.decodeStrings === false && typeof chunk === "string")
						? chunk
						: typeof chunk === "string"
							? Buffer.from(chunk)
							: chunk;
				writer.write(normalizedChunk).then(() => callback(), callback);
			},
			final(callback) {
				writer.close().then(() => callback(), callback);
			},
			destroy(error, callback) {
				Promise.resolve(error ? writer.abort(error) : writer.close()).finally(() => callback(error));
			},
		});
	}

	function newReadableWritablePairFromDuplex(duplex) {
		if (!duplex || typeof duplex.on !== "function" || typeof duplex.write !== "function") {
			throw createInvalidArgType('The "duplex" argument must be a stream.Duplex.');
		}
		return {
			readable: newReadableStreamFromStreamReadable(duplex),
			writable: newWritableStreamFromStreamWritable(duplex),
		};
	}

	function newStreamDuplexFromReadableWritablePair(pair, options) {
		if (!pair || !isReadableStream(pair.readable) || !isWritableStream(pair.writable)) {
			throw createInvalidArgType(
				'The "pair" argument must be an object with ReadableStream and WritableStream properties.',
			);
		}
		const runtimeRequire = getRuntimeRequire();
		const { Duplex } = runtimeRequire("stream");
		const reader = pair.readable.getReader();
		const writer = pair.writable.getWriter();
		const duplex = new Duplex({
			...(options || {}),
			read() {
				reader.read().then(({ value, done }) => {
					if (done) {
						this.push(null);
						return;
					}
					this.push(typeof value === "string" ? Buffer.from(value) : toBuffer(value));
				}, (error) => this.destroy(error));
			},
			write(chunk, _encoding, callback) {
				writer.write(chunk).then(() => callback(), callback);
			},
			final(callback) {
				writer.close().then(() => callback(), callback);
			},
			destroy(error, callback) {
				Promise.allSettled([
					reader.cancel(error),
					error ? writer.abort(error) : writer.close(),
				]).finally(() => callback(error));
			},
		});
		if (options?.encoding) {
			duplex.setEncoding(options.encoding);
		}
		return duplex;
	}

	function newWritableStreamFromStreamBase(stream) {
		return new WritableStream({
			write(chunk) {
				return new Promise((resolve, reject) => {
					if (typeof stream.onwrite !== "function") {
						resolve();
						return;
					}
					stream.onwrite(
						{
							oncomplete(error) {
								if (error) reject(error);
								else resolve();
							},
						},
						[toBuffer(chunk)],
					);
				});
			},
			close() {
				return new Promise((resolve) => {
					if (typeof stream.onshutdown !== "function") {
						resolve();
						return;
					}
					stream.onshutdown({
						oncomplete() {
							resolve();
						},
					});
				});
			},
		});
	}

	function newReadableStreamFromStreamBase(stream) {
		if (stream.onread) {
			throw createInvalidState("The stream is already reading");
		}
		return new ReadableStream({
			start(controller) {
				stream.onread = (chunk) => controller.enqueue(chunk);
				stream._secureExecOnEnd = () => controller.close();
			},
			cancel() {
				return new Promise((resolve) => {
					if (typeof stream.onshutdown !== "function") {
						resolve();
						return;
					}
					stream.onshutdown({
						oncomplete() {
							resolve();
						},
					});
				});
			},
		});
	}

	function readableStreamPipeTo(source, destination, preventClose, preventAbort, preventCancel, signal) {
		if (!isReadableStream(source)) {
			return Promise.reject(createInvalidArgType('The "source" argument must be a ReadableStream.'));
		}
		if (!isWritableStream(destination)) {
			return Promise.reject(createInvalidArgType('The "destination" argument must be a WritableStream.'));
		}
		if (signal != null && (typeof signal !== "object" || typeof signal.addEventListener !== "function")) {
			return Promise.reject(createInvalidArgType('The "signal" argument must be an AbortSignal.'));
		}
		return source.pipeTo(destination, {
			preventClose: Boolean(preventClose),
			preventAbort: Boolean(preventAbort),
			preventCancel: Boolean(preventCancel),
			signal,
		});
	}

	function readableStreamTee(stream) {
		return stream.tee();
	}

	function readableByteStreamControllerConvertPullIntoDescriptor(descriptor) {
		if (descriptor && descriptor.bytesFilled > descriptor.byteLength) {
			throw createInvalidState("Invalid pull-into descriptor");
		}
		return descriptor;
	}

	function readableStreamDefaultControllerEnqueue(controller, chunk) {
		if (controller?.[kState]?.streamState?.state !== "readable") return;
		controller.enqueue?.(chunk);
	}

	function readableByteStreamControllerEnqueue(controller, chunk) {
		if (controller?.[kState]?.streamState?.state !== "readable") return;
		controller.enqueue?.(chunk);
	}

	function readableStreamDefaultControllerCanCloseOrEnqueue(controller) {
		return controller?.[kState]?.streamState?.state === "readable";
	}

	function readableByteStreamControllerClose(controller) {
		if (controller?.[kState]?.streamState?.state !== "readable") return;
		controller.close?.();
	}

	function readableByteStreamControllerRespond(controller, bytesWritten) {
		if (controller?.[kState]?.pendingPullIntos?.length) {
			throw createInvalidArgValue("Invalid bytesWritten");
		}
		controller.respond?.(bytesWritten);
	}

	function readableStreamReaderGenericRelease(reader) {
		reader.releaseLock();
	}

	const state = {
		kState,
		isPromisePending: getPromiseState,
		ReadableStream,
		ReadableStreamDefaultReader,
		ReadableStreamBYOBReader,
		ReadableStreamBYOBRequest,
		ReadableByteStreamController,
		ReadableStreamDefaultController,
		WritableStream,
		WritableStreamDefaultController,
		WritableStreamDefaultWriter,
		TransformStream,
		TransformStreamDefaultController,
		ByteLengthQueuingStrategy,
		CountQueuingStrategy,
		TextEncoderStream,
		TextDecoderStream,
		CompressionStream,
		DecompressionStream,
		isReadableStream,
		isWritableStream,
		isTransformStream,
		newReadableStreamFromStreamReadable,
		newStreamReadableFromReadableStream,
		newWritableStreamFromStreamWritable,
		newStreamWritableFromWritableStream,
		newReadableWritablePairFromDuplex,
		newStreamDuplexFromReadableWritablePair,
		newWritableStreamFromStreamBase,
		newReadableStreamFromStreamBase,
		readableStreamPipeTo,
		readableStreamTee,
		readableByteStreamControllerConvertPullIntoDescriptor,
		readableStreamDefaultControllerEnqueue,
		readableByteStreamControllerEnqueue,
		readableStreamDefaultControllerCanCloseOrEnqueue,
		readableByteStreamControllerClose,
		readableByteStreamControllerRespond,
		readableStreamReaderGenericRelease,
		createInvalidThis,
		createIllegalConstructor,
	};

	ensureClassBrand(ReadableStream.prototype, ReadableStream, "ReadableStream");
	ensureClassBrand(ReadableStreamDefaultReader.prototype, ReadableStreamDefaultReader, "ReadableStreamDefaultReader");
	ensureClassBrand(ReadableStreamBYOBReader.prototype, ReadableStreamBYOBReader, "ReadableStreamBYOBReader");
	ensureClassBrand(ReadableStreamBYOBRequest.prototype, ReadableStreamBYOBRequest, "ReadableStreamBYOBRequest");
	ensureClassBrand(ReadableByteStreamController.prototype, ReadableByteStreamController, "ReadableByteStreamController");
	ensureClassBrand(ReadableStreamDefaultController.prototype, ReadableStreamDefaultController, "ReadableStreamDefaultController");
	ensureClassBrand(WritableStream.prototype, WritableStream, "WritableStream");
	ensureClassBrand(WritableStreamDefaultWriter.prototype, WritableStreamDefaultWriter, "WritableStreamDefaultWriter");
	ensureClassBrand(WritableStreamDefaultController.prototype, WritableStreamDefaultController, "WritableStreamDefaultController");
	ensureClassBrand(TransformStream.prototype, TransformStream, "TransformStream");
	ensureClassBrand(TransformStreamDefaultController.prototype, TransformStreamDefaultController, "TransformStreamDefaultController");
	ensureClassBrand(ByteLengthQueuingStrategy.prototype, ByteLengthQueuingStrategy, "ByteLengthQueuingStrategy");
	ensureClassBrand(CountQueuingStrategy.prototype, CountQueuingStrategy, "CountQueuingStrategy");
	ensureClassBrand(TextEncoderStream.prototype, TextEncoderStream, "TextEncoderStream");
	ensureClassBrand(TextDecoderStream.prototype, TextDecoderStream, "TextDecoderStream");
	ensureClassBrand(CompressionStream.prototype, CompressionStream, "CompressionStream");
	ensureClassBrand(DecompressionStream.prototype, DecompressionStream, "DecompressionStream");

	Object.defineProperty(ReadableStream, "name", { value: "ReadableStream" });
	Object.defineProperty(ReadableStreamDefaultReader, "name", { value: "ReadableStreamDefaultReader" });
	Object.defineProperty(ReadableStreamBYOBReader, "name", { value: "ReadableStreamBYOBReader" });
	Object.defineProperty(ReadableStreamBYOBRequest, "name", { value: "ReadableStreamBYOBRequest" });
	Object.defineProperty(ReadableByteStreamController, "name", { value: "ReadableByteStreamController" });
	Object.defineProperty(ReadableStreamDefaultController, "name", { value: "ReadableStreamDefaultController" });
	Object.defineProperty(WritableStream, "name", { value: "WritableStream" });
	Object.defineProperty(WritableStreamDefaultWriter, "name", { value: "WritableStreamDefaultWriter" });
	Object.defineProperty(WritableStreamDefaultController, "name", { value: "WritableStreamDefaultController" });
	Object.defineProperty(TransformStream, "name", { value: "TransformStream" });
	Object.defineProperty(TransformStreamDefaultController, "name", { value: "TransformStreamDefaultController" });
	Object.defineProperty(ByteLengthQueuingStrategy, "name", { value: "ByteLengthQueuingStrategy" });
	Object.defineProperty(CountQueuingStrategy, "name", { value: "CountQueuingStrategy" });
	Object.defineProperty(TextEncoderStream, "name", { value: "TextEncoderStream" });
	Object.defineProperty(TextDecoderStream, "name", { value: "TextDecoderStream" });
	Object.defineProperty(CompressionStream, "name", { value: "CompressionStream" });
	Object.defineProperty(DecompressionStream, "name", { value: "DecompressionStream" });

	transferState.defineTransferHooks(ReadableStream.prototype, (value) => value instanceof ReadableStream);
	transferState.defineTransferHooks(WritableStream.prototype, (value) => value instanceof WritableStream);
	transferState.defineTransferHooks(TransformStream.prototype, (value) => value instanceof TransformStream);

	ensureInspect(ByteLengthQueuingStrategy.prototype, "ByteLengthQueuingStrategy", function inspectStrategy() {
		return `ByteLengthQueuingStrategy { highWaterMark: ${this.highWaterMark} }`;
	});
	ensureInspect(CountQueuingStrategy.prototype, "CountQueuingStrategy", function inspectStrategy() {
		return `CountQueuingStrategy { highWaterMark: ${this.highWaterMark} }`;
	});
	ensureInspect(ReadableStream.prototype, "ReadableStream", function inspectReadable() {
		const current = this[kState];
		const supportsBYOB = current?.controller instanceof ponyfill.ReadableByteStreamController;
		return `ReadableStream { locked: ${this.locked}, state: '${current?.state ?? "readable"}', supportsBYOB: ${supportsBYOB} }`;
	});
	ensureInspect(WritableStream.prototype, "WritableStream", function inspectWritable() {
		const current = this[kState];
		return `WritableStream { locked: ${this.locked}, state: '${current?.state ?? "writable"}' }`;
	});
	ensureInspect(TransformStream.prototype, "TransformStream", function inspectTransform() {
		return "TransformStream {}";
	});
	ensureInspect(ReadableStreamDefaultReader.prototype, "ReadableStreamDefaultReader", function inspectReader() {
		return "ReadableStreamDefaultReader {}";
	});
	ensureInspect(ReadableStreamBYOBReader.prototype, "ReadableStreamBYOBReader", function inspectReader() {
		return "ReadableStreamBYOBReader {}";
	});
	ensureInspect(ReadableStreamBYOBRequest.prototype, "ReadableStreamBYOBRequest", function inspectRequest() {
		return "ReadableStreamBYOBRequest {}";
	});
	ensureInspect(ReadableByteStreamController.prototype, "ReadableByteStreamController", function inspectController() {
		return "ReadableByteStreamController {}";
	});
	ensureInspect(ReadableStreamDefaultController.prototype, "ReadableStreamDefaultController", function inspectController() {
		return "ReadableStreamDefaultController {}";
	});
	defineHidden(
		ReadableStreamDefaultController.prototype,
		inspectSymbol,
		function inspectController() {
			return "ReadableStreamDefaultController {}";
		},
	);
	ensureInspect(WritableStreamDefaultWriter.prototype, "WritableStreamDefaultWriter", function inspectWriter() {
		return "WritableStreamDefaultWriter {}";
	});
	ensureInspect(WritableStreamDefaultController.prototype, "WritableStreamDefaultController", function inspectController() {
		return "WritableStreamDefaultController {}";
	});
	ensureInspect(TransformStreamDefaultController.prototype, "TransformStreamDefaultController", function inspectController() {
		return "TransformStreamDefaultController {}";
	});

	globalThis[SHARED_KEY] = state;
	return state;
}
