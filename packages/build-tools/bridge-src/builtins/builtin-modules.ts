import upstreamUrlHelpersModule from "agentos-node-url-polyfill";
import { exposeCustomGlobal } from "../global-exposure.js";
import { TextDecoder } from "../polyfills/index.js";
import {
	bufferStdlibModuleNs,
	constantsStdlibModuleNs,
	eventsStdlibModuleNs,
	pathStdlibModuleNs,
	punycodeStdlibModuleNs,
	querystringStdlibModuleNs,
	streamStdlibModuleNs,
	stringDecoderStdlibModuleNs,
	urlStdlibModuleNs,
} from "../prelude.js";
import {
	BUFFER_CONSTANTS,
	BUFFER_MAX_LENGTH,
	BUFFER_MAX_STRING_LENGTH,
} from "./buffer-constants.js";
import {
	builtinConsoleModule,
	installBuiltinUtilFormatWithOptions,
} from "./console.js";
import { builtinCryptoModule } from "./crypto.js";
import { eventsModule } from "./events.js";
import {
	builtinDiagnosticsChannelModule,
	builtinInspectorModule,
	builtinStreamConsumersModule,
	builtinStreamPromisesModule,
	builtinTtyModule,
	builtinWorkerThreadsModule,
	createAccessDeniedBuiltinError,
} from "./misc-stubs.js";
import { builtinPerfHooksModule } from "./perf.js";
import { process_default } from "./process.js";
import {
	builtinAsyncHooksModule,
	builtinTimersPromisesModule,
} from "./timers.js";
import { builtinV8Module } from "./v8.js";
import { builtinVmModule } from "./vm.js";
import { resolveObjectURL, URL2, URLSearchParams } from "./whatwg-url.js";

// biome-ignore lint/complexity/noStaticOnlyClass: generated bridge consumers rely on this registry shape.
class BuiltinModuleRegistry {
	static builtinModules = [
		"assert",
		"assert/strict",
		"async_hooks",
		"buffer",
		"child_process",
		"console",
		"cluster",
		"constants",
		"crypto",
		"dgram",
		"diagnostics_channel",
		"domain",
		"dns",
		"dns/promises",
		"events",
		"fs",
		"fs/promises",
		"http",
		"http2",
		"https",
		"inspector",
		"module",
		"net",
		"os",
		"path",
		"path/posix",
		"path/win32",
		"perf_hooks",
		"process",
		"punycode",
		"querystring",
		"readline",
		"repl",
		"sqlite",
		"stream",
		"stream/consumers",
		"stream/promises",
		"stream/web",
		"string_decoder",
		"sys",
		"timers",
		"timers/promises",
		"trace_events",
		"tls",
		"tty",
		"url",
		"util",
		"util/types",
		"v8",
		"wasi",
		"worker_threads",
		"zlib",
		"vm",
	];
}
var builtinModules = BuiltinModuleRegistry.builtinModules;
var builtinTimersModule = {
	clearImmediate: globalThis.clearImmediate ?? (() => {}),
	clearInterval: globalThis.clearInterval ?? (() => {}),
	clearTimeout: globalThis.clearTimeout ?? (() => {}),
	setImmediate:
		globalThis.setImmediate ??
		((callback, ...args) =>
			globalThis.setTimeout?.(() => callback(...args), 0)),
	setInterval:
		globalThis.setInterval ??
		((callback, delay, ...args) =>
			globalThis.setTimeout?.(() => callback(...args), delay ?? 0)),
	setTimeout: globalThis.setTimeout ?? (() => void 0),
};
function unwrapStdlibModule(moduleNamespace) {
	if (
		moduleNamespace &&
		typeof moduleNamespace === "object" &&
		moduleNamespace.default != null
	) {
		return moduleNamespace.default;
	}
	return moduleNamespace;
}
function cloneStdlibModule(moduleNamespace) {
	const resolved = unwrapStdlibModule(moduleNamespace);
	if (resolved == null) {
		return resolved;
	}
	if (typeof resolved === "function") {
		return resolved;
	}
	if (typeof resolved === "object") {
		return { ...resolved };
	}
	return resolved;
}
function defineMissingModuleProperty(target, key, value) {
	if (target != null && typeof target[key] === "undefined") {
		target[key] = value;
	}
}
function defineModuleProperty(target, key, value) {
	if (target == null) return;
	Object.defineProperty(target, key, {
		configurable: true,
		writable: true,
		value,
	});
}
function bufferValidationBytes(input) {
	if (
		input instanceof ArrayBuffer ||
		(typeof SharedArrayBuffer !== "undefined" &&
			input instanceof SharedArrayBuffer)
	) {
		return new Uint8Array(input);
	}
	if (ArrayBuffer.isView(input) && !(input instanceof DataView)) {
		return new Uint8Array(input.buffer, input.byteOffset, input.byteLength);
	}
	const error = new TypeError(
		'The "input" argument must be an instance of ArrayBuffer, Buffer, or TypedArray',
	);
	error.code = "ERR_INVALID_ARG_TYPE";
	throw error;
}
function bufferIsAscii(input) {
	const bytes = bufferValidationBytes(input);
	for (let index = 0; index < bytes.length; index += 1) {
		if (bytes[index] > 0x7f) {
			return false;
		}
	}
	return true;
}
function bufferIsUtf8(input) {
	const bytes = bufferValidationBytes(input);
	let index = 0;
	while (index < bytes.length) {
		const first = bytes[index];
		if (first <= 0x7f) {
			index += 1;
			continue;
		}

		let continuationCount = 0;
		let minimumCodePoint = 0;
		let codePoint = 0;
		if (first >= 0xc2 && first <= 0xdf) {
			continuationCount = 1;
			minimumCodePoint = 0x80;
			codePoint = first & 0x1f;
		} else if (first >= 0xe0 && first <= 0xef) {
			continuationCount = 2;
			minimumCodePoint = 0x800;
			codePoint = first & 0x0f;
		} else if (first >= 0xf0 && first <= 0xf4) {
			continuationCount = 3;
			minimumCodePoint = 0x10000;
			codePoint = first & 0x07;
		} else {
			return false;
		}

		if (index + continuationCount >= bytes.length) {
			return false;
		}
		for (let offset = 1; offset <= continuationCount; offset += 1) {
			const continuation = bytes[index + offset];
			if ((continuation & 0xc0) !== 0x80) {
				return false;
			}
			codePoint = (codePoint << 6) | (continuation & 0x3f);
		}
		if (
			codePoint < minimumCodePoint ||
			codePoint > 0x10ffff ||
			(codePoint >= 0xd800 && codePoint <= 0xdfff)
		) {
			return false;
		}
		index += continuationCount + 1;
	}
	return true;
}
function trimNonRootTrailingSlash(pathValue) {
	return typeof pathValue === "string" &&
		pathValue.length > 1 &&
		pathValue.endsWith("/")
		? pathValue.slice(0, -1)
		: pathValue;
}
var builtinBufferStdlibModule = cloneStdlibModule(bufferStdlibModuleNs);
defineMissingModuleProperty(
	builtinBufferStdlibModule,
	"constants",
	BUFFER_CONSTANTS,
);
defineMissingModuleProperty(
	builtinBufferStdlibModule,
	"kMaxLength",
	BUFFER_MAX_LENGTH,
);
defineMissingModuleProperty(
	builtinBufferStdlibModule,
	"kStringMaxLength",
	BUFFER_MAX_STRING_LENGTH,
);
defineMissingModuleProperty(builtinBufferStdlibModule, "Blob", globalThis.Blob);
defineMissingModuleProperty(builtinBufferStdlibModule, "File", globalThis.File);
defineMissingModuleProperty(
	builtinBufferStdlibModule,
	"isAscii",
	bufferIsAscii,
);
defineMissingModuleProperty(builtinBufferStdlibModule, "isUtf8", bufferIsUtf8);
defineMissingModuleProperty(
	builtinBufferStdlibModule,
	"resolveObjectURL",
	resolveObjectURL,
);
var builtinConstantsStdlibModule = cloneStdlibModule(constantsStdlibModuleNs);
var builtinEventsStdlibModule = cloneStdlibModule(eventsStdlibModuleNs);
var builtinEventsConstructor = null;
var builtinEventsStdlibModuleInitialized = false;
function ensureBuiltinEventsStdlibModule() {
	if (builtinEventsStdlibModuleInitialized) {
		return builtinEventsStdlibModule;
	}
	builtinEventsStdlibModuleInitialized = true;
	builtinEventsConstructor =
		typeof builtinEventsStdlibModule === "function"
			? builtinEventsStdlibModule
			: builtinEventsStdlibModule?.EventEmitter;
	builtinEventsStdlibModule =
		typeof builtinEventsConstructor === "function"
			? Object.assign(builtinEventsConstructor, eventsModule)
			: eventsModule;
	builtinEventsStdlibModule.EventEmitter = builtinEventsStdlibModule;
	return builtinEventsStdlibModule;
}
var builtinPathStdlibModule = cloneStdlibModule(pathStdlibModuleNs);
// AgentOS targets Linux. Node exposes the selected platform implementation as
// both `path` and `path.posix`; cloning the stdlib namespace would otherwise
// leave `posix` pointing at the uncloned source object.
builtinPathStdlibModule.posix = builtinPathStdlibModule;
if (!builtinPathStdlibModule?.win32) {
	builtinPathStdlibModule.win32 =
		cloneStdlibModule(
			pathStdlibModuleNs?.win32 ?? pathStdlibModuleNs?.default?.win32,
		) ?? builtinPathStdlibModule;
}
if (builtinPathStdlibModule?.normalize) {
	const builtinPathNormalize = builtinPathStdlibModule.normalize.bind(
		builtinPathStdlibModule,
	);
	builtinPathStdlibModule.normalize = (pathValue) =>
		trimNonRootTrailingSlash(builtinPathNormalize(pathValue));
}
defineMissingModuleProperty(
	builtinPathStdlibModule,
	"toNamespacedPath",
	(pathValue) => String(pathValue),
);
defineMissingModuleProperty(
	builtinPathStdlibModule.posix,
	"toNamespacedPath",
	builtinPathStdlibModule.toNamespacedPath,
);
var builtinPunycodeStdlibModule = cloneStdlibModule(punycodeStdlibModuleNs);
var builtinQuerystringStdlibModule = cloneStdlibModule(
	querystringStdlibModuleNs,
);
var builtinStreamStdlibModule = cloneStdlibModule(streamStdlibModuleNs);
if (typeof builtinStreamStdlibModule?.Stream === "function") {
	Object.assign(builtinStreamStdlibModule.Stream, builtinStreamStdlibModule);
	builtinStreamStdlibModule = builtinStreamStdlibModule.Stream;
	builtinStreamStdlibModule.Stream = builtinStreamStdlibModule;
	const isBuiltinStreamInstance = (value) => {
		if (!value || (typeof value !== "object" && typeof value !== "function")) {
			return false;
		}
		const hasInstance = (candidateConstructor) =>
			typeof candidateConstructor === "function" &&
			Function.prototype[Symbol.hasInstance].call(candidateConstructor, value);
		return (
			hasInstance(builtinStreamStdlibModule.Readable) ||
			hasInstance(builtinStreamStdlibModule.Writable) ||
			hasInstance(builtinStreamStdlibModule.Duplex) ||
			hasInstance(builtinStreamStdlibModule.Transform) ||
			hasInstance(builtinStreamStdlibModule.PassThrough)
		);
	};
	Object.defineProperty(builtinStreamStdlibModule, Symbol.hasInstance, {
		configurable: true,
		value: isBuiltinStreamInstance,
	});
}
function defineReadableAsyncIterator(target) {
	if (!target || typeof target[Symbol.asyncIterator] === "function") {
		return;
	}
	Object.defineProperty(target, Symbol.asyncIterator, {
		configurable: true,
		value: function () {
			const stream = this;
			const queuedChunks = [];
			const pendingResolves = [];
			let done = false;
			let error = null;
			const flush = () => {
				while (pendingResolves.length > 0) {
					if (error) {
						pendingResolves.shift()(Promise.reject(error));
						continue;
					}
					if (queuedChunks.length > 0) {
						pendingResolves.shift()(
							Promise.resolve({ done: false, value: queuedChunks.shift() }),
						);
						continue;
					}
					if (done) {
						pendingResolves.shift()(
							Promise.resolve({ done: true, value: void 0 }),
						);
						continue;
					}
					break;
				}
			};
			const onData = (chunk) => {
				queuedChunks.push(chunk);
				flush();
			};
			const onEnd = () => {
				done = true;
				flush();
			};
			const onError = (reason) => {
				error = reason;
				done = true;
				flush();
			};
			stream.on?.("data", onData);
			stream.on?.("end", onEnd);
			stream.on?.("close", onEnd);
			stream.on?.("error", onError);
			stream.resume?.();
			return {
				next() {
					if (error) {
						return Promise.reject(error);
					}
					if (queuedChunks.length > 0) {
						return Promise.resolve({
							done: false,
							value: queuedChunks.shift(),
						});
					}
					if (done) {
						return Promise.resolve({ done: true, value: void 0 });
					}
					return new Promise((resolve) => {
						pendingResolves.push(resolve);
					});
				},
				return() {
					done = true;
					stream.off?.("data", onData);
					stream.off?.("end", onEnd);
					stream.off?.("close", onEnd);
					stream.off?.("error", onError);
					flush();
					return Promise.resolve({ done: true, value: void 0 });
				},
				[Symbol.asyncIterator]() {
					return this;
				},
			};
		},
	});
}
defineReadableAsyncIterator(builtinStreamStdlibModule?.Readable?.prototype);
defineReadableAsyncIterator(builtinStreamStdlibModule?.PassThrough?.prototype);
defineReadableAsyncIterator(builtinStreamStdlibModule?.Transform?.prototype);
defineReadableAsyncIterator(builtinStreamStdlibModule?.Duplex?.prototype);
let defaultByteHighWaterMark = 64 * 1024;
let defaultObjectHighWaterMark = 16;
defineMissingModuleProperty(
	builtinStreamStdlibModule,
	"getDefaultHighWaterMark",
	(objectMode) =>
		objectMode ? defaultObjectHighWaterMark : defaultByteHighWaterMark,
);
defineMissingModuleProperty(
	builtinStreamStdlibModule,
	"setDefaultHighWaterMark",
	(objectMode, value) => {
		if (!Number.isInteger(value) || value < 0) {
			const error = new RangeError(
				`The value of "value" is out of range. It must be a non-negative integer. Received ${value}.`,
			);
			error.code = "ERR_OUT_OF_RANGE";
			throw error;
		}
		if (objectMode) defaultObjectHighWaterMark = value;
		else defaultByteHighWaterMark = value;
	},
);
// readable-stream's browser build exposes toWeb(), but its lazy internal adapter
// table is empty. Replace those unusable stubs with the bridge-owned adapters.
defineModuleProperty(
	builtinStreamStdlibModule?.Readable,
	"toWeb",
	(stream) =>
		new ReadableStream({
			start(controller) {
				stream.on("data", (chunk) => controller.enqueue(chunk));
				stream.once("end", () => controller.close());
				stream.once("error", (error) => controller.error(error));
				stream.resume?.();
			},
			cancel(reason) {
				stream.destroy?.(reason instanceof Error ? reason : void 0);
			},
		}),
);
defineModuleProperty(
	builtinStreamStdlibModule?.Writable,
	"toWeb",
	(stream) =>
		new WritableStream({
			write(chunk) {
				return new Promise((resolve, reject) => {
					stream.write(chunk, (error) => (error ? reject(error) : resolve()));
				});
			},
			close() {
				return new Promise((resolve, reject) => {
					stream.once?.("error", reject);
					stream.end(resolve);
				});
			},
			abort(reason) {
				stream.destroy?.(reason instanceof Error ? reason : void 0);
			},
		}),
);
defineModuleProperty(builtinStreamStdlibModule?.Duplex, "toWeb", (stream) => ({
	readable: builtinStreamStdlibModule.Readable.toWeb(stream),
	writable: builtinStreamStdlibModule.Writable.toWeb(stream),
}));
defineMissingModuleProperty(
	builtinStreamStdlibModule,
	"isReadable",
	(stream) => {
		return (
			Boolean(stream) && stream.readable !== false && stream.destroyed !== true
		);
	},
);
defineMissingModuleProperty(
	builtinStreamStdlibModule,
	"isWritable",
	(stream) => {
		return (
			Boolean(stream) && stream.writable !== false && stream.destroyed !== true
		);
	},
);
defineMissingModuleProperty(
	builtinStreamStdlibModule,
	"isErrored",
	(stream) => {
		return stream?.errored != null;
	},
);
defineMissingModuleProperty(
	builtinStreamStdlibModule,
	"isDisturbed",
	(stream) => {
		return Boolean(
			stream?.locked ||
				stream?.disturbed === true ||
				stream?.readableDidRead === true,
		);
	},
);
var builtinStringDecoderStdlibModule = cloneStdlibModule(
	stringDecoderStdlibModuleNs,
);
function normalizeStringDecoderInput(value) {
	const BufferCtor = builtinBufferStdlibModule?.Buffer;
	if (
		typeof BufferCtor !== "function" ||
		value == null ||
		BufferCtor.isBuffer(value)
	) {
		return value;
	}
	if (ArrayBuffer.isView(value)) {
		return BufferCtor.from(value.buffer, value.byteOffset, value.byteLength);
	}
	if (
		value instanceof ArrayBuffer ||
		(typeof SharedArrayBuffer !== "undefined" &&
			value instanceof SharedArrayBuffer)
	) {
		return BufferCtor.from(value);
	}
	return value;
}
const BuiltinStringDecoder = builtinStringDecoderStdlibModule?.StringDecoder;
if (typeof BuiltinStringDecoder === "function") {
	for (const method of ["write", "end"]) {
		const original = BuiltinStringDecoder.prototype?.[method];
		if (typeof original !== "function") continue;
		BuiltinStringDecoder.prototype[method] = function stringDecoderMethod(
			value,
		) {
			return original.call(this, normalizeStringDecoderInput(value));
		};
	}
}
const upstreamUrlStdlibModule = unwrapStdlibModule(upstreamUrlHelpersModule);

function withNodeErrorCode(error, code) {
	error.code = code;
	return error;
}

function pathToFileURL2(filePath, options) {
	if (typeof filePath !== "string") {
		throw withNodeErrorCode(
			new TypeError('The "path" argument must be of type string.'),
			"ERR_INVALID_ARG_TYPE",
		);
	}
	if (options?.windows) {
		const windowsPath = filePath.replace(/\//g, "\\");
		let result = new URL2("file:///");
		if (windowsPath.startsWith("\\\\")) {
			const [host, ...segments] = windowsPath.slice(2).split("\\");
			result = new URL2("file:///");
			result.hostname = host;
			result.pathname = `/${segments.join("/")}`;
		} else if (/^[A-Za-z]:\\/.test(windowsPath)) {
			result = new URL2("file:///");
			result.pathname = `/${windowsPath.replace(/\\/g, "/")}`;
		} else {
			const relative = windowsPath.replace(/\\/g, "/");
			const trailingSlash = filePath.endsWith("\\") || filePath.endsWith("/");
			let resolved = builtinPathStdlibModule.posix.resolve(
				process_default.cwd(),
				relative,
			);
			if (trailingSlash && !resolved.endsWith("/")) resolved += "/";
			return upstreamUrlStdlibModule.pathToFileURL(resolved);
		}
		return result;
	}
	let resolved = builtinPathStdlibModule.posix.resolve(
		process_default.cwd(),
		filePath,
	);
	if (filePath.endsWith("/") && !resolved.endsWith("/")) {
		resolved += "/";
	}
	return upstreamUrlStdlibModule.pathToFileURL(resolved);
}

function fileURLToPath2(input, options) {
	if (!(input instanceof URL2) && typeof input !== "string") {
		throw withNodeErrorCode(
			new TypeError(
				'The "path" argument must be of type string or an instance of URL.',
			),
			"ERR_INVALID_ARG_TYPE",
		);
	}
	// Guest packages commonly pass already-decoded absolute paths through this
	// helper. Keep those paths inside the guest namespace instead of asking the
	// upstream URL parser to reinterpret them as URL strings.
	if (!options?.windows && typeof input === "string" && input.startsWith("/")) {
		return input;
	}
	try {
		if (options?.windows) {
			const parsed = input instanceof URL2 ? input : new URL2(input);
			if (parsed.protocol !== "file:") {
				throw withNodeErrorCode(
					new TypeError("The URL must be of scheme file"),
					"ERR_INVALID_URL_SCHEME",
				);
			}
			if (/%2f|%5c/i.test(parsed.pathname)) {
				throw withNodeErrorCode(
					new TypeError(
						"File URL path must not include encoded / or \\ characters",
					),
					"ERR_INVALID_FILE_URL_PATH",
				);
			}
			const pathname = decodeURIComponent(parsed.pathname);
			if (parsed.hostname) {
				return `\\\\${parsed.hostname}${pathname.replace(/\//g, "\\")}`;
			}
			if (!/^\/[A-Za-z]:/.test(pathname)) {
				throw withNodeErrorCode(
					new TypeError("File URL path must be absolute"),
					"ERR_INVALID_FILE_URL_PATH",
				);
			}
			return pathname.slice(1).replace(/\//g, "\\");
		}
		return upstreamUrlStdlibModule.fileURLToPath(input);
	} catch (error) {
		if (error?.code) throw error;
		const message = String(error?.message ?? error);
		if (message.includes("scheme file")) {
			throw withNodeErrorCode(error, "ERR_INVALID_URL_SCHEME");
		}
		if (message.includes("host must be") || message.includes("File URL host")) {
			throw withNodeErrorCode(error, "ERR_INVALID_FILE_URL_HOST");
		}
		if (
			message.includes("encoded /") ||
			message.includes("encoded \\") ||
			message.includes("must not include encoded")
		) {
			throw withNodeErrorCode(error, "ERR_INVALID_FILE_URL_PATH");
		}
		if (message.includes("Invalid URL")) {
			throw withNodeErrorCode(error, "ERR_INVALID_URL");
		}
		throw error;
	}
}

var builtinUrlStdlibModule = cloneStdlibModule(urlStdlibModuleNs);
var builtinUrlStdlibModuleInitialized = false;
function ensureBuiltinUrlStdlibModule() {
	if (builtinUrlStdlibModuleInitialized) {
		return builtinUrlStdlibModule;
	}
	builtinUrlStdlibModuleInitialized = true;
	builtinUrlStdlibModule.URL = URL2;
	builtinUrlStdlibModule.URLSearchParams = URLSearchParams;
	builtinUrlStdlibModule.fileURLToPath = fileURLToPath2;
	builtinUrlStdlibModule.pathToFileURL = pathToFileURL2;
	if (
		builtinUrlStdlibModule?.default &&
		typeof builtinUrlStdlibModule.default === "object"
	) {
		builtinUrlStdlibModule.default.URL = URL2;
		builtinUrlStdlibModule.default.URLSearchParams = URLSearchParams;
		builtinUrlStdlibModule.default.fileURLToPath = fileURLToPath2;
		builtinUrlStdlibModule.default.pathToFileURL = pathToFileURL2;
	}
	return builtinUrlStdlibModule;
}
function normalizeBuiltinRequest(request) {
	return String(request).replace(/^node:/, "");
}
let __jsRuntimeBuiltinAllowlist = null;
function rejectRestrictedBuiltinRequest(request) {
	const normalized = normalizeBuiltinRequest(request);
	// jsRuntime builtin allow-list gate. When the per-execution shim installed an
	// allow-list (non-node platforms => empty => deny all; node + explicit list),
	// deny any builtin whose root name is not permitted. Absent => unrestricted.
	const allow = __jsRuntimeBuiltinAllowlist;
	if (Array.isArray(allow)) {
		const root = String(normalized == null ? request : normalized)
			.replace(/^node:/, "")
			.split("/")[0];
		if (!allow.includes(root)) {
			throw createAccessDeniedBuiltinError(request);
		}
	}
	return normalized;
}
exposeCustomGlobal("__agentOSInitJsRuntime", (allowlist) => {
	__jsRuntimeBuiltinAllowlist = Array.isArray(allowlist)
		? allowlist.map(
				(name) =>
					String(name)
						.replace(/^node:/, "")
						.split("/")[0],
			)
		: null;
});
function loadBuiltinModule(request) {
	const normalized = rejectRestrictedBuiltinRequest(request);
	switch (normalized) {
		case "assert":
			return globalThis.__agentOsBuiltinAssertModule;
		case "assert/strict":
			return (
				globalThis.__agentOsBuiltinAssertModule?.strict ??
				globalThis.__agentOsBuiltinAssertModule
			);
		case "async_hooks":
			return builtinAsyncHooksModule;
		case "buffer":
			defineMissingModuleProperty(
				builtinBufferStdlibModule,
				"Blob",
				globalThis.Blob,
			);
			defineMissingModuleProperty(
				builtinBufferStdlibModule,
				"File",
				globalThis.File,
			);
			return builtinBufferStdlibModule;
		case "cluster":
			throw createAccessDeniedBuiltinError(request);
		case "crypto":
			return builtinCryptoModule;
		case "diagnostics_channel":
			return builtinDiagnosticsChannelModule;
		case "domain":
			throw createAccessDeniedBuiltinError(request);
		case "http":
			return _httpModule;
		case "http2":
			return _http2Module;
		case "events":
			return ensureBuiltinEventsStdlibModule();
		case "fs":
			return _fsModule;
		case "fs/promises":
			return _fsModule.promises;
		case "os":
			return _osModule;
		case "path":
			return builtinPathStdlibModule;
		case "path/posix":
			return builtinPathStdlibModule.posix;
		case "path/win32":
			return builtinPathStdlibModule.win32;
		case "perf_hooks":
			return builtinPerfHooksModule;
		case "process":
			return process_default;
		case "punycode":
			return builtinPunycodeStdlibModule;
		case "querystring":
			return builtinQuerystringStdlibModule;
		case "readline":
			return {
				createInterface(options = {}) {
					const input = options.input ?? null;
					const output = options.output ?? null;
					const listeners = new Map();
					let closed = false;
					let ended = false;
					let lineBuffer = "";
					const queuedLines = [];
					let pendingLineResolve = null;
					const pendingQuestionResolves = [];
					const textDecoder = new TextDecoder();
					const emit = (event, ...args) => {
						const current = listeners.get(event) ?? [];
						for (const listener of [...current]) {
							listener(...args);
						}
					};
					const enqueueLine = (line) => {
						if (pendingQuestionResolves.length > 0) {
							const resolve = pendingQuestionResolves.shift();
							resolve(line);
							return;
						}
						if (pendingLineResolve) {
							const resolve = pendingLineResolve;
							pendingLineResolve = null;
							resolve({ done: false, value: line });
							return;
						}
						queuedLines.push(line);
					};
					const emitLine = (line) => {
						emit("line", line);
						enqueueLine(line);
					};
					const flushBufferedLines = () => {
						let newlineIndex = lineBuffer.indexOf("\n");
						while (newlineIndex !== -1) {
							let line = lineBuffer.slice(0, newlineIndex);
							if (line.endsWith("\r")) {
								line = line.slice(0, -1);
							}
							lineBuffer = lineBuffer.slice(newlineIndex + 1);
							emitLine(line);
							newlineIndex = lineBuffer.indexOf("\n");
						}
					};
					const detachInput = () => {
						if (!input || typeof input.off !== "function") {
							return;
						}
						input.off("data", onData);
						input.off("end", onEnd);
					};
					const onData = (chunk) => {
						if (closed) {
							return;
						}
						if (typeof chunk === "string") {
							lineBuffer += chunk;
						} else if (chunk instanceof Uint8Array) {
							lineBuffer += textDecoder.decode(chunk, { stream: true });
						} else if (chunk != null) {
							lineBuffer += String(chunk);
						}
						flushBufferedLines();
					};
					const onEnd = () => {
						if (ended) {
							return;
						}
						ended = true;
						const trailing = textDecoder.decode();
						if (trailing) {
							lineBuffer += trailing;
						}
						flushBufferedLines();
						if (lineBuffer.length > 0) {
							emitLine(lineBuffer);
							lineBuffer = "";
						}
						api.close();
					};
					if (input && typeof input.on === "function") {
						input.on("data", onData);
						input.on("end", onEnd);
						if (typeof input.resume === "function") {
							input.resume();
						}
					}
					const iterator = {
						next() {
							if (queuedLines.length > 0) {
								return Promise.resolve({
									done: false,
									value: queuedLines.shift(),
								});
							}
							if (closed || ended) {
								return Promise.resolve({ done: true, value: void 0 });
							}
							return new Promise((resolve) => {
								pendingLineResolve = resolve;
							});
						},
						return() {
							api.close();
							return Promise.resolve({ done: true, value: void 0 });
						},
						[Symbol.asyncIterator]() {
							return this;
						},
					};
					const api = {
						addListener(event, listener) {
							return this.on(event, listener);
						},
						on(event, listener) {
							const current = listeners.get(event) ?? [];
							current.push(listener);
							listeners.set(event, current);
							return this;
						},
						once(event, listener) {
							const wrapped = (...args) => {
								this.off(event, wrapped);
								listener(...args);
							};
							return this.on(event, wrapped);
						},
						off(event, listener) {
							const current = listeners.get(event) ?? [];
							listeners.set(
								event,
								current.filter((candidate) => candidate !== listener),
							);
							return this;
						},
						removeListener(event, listener) {
							return this.off(event, listener);
						},
						close() {
							if (closed) {
								return;
							}
							closed = true;
							detachInput();
							while (pendingQuestionResolves.length > 0) {
								const resolve = pendingQuestionResolves.shift();
								resolve("");
							}
							if (pendingLineResolve) {
								const resolve = pendingLineResolve;
								pendingLineResolve = null;
								resolve({ done: true, value: void 0 });
							}
							emit("close");
						},
						question(prompt, callback) {
							if (output && typeof output.write === "function" && prompt) {
								output.write(String(prompt));
							}
							const readAnswer = () => {
								if (queuedLines.length > 0) {
									return Promise.resolve(queuedLines.shift());
								}
								if (closed || ended) {
									return Promise.resolve("");
								}
								return new Promise((resolve) => {
									pendingQuestionResolves.push(resolve);
								});
							};
							if (typeof callback === "function") {
								void readAnswer().then((answer) => {
									callback(answer);
								});
								return;
							}
							return readAnswer();
						},
						[Symbol.asyncIterator]() {
							return iterator;
						},
					};
					return api;
				},
			};
		case "repl":
			throw createAccessDeniedBuiltinError(request);
		case "stream":
			return builtinStreamStdlibModule;
		case "stream/consumers":
			return builtinStreamConsumersModule;
		case "stream/promises":
			return builtinStreamPromisesModule;
		case "string_decoder":
			return builtinStringDecoderStdlibModule;
		case "stream/web":
			return {
				ReadableStream: globalThis.ReadableStream,
				WritableStream: globalThis.WritableStream,
				TransformStream: globalThis.TransformStream,
				TextEncoderStream: globalThis.TextEncoderStream,
				TextDecoderStream: globalThis.TextDecoderStream,
				CompressionStream: globalThis.CompressionStream,
				DecompressionStream: globalThis.DecompressionStream,
			};
		case "timers":
			return builtinTimersModule;
		case "timers/promises":
			return builtinTimersPromisesModule;
		case "trace_events":
			throw createAccessDeniedBuiltinError(request);
		case "url":
			return ensureBuiltinUrlStdlibModule();
		case "sys":
			return installBuiltinUtilFormatWithOptions(
				globalThis.__agentOsBuiltinUtilModule,
			);
		case "util":
			return installBuiltinUtilFormatWithOptions(
				globalThis.__agentOsBuiltinUtilModule,
			);
		case "util/types":
			return installBuiltinUtilFormatWithOptions(
				globalThis.__agentOsBuiltinUtilModule,
			).types;
		case "child_process":
			return _childProcessModule;
		case "console":
			return builtinConsoleModule;
		case "constants":
			return builtinConstantsStdlibModule;
		case "dns":
			return _dnsModule;
		case "dns/promises":
			return _dnsModule.promises;
		case "net":
			return _netModule;
		case "tls":
			return _tlsModule;
		case "tty":
			return builtinTtyModule;
		case "dgram":
			return _dgramModule;
		case "sqlite":
			return _sqliteModule;
		case "https":
			return _httpsModule;
		case "inspector":
			return builtinInspectorModule;
		case "module":
			return _moduleModule;
		case "wasi":
			throw createAccessDeniedBuiltinError(request);
		case "zlib":
			return globalThis.__agentOsBuiltinZlibModule;
		case "v8":
			return builtinV8Module;
		case "vm":
			return builtinVmModule;
		case "worker_threads":
			return builtinWorkerThreadsModule;
		default: {
			const error = new Error(`Cannot find module '${request}'`);
			error.code = "MODULE_NOT_FOUND";
			throw error;
		}
	}
}

// Node 20.16+ exposes the same permission-gated builtin resolver on process.
// Install it on the shared process object so ESM dependencies can use it before
// an entrypoint-specific CommonJS `require` has been created.
defineMissingModuleProperty(
	process_default,
	"getBuiltinModule",
	loadBuiltinModule,
);

export {
	__jsRuntimeBuiltinAllowlist,
	builtinBufferStdlibModule,
	builtinConstantsStdlibModule,
	builtinEventsConstructor,
	builtinEventsStdlibModule,
	builtinEventsStdlibModuleInitialized,
	builtinModules,
	builtinPathStdlibModule,
	builtinPunycodeStdlibModule,
	builtinQuerystringStdlibModule,
	builtinStreamStdlibModule,
	builtinStringDecoderStdlibModule,
	builtinTimersModule,
	builtinUrlStdlibModule,
	builtinUrlStdlibModuleInitialized,
	cloneStdlibModule,
	defineMissingModuleProperty,
	defineReadableAsyncIterator,
	ensureBuiltinEventsStdlibModule,
	ensureBuiltinUrlStdlibModule,
	loadBuiltinModule,
	normalizeBuiltinRequest,
	rejectRestrictedBuiltinRequest,
	trimNonRootTrailingSlash,
	unwrapStdlibModule,
};
