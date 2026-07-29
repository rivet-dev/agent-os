import { exposeCustomGlobal } from "../global-exposure.js";
import { Response } from "./fetch.js";
import { _fdGetPath, _fs, decodeBridgeJson } from "./fs.js";
import {
	createErrorWithCode,
	createTypeErrorWithCode,
	dispatchHttp2CompatibilityRequest,
	formatReceivedType,
	http,
} from "./http.js";
import {
	buildSerializedTlsOptions,
	normalizeListenArgs,
	normalizeSocketTimeout,
} from "./net.js";
import {
	registerCapabilityReadiness,
	unregisterCapabilityReadiness,
} from "./readiness.js";
import { setImmediate } from "./timers.js";

var HTTP2_K_SOCKET = /* @__PURE__ */ Symbol.for("agentos.http2.kSocket");

var HTTP2_OPTIONS = /* @__PURE__ */ Symbol("options");

var http2Servers = /* @__PURE__ */ new Map();

var http2Sessions = /* @__PURE__ */ new Map();

var http2Streams = /* @__PURE__ */ new Map();

var pendingHttp2ClientStreamEvents = /* @__PURE__ */ new Map();

var scheduledHttp2ClientStreamFlushes = /* @__PURE__ */ new Set();

var queuedHttp2DispatchEvents = [];

var pendingHttp2CompatRequests = /* @__PURE__ */ new Map();

var scheduledHttp2DispatchDrain = false;

var retainedHttp2Handles = /* @__PURE__ */ new Map();

var nextHttp2ServerId = 1;

var Http2EventEmitter = class {
	_listeners = {};
	_onceListeners = {};
	on(event, listener) {
		if (!this._listeners[event]) this._listeners[event] = [];
		this._listeners[event].push(listener);
		return this;
	}
	addListener(event, listener) {
		return this.on(event, listener);
	}
	once(event, listener) {
		if (!this._onceListeners[event]) this._onceListeners[event] = [];
		this._onceListeners[event].push(listener);
		return this;
	}
	removeListener(event, listener) {
		const remove = (target) => {
			if (!target) return;
			const index = target.indexOf(listener);
			if (index !== -1) target.splice(index, 1);
		};
		remove(this._listeners[event]);
		remove(this._onceListeners[event]);
		return this;
	}
	off(event, listener) {
		return this.removeListener(event, listener);
	}
	listenerCount(event) {
		return (
			(this._listeners[event]?.length ?? 0) +
			(this._onceListeners[event]?.length ?? 0)
		);
	}
	setMaxListeners(_value) {
		return this;
	}
	emit(event, ...args) {
		let handled = false;
		const listeners = this._listeners[event];
		if (listeners) {
			for (const listener of [...listeners]) {
				listener.call(this, ...args);
				handled = true;
			}
		}
		const onceListeners = this._onceListeners[event];
		if (onceListeners) {
			this._onceListeners[event] = [];
			for (const listener of [...onceListeners]) {
				listener.call(this, ...args);
				handled = true;
			}
		}
		return handled;
	}
};

var Http2SocketProxy = class extends Http2EventEmitter {
	allowHalfOpen = false;
	encrypted = false;
	localAddress = "127.0.0.1";
	localPort = 0;
	localFamily = "IPv4";
	remoteAddress = "127.0.0.1";
	remotePort = 0;
	remoteFamily = "IPv4";
	servername;
	alpnProtocol = false;
	readable = true;
	writable = true;
	destroyed = false;
	_bridgeReadPollTimer = null;
	_loopbackServer = null;
	_onDestroy;
	_destroyCallbackInvoked = false;
	constructor(state, onDestroy) {
		super();
		this._onDestroy = onDestroy;
		this._applyState(state);
	}
	_applyState(state) {
		if (!state) return;
		this.allowHalfOpen = state.allowHalfOpen === true;
		this.encrypted = state.encrypted === true;
		this.localAddress = state.localAddress ?? this.localAddress;
		this.localPort = state.localPort ?? this.localPort;
		this.localFamily = state.localFamily ?? this.localFamily;
		this.remoteAddress = state.remoteAddress ?? this.remoteAddress;
		this.remotePort = state.remotePort ?? this.remotePort;
		this.remoteFamily = state.remoteFamily ?? this.remoteFamily;
		this.servername = state.servername;
		this.alpnProtocol = state.alpnProtocol ?? this.alpnProtocol;
	}
	_clearTimeoutTimer() {}
	_emitNet(event, error) {
		if (event === "error" && error) {
			this.emit("error", error);
			return;
		}
		if (event === "close") {
			if (!this._destroyCallbackInvoked) {
				this._destroyCallbackInvoked = true;
				queueMicrotask(() => {
					this._onDestroy?.();
				});
			}
			this.emit("close");
		}
	}
	end() {
		this.destroyed = true;
		this.readable = false;
		this.writable = false;
		this.emit("close");
		return this;
	}
	destroy() {
		if (this.destroyed) {
			return this;
		}
		this.destroyed = true;
		this.readable = false;
		this.writable = false;
		this._emitNet("close");
		return this;
	}
};

function createHttp2ArgTypeError(argumentName, expected, value) {
	return createTypeErrorWithCode(
		`The "${argumentName}" argument must be of type ${expected}. Received ${formatReceivedType(value)}`,
		"ERR_INVALID_ARG_TYPE",
	);
}

function createHttp2Error(code, message) {
	return createErrorWithCode(message, code);
}

function createHttp2SettingRangeError(setting, value) {
	const error = new RangeError(
		`Invalid value for setting "${setting}": ${String(value)}`,
	);
	error.code = "ERR_HTTP2_INVALID_SETTING_VALUE";
	return error;
}

function createHttp2SettingTypeError(setting, value) {
	const error = new TypeError(
		`Invalid value for setting "${setting}": ${String(value)}`,
	);
	error.code = "ERR_HTTP2_INVALID_SETTING_VALUE";
	return error;
}

var HTTP2_INTERNAL_BINDING_CONSTANTS = {
	NGHTTP2_NO_ERROR: 0,
	NGHTTP2_PROTOCOL_ERROR: 1,
	NGHTTP2_INTERNAL_ERROR: 2,
	NGHTTP2_FLOW_CONTROL_ERROR: 3,
	NGHTTP2_SETTINGS_TIMEOUT: 4,
	NGHTTP2_STREAM_CLOSED: 5,
	NGHTTP2_FRAME_SIZE_ERROR: 6,
	NGHTTP2_REFUSED_STREAM: 7,
	NGHTTP2_CANCEL: 8,
	NGHTTP2_COMPRESSION_ERROR: 9,
	NGHTTP2_CONNECT_ERROR: 10,
	NGHTTP2_ENHANCE_YOUR_CALM: 11,
	NGHTTP2_INADEQUATE_SECURITY: 12,
	NGHTTP2_HTTP_1_1_REQUIRED: 13,
	NGHTTP2_NV_FLAG_NONE: 0,
	NGHTTP2_NV_FLAG_NO_INDEX: 1,
	NGHTTP2_ERR_DEFERRED: -508,
	NGHTTP2_ERR_STREAM_ID_NOT_AVAILABLE: -509,
	NGHTTP2_ERR_STREAM_CLOSED: -510,
	NGHTTP2_ERR_INVALID_ARGUMENT: -501,
	NGHTTP2_ERR_FRAME_SIZE_ERROR: -522,
	NGHTTP2_ERR_NOMEM: -901,
	NGHTTP2_FLAG_NONE: 0,
	NGHTTP2_FLAG_END_STREAM: 1,
	NGHTTP2_FLAG_END_HEADERS: 4,
	NGHTTP2_FLAG_ACK: 1,
	NGHTTP2_FLAG_PADDED: 8,
	NGHTTP2_FLAG_PRIORITY: 32,
	NGHTTP2_DEFAULT_WEIGHT: 16,
	NGHTTP2_SETTINGS_HEADER_TABLE_SIZE: 1,
	NGHTTP2_SETTINGS_ENABLE_PUSH: 2,
	NGHTTP2_SETTINGS_MAX_CONCURRENT_STREAMS: 3,
	NGHTTP2_SETTINGS_INITIAL_WINDOW_SIZE: 4,
	NGHTTP2_SETTINGS_MAX_FRAME_SIZE: 5,
	NGHTTP2_SETTINGS_MAX_HEADER_LIST_SIZE: 6,
	NGHTTP2_SETTINGS_ENABLE_CONNECT_PROTOCOL: 8,
};

var HTTP2_NGHTTP2_ERROR_MESSAGES = {
	[HTTP2_INTERNAL_BINDING_CONSTANTS.NGHTTP2_ERR_DEFERRED]: "Data deferred",
	[HTTP2_INTERNAL_BINDING_CONSTANTS.NGHTTP2_ERR_STREAM_ID_NOT_AVAILABLE]:
		"Stream ID is not available",
	[HTTP2_INTERNAL_BINDING_CONSTANTS.NGHTTP2_ERR_STREAM_CLOSED]:
		"Stream was already closed or invalid",
	[HTTP2_INTERNAL_BINDING_CONSTANTS.NGHTTP2_ERR_INVALID_ARGUMENT]:
		"Invalid argument",
	[HTTP2_INTERNAL_BINDING_CONSTANTS.NGHTTP2_ERR_FRAME_SIZE_ERROR]:
		"Frame size error",
	[HTTP2_INTERNAL_BINDING_CONSTANTS.NGHTTP2_ERR_NOMEM]: "Out of memory",
};

var NghttpError = class extends Error {
	code = "ERR_HTTP2_ERROR";
	constructor(message) {
		super(message);
		this.name = "Error";
	}
};

function nghttp2ErrorString(code) {
	return HTTP2_NGHTTP2_ERROR_MESSAGES[code] ?? `HTTP/2 error (${String(code)})`;
}

function createHttp2InvalidArgValueError(property, value) {
	return createTypeErrorWithCode(
		`The property 'options.${property}' is invalid. Received ${formatHttp2InvalidValue(value)}`,
		"ERR_INVALID_ARG_VALUE",
	);
}

function formatHttp2InvalidValue(value) {
	if (typeof value === "function") {
		return `[Function${value.name ? `: ${value.name}` : ": function"}]`;
	}
	if (typeof value === "symbol") {
		return value.toString();
	}
	if (Array.isArray(value)) {
		return "[]";
	}
	if (value === null) {
		return "null";
	}
	if (typeof value === "object") {
		return "{}";
	}
	return String(value);
}

function createHttp2PayloadForbiddenError(statusCode) {
	return createHttp2Error(
		"ERR_HTTP2_PAYLOAD_FORBIDDEN",
		`Responses with ${String(statusCode)} status must not have a payload`,
	);
}

var S_IFMT = 61440;

var S_IFDIR = 16384;

var S_IFREG = 32768;

var S_IFIFO = 4096;

var S_IFSOCK = 49152;

var S_IFLNK = 40960;

function createHttp2BridgeStat(stat) {
	const atimeMs = stat.atimeMs ?? 0;
	const mtimeMs = stat.mtimeMs ?? atimeMs;
	const ctimeMs = stat.ctimeMs ?? mtimeMs;
	const birthtimeMs = stat.birthtimeMs ?? ctimeMs;
	const fileType = stat.mode & S_IFMT;
	return {
		size: stat.size,
		mode: stat.mode,
		atimeMs,
		mtimeMs,
		ctimeMs,
		birthtimeMs,
		atime: new Date(atimeMs),
		mtime: new Date(mtimeMs),
		ctime: new Date(ctimeMs),
		birthtime: new Date(birthtimeMs),
		isFile: () => fileType === S_IFREG,
		isDirectory: () => fileType === S_IFDIR,
		isFIFO: () => fileType === S_IFIFO,
		isSocket: () => fileType === S_IFSOCK,
		isSymbolicLink: () => fileType === S_IFLNK,
	};
}

function normalizeHttp2FileResponseOptions(options) {
	const normalized = options ?? {};
	const offset = normalized.offset;
	if (
		offset !== void 0 &&
		(typeof offset !== "number" || !Number.isFinite(offset))
	) {
		throw createHttp2InvalidArgValueError("offset", offset);
	}
	const length = normalized.length;
	if (
		length !== void 0 &&
		(typeof length !== "number" || !Number.isFinite(length))
	) {
		throw createHttp2InvalidArgValueError("length", length);
	}
	const statCheck = normalized.statCheck;
	if (statCheck !== void 0 && typeof statCheck !== "function") {
		throw createHttp2InvalidArgValueError("statCheck", statCheck);
	}
	const onError = normalized.onError;
	return {
		offset: offset === void 0 ? 0 : Math.max(0, Math.trunc(offset)),
		length: typeof length === "number" ? Math.trunc(length) : void 0,
		statCheck: typeof statCheck === "function" ? statCheck : void 0,
		onError: typeof onError === "function" ? onError : void 0,
	};
}

function sliceHttp2FileBody(body, offset, length) {
	const safeOffset = Math.max(0, Math.min(offset, body.length));
	if (length === void 0 || length < 0) {
		return body.subarray(safeOffset);
	}
	return body.subarray(safeOffset, Math.min(body.length, safeOffset + length));
}

var Http2Stream = class {
	constructor(_streamId) {
		this._streamId = _streamId;
	}
	_streamId;
	respond(headers) {
		if (typeof _networkHttp2StreamRespondRaw === "undefined") {
			throw new Error("http2 server stream respond bridge is not available");
		}
		_networkHttp2StreamRespondRaw.applySync(void 0, [
			this._streamId,
			serializeHttp2Headers(headers),
		]);
		return 0;
	}
};

var DEFAULT_HTTP2_SETTINGS = {
	headerTableSize: 4096,
	enablePush: true,
	initialWindowSize: 65535,
	maxFrameSize: 16384,
	maxConcurrentStreams: 4294967295,
	maxHeaderListSize: 65535,
	maxHeaderSize: 65535,
	enableConnectProtocol: false,
};

var DEFAULT_HTTP2_SESSION_STATE = {
	effectiveLocalWindowSize: 65535,
	localWindowSize: 65535,
	remoteWindowSize: 65535,
	nextStreamID: 1,
	outboundQueueSize: 1,
	deflateDynamicTableSize: 0,
	inflateDynamicTableSize: 0,
};

function cloneHttp2Settings(settings) {
	const cloned = {};
	for (const [key, value] of Object.entries(settings ?? {})) {
		if (key === "customSettings" && value && typeof value === "object") {
			const customSettings = {};
			for (const [customKey, customValue] of Object.entries(value)) {
				customSettings[Number(customKey)] = Number(customValue);
			}
			cloned.customSettings = customSettings;
			continue;
		}
		cloned[key] = value;
	}
	return cloned;
}

function cloneHttp2SessionRuntimeState(state) {
	return {
		...DEFAULT_HTTP2_SESSION_STATE,
		...(state ?? {}),
	};
}

function parseHttp2SessionRuntimeState(state) {
	if (!state || typeof state !== "object") {
		return void 0;
	}
	const record = state;
	const parsed = {};
	const numericKeys = [
		"effectiveLocalWindowSize",
		"localWindowSize",
		"remoteWindowSize",
		"nextStreamID",
		"outboundQueueSize",
		"deflateDynamicTableSize",
		"inflateDynamicTableSize",
	];
	for (const key of numericKeys) {
		if (typeof record[key] === "number") {
			parsed[key] = record[key];
		}
	}
	return parsed;
}

function validateHttp2Settings(settings, argumentName = "settings") {
	if (!settings || typeof settings !== "object" || Array.isArray(settings)) {
		throw createHttp2ArgTypeError(argumentName, "object", settings);
	}
	const record = settings;
	const normalized = {};
	const numberRanges = {
		headerTableSize: [0, 4294967295],
		initialWindowSize: [0, 4294967295],
		maxFrameSize: [16384, 16777215],
		maxConcurrentStreams: [0, 4294967295],
		maxHeaderListSize: [0, 4294967295],
		maxHeaderSize: [0, 4294967295],
	};
	for (const [key, value] of Object.entries(record)) {
		if (value === void 0) {
			continue;
		}
		if (key === "enablePush" || key === "enableConnectProtocol") {
			if (typeof value !== "boolean") {
				throw createHttp2SettingTypeError(key, value);
			}
			normalized[key] = value;
			continue;
		}
		if (key === "customSettings") {
			if (!value || typeof value !== "object" || Array.isArray(value)) {
				throw createHttp2SettingRangeError(key, value);
			}
			const customSettings = {};
			for (const [customKey, customValue] of Object.entries(value)) {
				const numericKey = Number(customKey);
				if (
					!Number.isInteger(numericKey) ||
					numericKey < 0 ||
					numericKey > 65535
				) {
					throw createHttp2SettingRangeError(key, value);
				}
				if (
					typeof customValue !== "number" ||
					!Number.isInteger(customValue) ||
					customValue < 0 ||
					customValue > 4294967295
				) {
					throw createHttp2SettingRangeError(key, value);
				}
				customSettings[numericKey] = customValue;
			}
			normalized.customSettings = customSettings;
			continue;
		}
		if (key in numberRanges) {
			const [min, max] = numberRanges[key];
			if (
				typeof value !== "number" ||
				!Number.isInteger(value) ||
				value < min ||
				value > max
			) {
				throw createHttp2SettingRangeError(key, value);
			}
			normalized[key] = value;
			continue;
		}
		normalized[key] = value;
	}
	return normalized;
}

function serializeHttp2Headers(headers) {
	return JSON.stringify(headers ?? {});
}

function parseHttp2Headers(headersJson) {
	if (!headersJson) {
		return {};
	}
	try {
		const parsed = JSON.parse(headersJson);
		return parsed && typeof parsed === "object" ? parsed : {};
	} catch {
		return {};
	}
}

function parseHttp2SessionState(data) {
	if (!data) {
		return null;
	}
	try {
		const parsed = JSON.parse(data);
		return parsed && typeof parsed === "object" ? parsed : null;
	} catch {
		return null;
	}
}

function parseHttp2SocketState(data) {
	if (!data) {
		return null;
	}
	try {
		const parsed = JSON.parse(data);
		return parsed && typeof parsed === "object" ? parsed : null;
	} catch {
		return null;
	}
}

function parseHttp2ErrorPayload(data) {
	if (!data) {
		return new Error("Unknown HTTP/2 bridge error");
	}
	try {
		const parsed = JSON.parse(data);
		const error = new Error(parsed.message ?? "Unknown HTTP/2 bridge error");
		if (parsed.name) error.name = parsed.name;
		if (parsed.code) error.code = parsed.code;
		return error;
	} catch {
		return new Error(data);
	}
}

function normalizeHttp2Headers(headers) {
	const normalized = {};
	if (!headers || typeof headers !== "object") {
		return normalized;
	}
	for (const [key, value] of Object.entries(headers)) {
		normalized[String(key)] = value;
	}
	return normalized;
}

function validateHttp2RequestOptions(options) {
	if (!options) {
		return;
	}
	const validators = {
		endStream: "boolean",
		weight: "number",
		parent: "number",
		exclusive: "boolean",
		silent: "boolean",
	};
	for (const [key, expectedType] of Object.entries(validators)) {
		if (!(key in options) || options[key] === void 0) {
			continue;
		}
		const value = options[key];
		if (expectedType === "boolean" && typeof value !== "boolean") {
			throw createHttp2ArgTypeError(key, "boolean", value);
		}
		if (expectedType === "number" && typeof value !== "number") {
			throw createHttp2ArgTypeError(key, "number", value);
		}
	}
}

function validateHttp2ConnectOptions(options) {
	if (!options || !options.settings || typeof options.settings !== "object") {
		return;
	}
	const settings = options.settings;
	if ("maxFrameSize" in settings) {
		const value = settings.maxFrameSize;
		if (
			typeof value !== "number" ||
			!Number.isInteger(value) ||
			value < 16384 ||
			value > 16777215
		) {
			throw createHttp2SettingRangeError("maxFrameSize", value);
		}
	}
}

function applyHttp2SessionState(session, state) {
	if (!state) {
		return;
	}
	session.encrypted = state.encrypted === true;
	session.alpnProtocol =
		state.alpnProtocol ?? (session.encrypted ? "h2" : "h2c");
	session.originSet =
		Array.isArray(state.originSet) && state.originSet.length > 0
			? [...state.originSet]
			: session.encrypted
				? []
				: void 0;
	if (state.localSettings && typeof state.localSettings === "object") {
		session.localSettings = cloneHttp2Settings(state.localSettings);
	}
	if (state.remoteSettings && typeof state.remoteSettings === "object") {
		session.remoteSettings = cloneHttp2Settings(state.remoteSettings);
	}
	if (state.state && typeof state.state === "object") {
		session._applyRuntimeState(parseHttp2SessionRuntimeState(state.state));
	}
	session.socket._applyState(state.socket);
}

function normalizeHttp2Authority(authority, options) {
	if (authority instanceof URL) {
		return authority;
	}
	if (typeof authority === "string") {
		return new URL(authority);
	}
	if (authority && typeof authority === "object") {
		const record = authority;
		const protocol =
			typeof (options?.protocol ?? record.protocol) === "string"
				? String(options?.protocol ?? record.protocol)
				: "http:";
		const hostname =
			typeof (
				options?.host ??
				record.host ??
				options?.hostname ??
				record.hostname
			) === "string"
				? String(
						options?.host ??
							record.host ??
							options?.hostname ??
							record.hostname,
					)
				: "localhost";
		const portValue = options?.port ?? record.port;
		const port = portValue === void 0 ? "" : String(portValue);
		return new URL(`${protocol}//${hostname}${port ? `:${port}` : ""}`);
	}
	return new URL("http://localhost");
}

function normalizeHttp2ConnectArgs(
	authorityOrOptions,
	optionsOrListener,
	maybeListener,
) {
	const listener =
		typeof optionsOrListener === "function"
			? optionsOrListener
			: typeof maybeListener === "function"
				? maybeListener
				: void 0;
	const options =
		typeof optionsOrListener === "function" ? {} : (optionsOrListener ?? {});
	return {
		authority: normalizeHttp2Authority(authorityOrOptions, options),
		options,
		listener,
	};
}

function resolveHttp2SocketId(socket) {
	if (!socket || typeof socket !== "object") {
		return void 0;
	}
	const value = socket._socketId;
	return typeof value === "number" && Number.isFinite(value) ? value : void 0;
}

var ClientHttp2Stream = class extends Http2EventEmitter {
	_streamId;
	_encoding;
	_utf8Remainder;
	_isPushStream;
	_session;
	_receivedResponse = false;
	_needsDrain = false;
	_pendingWritableBytes = 0;
	_drainScheduled = false;
	_writableHighWaterMark = 16 * 1024;
	rstCode = 0;
	readable = true;
	writable = true;
	writableEnded = false;
	writableFinished = false;
	destroyed = false;
	_writableState = {
		ended: false,
		finished: false,
		objectMode: false,
		corked: 0,
		length: 0,
	};
	constructor(streamId, session, isPushStream = false) {
		super();
		this._streamId = streamId;
		this._session = session;
		this._isPushStream = isPushStream;
		if (!isPushStream) {
			queueMicrotask(() => {
				this.emit("ready");
			});
		}
	}
	setEncoding(encoding) {
		this._encoding = encoding;
		this._utf8Remainder =
			this._encoding === "utf8" || this._encoding === "utf-8"
				? Buffer.alloc(0)
				: void 0;
		return this;
	}
	close() {
		this.end();
		return this;
	}
	destroy(error) {
		if (this.destroyed) {
			return this;
		}
		this.destroyed = true;
		if (error) {
			this.emit("error", error);
		}
		this.end();
		return this;
	}
	_scheduleDrain() {
		if (!this._needsDrain || this._drainScheduled) {
			return;
		}
		this._drainScheduled = true;
		queueMicrotask(() => {
			this._drainScheduled = false;
			if (!this._needsDrain) {
				return;
			}
			this._needsDrain = false;
			this._pendingWritableBytes = 0;
			this.emit("drain");
		});
	}
	write(data, encodingOrCallback, callback) {
		if (typeof _networkHttp2StreamWriteRaw === "undefined") {
			throw new Error("http2 session stream write bridge is not available");
		}
		const buffer = Buffer.isBuffer(data)
			? data
			: typeof data === "string"
				? Buffer.from(
						data,
						typeof encodingOrCallback === "string"
							? encodingOrCallback
							: "utf8",
					)
				: Buffer.from(data);
		const wrote = _networkHttp2StreamWriteRaw.applySync(void 0, [
			this._streamId,
			buffer.toString("base64"),
		]);
		this._pendingWritableBytes += buffer.byteLength;
		const shouldBackpressure =
			wrote === false ||
			this._pendingWritableBytes >= this._writableHighWaterMark;
		if (shouldBackpressure) {
			this._needsDrain = true;
		}
		const cb =
			typeof encodingOrCallback === "function" ? encodingOrCallback : callback;
		cb?.();
		return !shouldBackpressure;
	}
	end(data) {
		if (typeof _networkHttp2StreamEndRaw === "undefined") {
			throw new Error("http2 session stream end bridge is not available");
		}
		let encoded = null;
		if (data !== void 0) {
			const buffer = Buffer.isBuffer(data)
				? data
				: typeof data === "string"
					? Buffer.from(data)
					: Buffer.from(data);
			encoded = buffer.toString("base64");
		}
		_networkHttp2StreamEndRaw.applySync(void 0, [this._streamId, encoded]);
		this.writableEnded = true;
		this._writableState.ended = true;
		queueMicrotask(() => {
			this.writable = false;
			this.writableFinished = true;
			this._writableState.finished = true;
			this.emit("finish");
		});
		return this;
	}
	resume() {
		return this;
	}
	_emitPush(headers, flags) {
		if (process.env.AGENTOS_DEBUG_HTTP2_BRIDGE === "1") {
			console.error("[agentos http2 isolate] push", this._streamId);
		}
		this.emit("push", headers, flags ?? 0);
	}
	_hasReceivedResponse() {
		return this._receivedResponse;
	}
	_belongsTo(session) {
		return this._session === session;
	}
	_emitResponseHeaders(headers) {
		this._receivedResponse = true;
		if (process.env.AGENTOS_DEBUG_HTTP2_BRIDGE === "1") {
			console.error(
				"[agentos http2 isolate] response headers",
				this._streamId,
				this._isPushStream,
			);
		}
		if (!this._isPushStream) {
			this.emit("response", headers);
		}
	}
	_emitDataChunk(dataBase64) {
		if (!dataBase64) {
			return;
		}
		const chunkBuffer = Buffer.from(dataBase64, "base64");
		if (this._utf8Remainder !== void 0) {
			const buffer =
				this._utf8Remainder.length > 0
					? Buffer.concat([this._utf8Remainder, chunkBuffer])
					: chunkBuffer;
			const completeLength = getCompleteUtf8PrefixLength(buffer);
			const chunk = buffer.subarray(0, completeLength).toString("utf8");
			this._utf8Remainder =
				completeLength < buffer.length
					? buffer.subarray(completeLength)
					: Buffer.alloc(0);
			if (chunk.length > 0) {
				this.emit("data", chunk);
			}
		} else if (this._encoding) {
			this.emit("data", chunkBuffer.toString(this._encoding));
		} else {
			this.emit("data", chunkBuffer);
		}
		this._scheduleDrain();
	}
	_emitEnd() {
		if (this._utf8Remainder && this._utf8Remainder.length > 0) {
			const trailing = this._utf8Remainder.toString("utf8");
			this._utf8Remainder = Buffer.alloc(0);
			if (trailing.length > 0) {
				this.emit("data", trailing);
			}
		}
		this.readable = false;
		this.emit("end");
		this._scheduleDrain();
	}
	_emitClose(rstCode) {
		if (typeof rstCode === "number") {
			this.rstCode = rstCode;
		}
		this.destroyed = true;
		this.readable = false;
		this.writable = false;
		this._scheduleDrain();
		this.emit("close");
	}
};

function getCompleteUtf8PrefixLength(buffer) {
	if (buffer.length === 0) {
		return 0;
	}
	let continuationCount = 0;
	for (
		let index = buffer.length - 1;
		index >= 0 && continuationCount < 3;
		index -= 1
	) {
		if ((buffer[index] & 192) !== 128) {
			const trailingBytes = buffer.length - index;
			const lead = buffer[index];
			const expectedBytes =
				(lead & 128) === 0
					? 1
					: (lead & 224) === 192
						? 2
						: (lead & 240) === 224
							? 3
							: (lead & 248) === 240
								? 4
								: 1;
			return trailingBytes < expectedBytes ? index : buffer.length;
		}
		continuationCount += 1;
	}
	return continuationCount > 0
		? buffer.length - continuationCount
		: buffer.length;
}

var ServerHttp2Stream = class _ServerHttp2Stream extends Http2EventEmitter {
	_streamId;
	_binding;
	_responded = false;
	_endQueued = false;
	_pendingSyntheticErrorSuppressions = 0;
	_requestHeaders;
	_isPushStream;
	session;
	rstCode = 0;
	readable = true;
	writable = true;
	destroyed = false;
	_readableState;
	_writableState;
	constructor(streamId, session, requestHeaders, isPushStream = false) {
		super();
		this._streamId = streamId;
		this._binding = new Http2Stream(streamId);
		this.session = session;
		this._requestHeaders = requestHeaders;
		this._isPushStream = isPushStream;
		this._readableState = {
			flowing: null,
			ended: false,
			highWaterMark: 16 * 1024,
		};
		this._writableState = {
			ended: requestHeaders?.[":method"] === "HEAD",
		};
	}
	_closeWithCode(code) {
		this.rstCode = code;
		_networkHttp2StreamCloseRaw?.applySync(void 0, [this._streamId, code]);
	}
	_markSyntheticClose() {
		this.destroyed = true;
		this.readable = false;
		this.writable = false;
	}
	_shouldSuppressHostError() {
		if (this._pendingSyntheticErrorSuppressions <= 0) {
			return false;
		}
		this._pendingSyntheticErrorSuppressions -= 1;
		return true;
	}
	_emitNghttp2Error(errorCode) {
		const error = new NghttpError(nghttp2ErrorString(errorCode));
		this._pendingSyntheticErrorSuppressions += 1;
		this._markSyntheticClose();
		this.emit("error", error);
		this._closeWithCode(
			HTTP2_INTERNAL_BINDING_CONSTANTS.NGHTTP2_INTERNAL_ERROR,
		);
	}
	_emitInternalStreamError() {
		const error = createHttp2Error(
			"ERR_HTTP2_STREAM_ERROR",
			"Stream closed with error code NGHTTP2_INTERNAL_ERROR",
		);
		this._pendingSyntheticErrorSuppressions += 1;
		this._markSyntheticClose();
		this.emit("error", error);
		this._closeWithCode(
			HTTP2_INTERNAL_BINDING_CONSTANTS.NGHTTP2_INTERNAL_ERROR,
		);
	}
	_submitResponse(headers) {
		this._responded = true;
		const ngError = this._binding.respond(headers);
		if (typeof ngError === "number" && ngError !== 0) {
			this._emitNghttp2Error(ngError);
			return false;
		}
		return true;
	}
	respond(headers) {
		if (this.destroyed) {
			throw createHttp2Error(
				"ERR_HTTP2_INVALID_STREAM",
				"The stream has been destroyed",
			);
		}
		if (this._responded) {
			throw createHttp2Error(
				"ERR_HTTP2_HEADERS_SENT",
				"Response has already been initiated.",
			);
		}
		this._submitResponse(headers);
	}
	pushStream(headers, optionsOrCallback, maybeCallback) {
		if (this._isPushStream) {
			throw createHttp2Error(
				"ERR_HTTP2_NESTED_PUSH",
				"A push stream cannot initiate another push stream.",
			);
		}
		const callback =
			typeof optionsOrCallback === "function"
				? optionsOrCallback
				: maybeCallback;
		if (typeof callback !== "function") {
			throw createHttp2ArgTypeError("callback", "function", callback);
		}
		if (typeof _networkHttp2StreamPushStreamRaw === "undefined") {
			throw new Error("http2 server stream push bridge is not available");
		}
		const options =
			optionsOrCallback &&
			typeof optionsOrCallback === "object" &&
			!Array.isArray(optionsOrCallback)
				? optionsOrCallback
				: {};
		const resultJson = _networkHttp2StreamPushStreamRaw.applySync(void 0, [
			this._streamId,
			serializeHttp2Headers(normalizeHttp2Headers(headers)),
			JSON.stringify(options ?? {}),
		]);
		const result = JSON.parse(resultJson);
		if (result.error) {
			callback(parseHttp2ErrorPayload(result.error));
			return;
		}
		const pushStream = new _ServerHttp2Stream(
			Number(result.streamId),
			this.session,
			parseHttp2Headers(result.headers),
			true,
		);
		http2Streams.set(Number(result.streamId), pushStream);
		callback(null, pushStream, parseHttp2Headers(result.headers));
	}
	write(data) {
		if (this._writableState.ended) {
			queueMicrotask(() => {
				this.emit(
					"error",
					createHttp2Error("ERR_STREAM_WRITE_AFTER_END", "write after end"),
				);
			});
			return false;
		}
		if (typeof _networkHttp2StreamWriteRaw === "undefined") {
			throw new Error("http2 server stream write bridge is not available");
		}
		const buffer = Buffer.isBuffer(data)
			? data
			: typeof data === "string"
				? Buffer.from(data)
				: Buffer.from(data);
		return _networkHttp2StreamWriteRaw.applySync(void 0, [
			this._streamId,
			buffer.toString("base64"),
		]);
	}
	end(data) {
		if (!this._responded) {
			if (!this._submitResponse({ ":status": 200 })) {
				return;
			}
		}
		if (this._endQueued) {
			return;
		}
		if (typeof _networkHttp2StreamEndRaw === "undefined") {
			throw new Error("http2 server stream end bridge is not available");
		}
		this._writableState.ended = true;
		let encoded = null;
		if (data !== void 0) {
			const buffer = Buffer.isBuffer(data)
				? data
				: typeof data === "string"
					? Buffer.from(data)
					: Buffer.from(data);
			encoded = buffer.toString("base64");
		}
		this._endQueued = true;
		queueMicrotask(() => {
			if (!this._endQueued || this.destroyed) {
				return;
			}
			this._endQueued = false;
			_networkHttp2StreamEndRaw.applySync(void 0, [this._streamId, encoded]);
		});
	}
	pause() {
		this._readableState.flowing = false;
		_networkHttp2StreamPauseRaw?.applySync(void 0, [this._streamId]);
		return this;
	}
	resume() {
		this._readableState.flowing = true;
		_networkHttp2StreamResumeRaw?.applySync(void 0, [this._streamId]);
		return this;
	}
	respondWithFile(path, headers, options) {
		if (this.destroyed) {
			throw createHttp2Error(
				"ERR_HTTP2_INVALID_STREAM",
				"The stream has been destroyed",
			);
		}
		if (this._responded) {
			throw createHttp2Error(
				"ERR_HTTP2_HEADERS_SENT",
				"Response has already been initiated.",
			);
		}
		const normalizedOptions = normalizeHttp2FileResponseOptions(options);
		const responseHeaders = { ...(headers ?? {}) };
		const statusCode = responseHeaders[":status"];
		if (statusCode === 204 || statusCode === 205 || statusCode === 304) {
			throw createHttp2PayloadForbiddenError(Number(statusCode));
		}
		try {
			const statJson = _fs.stat.applySyncPromise(void 0, [path]);
			const bodyBase64 = _fs.readFileBinary.applySyncPromise(void 0, [path]);
			const stat = createHttp2BridgeStat(decodeBridgeJson(statJson));
			const callbackOptions = {
				offset: normalizedOptions.offset,
				length:
					normalizedOptions.length ??
					Math.max(0, stat.size - normalizedOptions.offset),
			};
			normalizedOptions.statCheck?.(stat, responseHeaders, callbackOptions);
			const body = Buffer.from(bodyBase64, "base64");
			const slicedBody = sliceHttp2FileBody(
				body,
				normalizedOptions.offset,
				normalizedOptions.length,
			);
			if (responseHeaders["content-length"] === void 0) {
				responseHeaders["content-length"] = slicedBody.byteLength;
			}
			if (
				!this._submitResponse({
					":status": 200,
					...responseHeaders,
				})
			) {
				return;
			}
			this.end(slicedBody);
			return;
		} catch {}
		if (typeof _networkHttp2StreamRespondWithFileRaw === "undefined") {
			throw new Error(
				"http2 server stream respondWithFile bridge is not available",
			);
		}
		this._responded = true;
		_networkHttp2StreamRespondWithFileRaw.applySync(void 0, [
			this._streamId,
			path,
			JSON.stringify(headers ?? {}),
			JSON.stringify(options ?? {}),
		]);
	}
	respondWithFD(fdOrHandle, headers, options) {
		const fd =
			typeof fdOrHandle === "number"
				? fdOrHandle
				: typeof fdOrHandle?.fd === "number"
					? fdOrHandle.fd
					: NaN;
		const path = Number.isFinite(fd)
			? _fdGetPath.applySync(void 0, [fd])
			: null;
		if (!path) {
			this._emitInternalStreamError();
			return;
		}
		this.respondWithFile(path, headers, options);
	}
	destroy(error) {
		if (this.destroyed) {
			return this;
		}
		this.destroyed = true;
		if (error) {
			this.emit("error", error);
		}
		this._closeWithCode(HTTP2_INTERNAL_BINDING_CONSTANTS.NGHTTP2_CANCEL);
		return this;
	}
	_emitData(dataBase64) {
		if (!dataBase64) {
			return;
		}
		this.emit("data", Buffer.from(dataBase64, "base64"));
	}
	_emitEnd() {
		this._readableState.ended = true;
		this.emit("end");
	}
	_emitDrain() {
		this.emit("drain");
	}
	_emitClose(rstCode) {
		if (typeof rstCode === "number") {
			this.rstCode = rstCode;
		}
		this.destroyed = true;
		this.emit("close");
	}
};

var Http2ServerRequest = class extends Http2EventEmitter {
	headers;
	method;
	url;
	connection;
	socket;
	stream;
	destroyed = false;
	readable = true;
	_readableState = {
		flowing: null,
		length: 0,
		ended: false,
		objectMode: false,
	};
	constructor(headers, socket, stream) {
		super();
		this.headers = headers;
		this.method =
			typeof headers[":method"] === "string"
				? String(headers[":method"])
				: "GET";
		this.url =
			typeof headers[":path"] === "string" ? String(headers[":path"]) : "/";
		this.connection = socket;
		this.socket = socket;
		this.stream = stream;
	}
	on(event, listener) {
		super.on(event, listener);
		if (event === "data" && this._readableState.flowing !== false) {
			this.resume();
		}
		return this;
	}
	once(event, listener) {
		super.once(event, listener);
		if (event === "data" && this._readableState.flowing !== false) {
			this.resume();
		}
		return this;
	}
	resume() {
		this._readableState.flowing = true;
		this.stream.resume();
		return this;
	}
	pause() {
		this._readableState.flowing = false;
		this.stream.pause();
		return this;
	}
	pipe(dest) {
		this.on("data", (chunk) => {
			const wrote = dest.write(chunk);
			if (wrote === false && typeof dest.once === "function") {
				this.pause();
				dest.once("drain", () => this.resume());
			}
		});
		this.on("end", () => dest.end());
		this.resume();
		return dest;
	}
	unpipe() {
		return this;
	}
	read() {
		return null;
	}
	isPaused() {
		return this._readableState.flowing === false;
	}
	setEncoding() {
		return this;
	}
	_emitData(chunk) {
		this._readableState.length += chunk.byteLength;
		this.emit("data", chunk);
	}
	_emitEnd() {
		this._readableState.ended = true;
		this.emit("end");
		this.emit("close");
	}
	_emitError(error) {
		this.emit("error", error);
	}
	destroy(err) {
		this.destroyed = true;
		if (err) {
			this.emit("error", err);
		}
		this.emit("close");
		return this;
	}
};

var Http2ServerResponse = class extends Http2EventEmitter {
	_stream;
	_headers = {};
	_statusCode = 200;
	headersSent = false;
	writable = true;
	writableEnded = false;
	writableFinished = false;
	socket;
	connection;
	stream;
	_writableState = {
		ended: false,
		finished: false,
		objectMode: false,
		corked: 0,
		length: 0,
	};
	constructor(stream) {
		super();
		this._stream = stream;
		this.stream = stream;
		this.socket = stream.session.socket;
		this.connection = this.socket;
	}
	writeHead(statusCode, headers) {
		this._statusCode = statusCode;
		this._headers = {
			...this._headers,
			...(headers ?? {}),
			":status": statusCode,
		};
		this._stream.respond(this._headers);
		this.headersSent = true;
		return this;
	}
	setHeader(name, value) {
		this._headers[name] = value;
		return this;
	}
	getHeader(name) {
		return this._headers[name];
	}
	hasHeader(name) {
		return Object.hasOwn(this._headers, name);
	}
	removeHeader(name) {
		delete this._headers[name];
	}
	write(data, encodingOrCallback, callback) {
		if (!(":status" in this._headers)) {
			this._headers[":status"] = this._statusCode;
			this._stream.respond(this._headers);
			this.headersSent = true;
		}
		const wrote = this._stream.write(
			typeof data === "string" && typeof encodingOrCallback === "string"
				? Buffer.from(data, encodingOrCallback)
				: data,
		);
		const cb =
			typeof encodingOrCallback === "function" ? encodingOrCallback : callback;
		cb?.();
		return wrote;
	}
	end(data) {
		if (!(":status" in this._headers)) {
			this._headers[":status"] = this._statusCode;
			this._stream.respond(this._headers);
			this.headersSent = true;
		}
		this.writableEnded = true;
		this._writableState.ended = true;
		this._stream.end(data);
		queueMicrotask(() => {
			this.writable = false;
			this.writableFinished = true;
			this._writableState.finished = true;
			this.emit("finish");
			this.emit("close");
		});
		return this;
	}
	destroy(err) {
		if (err) {
			this.emit("error", err);
		}
		this.writable = false;
		this.writableEnded = true;
		this.writableFinished = true;
		this.emit("close");
		return this;
	}
};

var Http2Session = class extends Http2EventEmitter {
	encrypted = false;
	alpnProtocol = false;
	originSet;
	localSettings = cloneHttp2Settings(DEFAULT_HTTP2_SETTINGS);
	remoteSettings = cloneHttp2Settings(DEFAULT_HTTP2_SETTINGS);
	pendingSettingsAck = false;
	socket;
	state = cloneHttp2SessionRuntimeState(DEFAULT_HTTP2_SESSION_STATE);
	_sessionId;
	capabilityId;
	capabilityGeneration;
	_waitStarted = false;
	_pendingSettingsAckCount = 0;
	_awaitingInitialSettingsAck = false;
	_settingsCallbacks = [];
	constructor(sessionId, socketState, identity) {
		super();
		this._sessionId = sessionId;
		this.capabilityId = identity?.capabilityId;
		this.capabilityGeneration = identity?.capabilityGeneration;
		this.socket = new Http2SocketProxy(socketState, () => {
			setTimeout(() => {
				this.destroy();
			}, 0);
		});
		this[HTTP2_K_SOCKET] = this.socket;
	}
	_retain() {
		if (
			this._waitStarted ||
			typeof _networkHttp2SessionPollRaw === "undefined"
		) {
			return;
		}
		this._waitStarted = true;
		registerCapabilityReadiness(this, () =>
			wakeRetainedHttp2Handle("session", this._sessionId),
		);
		pollRetainedHttp2Handle(
			"session",
			this._sessionId,
			() => this._waitStarted && http2Sessions.get(this._sessionId) === this,
			() =>
				_networkHttp2SessionPollRaw.applySyncPromise(void 0, [
					this._sessionId,
					0,
				]),
			(error) =>
				this.emit(
					"error",
					error instanceof Error ? error : new Error(String(error)),
				),
		);
	}
	_release() {
		this._waitStarted = false;
		retainedHttp2Handles.delete(`session:${this._sessionId}`);
		unregisterCapabilityReadiness(this);
	}
	_beginInitialSettingsAck() {
		this._awaitingInitialSettingsAck = true;
		this._pendingSettingsAckCount += 1;
		this.pendingSettingsAck = true;
	}
	_applyLocalSettings(settings) {
		this.localSettings = cloneHttp2Settings(settings);
		if (this._awaitingInitialSettingsAck) {
			this._awaitingInitialSettingsAck = false;
			this._pendingSettingsAckCount = Math.max(
				0,
				this._pendingSettingsAckCount - 1,
			);
			this.pendingSettingsAck = this._pendingSettingsAckCount > 0;
		}
		this.emit("localSettings", this.localSettings);
	}
	_applyRemoteSettings(settings) {
		this.remoteSettings = cloneHttp2Settings(settings);
		this.emit("remoteSettings", this.remoteSettings);
	}
	_applyRuntimeState(state) {
		this.state = cloneHttp2SessionRuntimeState(state);
	}
	_ackSettings() {
		this._pendingSettingsAckCount = Math.max(
			0,
			this._pendingSettingsAckCount - 1,
		);
		this.pendingSettingsAck = this._pendingSettingsAckCount > 0;
		const callback = this._settingsCallbacks.shift();
		callback?.();
	}
	request(headers, options) {
		if (typeof _networkHttp2SessionRequestRaw === "undefined") {
			throw new Error("http2 session request bridge is not available");
		}
		validateHttp2RequestOptions(options);
		const streamId = _networkHttp2SessionRequestRaw.applySync(void 0, [
			this._sessionId,
			serializeHttp2Headers(normalizeHttp2Headers(headers)),
			JSON.stringify(options ?? {}),
		]);
		const stream = new ClientHttp2Stream(streamId, this);
		http2Streams.set(streamId, stream);
		return stream;
	}
	settings(settings, callback) {
		if (callback !== void 0 && typeof callback !== "function") {
			throw createHttp2ArgTypeError("callback", "function", callback);
		}
		if (typeof _networkHttp2SessionSettingsRaw === "undefined") {
			throw new Error("http2 session settings bridge is not available");
		}
		const normalized = validateHttp2Settings(settings);
		_networkHttp2SessionSettingsRaw.applySync(void 0, [
			this._sessionId,
			JSON.stringify(normalized),
		]);
		this._pendingSettingsAckCount += 1;
		this.pendingSettingsAck = true;
		if (callback) {
			this._settingsCallbacks.push(callback);
		}
	}
	setLocalWindowSize(windowSize) {
		if (typeof windowSize !== "number" || Number.isNaN(windowSize)) {
			throw createHttp2ArgTypeError("windowSize", "number", windowSize);
		}
		if (
			!Number.isInteger(windowSize) ||
			windowSize < 0 ||
			windowSize > 2147483647
		) {
			const error = new RangeError(
				`The value of "windowSize" is out of range. It must be >= 0 && <= 2147483647. Received ${windowSize}`,
			);
			error.code = "ERR_OUT_OF_RANGE";
			throw error;
		}
		if (typeof _networkHttp2SessionSetLocalWindowSizeRaw === "undefined") {
			throw new Error(
				"http2 session setLocalWindowSize bridge is not available",
			);
		}
		const result = _networkHttp2SessionSetLocalWindowSizeRaw.applySync(void 0, [
			this._sessionId,
			windowSize,
		]);
		this._applyRuntimeState(parseHttp2SessionState(result)?.state);
	}
	goaway(code = 0, lastStreamID = 0, opaqueData) {
		const payload =
			opaqueData === void 0
				? null
				: Buffer.isBuffer(opaqueData)
					? opaqueData.toString("base64")
					: typeof opaqueData === "string"
						? Buffer.from(opaqueData).toString("base64")
						: Buffer.from(opaqueData).toString("base64");
		_networkHttp2SessionGoawayRaw?.applySync(void 0, [
			this._sessionId,
			code,
			lastStreamID,
			payload,
		]);
	}
	close() {
		const pendingStreams = Array.from(http2Streams.entries()).filter(
			([, stream]) =>
				typeof stream._belongsTo === "function" &&
				stream._belongsTo(this) &&
				!stream._hasReceivedResponse(),
		);
		if (pendingStreams.length > 0) {
			const error = createHttp2Error(
				"ERR_HTTP2_GOAWAY_SESSION",
				"The HTTP/2 session is closing before the stream could be established.",
			);
			queueMicrotask(() => {
				for (const [streamId, stream] of pendingStreams) {
					if (http2Streams.get(streamId) !== stream) {
						continue;
					}
					stream.emit("error", error);
					stream.emit("close");
					http2Streams.delete(streamId);
				}
			});
			if (typeof _networkHttp2SessionDestroyRaw !== "undefined") {
				_networkHttp2SessionDestroyRaw.applySync(void 0, [this._sessionId]);
				return;
			}
		}
		if (typeof _networkHttp2SessionCloseRaw === "undefined") {
			throw new Error("http2 session close bridge is not available");
		}
		_networkHttp2SessionCloseRaw.applySync(void 0, [this._sessionId]);
	}
	destroy() {
		if (typeof _networkHttp2SessionDestroyRaw !== "undefined") {
			_networkHttp2SessionDestroyRaw.applySync(void 0, [this._sessionId]);
			return;
		}
		this.close();
	}
};

var Http2Server = class extends Http2EventEmitter {
	allowHalfOpen;
	allowHTTP1;
	encrypted;
	_serverId;
	capabilityId;
	capabilityGeneration;
	listening = false;
	_address = null;
	_options;
	_timeoutMs = 0;
	_waitStarted = false;
	constructor(options, listener, encrypted) {
		super();
		this.allowHalfOpen = options?.allowHalfOpen === true;
		this.allowHTTP1 = options?.allowHTTP1 === true;
		this.encrypted = encrypted;
		const initialSettings =
			options?.settings &&
			typeof options.settings === "object" &&
			!Array.isArray(options.settings)
				? cloneHttp2Settings(options.settings)
				: {};
		this._options = {
			...(options ?? {}),
			settings: initialSettings,
		};
		this._serverId = nextHttp2ServerId++;
		this[HTTP2_OPTIONS] = {
			settings: cloneHttp2Settings(initialSettings),
			unknownProtocolTimeout: 1e4,
			...(encrypted ? { ALPNProtocols: ["h2"] } : {}),
		};
		if (listener) {
			this.on("request", listener);
		}
		http2Servers.set(this._serverId, this);
	}
	address() {
		return this._address;
	}
	_retain() {
		if (
			this._waitStarted ||
			typeof _networkHttp2ServerPollRaw === "undefined"
		) {
			return;
		}
		this._waitStarted = true;
		registerCapabilityReadiness(this, () =>
			wakeRetainedHttp2Handle("server", this._serverId),
		);
		pollRetainedHttp2Handle(
			"server",
			this._serverId,
			() => this._waitStarted && http2Servers.get(this._serverId) === this,
			() =>
				_networkHttp2ServerPollRaw.applySyncPromise(void 0, [
					this._serverId,
					0,
				]),
			(error) =>
				this.emit(
					"error",
					error instanceof Error ? error : new Error(String(error)),
				),
		);
	}
	_release() {
		this._waitStarted = false;
		retainedHttp2Handles.delete(`server:${this._serverId}`);
		unregisterCapabilityReadiness(this);
	}
	setTimeout(timeout, callback) {
		this._timeoutMs = normalizeSocketTimeout(timeout);
		if (callback) {
			this.on("timeout", callback);
		}
		return this;
	}
	updateSettings(settings) {
		const normalized = validateHttp2Settings(settings);
		const mergedSettings = {
			...cloneHttp2Settings(this._options.settings),
			...cloneHttp2Settings(normalized),
		};
		this._options = {
			...this._options,
			settings: mergedSettings,
		};
		const optionsState = this[HTTP2_OPTIONS];
		optionsState.settings = cloneHttp2Settings(mergedSettings);
		return this;
	}
	listen(portOrOptions, hostOrCallback, backlogOrCallback, callback) {
		if (typeof _networkHttp2ServerListenRaw === "undefined") {
			throw new Error(
				`http2.${this.encrypted ? "createSecureServer" : "createServer"} is not supported in sandbox`,
			);
		}
		const options = normalizeListenArgs(
			portOrOptions,
			hostOrCallback,
			backlogOrCallback,
			callback,
		);
		if (options.callback) {
			this.once("listening", options.callback);
		}
		const payload = {
			serverId: this._serverId,
			secure: this.encrypted,
			port: options.port,
			host: options.host,
			backlog: options.backlog,
			allowHalfOpen: this.allowHalfOpen,
			allowHTTP1: this._options.allowHTTP1 === true,
			timeout: this._timeoutMs,
			settings: this._options.settings,
			remoteCustomSettings: this._options.remoteCustomSettings,
			tls: this.encrypted
				? buildSerializedTlsOptions(
						{
							...this._options,
							...(portOrOptions && typeof portOrOptions === "object"
								? portOrOptions
								: {}),
						},
						{ isServer: true },
					)
				: void 0,
		};
		const result = JSON.parse(
			_networkHttp2ServerListenRaw.applySyncPromise(void 0, [
				JSON.stringify(payload),
			]),
		);
		this._address = result.address ?? null;
		this.capabilityId = result.capabilityId;
		this.capabilityGeneration = result.capabilityGeneration;
		this.listening = true;
		this._retain();
		_registerHandle?.(`http2:server:${this._serverId}`, "http2 server");
		this.emit("listening");
		return this;
	}
	close(callback) {
		if (callback) {
			this.once("close", callback);
		}
		if (!this.listening) {
			this._release();
			queueMicrotask(() => this.emit("close"));
			return this;
		}
		if (typeof _networkHttp2ServerCloseRaw === "undefined") {
			throw new Error("http2 server close bridge is not available");
		}
		// Closing the listener is an immediate control-plane state transition in
		// the sidecar. Wait for that response on the direct sync-response lane so
		// the terminal event is durable before re-waking the bounded event drain.
		// An async promise here can otherwise park the only follow-up wake behind
		// the event that the promise is meant to expose.
		_networkHttp2ServerCloseRaw.applySyncPromise(void 0, [this._serverId]);
		wakeRetainedHttp2Handle("server", this._serverId);
		return this;
	}
};

function createHttp2Server(secure, optionsOrListener, maybeListener) {
	const listener =
		typeof optionsOrListener === "function" ? optionsOrListener : maybeListener;
	const options =
		optionsOrListener &&
		typeof optionsOrListener === "object" &&
		!Array.isArray(optionsOrListener)
			? optionsOrListener
			: void 0;
	return new Http2Server(options, listener, secure);
}

function connectHttp2(authorityOrOptions, optionsOrListener, maybeListener) {
	if (typeof _networkHttp2SessionConnectRaw === "undefined") {
		throw new Error("http2.connect is not supported in sandbox");
	}
	const { authority, options, listener } = normalizeHttp2ConnectArgs(
		authorityOrOptions,
		optionsOrListener,
		maybeListener,
	);
	if (authority.protocol !== "http:" && authority.protocol !== "https:") {
		throw createHttp2Error(
			"ERR_HTTP2_UNSUPPORTED_PROTOCOL",
			`protocol "${authority.protocol}" is unsupported.`,
		);
	}
	validateHttp2ConnectOptions(options);
	const socketId = options.createConnection
		? resolveHttp2SocketId(options.createConnection())
		: void 0;
	const rawPort = options.port ?? authority.port;
	const port =
		rawPort === "" || rawPort === void 0 || rawPort === null
			? void 0
			: Number(rawPort);
	const response = JSON.parse(
		_networkHttp2SessionConnectRaw.applySyncPromise(void 0, [
			JSON.stringify({
				authority: authority.toString(),
				protocol: authority.protocol,
				host: options.host ?? options.hostname ?? authority.hostname,
				port,
				localAddress: options.localAddress,
				family: options.family,
				socketId,
				settings: options.settings,
				remoteCustomSettings: options.remoteCustomSettings,
				tls:
					authority.protocol === "https:"
						? buildSerializedTlsOptions(options, {
								servername:
									typeof options.servername === "string"
										? options.servername
										: authority.hostname,
							})
						: void 0,
			}),
		]),
	);
	const initialState = parseHttp2SessionState(response.state);
	const session = new Http2Session(
		response.sessionId,
		initialState?.socket ?? void 0,
		response,
	);
	applyHttp2SessionState(session, initialState);
	session._beginInitialSettingsAck();
	session._retain();
	if (listener) {
		session.once("connect", () => listener(session));
	}
	http2Sessions.set(response.sessionId, session);
	_registerHandle?.(`http2:session:${response.sessionId}`, "http2 session");
	if (authority.protocol === "https:") {
		session.socket.once("secureConnect", () => {});
	}
	return session;
}

function getOrCreateHttp2Session(sessionId, state) {
	let session = http2Sessions.get(sessionId);
	if (!session) {
		session = new Http2Session(sessionId, state?.socket ?? void 0, state);
		http2Sessions.set(sessionId, session);
	}
	applyHttp2SessionState(session, state);
	return session;
}

function queuePendingHttp2ClientStreamEvent(streamId, event) {
	const pending = pendingHttp2ClientStreamEvents.get(streamId) ?? [];
	pending.push(event);
	pendingHttp2ClientStreamEvents.set(streamId, pending);
}

function schedulePendingHttp2ClientStreamEventsFlush(streamId) {
	if (scheduledHttp2ClientStreamFlushes.has(streamId)) {
		return;
	}
	scheduledHttp2ClientStreamFlushes.add(streamId);
	const flush = () => {
		scheduledHttp2ClientStreamFlushes.delete(streamId);
		flushPendingHttp2ClientStreamEvents(streamId);
	};
	const scheduleImmediate = globalThis.setImmediate;
	if (typeof scheduleImmediate === "function") {
		scheduleImmediate(flush);
		return;
	}
	setTimeout(flush, 0);
}

function flushPendingHttp2ClientStreamEvents(streamId) {
	const stream = http2Streams.get(streamId);
	if (!stream || typeof stream._emitResponseHeaders !== "function") {
		return;
	}
	const pending = pendingHttp2ClientStreamEvents.get(streamId);
	if (!pending || pending.length === 0) {
		return;
	}
	pendingHttp2ClientStreamEvents.delete(streamId);
	for (const event of pending) {
		if (event.kind === "push") {
			stream._emitPush(parseHttp2Headers(event.data), event.extraNumber);
			continue;
		}
		if (event.kind === "responseHeaders") {
			stream._emitResponseHeaders(parseHttp2Headers(event.data));
			continue;
		}
		if (event.kind === "data") {
			stream._emitDataChunk(event.data);
			continue;
		}
		if (event.kind === "end") {
			stream._emitEnd();
			continue;
		}
		if (event.kind === "error") {
			stream.emit("error", parseHttp2ErrorPayload(event.data));
			continue;
		}
		if (typeof stream._emitClose === "function") {
			stream._emitClose(event.extraNumber);
		} else {
			stream.emit("close");
		}
		http2Streams.delete(streamId);
	}
}

function http2Dispatch(
	kind,
	id,
	data,
	extra,
	extraNumber,
	extraHeaders,
	flags,
) {
	if (kind === "sessionConnect") {
		const session = http2Sessions.get(id);
		if (!session) return;
		const state = parseHttp2SessionState(data);
		applyHttp2SessionState(session, state);
		if (session.encrypted) {
			session.socket.emit("secureConnect");
		}
		session.emit("connect");
		return;
	}
	if (kind === "sessionClose") {
		const session = http2Sessions.get(id);
		if (!session) return;
		session._release();
		session.emit("close");
		http2Sessions.delete(id);
		_unregisterHandle?.(`http2:session:${id}`);
		return;
	}
	if (kind === "sessionError") {
		const session = http2Sessions.get(id);
		if (!session) return;
		session.emit("error", parseHttp2ErrorPayload(data));
		return;
	}
	if (kind === "sessionLocalSettings") {
		const session = http2Sessions.get(id);
		if (!session) return;
		session._applyLocalSettings(parseHttp2Headers(data));
		return;
	}
	if (kind === "sessionRemoteSettings") {
		const session = http2Sessions.get(id);
		if (!session) return;
		session._applyRemoteSettings(parseHttp2Headers(data));
		return;
	}
	if (kind === "sessionSettingsAck") {
		const session = http2Sessions.get(id);
		if (!session) return;
		session._ackSettings();
		return;
	}
	if (kind === "sessionGoaway") {
		const session = http2Sessions.get(id);
		if (!session) return;
		session.emit(
			"goaway",
			Number(extraNumber ?? 0),
			Number(flags ?? 0),
			data ? Buffer.from(data, "base64") : Buffer.alloc(0),
		);
		return;
	}
	if (kind === "clientPushStream") {
		const session = http2Sessions.get(id);
		if (!session) return;
		const streamId = Number(data);
		const stream = new ClientHttp2Stream(streamId, session, true);
		http2Streams.set(streamId, stream);
		session.emit(
			"stream",
			stream,
			parseHttp2Headers(extraHeaders),
			Number(flags ?? 0),
		);
		schedulePendingHttp2ClientStreamEventsFlush(streamId);
		return;
	}
	if (kind === "clientPushHeaders") {
		queuePendingHttp2ClientStreamEvent(id, {
			kind: "push",
			data,
			extraNumber: Number(extraNumber ?? 0),
		});
		schedulePendingHttp2ClientStreamEventsFlush(id);
		return;
	}
	if (kind === "clientResponseHeaders") {
		queuePendingHttp2ClientStreamEvent(id, {
			kind: "responseHeaders",
			data,
		});
		schedulePendingHttp2ClientStreamEventsFlush(id);
		return;
	}
	if (kind === "clientData") {
		queuePendingHttp2ClientStreamEvent(id, {
			kind: "data",
			data,
		});
		schedulePendingHttp2ClientStreamEventsFlush(id);
		return;
	}
	if (kind === "clientEnd") {
		queuePendingHttp2ClientStreamEvent(id, {
			kind: "end",
		});
		schedulePendingHttp2ClientStreamEventsFlush(id);
		return;
	}
	if (kind === "clientClose") {
		queuePendingHttp2ClientStreamEvent(id, {
			kind: "close",
			extraNumber: Number(extraNumber ?? 0),
		});
		schedulePendingHttp2ClientStreamEventsFlush(id);
		return;
	}
	if (kind === "clientError") {
		queuePendingHttp2ClientStreamEvent(id, {
			kind: "error",
			data,
		});
		schedulePendingHttp2ClientStreamEventsFlush(id);
		return;
	}
	if (kind === "serverStream") {
		const server = http2Servers.get(id);
		if (!server) return;
		const sessionState = parseHttp2SessionState(extra);
		const sessionId = Number(extraNumber);
		const session = getOrCreateHttp2Session(sessionId, sessionState);
		const streamId = Number(data);
		const headers = parseHttp2Headers(extraHeaders);
		const numericFlags = Number(flags ?? 0);
		const stream = new ServerHttp2Stream(streamId, session, headers);
		http2Streams.set(streamId, stream);
		server.emit("stream", stream, headers, numericFlags);
		if (server.listenerCount("request") > 0) {
			const request = new Http2ServerRequest(headers, session.socket, stream);
			const response = new Http2ServerResponse(stream);
			stream.on("data", (chunk) => {
				request._emitData(chunk);
			});
			stream.on("end", () => {
				request._emitEnd();
			});
			stream.on("error", (error) => {
				request._emitError(error);
			});
			stream.on("drain", () => {
				response.emit("drain");
			});
			server.emit("request", request, response);
		}
		return;
	}
	if (kind === "serverStreamData") {
		const stream = http2Streams.get(id);
		if (!stream || typeof stream._emitData !== "function") return;
		stream._emitData(data);
		return;
	}
	if (kind === "serverStreamEnd") {
		const stream = http2Streams.get(id);
		if (!stream || typeof stream._emitEnd !== "function") return;
		stream._emitEnd();
		return;
	}
	if (kind === "serverStreamDrain") {
		const stream = http2Streams.get(id);
		if (!stream || typeof stream._emitDrain !== "function") return;
		stream._emitDrain();
		return;
	}
	if (kind === "serverStreamError") {
		const stream = http2Streams.get(id);
		if (!stream) return;
		if (
			typeof stream._shouldSuppressHostError === "function" &&
			stream._shouldSuppressHostError()
		) {
			return;
		}
		stream.emit("error", parseHttp2ErrorPayload(data));
		return;
	}
	if (kind === "serverStreamClose") {
		const stream = http2Streams.get(id);
		if (!stream || typeof stream._emitClose !== "function") return;
		stream._emitClose(Number(extraNumber ?? 0));
		http2Streams.delete(id);
		return;
	}
	if (kind === "serverSession") {
		const server = http2Servers.get(id);
		if (!server) return;
		const sessionId = Number(extraNumber);
		const session = getOrCreateHttp2Session(
			sessionId,
			parseHttp2SessionState(data),
		);
		server.emit("session", session);
		return;
	}
	if (kind === "serverTimeout") {
		http2Servers.get(id)?.emit("timeout");
		return;
	}
	if (kind === "serverConnection") {
		http2Servers
			.get(id)
			?.emit(
				"connection",
				new Http2SocketProxy(parseHttp2SocketState(data) ?? void 0),
			);
		return;
	}
	if (kind === "serverSecureConnection") {
		http2Servers
			.get(id)
			?.emit(
				"secureConnection",
				new Http2SocketProxy(parseHttp2SocketState(data) ?? void 0),
			);
		return;
	}
	if (kind === "serverClose") {
		const server = http2Servers.get(id);
		if (!server) return;
		server.listening = false;
		server._release();
		server.emit("close");
		http2Servers.delete(id);
		_unregisterHandle?.(`http2:server:${id}`);
		return;
	}
	if (kind === "serverCompatRequest") {
		pendingHttp2CompatRequests.set(Number(extraNumber), {
			serverId: id,
			requestJson: data ?? "{}",
		});
		void dispatchHttp2CompatibilityRequest(id, Number(extraNumber));
	}
}

function dispatchPolledHttp2Event(event, terminalKind) {
	if (typeof event === "string") {
		event = JSON.parse(event);
	}
	if (!event || typeof event !== "object" || typeof event.kind !== "string") {
		return false;
	}
	const id = Number(event.id);
	if (!Number.isFinite(id)) {
		return false;
	}
	http2Dispatch(
		event.kind,
		id,
		typeof event.data === "string" ? event.data : void 0,
		typeof event.extra === "string" ? event.extra : void 0,
		typeof event.extraNumber === "string" ||
			typeof event.extraNumber === "number"
			? event.extraNumber
			: void 0,
		typeof event.extraHeaders === "string" ? event.extraHeaders : void 0,
		typeof event.flags === "string" || typeof event.flags === "number"
			? event.flags
			: void 0,
	);
	return event.kind === terminalKind;
}

function pollRetainedHttp2Handle(kind, id, isActive, poll, onError) {
	const handleKey = `${kind}:${id}`;
	const terminalKind = kind === "server" ? "serverClose" : "sessionClose";
	const handle = {
		active: true,
		running: false,
		pendingWake: false,
		poll,
		tick: null,
	};
	const scheduleTick = () => {
		queueMicrotask(() => {
			if (retainedHttp2Handles.get(handleKey) === handle && handle.active) {
				handle.tick?.();
			}
		});
	};
	const tick = () => {
		if (!isActive()) {
			handle.active = false;
			retainedHttp2Handles.delete(handleKey);
			return;
		}
		if (handle.running) {
			handle.pendingWake = true;
			return;
		}
		handle.running = true;
		try {
			let sawTerminal = false;
			let hitDrainLimit = false;
			do {
				handle.pendingWake = false;
				hitDrainLimit = false;
				let i = 0;
				for (; i < 64 && isActive(); i++) {
					let event;
					try {
						event = poll();
					} catch (error) {
						onError(error);
						return;
					}
					if (!event) {
						break;
					}
					sawTerminal = dispatchPolledHttp2Event(event, terminalKind);
					if (sawTerminal) {
						break;
					}
				}
				hitDrainLimit = i === 64;
			} while (!sawTerminal && handle.pendingWake && isActive());
			if (!sawTerminal && hitDrainLimit && isActive()) {
				// The sidecar only wakes on empty-to-nonempty. If one bounded turn
				// leaves durable events queued, schedule exactly one continuation.
				scheduleTick();
			} else if (sawTerminal) {
				handle.active = false;
				retainedHttp2Handles.delete(handleKey);
			}
		} finally {
			handle.running = false;
			if (handle.pendingWake && handle.active && isActive()) {
				scheduleTick();
			}
		}
	};
	handle.tick = tick;
	retainedHttp2Handles.set(handleKey, handle);
	scheduleTick();
}

function wakeRetainedHttp2Handle(kind, id) {
	const handle = retainedHttp2Handles.get(`${kind}:${id}`);
	if (!handle || !handle.active) {
		return;
	}
	handle.pendingWake = true;
	if (handle.running) {
		return;
	}
	queueMicrotask(() => {
		if (retainedHttp2Handles.get(`${kind}:${id}`) === handle) {
			handle.tick?.();
		}
	});
}

function onHttp2RetainDispatch(payload) {
	if (!payload || payload.event !== "http2") {
		return;
	}
	const kind =
		payload.kind === "server"
			? "server"
			: payload.kind === "session"
				? "session"
				: null;
	const id = Number(payload.id);
	if (!kind || !Number.isFinite(id)) {
		return;
	}
	wakeRetainedHttp2Handle(kind, id);
}

function scheduleQueuedHttp2DispatchDrain() {
	if (scheduledHttp2DispatchDrain) {
		return;
	}
	scheduledHttp2DispatchDrain = true;
	const drain = () => {
		scheduledHttp2DispatchDrain = false;
		while (queuedHttp2DispatchEvents.length > 0) {
			const event = queuedHttp2DispatchEvents.shift();
			if (!event) {
				continue;
			}
			http2Dispatch(
				event.kind,
				event.id,
				event.data,
				event.extra,
				event.extraNumber,
				event.extraHeaders,
				event.flags,
			);
		}
	};
	queueMicrotask(drain);
}

function onHttp2Dispatch(_eventType, payload) {
	if (!payload || typeof payload !== "object") {
		return;
	}
	const event = payload;
	if (typeof event.kind !== "string" || typeof event.id !== "number") {
		return;
	}
	if (process.env.AGENTOS_DEBUG_HTTP2_BRIDGE === "1") {
		console.error("[agentos http2 isolate dispatch]", event.kind, event.id);
	}
	const kind = event.kind;
	const id = event.id;
	const data = typeof event.data === "string" ? event.data : void 0;
	const extra = typeof event.extra === "string" ? event.extra : void 0;
	const normalizedExtraNumber =
		typeof event.extraNumber === "string" ||
		typeof event.extraNumber === "number"
			? event.extraNumber
			: void 0;
	const extraHeaders =
		typeof event.extraHeaders === "string" ? event.extraHeaders : void 0;
	const flags =
		typeof event.flags === "string" || typeof event.flags === "number"
			? event.flags
			: void 0;
	queuedHttp2DispatchEvents.push({
		kind,
		id,
		data,
		extra,
		extraNumber: normalizedExtraNumber,
		extraHeaders,
		flags,
	});
	scheduleQueuedHttp2DispatchDrain();
}

exposeCustomGlobal("_http2RetainDispatch", onHttp2RetainDispatch);

var http2 = {
	Http2ServerRequest,
	Http2ServerResponse,
	Http2Stream,
	NghttpError,
	nghttp2ErrorString,
	constants: {
		HTTP2_HEADER_METHOD: ":method",
		HTTP2_HEADER_PATH: ":path",
		HTTP2_HEADER_SCHEME: ":scheme",
		HTTP2_HEADER_AUTHORITY: ":authority",
		HTTP2_HEADER_STATUS: ":status",
		HTTP2_HEADER_CONTENT_TYPE: "content-type",
		HTTP2_HEADER_CONTENT_LENGTH: "content-length",
		HTTP2_HEADER_LAST_MODIFIED: "last-modified",
		HTTP2_HEADER_ACCEPT: "accept",
		HTTP2_HEADER_ACCEPT_ENCODING: "accept-encoding",
		HTTP2_HEADER_EXPECT: "expect",
		HTTP2_HEADER_LOCATION: "location",
		HTTP2_HEADER_USER_AGENT: "user-agent",
		HTTP_STATUS_REQUEST_TIMEOUT: 408,
		HTTP_STATUS_TOO_MANY_REQUESTS: 429,
		HTTP_STATUS_INTERNAL_SERVER_ERROR: 500,
		HTTP2_METHOD_GET: "GET",
		HTTP2_METHOD_POST: "POST",
		HTTP2_METHOD_PUT: "PUT",
		HTTP2_METHOD_DELETE: "DELETE",
		...HTTP2_INTERNAL_BINDING_CONSTANTS,
		DEFAULT_SETTINGS_MAX_HEADER_LIST_SIZE: 65535,
	},
	getDefaultSettings() {
		return cloneHttp2Settings(DEFAULT_HTTP2_SETTINGS);
	},
	connect: connectHttp2,
	createServer: createHttp2Server.bind(void 0, false),
	createSecureServer: createHttp2Server.bind(void 0, true),
};

export {
	applyHttp2SessionState,
	ClientHttp2Stream,
	cloneHttp2SessionRuntimeState,
	cloneHttp2Settings,
	connectHttp2,
	createHttp2ArgTypeError,
	createHttp2BridgeStat,
	createHttp2Error,
	createHttp2InvalidArgValueError,
	createHttp2PayloadForbiddenError,
	createHttp2Server,
	createHttp2SettingRangeError,
	createHttp2SettingTypeError,
	DEFAULT_HTTP2_SESSION_STATE,
	DEFAULT_HTTP2_SETTINGS,
	flushPendingHttp2ClientStreamEvents,
	formatHttp2InvalidValue,
	getCompleteUtf8PrefixLength,
	getOrCreateHttp2Session,
	HTTP2_INTERNAL_BINDING_CONSTANTS,
	HTTP2_K_SOCKET,
	HTTP2_NGHTTP2_ERROR_MESSAGES,
	HTTP2_OPTIONS,
	Http2EventEmitter,
	Http2Server,
	Http2ServerRequest,
	Http2ServerResponse,
	Http2Session,
	Http2SocketProxy,
	Http2Stream,
	http2,
	http2Dispatch,
	http2Servers,
	http2Sessions,
	http2Streams,
	NghttpError,
	nextHttp2ServerId,
	nghttp2ErrorString,
	normalizeHttp2Authority,
	normalizeHttp2ConnectArgs,
	normalizeHttp2FileResponseOptions,
	normalizeHttp2Headers,
	onHttp2Dispatch,
	parseHttp2ErrorPayload,
	parseHttp2Headers,
	parseHttp2SessionRuntimeState,
	parseHttp2SessionState,
	parseHttp2SocketState,
	pendingHttp2ClientStreamEvents,
	pendingHttp2CompatRequests,
	queuedHttp2DispatchEvents,
	queuePendingHttp2ClientStreamEvent,
	resolveHttp2SocketId,
	S_IFDIR,
	S_IFIFO,
	S_IFLNK,
	S_IFMT,
	S_IFREG,
	S_IFSOCK,
	ServerHttp2Stream,
	scheduledHttp2ClientStreamFlushes,
	scheduledHttp2DispatchDrain,
	schedulePendingHttp2ClientStreamEventsFlush,
	scheduleQueuedHttp2DispatchDrain,
	serializeHttp2Headers,
	sliceHttp2FileBody,
	validateHttp2ConnectOptions,
	validateHttp2RequestOptions,
	validateHttp2Settings,
};
