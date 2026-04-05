import {
	createFsStub,
	createNetworkStub,
	loadFile,
	mkdir,
	resolveModule,
} from "./runtime.js";
import type {
	ExecOptions,
	ExecResult,
	NetworkAdapter,
	NodeRuntimeDriver,
	NodeRuntimeDriverFactory,
	RunResult,
	RuntimeDriverOptions,
	StdioHook,
	TimingMitigation,
	VirtualDirEntry,
	VirtualFileSystem,
	VirtualStat,
} from "./runtime.js";
import { getBrowserSystemDriverOptions } from "./driver.js";
import {
	SYNC_BRIDGE_KIND_BINARY,
	SYNC_BRIDGE_KIND_JSON,
	SYNC_BRIDGE_KIND_NONE,
	SYNC_BRIDGE_KIND_TEXT,
	SYNC_BRIDGE_PAYLOAD_LIMIT_ERROR_CODE,
	SYNC_BRIDGE_SIGNAL_KIND_INDEX,
	SYNC_BRIDGE_SIGNAL_LENGTH_INDEX,
	SYNC_BRIDGE_SIGNAL_STATE_INDEX,
	SYNC_BRIDGE_SIGNAL_STATE_READY,
	SYNC_BRIDGE_SIGNAL_STATUS_INDEX,
	SYNC_BRIDGE_STATUS_ERROR,
	SYNC_BRIDGE_STATUS_OK,
	assertBrowserSyncBridgeSupport,
	createBrowserSyncBridgePayload,
	toBrowserSyncBridgeError,
	type BrowserSyncBridgePayload,
	type BrowserWorkerSyncRequestMessage,
} from "./sync-bridge.js";
import type {
	BrowserWorkerExecOptions,
	BrowserWorkerInitPayload,
	BrowserWorkerOutboundMessage,
	BrowserWorkerRequestMessage,
	BrowserWorkerResponseMessage,
	BrowserWorkerStdioMessage,
	SerializedPermissions,
} from "./worker-protocol.js";

export interface BrowserRuntimeDriverFactoryOptions {
	workerUrl?: URL | string;
}

type PendingRequest = {
	resolve(value: unknown): void;
	reject(reason: unknown): void;
	hook?: StdioHook;
};

type SyncBridgeResponse =
	| { kind: typeof SYNC_BRIDGE_KIND_NONE }
	| { kind: typeof SYNC_BRIDGE_KIND_TEXT; value: string }
	| { kind: typeof SYNC_BRIDGE_KIND_BINARY; value: Uint8Array }
	| { kind: typeof SYNC_BRIDGE_KIND_JSON; value: unknown };

const DEFAULT_BROWSER_TIMING_MITIGATION: TimingMitigation = "freeze";

const BROWSER_OPTION_VALIDATORS = [
	{
		label: "memoryLimit",
		hasValue: (options: RuntimeDriverOptions) => options.memoryLimit !== undefined,
	},
	{
		label: "cpuTimeLimitMs",
		hasValue: (options: RuntimeDriverOptions) =>
			options.cpuTimeLimitMs !== undefined,
	},
];

function serializePermissions(
	permissions?: RuntimeDriverOptions["system"]["permissions"],
): SerializedPermissions | undefined {
	if (!permissions) {
		return undefined;
	}
	const serialize = (fn?: unknown) =>
		typeof fn === "function" ? fn.toString() : undefined;
	return {
		fs: serialize(permissions.fs),
		network: serialize(permissions.network),
		childProcess: serialize(permissions.childProcess),
		env: serialize(permissions.env),
	};
}

function resolveWorkerUrl(workerUrl?: URL | string): URL {
	if (workerUrl instanceof URL) {
		return workerUrl;
	}
	if (workerUrl) {
		return new URL(workerUrl, import.meta.url);
	}
	return new URL("./worker.js", import.meta.url);
}

function createWorkerControlToken(): string {
	if (typeof globalThis.crypto?.randomUUID === "function") {
		return globalThis.crypto.randomUUID();
	}
	return `browser-runtime-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

function toBrowserWorkerExecOptions(
	options?: ExecOptions,
): BrowserWorkerExecOptions | undefined {
	if (!options) {
		return undefined;
	}
	return {
		filePath: options.filePath,
		env: options.env,
		cwd: options.cwd,
		stdin: options.stdin,
		timingMitigation: options.timingMitigation,
	};
}

function validateBrowserRuntimeOptions(options: RuntimeDriverOptions): void {
	const unsupported = BROWSER_OPTION_VALIDATORS
		.filter((validator) => validator.hasValue(options))
		.map((validator) => validator.label);
	if (unsupported.length === 0) {
		return;
	}
	throw new Error(
		`Browser runtime does not support Node-only options: ${unsupported.join(", ")}`,
	);
}

function validateBrowserExecOptions(options?: ExecOptions): void {
	const unsupported: string[] = [];
	if (options?.cpuTimeLimitMs !== undefined) {
		unsupported.push("cpuTimeLimitMs");
	}
	if (unsupported.length === 0) {
		return;
	}
	throw new Error(
		`Browser runtime does not support Node-only exec options: ${unsupported.join(", ")}`,
	);
}

function isStdioMessage(
	message: BrowserWorkerOutboundMessage,
): message is BrowserWorkerStdioMessage {
	return message.type === "stdio";
}

function isResponseMessage(
	message: BrowserWorkerOutboundMessage,
): message is BrowserWorkerResponseMessage {
	return message.type === "response";
}

function isSyncRequestMessage(
	message: BrowserWorkerOutboundMessage,
): message is BrowserWorkerSyncRequestMessage {
	return message.type === "sync-request";
}

function createSyncBridgeFilesystem(
	options: RuntimeDriverOptions,
): VirtualFileSystem {
	return options.system.filesystem ?? createFsStub();
}

function throwBridgePayloadTooLarge(
	label: string,
	actualBytes: number,
	maxBytes: number,
): never {
	const error = new Error(
		`[${SYNC_BRIDGE_PAYLOAD_LIMIT_ERROR_CODE}] ${label}: payload is ${actualBytes} bytes, limit is ${maxBytes} bytes`,
	);
	(error as { code?: string }).code = SYNC_BRIDGE_PAYLOAD_LIMIT_ERROR_CODE;
	throw error;
}

function toUint8Array(value: unknown): Uint8Array {
	if (value instanceof Uint8Array) {
		return value;
	}
	if (ArrayBuffer.isView(value)) {
		return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
	}
	if (value instanceof ArrayBuffer) {
		return new Uint8Array(value);
	}
	return new TextEncoder().encode(String(value));
}

function createSyncBridgeResponseBytes(
	response: SyncBridgeResponse,
	encoder: TextEncoder,
): Uint8Array {
	switch (response.kind) {
		case SYNC_BRIDGE_KIND_NONE:
			return new Uint8Array(0);
		case SYNC_BRIDGE_KIND_TEXT:
			return encoder.encode(response.value);
		case SYNC_BRIDGE_KIND_BINARY:
			return response.value;
		case SYNC_BRIDGE_KIND_JSON:
			return encoder.encode(JSON.stringify(response.value));
		default:
			return new Uint8Array(0);
	}
}

async function handleSyncBridgeOperation(
	filesystem: VirtualFileSystem,
	message: BrowserWorkerSyncRequestMessage,
): Promise<SyncBridgeResponse> {
	switch (message.operation) {
		case "fs.readFile":
			return {
				kind: SYNC_BRIDGE_KIND_TEXT,
				value: await filesystem.readTextFile(String(message.args[0])),
			};
		case "fs.writeFile":
			await filesystem.writeFile(
				String(message.args[0]),
				String(message.args[1] ?? ""),
			);
			return { kind: SYNC_BRIDGE_KIND_NONE };
		case "fs.readFileBinary":
			return {
				kind: SYNC_BRIDGE_KIND_BINARY,
				value: await filesystem.readFile(String(message.args[0])),
			};
		case "fs.writeFileBinary":
			await filesystem.writeFile(
				String(message.args[0]),
				toUint8Array(message.args[1]),
			);
			return { kind: SYNC_BRIDGE_KIND_NONE };
		case "fs.readDir":
			return {
				kind: SYNC_BRIDGE_KIND_JSON,
				value: await filesystem.readDirWithTypes(String(message.args[0])),
			};
		case "fs.createDir":
			await filesystem.createDir(String(message.args[0]));
			return { kind: SYNC_BRIDGE_KIND_NONE };
		case "fs.mkdir":
			await mkdir(filesystem, String(message.args[0]));
			return { kind: SYNC_BRIDGE_KIND_NONE };
		case "fs.rmdir":
			await filesystem.removeDir(String(message.args[0]));
			return { kind: SYNC_BRIDGE_KIND_NONE };
		case "fs.exists":
			return {
				kind: SYNC_BRIDGE_KIND_JSON,
				value: await filesystem.exists(String(message.args[0])),
			};
		case "fs.stat":
			return {
				kind: SYNC_BRIDGE_KIND_JSON,
				value: await filesystem.stat(String(message.args[0])),
			};
		case "fs.lstat":
			return {
				kind: SYNC_BRIDGE_KIND_JSON,
				value: await filesystem.lstat(String(message.args[0])),
			};
		case "fs.unlink":
			await filesystem.removeFile(String(message.args[0]));
			return { kind: SYNC_BRIDGE_KIND_NONE };
		case "fs.rename":
			await filesystem.rename(
				String(message.args[0]),
				String(message.args[1]),
			);
			return { kind: SYNC_BRIDGE_KIND_NONE };
		case "fs.realpath":
			return {
				kind: SYNC_BRIDGE_KIND_TEXT,
				value: await filesystem.realpath(String(message.args[0])),
			};
		case "fs.readlink":
			return {
				kind: SYNC_BRIDGE_KIND_TEXT,
				value: await filesystem.readlink(String(message.args[0])),
			};
		case "fs.symlink":
			await filesystem.symlink(
				String(message.args[0]),
				String(message.args[1]),
			);
			return { kind: SYNC_BRIDGE_KIND_NONE };
		case "fs.link":
			await filesystem.link(
				String(message.args[0]),
				String(message.args[1]),
			);
			return { kind: SYNC_BRIDGE_KIND_NONE };
		case "fs.chmod":
			await filesystem.chmod(
				String(message.args[0]),
				Number(message.args[1]),
			);
			return { kind: SYNC_BRIDGE_KIND_NONE };
		case "fs.truncate":
			await filesystem.truncate(
				String(message.args[0]),
				Number(message.args[1]),
			);
			return { kind: SYNC_BRIDGE_KIND_NONE };
		case "module.resolve": {
			const resolved = await resolveModule(
				String(message.args[0]),
				String(message.args[1]),
				filesystem,
			);
			return resolved === null
				? { kind: SYNC_BRIDGE_KIND_NONE }
				: { kind: SYNC_BRIDGE_KIND_TEXT, value: resolved };
		}
		case "module.loadFile": {
			const source = await loadFile(String(message.args[0]), filesystem);
			return source === null
				? { kind: SYNC_BRIDGE_KIND_NONE }
				: { kind: SYNC_BRIDGE_KIND_TEXT, value: source };
		}
		default:
			throw new Error(
				`Unsupported browser sync bridge operation: ${String(message.operation)}`,
			);
	}
}

export class BrowserRuntimeDriver implements NodeRuntimeDriver {
	private readonly worker: Worker;
	private readonly pending = new Map<number, PendingRequest>();
	private readonly controlToken = createWorkerControlToken();
	private readonly defaultOnStdio?: StdioHook;
	private readonly defaultTimingMitigation: TimingMitigation;
	private readonly networkAdapter: NetworkAdapter;
	private readonly syncBridge: BrowserSyncBridgePayload;
	private readonly syncFilesystem: VirtualFileSystem;
	private readonly ready: Promise<void>;
	private readonly encoder = new TextEncoder();
	private nextId = 1;
	private disposed = false;

	constructor(
		private readonly options: RuntimeDriverOptions,
		factoryOptions: BrowserRuntimeDriverFactoryOptions = {},
	) {
		if (typeof Worker === "undefined") {
			throw new Error("Browser runtime requires a global Worker implementation");
		}
		assertBrowserSyncBridgeSupport();

		this.defaultOnStdio = options.onStdio;
		this.defaultTimingMitigation =
			options.timingMitigation ??
			options.runtime.process.timingMitigation ??
			DEFAULT_BROWSER_TIMING_MITIGATION;
		this.networkAdapter = options.system.network ?? createNetworkStub();
		this.syncBridge = createBrowserSyncBridgePayload(options.payloadLimits);
		this.syncFilesystem = createSyncBridgeFilesystem(options);
		this.worker = new Worker(resolveWorkerUrl(factoryOptions.workerUrl), {
			type: "module",
		});
		this.worker.onmessage = this.handleWorkerMessage;
		this.worker.onerror = this.handleWorkerError;

		const browserSystemOptions = getBrowserSystemDriverOptions(options.system);
		const initPayload: BrowserWorkerInitPayload = {
			processConfig: options.runtime.process,
			osConfig: options.runtime.os,
			permissions: serializePermissions(options.system.permissions),
			filesystem: browserSystemOptions.filesystem,
			networkEnabled: browserSystemOptions.networkEnabled,
			timingMitigation: this.defaultTimingMitigation,
			payloadLimits: options.payloadLimits,
			syncBridge: this.syncBridge,
		};

		this.ready = this.callWorker("init", initPayload).then(() => undefined);
		this.ready.catch(() => undefined);
	}

	get network(): Pick<NetworkAdapter, "fetch" | "dnsLookup" | "httpRequest"> {
		const adapter = this.networkAdapter;
		return {
			fetch: (url, options) => adapter.fetch(url, options),
			dnsLookup: (hostname) => adapter.dnsLookup(hostname),
			httpRequest: (url, options) => adapter.httpRequest(url, options),
		};
	}

	private handleWorkerError = (event: ErrorEvent): void => {
		if (this.disposed) {
			return;
		}
		const error =
			event.error instanceof Error
				? event.error
				: new Error(
						event.message
							? `Browser runtime worker error: ${event.message} (${event.filename}:${event.lineno}:${event.colno})`
							: "Browser runtime worker error",
					);
		this.cleanup(error, { terminateWorker: true });
	};

	private handleWorkerMessage = (
		event: MessageEvent<BrowserWorkerOutboundMessage>,
	): void => {
		if (this.disposed) {
			return;
		}
		const message = event.data;
		if (message.controlToken !== this.controlToken) {
			return;
		}

		if (isSyncRequestMessage(message)) {
			void this.handleSyncRequest(message);
			return;
		}

		if (isStdioMessage(message)) {
			const pending = this.pending.get(message.requestId);
			const hook = pending?.hook ?? this.defaultOnStdio;
			if (!hook) {
				return;
			}
			try {
				hook({ channel: message.channel, message: message.message });
			} catch {
				// Ignore host hook errors so sandbox execution can continue.
			}
			return;
		}

		if (!isResponseMessage(message)) {
			return;
		}

		const pending = this.pending.get(message.id);
		if (!pending) {
			return;
		}
		this.pending.delete(message.id);

		if (message.ok) {
			pending.resolve(message.result);
			return;
		}

		const error = new Error(message.error.message);
		if (message.error.stack) {
			error.stack = message.error.stack;
		}
		(error as { code?: string }).code = message.error.code;
		pending.reject(error);
	};

	private async handleSyncRequest(
		message: BrowserWorkerSyncRequestMessage,
	): Promise<void> {
		const signal = new Int32Array(this.syncBridge.signalBuffer);
		const data = new Uint8Array(this.syncBridge.dataBuffer);
		try {
			const response = await handleSyncBridgeOperation(
				this.syncFilesystem,
				message,
			);
			const bytes = createSyncBridgeResponseBytes(response, this.encoder);
			if (bytes.byteLength > data.byteLength) {
				const suffix =
					typeof message.args[0] === "string" ? ` ${message.args[0]}` : "";
				throwBridgePayloadTooLarge(
					`${message.operation}${suffix}`,
					bytes.byteLength,
					data.byteLength,
				);
			}

			data.set(bytes, 0);
			Atomics.store(signal, SYNC_BRIDGE_SIGNAL_STATUS_INDEX, SYNC_BRIDGE_STATUS_OK);
			Atomics.store(signal, SYNC_BRIDGE_SIGNAL_KIND_INDEX, response.kind);
			Atomics.store(signal, SYNC_BRIDGE_SIGNAL_LENGTH_INDEX, bytes.byteLength);
		} catch (error) {
			let bytes = this.encoder.encode(
				JSON.stringify(toBrowserSyncBridgeError(error)),
			);
			if (bytes.byteLength > data.byteLength) {
				bytes = this.encoder.encode(
					JSON.stringify({
						message:
							"Browser runtime sync bridge error exceeded shared buffer capacity",
						code: SYNC_BRIDGE_PAYLOAD_LIMIT_ERROR_CODE,
					}),
				);
			}

			data.set(bytes, 0);
			Atomics.store(signal, SYNC_BRIDGE_SIGNAL_STATUS_INDEX, SYNC_BRIDGE_STATUS_ERROR);
			Atomics.store(signal, SYNC_BRIDGE_SIGNAL_KIND_INDEX, SYNC_BRIDGE_KIND_JSON);
			Atomics.store(signal, SYNC_BRIDGE_SIGNAL_LENGTH_INDEX, bytes.byteLength);
		}

		Atomics.store(signal, SYNC_BRIDGE_SIGNAL_STATE_INDEX, SYNC_BRIDGE_SIGNAL_STATE_READY);
		Atomics.notify(signal, SYNC_BRIDGE_SIGNAL_STATE_INDEX, 1);
	}

	private rejectAllPending(error: Error): void {
		const entries = Array.from(this.pending.values());
		this.pending.clear();
		for (const pending of entries) {
			pending.reject(error);
		}
	}

	private clearWorkerHandlers(): void {
		try {
			this.worker.onmessage = null;
		} catch {
			// Ignore host Worker implementations with non-writable event hooks.
		}
		try {
			this.worker.onerror = null;
		} catch {
			// Ignore host Worker implementations with non-writable event hooks.
		}
	}

	private resetSyncBridgeState(): void {
		new Int32Array(this.syncBridge.signalBuffer).fill(0);
		new Uint8Array(this.syncBridge.dataBuffer).fill(0);
	}

	private cleanup(
		error: Error,
		options: { terminateWorker?: boolean } = {},
	): void {
		if (this.disposed) {
			this.rejectAllPending(error);
			return;
		}
		this.disposed = true;
		this.clearWorkerHandlers();
		if (options.terminateWorker) {
			try {
				this.worker.terminate();
			} catch {
				// Ignore termination errors while tearing down a broken worker.
			}
		}
		this.resetSyncBridgeState();
		this.rejectAllPending(error);
	}

	private callWorker<T>(
		type: BrowserWorkerRequestMessage["type"],
		payload?: unknown,
		hook?: StdioHook,
	): Promise<T> {
		if (this.disposed) {
			return Promise.reject(new Error("Browser runtime has been disposed"));
		}
		const id = this.nextId++;
		const message: BrowserWorkerRequestMessage =
			payload === undefined
				? ({
						controlToken: this.controlToken,
						id,
						type,
					} as BrowserWorkerRequestMessage)
				: ({
						controlToken: this.controlToken,
						id,
						type,
						payload,
					} as BrowserWorkerRequestMessage);

		return new Promise<T>((resolve, reject) => {
			this.pending.set(id, { resolve, reject, hook });
			try {
				this.worker.postMessage(message);
			} catch (error) {
				this.pending.delete(id);
				reject(error);
			}
		});
	}

	async run<T = unknown>(code: string, filePath?: string): Promise<RunResult<T>> {
		await this.ready;
		const hook = this.defaultOnStdio;
		return this.callWorker<RunResult<T>>(
			"run",
			{
				code,
				filePath,
				captureStdio: Boolean(hook),
			},
			hook,
		);
	}

	async exec(code: string, options?: ExecOptions): Promise<ExecResult> {
		validateBrowserExecOptions(options);
		await this.ready;
		const hook = options?.onStdio ?? this.defaultOnStdio;
		return this.callWorker<ExecResult>(
			"exec",
			{
				code,
				options: toBrowserWorkerExecOptions(options),
				captureStdio: Boolean(hook),
			},
			hook,
		);
	}

	dispose(): void {
		if (this.disposed) {
			return;
		}
		this.cleanup(new Error("Browser runtime has been disposed"), {
			terminateWorker: true,
		});
	}

	async terminate(): Promise<void> {
		this.dispose();
	}
}

export function createBrowserRuntimeDriverFactory(
	factoryOptions: BrowserRuntimeDriverFactoryOptions = {},
): NodeRuntimeDriverFactory {
	return {
		createRuntimeDriver(options) {
			validateBrowserRuntimeOptions(options);
			return new BrowserRuntimeDriver(options, factoryOptions);
		},
	};
}
