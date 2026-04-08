import { transform } from "sucrase";
import { createBrowserNetworkAdapter } from "./driver.js";
import { validatePermissionSource } from "./permission-validation.js";
import type {
	CommandExecutor,
	ExecResult,
	NetworkAdapter,
	Permissions,
	RunResult,
	StdioChannel,
	TimingMitigation,
	VirtualDirEntry,
	VirtualStat,
} from "./runtime.js";
import {
	createCommandExecutorStub,
	createNetworkStub,
	exposeCustomGlobal,
	exposeMutableRuntimeStateGlobal,
	filterEnv,
	getIsolateRuntimeSource,
	getRequireSetupCode,
	isESM,
	POLYFILL_CODE_MAP,
	transformDynamicImport,
	wrapNetworkAdapter,
} from "./runtime.js";
import {
	assertBrowserSyncBridgeSupport,
	type BrowserSyncBridgeErrorPayload,
	type BrowserSyncBridgePayload,
	type BrowserWorkerSyncOperation,
	SYNC_BRIDGE_KIND_BINARY,
	SYNC_BRIDGE_KIND_JSON,
	SYNC_BRIDGE_KIND_NONE,
	SYNC_BRIDGE_KIND_TEXT,
	SYNC_BRIDGE_SIGNAL_KIND_INDEX,
	SYNC_BRIDGE_SIGNAL_LENGTH_INDEX,
	SYNC_BRIDGE_SIGNAL_STATE_IDLE,
	SYNC_BRIDGE_SIGNAL_STATE_INDEX,
	SYNC_BRIDGE_SIGNAL_STATUS_INDEX,
	SYNC_BRIDGE_STATUS_ERROR,
} from "./sync-bridge.js";
import type {
	BrowserWorkerExecOptions,
	BrowserWorkerInitPayload,
	BrowserWorkerOutboundMessage,
	BrowserWorkerRequestMessage,
	SerializedPermissions,
} from "./worker-protocol.js";

let networkAdapter: NetworkAdapter | null = null;
let commandExecutor: CommandExecutor | null = null;
let permissions: Permissions | undefined;
let initialized = false;
let controlToken: string | null = null;
let runtimeTimingMitigation: TimingMitigation = "freeze";
let runtimeProcessConfig: Record<string, unknown> | null = null;
let activeProcessRequestId: number | null = null;

const dynamicImportCache = new Map<string, unknown>();
const MAX_ERROR_MESSAGE_CHARS = 8192;
const MAX_STDIO_MESSAGE_CHARS = 8192;
const MAX_STDIO_DEPTH = 6;
const MAX_STDIO_OBJECT_KEYS = 60;
const MAX_STDIO_ARRAY_ITEMS = 120;

// Payload size defaults matching the Node runtime path
const DEFAULT_BASE64_TRANSFER_BYTES = 16 * 1024 * 1024;
const DEFAULT_JSON_PAYLOAD_BYTES = 4 * 1024 * 1024;
const PAYLOAD_LIMIT_ERROR_CODE = "ERR_SANDBOX_PAYLOAD_TOO_LARGE";

let base64TransferLimitBytes = DEFAULT_BASE64_TRANSFER_BYTES;
let jsonPayloadLimitBytes = DEFAULT_JSON_PAYLOAD_BYTES;

const encoder = new TextEncoder();
const decoder = new TextDecoder();
// biome-ignore lint/security/noGlobalEval: the browser worker intentionally evaluates isolated runtime source strings.
const globalEval = eval as (source: string) => unknown;
const SHARED_ARRAY_BUFFER_FREEZE_KEYS = [
	"byteLength",
	"slice",
	"grow",
	"maxByteLength",
	"growable",
] as const;

type TimingGlobalsSnapshot = {
	captured: boolean;
	dateDescriptor?: PropertyDescriptor;
	dateValue?: DateConstructor;
	performanceDescriptor?: PropertyDescriptor;
	performanceValue?: Performance;
	sharedArrayBufferDescriptor?: PropertyDescriptor;
	sharedArrayBufferValue?: typeof SharedArrayBuffer;
	sharedArrayBufferPrototypeDescriptors: Map<
		string,
		PropertyDescriptor | undefined
	>;
};

const timingGlobals: TimingGlobalsSnapshot = {
	captured: false,
	sharedArrayBufferPrototypeDescriptors: new Map(),
};

function getUtf8ByteLength(text: string): number {
	return encoder.encode(text).byteLength;
}

function getRequiredControlToken(): string {
	if (!controlToken) {
		throw new Error(
			"Browser runtime worker control channel is not initialized",
		);
	}
	return controlToken;
}

function captureTimingGlobals(): void {
	if (timingGlobals.captured) {
		return;
	}

	timingGlobals.captured = true;
	timingGlobals.dateDescriptor = Object.getOwnPropertyDescriptor(
		globalThis,
		"Date",
	);
	timingGlobals.dateValue = globalThis.Date;
	timingGlobals.performanceDescriptor = Object.getOwnPropertyDescriptor(
		globalThis,
		"performance",
	);
	timingGlobals.performanceValue = globalThis.performance;
	timingGlobals.sharedArrayBufferDescriptor = Object.getOwnPropertyDescriptor(
		globalThis,
		"SharedArrayBuffer",
	);
	timingGlobals.sharedArrayBufferValue = globalThis.SharedArrayBuffer;

	const sharedArrayBufferCtor = globalThis.SharedArrayBuffer;
	if (typeof sharedArrayBufferCtor !== "function") {
		return;
	}

	const prototype = sharedArrayBufferCtor.prototype as Record<string, unknown>;
	for (const key of SHARED_ARRAY_BUFFER_FREEZE_KEYS) {
		timingGlobals.sharedArrayBufferPrototypeDescriptors.set(
			key,
			Object.getOwnPropertyDescriptor(prototype, key),
		);
	}
}

function restoreGlobalProperty(
	name: "Date" | "performance" | "SharedArrayBuffer",
	descriptor?: PropertyDescriptor,
): void {
	if (descriptor) {
		try {
			Object.defineProperty(globalThis, name, descriptor);
			return;
		} catch {
			if ("value" in descriptor) {
				(globalThis as Record<string, unknown>)[name] = descriptor.value;
				return;
			}
		}
	}

	Reflect.deleteProperty(globalThis, name);
}

function restoreSharedArrayBufferPrototype(): void {
	const sharedArrayBufferCtor = timingGlobals.sharedArrayBufferValue;
	if (typeof sharedArrayBufferCtor !== "function") {
		return;
	}

	const prototype = sharedArrayBufferCtor.prototype as Record<string, unknown>;
	for (const key of SHARED_ARRAY_BUFFER_FREEZE_KEYS) {
		const descriptor =
			timingGlobals.sharedArrayBufferPrototypeDescriptors.get(key);
		try {
			if (descriptor) {
				Object.defineProperty(prototype, key, descriptor);
			} else {
				delete prototype[key];
			}
		} catch {
			// Ignore non-configurable SharedArrayBuffer prototype properties.
		}
	}
}

function restoreTimingMitigationOff(): void {
	captureTimingGlobals();
	restoreGlobalProperty("Date", timingGlobals.dateDescriptor);
	restoreGlobalProperty("performance", timingGlobals.performanceDescriptor);
	restoreSharedArrayBufferPrototype();
	restoreGlobalProperty(
		"SharedArrayBuffer",
		timingGlobals.sharedArrayBufferDescriptor,
	);

	if (
		typeof globalThis.performance === "undefined" ||
		globalThis.performance === null
	) {
		Object.defineProperty(globalThis, "performance", {
			value: {
				now: () => Date.now(),
			},
			configurable: true,
			writable: true,
		});
	}
}

function applyTimingMitigation(
	timingMitigation: TimingMitigation,
	frozenTimeMs?: number,
): number | undefined {
	captureTimingGlobals();
	restoreTimingMitigationOff();
	if (timingMitigation !== "freeze") {
		return undefined;
	}

	const frozenTimeValue =
		typeof frozenTimeMs === "number" && Number.isFinite(frozenTimeMs)
			? Math.trunc(frozenTimeMs)
			: Date.now();
	const originalDate =
		timingGlobals.dateValue ?? timingGlobals.dateDescriptor?.value ?? Date;
	const frozenDateNow = () => frozenTimeValue;
	const FrozenDate = function (...args: unknown[]) {
		if (new.target) {
			if (args.length === 0) {
				return new originalDate(frozenTimeValue);
			}
			return new originalDate(
				...(args as ConstructorParameters<DateConstructor>),
			);
		}
		return originalDate();
	} as unknown as DateConstructor;
	Object.defineProperty(FrozenDate, "prototype", {
		value: originalDate.prototype,
		writable: false,
		configurable: false,
	});
	Object.defineProperty(FrozenDate, "now", {
		value: frozenDateNow,
		configurable: true,
		writable: false,
	});
	FrozenDate.parse = originalDate.parse;
	FrozenDate.UTC = originalDate.UTC;
	try {
		Object.defineProperty(globalThis, "Date", {
			value: FrozenDate,
			configurable: true,
			writable: false,
		});
	} catch {
		(globalThis as Record<string, unknown>).Date = FrozenDate;
	}

	const frozenPerformance = Object.create(null) as Record<string, unknown>;
	const originalPerformance = timingGlobals.performanceValue;
	if (
		typeof originalPerformance !== "undefined" &&
		originalPerformance !== null
	) {
		const source = originalPerformance as unknown as Record<string, unknown>;
		for (const key of Object.getOwnPropertyNames(
			Object.getPrototypeOf(originalPerformance) ?? originalPerformance,
		)) {
			if (key === "now") {
				continue;
			}
			try {
				const value = source[key];
				frozenPerformance[key] =
					typeof value === "function" ? value.bind(originalPerformance) : value;
			} catch {
				// Ignore performance accessors that throw in this host.
			}
		}
	}
	Object.defineProperty(frozenPerformance, "now", {
		value: () => 0,
		configurable: true,
		writable: false,
	});
	Object.freeze(frozenPerformance);
	try {
		Object.defineProperty(globalThis, "performance", {
			value: frozenPerformance,
			configurable: true,
			writable: false,
		});
	} catch {
		(globalThis as Record<string, unknown>).performance = frozenPerformance;
	}

	const sharedArrayBufferCtor = timingGlobals.sharedArrayBufferValue;
	if (typeof sharedArrayBufferCtor === "function") {
		const prototype = sharedArrayBufferCtor.prototype as Record<
			string,
			unknown
		>;
		for (const key of SHARED_ARRAY_BUFFER_FREEZE_KEYS) {
			try {
				Object.defineProperty(prototype, key, {
					get() {
						throw new TypeError(
							"SharedArrayBuffer is not available in sandbox",
						);
					},
					configurable: true,
				});
			} catch {
				// Ignore non-configurable SharedArrayBuffer prototype properties.
			}
		}
	}
	try {
		Object.defineProperty(globalThis, "SharedArrayBuffer", {
			value: undefined,
			configurable: true,
			writable: false,
			enumerable: false,
		});
	} catch {
		Reflect.deleteProperty(globalThis, "SharedArrayBuffer");
	}

	return frozenTimeValue;
}

function assertPayloadByteLength(
	payloadLabel: string,
	actualBytes: number,
	maxBytes: number,
): void {
	if (actualBytes <= maxBytes) return;
	const error = new Error(
		`[${PAYLOAD_LIMIT_ERROR_CODE}] ${payloadLabel}: payload is ${actualBytes} bytes, limit is ${maxBytes} bytes`,
	);
	(error as { code?: string }).code = PAYLOAD_LIMIT_ERROR_CODE;
	throw error;
}

function assertTextPayloadSize(
	payloadLabel: string,
	text: string,
	maxBytes: number,
): void {
	assertPayloadByteLength(payloadLabel, getUtf8ByteLength(text), maxBytes);
}

function boundErrorMessage(message: string): string {
	if (message.length <= MAX_ERROR_MESSAGE_CHARS) {
		return message;
	}
	return `${message.slice(0, MAX_ERROR_MESSAGE_CHARS)}...[Truncated]`;
}

function boundStdioMessage(message: string): string {
	if (message.length <= MAX_STDIO_MESSAGE_CHARS) {
		return message;
	}
	return `${message.slice(0, MAX_STDIO_MESSAGE_CHARS)}...[Truncated]`;
}

function revivePermission(
	source?: string,
): ((req: unknown) => { allow: boolean }) | undefined {
	if (!source) return undefined;

	// Validate source before eval to prevent code injection
	if (!validatePermissionSource(source)) return undefined;

	try {
		const fn = new Function(`return (${source});`)();
		if (typeof fn === "function") return fn;
		return undefined;
	} catch {
		return undefined;
	}
}

/** Deserialize permission callbacks that were stringified for transfer across the Worker boundary. */
function revivePermissions(
	serialized?: SerializedPermissions,
): Permissions | undefined {
	if (!serialized) return undefined;
	const perms: Permissions = {};
	perms.fs = revivePermission(serialized.fs);
	perms.network = revivePermission(serialized.network);
	perms.childProcess = revivePermission(serialized.childProcess);
	perms.env = revivePermission(serialized.env);
	return perms;
}

/**
 * Wrap a sync function in the bridge calling convention (`applySync`) so
 * bridge code can call it the same way it calls bridge References.
 */
function makeApplySync<TArgs extends unknown[], TResult>(
	fn: (...args: TArgs) => TResult,
) {
	const applySync = (_ctx: undefined, args: TArgs): TResult => fn(...args);
	return {
		applySync,
		applySyncPromise: applySync,
	};
}

function makeApplySyncPromise<TArgs extends unknown[], TResult>(
	fn: (...args: TArgs) => Promise<TResult>,
) {
	return {
		applySyncPromise(_ctx: undefined, args: TArgs): Promise<TResult> {
			return fn(...args);
		},
	};
}

function makeApplyPromise<TArgs extends unknown[], TResult>(
	fn: (...args: TArgs) => Promise<TResult>,
) {
	return {
		apply(_ctx: undefined, args: TArgs): Promise<TResult> {
			return fn(...args);
		},
	};
}

function normalizeTextEncoding(options?: unknown): BufferEncoding | null {
	if (typeof options === "string") {
		return options as BufferEncoding;
	}

	if (options && typeof options === "object" && "encoding" in options) {
		const encoding = (options as { encoding?: unknown }).encoding;
		return typeof encoding === "string" ? (encoding as BufferEncoding) : null;
	}

	return null;
}

function toBinaryView(data: unknown): Uint8Array {
	if (data instanceof Uint8Array) {
		return data;
	}
	if (ArrayBuffer.isView(data)) {
		return new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
	}
	if (data instanceof ArrayBuffer) {
		return new Uint8Array(data);
	}
	return new TextEncoder().encode(String(data));
}

function toNodeBuffer(bytes: Uint8Array): Uint8Array | Buffer {
	if (typeof Buffer === "function") {
		return Buffer.from(bytes);
	}
	return bytes;
}

function createStats(stat: VirtualStat) {
	return {
		...stat,
		isFile: () => !stat.isDirectory && !stat.isSymbolicLink,
		isDirectory: () => stat.isDirectory,
		isSymbolicLink: () => stat.isSymbolicLink,
	};
}

function createDirent(entry: VirtualDirEntry) {
	return {
		name: entry.name,
		isFile: () => !entry.isDirectory && !entry.isSymbolicLink,
		isDirectory: () => entry.isDirectory,
		isSymbolicLink: () => Boolean(entry.isSymbolicLink),
	};
}

function createFsModule(syncBridge: ReturnType<typeof createSyncBridgeClient>) {
	const readFileSync = (path: string, options?: unknown) => {
		const encoding = normalizeTextEncoding(options);
		if (encoding) {
			return syncBridge.requestText("fs.readFile", [path]);
		}
		return toNodeBuffer(syncBridge.requestBinary("fs.readFileBinary", [path]));
	};

	const writeFileSync = (path: string, content: unknown) => {
		if (typeof content === "string") {
			syncBridge.requestVoid("fs.writeFile", [path, content]);
			return;
		}

		syncBridge.requestVoid("fs.writeFileBinary", [path, toBinaryView(content)]);
	};

	const mkdirSync = (
		path: string,
		options?: { recursive?: boolean } | boolean,
	) => {
		const recursive =
			typeof options === "boolean" ? options : (options?.recursive ?? true);
		if (recursive) {
			syncBridge.requestVoid("fs.mkdir", [path]);
			return;
		}
		syncBridge.requestVoid("fs.createDir", [path]);
	};

	const readdirSync = (path: string, options?: { withFileTypes?: boolean }) => {
		const entries = syncBridge.requestJson<VirtualDirEntry[]>("fs.readDir", [
			path,
		]);
		if (options?.withFileTypes) {
			return entries.map((entry) => createDirent(entry));
		}
		return entries.map((entry) => entry.name);
	};

	const statSync = (path: string) =>
		createStats(syncBridge.requestJson<VirtualStat>("fs.stat", [path]));
	const lstatSync = (path: string) =>
		createStats(syncBridge.requestJson<VirtualStat>("fs.lstat", [path]));

	const promises = {
		readFile(path: string, options?: unknown) {
			return Promise.resolve(readFileSync(path, options));
		},
		writeFile(path: string, content: unknown) {
			writeFileSync(path, content);
			return Promise.resolve();
		},
		mkdir(path: string, options?: { recursive?: boolean } | boolean) {
			mkdirSync(path, options);
			return Promise.resolve();
		},
		readdir(path: string, options?: { withFileTypes?: boolean }) {
			return Promise.resolve(readdirSync(path, options));
		},
		stat(path: string) {
			return Promise.resolve(statSync(path));
		},
		lstat(path: string) {
			return Promise.resolve(lstatSync(path));
		},
		unlink(path: string) {
			syncBridge.requestVoid("fs.unlink", [path]);
			return Promise.resolve();
		},
		rmdir(path: string) {
			syncBridge.requestVoid("fs.rmdir", [path]);
			return Promise.resolve();
		},
		rm(path: string) {
			syncBridge.requestVoid("fs.unlink", [path]);
			return Promise.resolve();
		},
		rename(oldPath: string, newPath: string) {
			syncBridge.requestVoid("fs.rename", [oldPath, newPath]);
			return Promise.resolve();
		},
		realpath(path: string) {
			return Promise.resolve(syncBridge.requestText("fs.realpath", [path]));
		},
		readlink(path: string) {
			return Promise.resolve(syncBridge.requestText("fs.readlink", [path]));
		},
		symlink(target: string, path: string) {
			syncBridge.requestVoid("fs.symlink", [target, path]);
			return Promise.resolve();
		},
		link(existingPath: string, newPath: string) {
			syncBridge.requestVoid("fs.link", [existingPath, newPath]);
			return Promise.resolve();
		},
		chmod(path: string, mode: number) {
			syncBridge.requestVoid("fs.chmod", [path, mode]);
			return Promise.resolve();
		},
		truncate(path: string, length = 0) {
			syncBridge.requestVoid("fs.truncate", [path, length]);
			return Promise.resolve();
		},
	};

	return {
		readFileSync,
		writeFileSync,
		mkdirSync,
		readdirSync,
		existsSync(path: string) {
			return syncBridge.requestJson<boolean>("fs.exists", [path]);
		},
		statSync,
		lstatSync,
		unlinkSync(path: string) {
			syncBridge.requestVoid("fs.unlink", [path]);
		},
		rmdirSync(path: string) {
			syncBridge.requestVoid("fs.rmdir", [path]);
		},
		rmSync(path: string) {
			syncBridge.requestVoid("fs.unlink", [path]);
		},
		renameSync(oldPath: string, newPath: string) {
			syncBridge.requestVoid("fs.rename", [oldPath, newPath]);
		},
		realpathSync(path: string) {
			return syncBridge.requestText("fs.realpath", [path]);
		},
		readlinkSync(path: string) {
			return syncBridge.requestText("fs.readlink", [path]);
		},
		symlinkSync(target: string, path: string) {
			syncBridge.requestVoid("fs.symlink", [target, path]);
		},
		linkSync(existingPath: string, newPath: string) {
			syncBridge.requestVoid("fs.link", [existingPath, newPath]);
		},
		chmodSync(path: string, mode: number) {
			syncBridge.requestVoid("fs.chmod", [path, mode]);
		},
		truncateSync(path: string, length = 0) {
			syncBridge.requestVoid("fs.truncate", [path, length]);
		},
		promises,
	};
}

// Save real postMessage before sandbox code can replace it
const _realPostMessage = self.postMessage.bind(self);

function postResponse(
	message:
		| {
				type: "response";
				id: number;
				ok: true;
				result: ExecResult | RunResult | true;
		  }
		| {
				type: "response";
				id: number;
				ok: false;
				error: { message: string; stack?: string; code?: string };
		  },
): void {
	_realPostMessage({
		controlToken: getRequiredControlToken(),
		...message,
	} satisfies BrowserWorkerOutboundMessage);
}

function postSyncRequest(message: {
	type: "sync-request";
	requestId: number;
	operation: BrowserWorkerSyncOperation;
	args: unknown[];
}): void {
	_realPostMessage({
		controlToken: getRequiredControlToken(),
		...message,
	} satisfies BrowserWorkerOutboundMessage);
}

function postStdio(
	requestId: number,
	channel: StdioChannel,
	message: string,
): void {
	const payload: BrowserWorkerOutboundMessage = {
		controlToken: getRequiredControlToken(),
		type: "stdio",
		requestId,
		channel,
		message,
	};
	_realPostMessage(payload);
}

function formatConsoleValue(
	value: unknown,
	seen = new WeakSet<object>(),
	depth = 0,
): string {
	if (value === null) {
		return "null";
	}
	if (value === undefined) {
		return "undefined";
	}
	if (typeof value === "string") {
		return value;
	}
	if (typeof value === "number" || typeof value === "boolean") {
		return String(value);
	}
	if (typeof value === "bigint") {
		return `${value.toString()}n`;
	}
	if (typeof value === "symbol") {
		return value.toString();
	}
	if (typeof value === "function") {
		return `[Function ${value.name || "anonymous"}]`;
	}
	if (typeof value !== "object") {
		return String(value);
	}
	if (seen.has(value)) {
		return "[Circular]";
	}
	if (depth >= MAX_STDIO_DEPTH) {
		return "[MaxDepth]";
	}

	seen.add(value);
	try {
		if (Array.isArray(value)) {
			const out = value
				.slice(0, MAX_STDIO_ARRAY_ITEMS)
				.map((item) => formatConsoleValue(item, seen, depth + 1));
			if (value.length > MAX_STDIO_ARRAY_ITEMS) {
				out.push('"[Truncated]"');
			}
			return `[${out.join(", ")}]`;
		}

		const entries: string[] = [];
		for (const key of Object.keys(value).slice(0, MAX_STDIO_OBJECT_KEYS)) {
			entries.push(
				`${key}: ${formatConsoleValue(
					(value as Record<string, unknown>)[key],
					seen,
					depth + 1,
				)}`,
			);
		}
		if (Object.keys(value).length > MAX_STDIO_OBJECT_KEYS) {
			entries.push('"[Truncated]"');
		}
		return `{ ${entries.join(", ")} }`;
	} catch {
		return "[Unserializable]";
	} finally {
		seen.delete(value);
	}
}

function emitStdio(
	requestId: number,
	channel: StdioChannel,
	args: unknown[],
): void {
	const message = boundStdioMessage(
		args.map((arg) => formatConsoleValue(arg)).join(" "),
	);
	postStdio(requestId, channel, message);
}

function createSyncBridgeClient(payload: BrowserSyncBridgePayload) {
	const signal = new Int32Array(payload.signalBuffer);
	const data = new Uint8Array(payload.dataBuffer);
	let nextRequestId = 1;
	const timeoutMs = payload.timeoutMs ?? 30_000;

	function readBytes(length: number): Uint8Array {
		if (length <= 0) {
			return new Uint8Array(0);
		}
		return data.slice(0, length);
	}

	function requestRaw(
		operation: BrowserWorkerSyncOperation,
		args: unknown[],
	): {
		kind: number;
		bytes: Uint8Array;
	} {
		Atomics.store(
			signal,
			SYNC_BRIDGE_SIGNAL_STATE_INDEX,
			SYNC_BRIDGE_SIGNAL_STATE_IDLE,
		);
		Atomics.store(signal, SYNC_BRIDGE_SIGNAL_STATUS_INDEX, 0);
		Atomics.store(signal, SYNC_BRIDGE_SIGNAL_KIND_INDEX, SYNC_BRIDGE_KIND_NONE);
		Atomics.store(signal, SYNC_BRIDGE_SIGNAL_LENGTH_INDEX, 0);

		postSyncRequest({
			type: "sync-request",
			requestId: nextRequestId++,
			operation,
			args,
		});

		while (true) {
			const result = Atomics.wait(
				signal,
				SYNC_BRIDGE_SIGNAL_STATE_INDEX,
				SYNC_BRIDGE_SIGNAL_STATE_IDLE,
				timeoutMs,
			);
			if (result !== "timed-out") {
				break;
			}
			throw new Error(
				`Browser runtime sync bridge timed out while handling ${operation}`,
			);
		}

		const status = Atomics.load(signal, SYNC_BRIDGE_SIGNAL_STATUS_INDEX);
		const kind = Atomics.load(signal, SYNC_BRIDGE_SIGNAL_KIND_INDEX);
		const length = Atomics.load(signal, SYNC_BRIDGE_SIGNAL_LENGTH_INDEX);
		const bytes = readBytes(length);
		Atomics.store(
			signal,
			SYNC_BRIDGE_SIGNAL_STATE_INDEX,
			SYNC_BRIDGE_SIGNAL_STATE_IDLE,
		);

		if (status === SYNC_BRIDGE_STATUS_ERROR) {
			const errorPayload = JSON.parse(
				decoder.decode(bytes),
			) as BrowserSyncBridgeErrorPayload;
			const error = new Error(errorPayload.message);
			if (errorPayload.code) {
				(error as { code?: string }).code = errorPayload.code;
			}
			throw error;
		}

		return { kind, bytes };
	}

	return {
		requestVoid(operation: BrowserWorkerSyncOperation, args: unknown[]) {
			requestRaw(operation, args);
		},
		requestText(operation: BrowserWorkerSyncOperation, args: unknown[]) {
			const result = requestRaw(operation, args);
			if (result.kind !== SYNC_BRIDGE_KIND_TEXT) {
				throw new Error(
					`Expected text response from ${operation}, received kind ${result.kind}`,
				);
			}
			return decoder.decode(result.bytes);
		},
		requestNullableText(
			operation: BrowserWorkerSyncOperation,
			args: unknown[],
		) {
			const result = requestRaw(operation, args);
			if (result.kind === SYNC_BRIDGE_KIND_NONE) {
				return null;
			}
			if (result.kind !== SYNC_BRIDGE_KIND_TEXT) {
				throw new Error(
					`Expected text response from ${operation}, received kind ${result.kind}`,
				);
			}
			return decoder.decode(result.bytes);
		},
		requestBinary(operation: BrowserWorkerSyncOperation, args: unknown[]) {
			const result = requestRaw(operation, args);
			if (result.kind !== SYNC_BRIDGE_KIND_BINARY) {
				throw new Error(
					`Expected binary response from ${operation}, received kind ${result.kind}`,
				);
			}
			return result.bytes;
		},
		requestJson<T>(operation: BrowserWorkerSyncOperation, args: unknown[]) {
			const result = requestRaw(operation, args);
			if (result.kind !== SYNC_BRIDGE_KIND_JSON) {
				throw new Error(
					`Expected JSON response from ${operation}, received kind ${result.kind}`,
				);
			}
			return JSON.parse(decoder.decode(result.bytes)) as T;
		},
	};
}

/**
 * Initialize the worker-side runtime: set up filesystem, network, bridge
 * globals, and load the bridge bundle. Called once before any exec/run.
 */
async function initRuntime(payload: BrowserWorkerInitPayload): Promise<void> {
	if (initialized) return;
	assertBrowserSyncBridgeSupport();
	captureTimingGlobals();
	if (!payload.syncBridge) {
		throw new Error(
			"Browser runtime sync bridge is required for filesystem and module loading parity",
		);
	}

	permissions = revivePermissions(payload.permissions);
	const syncBridge = createSyncBridgeClient(payload.syncBridge);

	// Apply payload limits (use defaults if not configured)
	base64TransferLimitBytes =
		payload.payloadLimits?.base64TransferBytes ?? DEFAULT_BASE64_TRANSFER_BYTES;
	jsonPayloadLimitBytes =
		payload.payloadLimits?.jsonPayloadBytes ?? DEFAULT_JSON_PAYLOAD_BYTES;

	if (payload.networkEnabled) {
		networkAdapter = wrapNetworkAdapter(
			createBrowserNetworkAdapter(),
			permissions,
		);
	} else {
		networkAdapter = createNetworkStub();
	}

	commandExecutor = createCommandExecutorStub();

	const processConfig = payload.processConfig ?? {};
	runtimeProcessConfig = processConfig as Record<string, unknown>;
	runtimeTimingMitigation =
		payload.timingMitigation ??
		processConfig.timingMitigation ??
		runtimeTimingMitigation;
	processConfig.env = filterEnv(processConfig.env, permissions);
	processConfig.timingMitigation = runtimeTimingMitigation;
	delete processConfig.frozenTimeMs;
	exposeCustomGlobal("_processConfig", processConfig);
	exposeCustomGlobal("_osConfig", payload.osConfig ?? {});

	// Set up filesystem bridge globals before loading runtime shims.
	const readFileRef = makeApplySync((path: string) => {
		const text = syncBridge.requestText("fs.readFile", [path]);
		assertTextPayloadSize(`fs.readFile ${path}`, text, jsonPayloadLimitBytes);
		return text;
	});
	const writeFileRef = makeApplySync((path: string, content: string) => {
		assertTextPayloadSize(
			`fs.writeFile ${path}`,
			content,
			jsonPayloadLimitBytes,
		);
		syncBridge.requestVoid("fs.writeFile", [path, content]);
	});
	const readFileBinaryRef = makeApplySync((path: string) => {
		const data = syncBridge.requestBinary("fs.readFileBinary", [path]);
		assertPayloadByteLength(
			`fs.readFileBinary ${path}`,
			data.byteLength,
			base64TransferLimitBytes,
		);
		return data;
	});
	const writeFileBinaryRef = makeApplySync(
		(path: string, binaryContent: Uint8Array) => {
			assertPayloadByteLength(
				`fs.writeFileBinary ${path}`,
				binaryContent.byteLength,
				base64TransferLimitBytes,
			);
			syncBridge.requestVoid("fs.writeFileBinary", [path, binaryContent]);
		},
	);
	const readDirRef = makeApplySync((path: string) => {
		const json = JSON.stringify(
			syncBridge.requestJson<VirtualDirEntry[]>("fs.readDir", [path]),
		);
		assertTextPayloadSize(`fs.readDir ${path}`, json, jsonPayloadLimitBytes);
		return json;
	});
	const mkdirRef = makeApplySync((path: string) => {
		syncBridge.requestVoid("fs.mkdir", [path]);
	});
	const rmdirRef = makeApplySync((path: string) => {
		syncBridge.requestVoid("fs.rmdir", [path]);
	});
	const existsRef = makeApplySync((path: string) => {
		return syncBridge.requestJson<boolean>("fs.exists", [path]);
	});
	const statRef = makeApplySync((path: string) => {
		return JSON.stringify(
			syncBridge.requestJson<VirtualStat>("fs.stat", [path]),
		);
	});
	const unlinkRef = makeApplySync((path: string) => {
		syncBridge.requestVoid("fs.unlink", [path]);
	});
	const renameRef = makeApplySync((oldPath: string, newPath: string) => {
		syncBridge.requestVoid("fs.rename", [oldPath, newPath]);
	});

	exposeCustomGlobal("_fs", {
		readFile: readFileRef,
		writeFile: writeFileRef,
		readFileBinary: readFileBinaryRef,
		writeFileBinary: writeFileBinaryRef,
		readDir: readDirRef,
		mkdir: mkdirRef,
		rmdir: rmdirRef,
		exists: existsRef,
		stat: statRef,
		unlink: unlinkRef,
		rename: renameRef,
	});

	exposeCustomGlobal("_loadPolyfill", (moduleName: string) => {
		const name = moduleName.replace(/^node:/, "");
		const polyfillMap = POLYFILL_CODE_MAP as Record<string, string>;
		return polyfillMap[name] ?? null;
	});

	const resolveModuleSync = (
		request: string,
		fromDir: string,
		mode?: "require" | "import",
	) => {
		return syncBridge.requestNullableText("module.resolve", [
			request,
			fromDir,
			mode ?? "require",
		]);
	};
	const loadFileSync = (path: string, _mode?: "require" | "import") => {
		const source = syncBridge.requestNullableText("module.loadFile", [path]);
		if (source === null) {
			return null;
		}
		let code = source;
		if (isESM(source, path)) {
			code = transform(code, { transforms: ["imports"] }).code;
		}
		return transformDynamicImport(code);
	};

	exposeCustomGlobal("_resolveModuleSync", resolveModuleSync);
	exposeCustomGlobal("_loadFileSync", loadFileSync);
	exposeCustomGlobal("_resolveModule", resolveModuleSync);
	exposeCustomGlobal("_loadFile", loadFileSync);

	exposeCustomGlobal("_scheduleTimer", {
		apply(_ctx: undefined, args: [number]) {
			return new Promise<void>((resolve) => {
				setTimeout(resolve, args[0]);
			});
		},
	});

	const netAdapter = networkAdapter ?? createNetworkStub();
	exposeCustomGlobal(
		"_networkFetchRaw",
		makeApplyPromise(async (url: string, optionsJson: string) => {
			const options = JSON.parse(optionsJson);
			const result = await netAdapter.fetch(url, options);
			return JSON.stringify(result);
		}),
	);
	exposeCustomGlobal(
		"_networkDnsLookupRaw",
		makeApplyPromise(async (hostname: string) => {
			const result = await netAdapter.dnsLookup(hostname);
			return JSON.stringify(result);
		}),
	);
	exposeCustomGlobal(
		"_networkHttpRequestRaw",
		makeApplyPromise(async (url: string, optionsJson: string) => {
			const options = JSON.parse(optionsJson);
			const result = await netAdapter.httpRequest(url, options);
			return JSON.stringify(result);
		}),
	);

	const execAdapter = commandExecutor ?? createCommandExecutorStub();
	let nextSessionId = 1;
	const sessions = new Map<number, ReturnType<CommandExecutor["spawn"]>>();
	const getDispatch = () =>
		(globalThis as Record<string, unknown>)._childProcessDispatch as
			| ((
					sessionId: number,
					type: "stdout" | "stderr" | "exit",
					data: Uint8Array | number,
			  ) => void)
			| undefined;

	exposeCustomGlobal(
		"_childProcessSpawnStart",
		makeApplySync((command: string, argsJson: string, optionsJson: string) => {
			const args = JSON.parse(argsJson) as string[];
			const options = JSON.parse(optionsJson) as {
				cwd?: string;
				env?: Record<string, string>;
			};
			const sessionId = nextSessionId++;
			const proc = execAdapter.spawn(command, args, {
				cwd: options.cwd,
				env: options.env,
				onStdout: (data) => {
					getDispatch()?.(sessionId, "stdout", data);
				},
				onStderr: (data) => {
					getDispatch()?.(sessionId, "stderr", data);
				},
			});
			void proc.wait().then((code) => {
				getDispatch()?.(sessionId, "exit", code);
				sessions.delete(sessionId);
			});
			sessions.set(sessionId, proc);
			return sessionId;
		}),
	);

	exposeCustomGlobal(
		"_childProcessStdinWrite",
		makeApplySync((sessionId: number, data: Uint8Array) => {
			sessions.get(sessionId)?.writeStdin(data);
		}),
	);

	exposeCustomGlobal(
		"_childProcessStdinClose",
		makeApplySync((sessionId: number) => {
			sessions.get(sessionId)?.closeStdin();
		}),
	);

	exposeCustomGlobal(
		"_childProcessKill",
		makeApplySync((sessionId: number, signal: number) => {
			sessions.get(sessionId)?.kill(signal);
		}),
	);

	exposeCustomGlobal(
		"_childProcessSpawnSync",
		makeApplySyncPromise(
			async (command: string, argsJson: string, optionsJson: string) => {
				const args = JSON.parse(argsJson) as string[];
				const options = JSON.parse(optionsJson) as {
					cwd?: string;
					env?: Record<string, string>;
				};
				const stdoutChunks: Uint8Array[] = [];
				const stderrChunks: Uint8Array[] = [];
				const proc = execAdapter.spawn(command, args, {
					cwd: options.cwd,
					env: options.env,
					onStdout: (data) => stdoutChunks.push(data),
					onStderr: (data) => stderrChunks.push(data),
				});
				const exitCode = await proc.wait();
				const decoder = new TextDecoder();
				const stdout = stdoutChunks.map((c) => decoder.decode(c)).join("");
				const stderr = stderrChunks.map((c) => decoder.decode(c)).join("");
				return JSON.stringify({ stdout, stderr, code: exitCode });
			},
		),
	);
	exposeCustomGlobal("_fsModule", createFsModule(syncBridge));
	exposeMutableRuntimeStateGlobal("_moduleCache", {});
	exposeMutableRuntimeStateGlobal("_pendingModules", {});
	exposeMutableRuntimeStateGlobal("_currentModule", { dirname: "/" });
	globalEval(getRequireSetupCode());
	ensureProcessGlobal();

	// Block dangerous Web APIs that bypass bridge permission checks
	const dangerousApis = [
		"XMLHttpRequest",
		"WebSocket",
		"importScripts",
		"indexedDB",
		"caches",
		"BroadcastChannel",
	];
	for (const api of dangerousApis) {
		try {
			delete (self as unknown as Record<string, unknown>)[api];
		} catch {
			// May not exist or may be non-configurable
		}
		Object.defineProperty(self, api, {
			get() {
				throw new ReferenceError(`${api} is not available in sandbox`);
			},
			configurable: false,
		});
	}

	// Lock down self.onmessage so sandbox code cannot hijack the control channel
	const currentHandler = self.onmessage;
	Object.defineProperty(self, "onmessage", {
		value: currentHandler,
		writable: false,
		configurable: false,
	});

	// Block self.postMessage so sandbox code cannot forge responses to host
	Object.defineProperty(self, "postMessage", {
		get() {
			throw new TypeError("postMessage is not available in sandbox");
		},
		configurable: false,
	});

	initialized = true;
}

function resetModuleState(cwd: string): void {
	exposeMutableRuntimeStateGlobal("_moduleCache", {});
	exposeMutableRuntimeStateGlobal("_pendingModules", {});
	exposeMutableRuntimeStateGlobal("_currentModule", { dirname: cwd });
}

function setDynamicImportFallback(): void {
	exposeMutableRuntimeStateGlobal("__dynamicImport", (specifier: string) => {
		const cached = dynamicImportCache.get(specifier);
		if (cached) return Promise.resolve(cached);
		try {
			const runtimeRequire = (globalThis as Record<string, unknown>).require as
				| ((request: string) => unknown)
				| undefined;
			if (typeof runtimeRequire !== "function") {
				throw new Error("require is not available in browser runtime");
			}
			const mod = runtimeRequire(specifier);
			return Promise.resolve({
				default: mod,
				...(mod as Record<string, unknown>),
			});
		} catch (e) {
			return Promise.reject(
				new Error(`Cannot dynamically import '${specifier}': ${String(e)}`),
			);
		}
	});
}

function toProcessChunk(
	value: string,
	encoding: string | null,
): string | Uint8Array {
	if (encoding) {
		return value;
	}
	return encoder.encode(value);
}

function normalizeProcessOutputChunk(chunk: unknown): string {
	if (typeof chunk === "string") {
		return chunk;
	}
	if (chunk instanceof Uint8Array) {
		return decoder.decode(chunk);
	}
	if (ArrayBuffer.isView(chunk)) {
		return decoder.decode(
			new Uint8Array(chunk.buffer, chunk.byteOffset, chunk.byteLength),
		);
	}
	if (chunk instanceof ArrayBuffer) {
		return decoder.decode(new Uint8Array(chunk));
	}
	return String(chunk);
}

function emitProcessStdio(channel: StdioChannel, chunk: unknown): boolean {
	if (activeProcessRequestId === null) {
		return true;
	}
	emitStdio(activeProcessRequestId, channel, [
		normalizeProcessOutputChunk(chunk),
	]);
	return true;
}

function createBrowserProcess(): Record<string, unknown> {
	type BrowserProcessListener = (value?: unknown) => void;
	type BrowserProcessListenerMap = Record<string, BrowserProcessListener[]>;
	type BrowserStdin = {
		readable: boolean;
		paused: boolean;
		encoding: string | null;
		isRaw: boolean;
		read(size?: number): string | Uint8Array | null;
		on(event: string, listener: BrowserProcessListener): BrowserStdin;
		once(event: string, listener: BrowserProcessListener): BrowserStdin;
		off(event: string, listener: BrowserProcessListener): BrowserStdin;
		removeListener(
			event: string,
			listener: BrowserProcessListener,
		): BrowserStdin;
		emit(event: string, value?: unknown): boolean;
		pause(): BrowserStdin;
		resume(): BrowserStdin;
		setEncoding(encoding: string): BrowserStdin;
		setRawMode(mode: boolean): BrowserStdin;
		readonly isTTY: boolean;
		[Symbol.asyncIterator](): AsyncGenerator<string, void, void>;
	};

	let cwd = "/";
	let stdinData = "";
	let stdinPosition = 0;
	let stdinEnded = false;
	let stdinFlushQueued = false;
	const stdinListeners: BrowserProcessListenerMap = Object.create(null);
	const stdinOnceListeners: BrowserProcessListenerMap = Object.create(null);

	const emitStdinListeners = (event: string, value?: unknown): boolean => {
		const listeners = [
			...(stdinListeners[event] ?? []),
			...(stdinOnceListeners[event] ?? []),
		];
		stdinOnceListeners[event] = [];
		for (const listener of listeners) {
			listener(value);
		}
		return listeners.length > 0;
	};

	const clearStdinListeners = (): void => {
		for (const key of Object.keys(stdinListeners)) {
			stdinListeners[key] = [];
		}
		for (const key of Object.keys(stdinOnceListeners)) {
			stdinOnceListeners[key] = [];
		}
	};

	const flushStdin = (): void => {
		stdinFlushQueued = false;
		if (stdin.paused || stdinEnded) {
			return;
		}
		if (stdinPosition < stdinData.length) {
			const chunk = stdinData.slice(stdinPosition);
			stdinPosition = stdinData.length;
			emitStdinListeners("data", toProcessChunk(chunk, stdin.encoding));
		}
		if (!stdinEnded) {
			stdinEnded = true;
			emitStdinListeners("end");
			emitStdinListeners("close");
		}
	};

	const scheduleStdinFlush = (): void => {
		if (stdinFlushQueued) {
			return;
		}
		stdinFlushQueued = true;
		queueMicrotask(flushStdin);
	};

	const stdin: BrowserStdin = {
		readable: true,
		paused: true,
		encoding: null,
		isRaw: false,
		read(size?: number) {
			if (stdinPosition >= stdinData.length) {
				return null;
			}
			const chunk = size
				? stdinData.slice(stdinPosition, stdinPosition + size)
				: stdinData.slice(stdinPosition);
			stdinPosition += chunk.length;
			return toProcessChunk(chunk, stdin.encoding);
		},
		on(event, listener) {
			if (!stdinListeners[event]) {
				stdinListeners[event] = [];
			}
			stdinListeners[event].push(listener);
			if (event === "data" && stdin.paused) {
				stdin.resume();
			}
			return stdin;
		},
		once(event, listener) {
			if (!stdinOnceListeners[event]) {
				stdinOnceListeners[event] = [];
			}
			stdinOnceListeners[event].push(listener);
			if (event === "data" && stdin.paused) {
				stdin.resume();
			}
			return stdin;
		},
		off(event, listener) {
			if (!stdinListeners[event]) {
				return stdin;
			}
			stdinListeners[event] = stdinListeners[event].filter(
				(candidate) => candidate !== listener,
			);
			return stdin;
		},
		removeListener(event, listener) {
			return stdin.off(event, listener);
		},
		emit(event, value) {
			return emitStdinListeners(event, value);
		},
		pause() {
			stdin.paused = true;
			return stdin;
		},
		resume() {
			stdin.paused = false;
			scheduleStdinFlush();
			return stdin;
		},
		setEncoding(encoding) {
			stdin.encoding = encoding;
			return stdin;
		},
		setRawMode(mode) {
			stdin.isRaw = mode;
			return stdin;
		},
		get isTTY() {
			return false;
		},
		async *[Symbol.asyncIterator]() {
			const remaining = stdinData.slice(stdinPosition);
			for (const line of remaining.split("\n")) {
				if (line.length > 0) {
					yield line;
				}
			}
		},
	};

	const processBridge = {
		browser: true,
		env: {} as Record<string, string>,
		argv: ["node"],
		argv0: "node",
		pid: 1,
		ppid: 0,
		platform: "browser",
		version: "v22.0.0",
		versions: {
			node: "22.0.0",
		},
		stdin,
		stdout: {
			isTTY: false,
			write(chunk: unknown) {
				return emitProcessStdio("stdout", chunk);
			},
		},
		stderr: {
			isTTY: false,
			write(chunk: unknown) {
				return emitProcessStdio("stderr", chunk);
			},
		},
		exitCode: 0,
		cwd: () => cwd,
		chdir: (nextCwd: string) => {
			cwd = String(nextCwd);
		},
		nextTick: (callback: (...args: unknown[]) => void, ...args: unknown[]) => {
			queueMicrotask(() => callback(...args));
		},
		exit(code?: number) {
			const exitCode =
				typeof code === "number" ? code : (processBridge.exitCode ?? 0);
			processBridge.exitCode = exitCode;
			throw new Error(`process.exit(${exitCode})`);
		},
		on() {
			return processBridge;
		},
		once() {
			return processBridge;
		},
		off() {
			return processBridge;
		},
		removeListener() {
			return processBridge;
		},
		emit() {
			return false;
		},
		__agentOsRefreshProcess(nextConfig?: Record<string, unknown>) {
			clearStdinListeners();
			stdinData = typeof nextConfig?.stdin === "string" ? nextConfig.stdin : "";
			stdinPosition = 0;
			stdinEnded = false;
			stdinFlushQueued = false;
			stdin.paused = true;
			stdin.encoding = null;
			stdin.isRaw = false;
			processBridge.exitCode = 0;
			processBridge.env =
				nextConfig?.env && typeof nextConfig.env === "object"
					? { ...(nextConfig.env as Record<string, string>) }
					: {};
			if (typeof nextConfig?.cwd === "string") {
				cwd = nextConfig.cwd;
			}
			processBridge.argv = Array.isArray(nextConfig?.argv)
				? nextConfig.argv.map((value) => String(value))
				: ["node"];
			processBridge.argv0 = processBridge.argv[0] ?? "node";
			if (typeof nextConfig?.platform === "string") {
				processBridge.platform = nextConfig.platform;
			}
			if (typeof nextConfig?.version === "string") {
				processBridge.version = nextConfig.version;
				processBridge.versions.node = nextConfig.version.replace(/^v/, "");
			}
			if (typeof nextConfig?.pid === "number") {
				processBridge.pid = nextConfig.pid;
			}
			if (typeof nextConfig?.ppid === "number") {
				processBridge.ppid = nextConfig.ppid;
			}
		},
	};

	return processBridge;
}

function getRuntimeProcess(): Record<string, unknown> | undefined {
	const proc = (globalThis as Record<string, unknown>).process;
	if (!proc || typeof proc !== "object") {
		return undefined;
	}
	return proc as Record<string, unknown>;
}

function refreshRuntimeProcess(): void {
	const proc = getRuntimeProcess();
	const refresh = proc?.__agentOsRefreshProcess as
		| ((nextConfig?: Record<string, unknown> | null) => void)
		| undefined;
	if (typeof refresh === "function") {
		refresh(runtimeProcessConfig);
	}
}

function ensureProcessGlobal(): void {
	if (getRuntimeProcess()) {
		refreshRuntimeProcess();
		return;
	}

	exposeMutableRuntimeStateGlobal("process", createBrowserProcess());
	refreshRuntimeProcess();
}

function captureConsole(
	requestId: number,
	captureStdio: boolean,
): {
	restore: () => void;
} {
	const original = console;
	if (!captureStdio) {
		const sandboxConsole = {
			log: () => undefined,
			info: () => undefined,
			warn: () => undefined,
			error: () => undefined,
		};
		(globalThis as Record<string, unknown>).console = sandboxConsole;
		return {
			restore: () => {
				(globalThis as Record<string, unknown>).console = original;
			},
		};
	}

	const sandboxConsole = {
		log: (...args: unknown[]) => emitStdio(requestId, "stdout", args),
		info: (...args: unknown[]) => emitStdio(requestId, "stdout", args),
		warn: (...args: unknown[]) => emitStdio(requestId, "stderr", args),
		error: (...args: unknown[]) => emitStdio(requestId, "stderr", args),
	};
	(globalThis as Record<string, unknown>).console = sandboxConsole;
	return {
		restore: () => {
			(globalThis as Record<string, unknown>).console = original;
		},
	};
}

function updateProcessConfig(
	options: BrowserWorkerExecOptions | undefined,
	timingMitigation: TimingMitigation,
	frozenTimeMs?: number,
): void {
	if (runtimeProcessConfig) {
		runtimeProcessConfig.timingMitigation = timingMitigation;
		if (frozenTimeMs === undefined) {
			delete runtimeProcessConfig.frozenTimeMs;
		} else {
			runtimeProcessConfig.frozenTimeMs = frozenTimeMs;
		}
		runtimeProcessConfig.stdin = options?.stdin ?? "";
		if (options?.env) {
			const filtered = filterEnv(options.env, permissions);
			const currentEnv =
				runtimeProcessConfig.env && typeof runtimeProcessConfig.env === "object"
					? (runtimeProcessConfig.env as Record<string, string>)
					: {};
			runtimeProcessConfig.env = { ...currentEnv, ...filtered };
		}
	}

	refreshRuntimeProcess();

	const proc = getRuntimeProcess();
	if (!proc) return;
	proc.exitCode = 0;
	proc.timingMitigation = timingMitigation;
	if (frozenTimeMs === undefined) {
		delete proc.frozenTimeMs;
	} else {
		proc.frozenTimeMs = frozenTimeMs;
	}
	if (options?.cwd && typeof proc.chdir === "function") {
		exposeMutableRuntimeStateGlobal("__runtimeProcessCwdOverride", options.cwd);
		globalEval(getIsolateRuntimeSource("overrideProcessCwd"));
		try {
			proc.chdir(options.cwd);
		} catch (error) {
			if (
				!(
					error instanceof Error &&
					error.message.includes("process.chdir() is not supported in workers")
				)
			) {
				throw error;
			}
		}
	}
}

/**
 * Execute user code as a script (process-style). Transforms ESM/dynamic
 * imports, sets up module/exports globals, and waits for active handles.
 */
async function execScript(
	requestId: number,
	code: string,
	options?: BrowserWorkerExecOptions,
	captureStdio = false,
): Promise<ExecResult> {
	resetModuleState(options?.cwd ?? "/");
	const timingMitigation = options?.timingMitigation ?? runtimeTimingMitigation;
	const frozenTimeMs = applyTimingMitigation(timingMitigation);
	updateProcessConfig(options, timingMitigation, frozenTimeMs);
	setDynamicImportFallback();

	const previousProcessRequestId = activeProcessRequestId;
	activeProcessRequestId = captureStdio ? requestId : null;
	const { restore } = captureConsole(requestId, captureStdio);
	try {
		let transformed = code;
		if (isESM(code, options?.filePath)) {
			transformed = transform(transformed, { transforms: ["imports"] }).code;
		}
		transformed = transformDynamicImport(transformed);

		exposeMutableRuntimeStateGlobal("module", { exports: {} });
		const moduleRef = (globalThis as Record<string, unknown>).module as {
			exports?: unknown;
		};
		exposeMutableRuntimeStateGlobal("exports", moduleRef.exports);

		if (options?.filePath) {
			const dirname = options.filePath.includes("/")
				? options.filePath.substring(0, options.filePath.lastIndexOf("/")) ||
					"/"
				: "/";
			exposeMutableRuntimeStateGlobal("__filename", options.filePath);
			exposeMutableRuntimeStateGlobal("__dirname", dirname);
			exposeMutableRuntimeStateGlobal("_currentModule", {
				dirname,
				filename: options.filePath,
			});
		}

		// Await the eval result so async IIFEs / top-level promise expressions
		// resolve before we check for active handles.
		const evalResult = globalEval(transformed);
		if (
			evalResult &&
			typeof evalResult === "object" &&
			typeof (evalResult as Record<string, unknown>).then === "function"
		) {
			await evalResult;
		}
		await Promise.resolve();

		const waitForActiveHandles = (globalThis as Record<string, unknown>)
			._waitForActiveHandles as (() => Promise<void>) | undefined;
		if (typeof waitForActiveHandles === "function") {
			await waitForActiveHandles();
		}

		const exitCode =
			((globalThis as Record<string, unknown>).process as { exitCode?: number })
				?.exitCode ?? 0;

		return {
			code: exitCode,
		};
	} catch (err) {
		const message = err instanceof Error ? err.message : String(err);
		const exitMatch = message.match(/process\.exit\((\d+)\)/);
		if (exitMatch) {
			const exitCode = Number.parseInt(exitMatch[1], 10);
			return {
				code: exitCode,
			};
		}
		return {
			code: 1,
			errorMessage: boundErrorMessage(message),
		};
	} finally {
		activeProcessRequestId = previousProcessRequestId;
		restore();
	}
}

async function runScript<T = unknown>(
	requestId: number,
	code: string,
	filePath?: string,
	captureStdio = false,
): Promise<RunResult<T>> {
	const execResult = await execScript(
		requestId,
		code,
		{ filePath },
		captureStdio,
	);
	const moduleObj = (globalThis as Record<string, unknown>).module as {
		exports?: T;
	};
	return {
		...execResult,
		exports: moduleObj?.exports,
	};
}

self.onmessage = async (event: MessageEvent<BrowserWorkerRequestMessage>) => {
	const message = event.data;
	try {
		if (message.type === "init") {
			if (
				typeof message.controlToken !== "string" ||
				message.controlToken.length === 0
			) {
				return;
			}
			if (controlToken && message.controlToken !== controlToken) {
				return;
			}
			controlToken = message.controlToken;
			await initRuntime(message.payload);
			postResponse({
				type: "response",
				id: message.id,
				ok: true,
				result: true,
			});
			return;
		}
		if (!controlToken || message.controlToken !== controlToken) {
			return;
		}
		if (!initialized) {
			throw new Error("Sandbox worker not initialized");
		}
		if (message.type === "exec") {
			const result = await execScript(
				message.id,
				message.payload.code,
				message.payload.options,
				message.payload.captureStdio,
			);
			postResponse({ type: "response", id: message.id, ok: true, result });
			return;
		}
		if (message.type === "run") {
			const result = await runScript(
				message.id,
				message.payload.code,
				message.payload.filePath,
				message.payload.captureStdio,
			);
			postResponse({ type: "response", id: message.id, ok: true, result });
			return;
		}
		if (message.type === "dispose") {
			postResponse({
				type: "response",
				id: message.id,
				ok: true,
				result: true,
			});
			close();
		}
	} catch (err) {
		const error = err as { message?: string; stack?: string; code?: string };
		postResponse({
			type: "response",
			id: message.id,
			ok: false,
			error: {
				message: error?.message ?? String(err),
				stack: error?.stack,
				code: error?.code,
			},
		});
	}
};
