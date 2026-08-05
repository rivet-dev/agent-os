import type { ProtocolFramePayloadCodec } from "@rivet-dev/agentos-runtime-core/protocol-frames";
import type { CreateVmConfig } from "@rivet-dev/agentos-runtime-core/vm-config";
import {
	type BrowserChildProcessPollEvent,
	decodeChildProcessInput,
	encodeChildProcessBytes,
	parseChildProcessSpawnRequest,
} from "./child-process-bridge.js";
import type { ConvergedServicer } from "./converged-driver-setup.js";
import { getBrowserSystemDriverOptions } from "./driver.js";
import { base64ToBytes, toUint8Array } from "./encoding.js";
import type {
	CommandExecutor,
	ExecOptions,
	ExecResult,
	NetworkAdapter,
	NodeRuntimeDriver,
	NodeRuntimeDriverFactory,
	Permissions,
	PtyOpenResult,
	RunResult,
	RuntimeDriverOptions,
	StdioHook,
	TimingMitigation,
} from "./runtime.js";
import {
	createNetworkStub,
	filterEnv,
	wrapCommandExecutor,
} from "./runtime.js";
import {
	applyProcessSignalStateUpdate,
	type BrowserSignalRegistration,
	parseProcessSignalStateArgs,
} from "./signals.js";
import {
	assertBrowserSyncBridgeSupport,
	type BrowserSyncBridgePayload,
	type BrowserWorkerSyncRequestMessage,
	createBrowserSyncBridgePayload,
	isBrowserWorkerSyncOperation,
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
	toBrowserSyncBridgeError,
} from "./sync-bridge.js";
import type {
	BrowserWorkerExecOptions,
	BrowserWorkerExtensionResponse,
	BrowserWorkerInitPayload,
	BrowserWorkerOutboundMessage,
	BrowserWorkerRequestMessage,
	BrowserWorkerResponseMessage,
	BrowserWorkerPtyOpenedMessage,
	BrowserWorkerStdioMessage,
} from "./worker-protocol.js";

export interface BrowserRuntimeDriverFactoryOptions {
	workerUrl?: URL | string;
	// When provided, guest sync-bridge syscalls are serviced by the converged
	// wasm kernel (fs/net/dns/module) instead of the legacy in-process TS kernel,
	// with the remaining families falling back to legacy. The setup module is
	// imported dynamically so the unbundled legacy load never pulls in its
	// bundled-only `@rivet-dev/agentos-runtime-core` imports.
	convergedSidecar?: ConvergedSidecarFactoryOptions;
}

export interface ConvergedSidecarHandle {
	pushFrame(frame: Uint8Array): Uint8Array;
	// Sets the execution id the sidecar's execution host bridge echoes for the
	// next `execute`; omitted when the sidecar has no execution host bridge
	// (then guest net/dgram are unavailable but fs/module still converge).
	setNextExecutionId?(executionId: string): void;
}

export interface ConvergedSidecarFactoryOptions {
	loadSidecar(): Promise<ConvergedSidecarHandle>;
	config: CreateVmConfig;
	codec?: ProtocolFramePayloadCodec;
	onFsReadDenied?: () => void;
}

type PendingRequest = {
	resolve(value: unknown): void;
	reject(reason: unknown): void;
	hook?: StdioHook;
	onPtyOpen?: (pty: PtyOpenResult) => void;
	executionId?: string;
};

type SyncBridgeResponse =
	| { kind: typeof SYNC_BRIDGE_KIND_NONE }
	| { kind: typeof SYNC_BRIDGE_KIND_TEXT; value: string }
	| { kind: typeof SYNC_BRIDGE_KIND_BINARY; value: Uint8Array }
	| { kind: typeof SYNC_BRIDGE_KIND_JSON; value: unknown };

type BrowserChildProcessSession = {
	executionId: string;
	process: ReturnType<CommandExecutor["spawn"]>;
	events: BrowserChildProcessPollEvent[];
	exited: boolean;
};

function normalizeCommandWaitResult(
	result: Awaited<ReturnType<BrowserChildProcessSession["process"]["wait"]>>,
): Extract<BrowserChildProcessPollEvent, { type: "exit" }> {
	if (typeof result === "number") {
		return { type: "exit", exitCode: result, signal: null };
	}
	return {
		type: "exit",
		exitCode: result.exitCode,
		signal: result.signal,
	};
}

type BrowserNetworkPermission = NonNullable<
	RuntimeDriverOptions["system"]["permissions"]
>["network"];

type BrowserSyncBridgeHost = {
	commandExecutor: CommandExecutor;
	networkAdapter: NetworkAdapter;
	childProcessSessions: Map<number, BrowserChildProcessSession>;
	signalStates: Map<string, Map<number, BrowserSignalRegistration>>;
	allocateChildProcessSessionId(): number;
	networkPermission?: BrowserNetworkPermission;
};

const DEFAULT_BROWSER_TIMING_MITIGATION: TimingMitigation = "freeze";

const BROWSER_OPTION_VALIDATORS = [
	{
		label: "memoryLimit",
		hasValue: (options: RuntimeDriverOptions) =>
			options.memoryLimit !== undefined,
	},
	{
		label: "cpuTimeLimitMs",
		hasValue: (options: RuntimeDriverOptions) =>
			options.cpuTimeLimitMs !== undefined,
	},
];

// Permission predicates are consumed only on the trusted main thread (here and
// in `wrapCommandExecutor`); they are never serialized to or evaluated in the
// untrusted guest worker. The kernel is the sole enforcement point for fs/net.
// The one thing the worker needs is an already-filtered env, so apply the env
// predicate to a process config on this side before it crosses the boundary.
function filterProcessConfigEnv<T extends { env?: Record<string, string> }>(
	processConfig: T,
	permissions?: Permissions,
): T {
	if (!permissions?.env || !processConfig.env) {
		return processConfig;
	}
	return { ...processConfig, env: filterEnv(processConfig.env, permissions) };
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
	permissions?: Permissions,
): BrowserWorkerExecOptions | undefined {
	if (!options) {
		return undefined;
	}
	return {
		filePath: options.filePath,
		// Filter per-exec env on the trusted main thread before it reaches the
		// guest worker (the worker no longer re-applies the env permission).
		env:
			options.env && permissions?.env
				? filterEnv(options.env, permissions)
				: options.env,
		cwd: options.cwd,
		stdin: options.stdin,
		stdioPty: options.stdioPty
			? {
					open: options.stdioPty.open,
					slaveFd: options.stdioPty.slaveFd,
					columns: options.stdioPty.columns,
					rows: options.stdioPty.rows,
				}
			: undefined,
		timingMitigation: options.timingMitigation,
		persistent: options.persistent,
		streamingStdin: options.streamingStdin,
	};
}

function validateBrowserRuntimeOptions(options: RuntimeDriverOptions): void {
	const unsupported = BROWSER_OPTION_VALIDATORS.filter((validator) =>
		validator.hasValue(options),
	).map((validator) => validator.label);
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

function isPtyOpenedMessage(
	message: BrowserWorkerOutboundMessage,
): message is BrowserWorkerPtyOpenedMessage {
	return message.type === "pty-opened";
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

async function waitForChildProcessEvent(
	session: BrowserChildProcessSession,
	waitMs: number,
): Promise<BrowserChildProcessPollEvent | null> {
	const deadline = Date.now() + Math.max(0, waitMs);
	while (
		session.events.length === 0 &&
		!session.exited &&
		Date.now() < deadline
	) {
		await new Promise((resolve) => setTimeout(resolve, 5));
	}
	return session.events.shift() ?? null;
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
	host: BrowserSyncBridgeHost,
	message: BrowserWorkerSyncRequestMessage,
): Promise<SyncBridgeResponse> {
	switch (message.operation) {
		case "child_process.spawn": {
			const request = parseChildProcessSpawnRequest(
				message.args[0],
				"child_process.spawn request",
			);
			const { command, args } = request;
			const options = request.options ?? {};
			const sessionId = host.allocateChildProcessSessionId();
			const events: BrowserChildProcessPollEvent[] = [];
			const process = host.commandExecutor.spawn(command, args, {
				argv0: options.argv0,
				cwd: options.cwd,
				env: options.env,
				pty:
					options.pty &&
					Number.isInteger(options.pty.cols) &&
					Number.isInteger(options.pty.rows)
						? { cols: options.pty.cols!, rows: options.pty.rows! }
						: undefined,
				onStdout: (data) => {
					events.push({
						type: "stdout",
						data: encodeChildProcessBytes(data),
					});
				},
				onStderr: (data) => {
					events.push({
						type: "stderr",
						data: encodeChildProcessBytes(data),
					});
				},
			});
			const session: BrowserChildProcessSession = {
				executionId: message.executionId,
				process,
				events,
				exited: false,
			};
			void process
				.wait()
				.then((result) => {
					session.exited = true;
					session.events.push(normalizeCommandWaitResult(result));
				})
				.catch((error) => {
					session.exited = true;
					const message =
						error instanceof Error ? error.message : String(error);
					session.events.push({
						type: "stderr",
						data: encodeChildProcessBytes(new TextEncoder().encode(message)),
					});
					session.events.push({ type: "exit", exitCode: 1, signal: null });
				});
			host.childProcessSessions.set(sessionId, session);
			return { kind: SYNC_BRIDGE_KIND_JSON, value: sessionId };
		}
		case "child_process.poll": {
			const sessionId = Number(message.args[0]);
			const waitMs = Number(message.args[1] ?? 0);
			const session = host.childProcessSessions.get(sessionId);
			if (!session || session.executionId !== message.executionId) {
				return { kind: SYNC_BRIDGE_KIND_JSON, value: null };
			}
			const event = await waitForChildProcessEvent(session, waitMs);
			if (event?.type === "exit" && session.events.length === 0) {
				host.childProcessSessions.delete(sessionId);
			}
			return { kind: SYNC_BRIDGE_KIND_JSON, value: event };
		}
		case "child_process.write_stdin": {
			const sessionId = Number(message.args[0]);
			const session = host.childProcessSessions.get(sessionId);
			if (!session || session.executionId !== message.executionId) {
				throw new Error(`unknown child_process session ${sessionId}`);
			}
			session?.process.writeStdin(toUint8Array(message.args[1]));
			return { kind: SYNC_BRIDGE_KIND_NONE };
		}
		case "child_process.close_stdin": {
			const sessionId = Number(message.args[0]);
			const session = host.childProcessSessions.get(sessionId);
			if (!session || session.executionId !== message.executionId) {
				throw new Error(`unknown child_process session ${sessionId}`);
			}
			session.process.closeStdin();
			return { kind: SYNC_BRIDGE_KIND_NONE };
		}
		case "child_process.kill": {
			const sessionId = Number(message.args[0]);
			const signal = Number(message.args[1]);
			const session = host.childProcessSessions.get(sessionId);
			if (!session || session.executionId !== message.executionId) {
				throw new Error(`unknown child_process session ${sessionId}`);
			}
			const accepted = session.process.kill(signal);
			return {
				kind: SYNC_BRIDGE_KIND_JSON,
				value: accepted !== false,
			};
		}
		case "child_process.resize_pty": {
			const sessionId = Number(message.args[0]);
			const cols = Number(message.args[1]);
			const rows = Number(message.args[2]);
			const session = host.childProcessSessions.get(sessionId);
			if (!session || session.executionId !== message.executionId) {
				throw new Error(`unknown child_process session ${sessionId}`);
			}
			if (
				!Number.isInteger(cols) ||
				cols <= 0 ||
				!Number.isInteger(rows) ||
				rows <= 0
			) {
				throw new Error("EINVAL: PTY dimensions must be greater than zero");
			}
			if (typeof session.process.resizePty !== "function") {
				throw new Error("ENOTTY: child process was not spawned with a PTY");
			}
			session.process.resizePty(cols, rows);
			return { kind: SYNC_BRIDGE_KIND_NONE };
		}
		case "child_process.spawn_sync": {
			const request = parseChildProcessSpawnRequest(
				message.args[0],
				"child_process.spawn_sync request",
			);
			const { command, args } = request;
			const options = request.options ?? {};
			const stdoutChunks: Uint8Array[] = [];
			const stderrChunks: Uint8Array[] = [];
			const proc = host.commandExecutor.spawn(command, args, {
				argv0: options.argv0,
				cwd: options.cwd,
				env: options.env,
				onStdout: (data) => stdoutChunks.push(data),
				onStderr: (data) => stderrChunks.push(data),
			});
			const input = decodeChildProcessInput(options.input);
			if (input !== undefined) {
				proc.writeStdin(input);
			}
			proc.closeStdin();
			const waitResult = normalizeCommandWaitResult(await proc.wait());
			const decoder = new TextDecoder();
			return {
				kind: SYNC_BRIDGE_KIND_TEXT,
				value: JSON.stringify({
					stdout: stdoutChunks.map((chunk) => decoder.decode(chunk)).join(""),
					stderr: stderrChunks.map((chunk) => decoder.decode(chunk)).join(""),
					code: waitResult.exitCode,
					signal: waitResult.signal,
				}),
			};
		}
		case "process.signal_state": {
			const { signal, registration } = parseProcessSignalStateArgs(
				message.args,
			);
			applyProcessSignalStateUpdate(
				host.signalStates,
				message.executionId,
				signal,
				registration,
			);
			return { kind: SYNC_BRIDGE_KIND_NONE };
		}
		case "network.fetch": {
			const url = String(message.args[0] ?? "");
			const options = (message.args[1] ?? {}) as Parameters<
				NetworkAdapter["fetch"]
			>[1];
			const result = await host.networkAdapter.fetch(url, options);
			return { kind: SYNC_BRIDGE_KIND_JSON, value: result };
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
	private readonly commandExecutor: CommandExecutor;
	private readonly syncBridge: BrowserSyncBridgePayload;
	private readonly childProcessSessions = new Map<
		number,
		BrowserChildProcessSession
	>();
	private readonly signalStates = new Map<
		string,
		Map<number, BrowserSignalRegistration>
	>();
	private readonly ready: Promise<void>;
	private readonly encoder = new TextEncoder();
	private nextId = 1;
	private nextExecutionId = 1;
	private nextChildProcessSessionId = 1;
	private disposed = false;
	private readonly networkPermission?: BrowserNetworkPermission;
	private readonly permissions?: Permissions;
	private convergedServicer?: ConvergedServicer;
	private readonly convergedReady?: Promise<void>;

	constructor(
		options: RuntimeDriverOptions,
		factoryOptions: BrowserRuntimeDriverFactoryOptions = {},
	) {
		if (typeof Worker === "undefined") {
			throw new Error(
				"Browser runtime requires a global Worker implementation",
			);
		}
		assertBrowserSyncBridgeSupport();

		this.defaultOnStdio = options.onStdio;
		this.defaultTimingMitigation =
			options.timingMitigation ??
			options.runtime.process.timingMitigation ??
			DEFAULT_BROWSER_TIMING_MITIGATION;
		this.networkAdapter = options.system.network ?? createNetworkStub();
		this.networkPermission = options.system.permissions?.network;
		this.permissions = options.system.permissions;
		this.commandExecutor = wrapCommandExecutor(
			options.system.commandExecutor ?? {
				spawn() {
					throw new Error("ENOSYS: child_process.spawn is not supported");
				},
			},
			options.system.permissions,
		);
		this.syncBridge = createBrowserSyncBridgePayload(options.payloadLimits);
		this.worker = new Worker(resolveWorkerUrl(factoryOptions.workerUrl), {
			type: "module",
		});
		this.worker.onmessage = this.handleWorkerMessage;
		this.worker.onerror = this.handleWorkerError;

		const browserSystemOptions = getBrowserSystemDriverOptions(options.system);
		const initPayload: BrowserWorkerInitPayload = {
			processConfig: filterProcessConfigEnv(
				options.runtime.process,
				options.system.permissions,
			),
			// The converged sidecar VM config is trusted host input and the source of
			// truth for policy. Never derive these caps from guest bootstrap options.
			processLimits: factoryOptions.convergedSidecar?.config.limits?.process,
			osConfig: options.runtime.os,
			filesystem: browserSystemOptions.filesystem,
			networkEnabled: browserSystemOptions.networkEnabled,
			timingMitigation: this.defaultTimingMitigation,
			payloadLimits: options.payloadLimits,
			syncBridge: this.syncBridge,
		};

		this.ready = this.callWorker("init", initPayload).then(() => undefined);
		this.ready.catch(() => undefined);

		if (factoryOptions.convergedSidecar) {
			this.convergedReady = this.setupConvergedSidecar(
				factoryOptions.convergedSidecar,
			);
			this.convergedReady.catch(() => undefined);
		}
	}

	private async setupConvergedSidecar(
		options: ConvergedSidecarFactoryOptions,
	): Promise<void> {
		const [{ createConvergedServicer }, sidecar] = await Promise.all([
			import("./converged-driver-setup.js"),
			options.loadSidecar(),
		]);
		this.convergedServicer = createConvergedServicer({
			pushFrame: sidecar.pushFrame,
			config: options.config,
			codec: options.codec,
			setNextExecutionId: sidecar.setNextExecutionId?.bind(sidecar),
			onFsReadDenied: options.onFsReadDenied,
		});
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

		if (isPtyOpenedMessage(message)) {
			const pending = this.pending.get(message.requestId);
			if (pending?.executionId !== message.executionId) {
				return;
			}
			try {
				pending.onPtyOpen?.({
					masterFd: message.masterFd,
					slaveFd: message.slaveFd,
					path: message.path,
					columns: message.columns,
					rows: message.rows,
				});
			} catch {
				// Ignore host callback errors so the guest execution can continue.
			}
			return;
		}

		if (isStdioMessage(message)) {
			const pending = this.pending.get(message.requestId);
			if (pending?.executionId !== message.executionId) {
				return;
			}
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
		if (pending.executionId) {
			this.cleanupExecutionState(pending.executionId);
		}

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
			if (
				!this.hasPendingExecutionRequest(
					message.processRequestId,
					message.executionId,
				)
			) {
				throw new Error(
					`Browser runtime sync bridge request for unknown execution ${message.executionId}`,
				);
			}
			if (!isBrowserWorkerSyncOperation(message.operation)) {
				throw new Error(
					`Unsupported browser sync bridge operation: ${String(message.operation)}`,
				);
			}
			const legacyServicer = (operation: string, args: readonly unknown[]) =>
				handleSyncBridgeOperation(
					{
						commandExecutor: this.commandExecutor,
						networkAdapter: this.networkAdapter,
						childProcessSessions: this.childProcessSessions,
						signalStates: this.signalStates,
						allocateChildProcessSessionId: () =>
							this.allocateChildProcessSessionId(),
						networkPermission: this.networkPermission,
					},
					{
						...message,
						operation: operation as typeof message.operation,
						args: [...args],
					},
				);
			// Converged-only: every guest syscall is serviced by the wasm kernel via
			// the converged servicer. The legacy in-process TS kernel is gone; the
			// `legacyServicer` survives ONLY as the converged router's fallback for
			// host capabilities (child_process.* / process.signal_state), never as a
			// standalone guest-syscall path.
			if (this.convergedReady === undefined) {
				throw new Error(
					"Browser runtime requires a converged wasm sidecar; the legacy in-process kernel has been removed",
				);
			}
			await this.convergedReady;
			if (!this.convergedServicer) {
				throw new Error(
					"Converged sidecar servicer is unavailable after setup",
				);
			}
			const response = await this.convergedServicer.route(
				message.executionId,
				message.operation,
				message.args,
				legacyServicer,
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
			Atomics.store(
				signal,
				SYNC_BRIDGE_SIGNAL_STATUS_INDEX,
				SYNC_BRIDGE_STATUS_OK,
			);
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
			Atomics.store(
				signal,
				SYNC_BRIDGE_SIGNAL_STATUS_INDEX,
				SYNC_BRIDGE_STATUS_ERROR,
			);
			Atomics.store(
				signal,
				SYNC_BRIDGE_SIGNAL_KIND_INDEX,
				SYNC_BRIDGE_KIND_JSON,
			);
			Atomics.store(signal, SYNC_BRIDGE_SIGNAL_LENGTH_INDEX, bytes.byteLength);
		}

		Atomics.store(
			signal,
			SYNC_BRIDGE_SIGNAL_STATE_INDEX,
			SYNC_BRIDGE_SIGNAL_STATE_READY,
		);
		Atomics.notify(signal, SYNC_BRIDGE_SIGNAL_STATE_INDEX, 1);
	}

	private rejectAllPending(error: Error): void {
		const entries = Array.from(this.pending.values());
		this.pending.clear();
		for (const pending of entries) {
			if (pending.executionId) {
				this.cleanupExecutionState(pending.executionId);
			}
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

	private allocateExecutionId(): string {
		return `exec-${this.nextExecutionId++}`;
	}

	private allocateChildProcessSessionId(): number {
		return this.nextChildProcessSessionId++;
	}

	private hasPendingExecutionRequest(
		requestId: number,
		executionId: string,
	): boolean {
		const pending = this.pending.get(requestId);
		return pending?.executionId === executionId;
	}

	private cleanupExecutionState(executionId: string): void {
		this.signalStates.delete(executionId);
		for (const [sessionId, session] of this.childProcessSessions) {
			if (session.executionId === executionId) {
				this.childProcessSessions.delete(sessionId);
			}
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
		this.signalStates.clear();
		this.rejectAllPending(error);
	}

	// Fire-and-forget control message (no response expected) — for streaming stdin.
	private postControl(
		fields:
			| { type: "write-stdin"; executionId: string; data: string }
			| { type: "end-stdin"; executionId: string }
			| {
					type: "resize-pty";
					executionId: string;
					columns: number;
					rows: number;
			  },
	): void {
		const id = this.nextId++;
		try {
			this.worker.postMessage({
				controlToken: this.controlToken,
				id,
				...fields,
			} as BrowserWorkerRequestMessage);
		} catch {
			// Worker gone / disposed — nothing to do.
		}
	}

	private callWorker<T>(
		type: BrowserWorkerRequestMessage["type"],
		payload?: unknown,
		hook?: StdioHook,
		executionId?: string,
		onPtyOpen?: (pty: PtyOpenResult) => void,
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
			this.pending.set(id, {
				resolve,
				reject,
				hook,
				executionId,
				onPtyOpen,
			});
			try {
				this.worker.postMessage(message);
			} catch (error) {
				this.pending.delete(id);
				if (executionId) {
					this.cleanupExecutionState(executionId);
				}
				reject(error);
			}
		});
	}

	async run<T = unknown>(
		code: string,
		filePath?: string,
	): Promise<RunResult<T>> {
		await this.ready;
		const hook = this.defaultOnStdio;
		const executionId = this.allocateExecutionId();
		return this.callWorker<RunResult<T>>(
			"run",
			{
				executionId,
				code,
				filePath,
				captureStdio: Boolean(hook),
			},
			hook,
			executionId,
		);
	}

	async exec(code: string, options?: ExecOptions): Promise<ExecResult> {
		validateBrowserExecOptions(options);
		await this.ready;
		const hook = options?.onStdio ?? this.defaultOnStdio;
		const executionId = this.allocateExecutionId();
		// Hand the execution id to the caller BEFORE awaiting completion so it can drive
		// streaming stdin (writeStdin/endStdin) while the program runs.
		options?.onStart?.(executionId);
		return this.callWorker<ExecResult>(
			"exec",
			{
				executionId,
				code,
				options: toBrowserWorkerExecOptions(options, this.permissions),
				captureStdio: Boolean(hook),
			},
			hook,
			executionId,
			options?.stdioPty?.onOpen,
		);
	}

	/** Feed stdin to a running `streamingStdin` execution. */
	writeStdin(executionId: string, data: string): void {
		if (this.disposed) return;
		this.postControl({ type: "write-stdin", executionId, data });
	}

	/** End stdin for a running `streamingStdin` execution (the program sees EOF). */
	endStdin(executionId: string): void {
		if (this.disposed) return;
		this.postControl({ type: "end-stdin", executionId });
	}

	private async routePty(
		executionId: string,
		operation: "pty.read" | "pty.write" | "pty.resize" | "pty.close",
		args: readonly unknown[],
	): Promise<SyncBridgeResponse> {
		await this.ready;
		if (this.convergedReady === undefined) {
			throw new Error("PTY operations require a converged wasm sidecar");
		}
		await this.convergedReady;
		if (!this.convergedServicer) {
			throw new Error("Converged sidecar servicer is unavailable after setup");
		}
		return this.convergedServicer.route(
			executionId,
			operation,
			args,
			async () => {
				throw new Error(
					`legacy PTY fallback is not available for ${operation}`,
				);
			},
		);
	}

	async writePty(
		executionId: string,
		fd: number,
		data: string | Uint8Array,
	): Promise<number> {
		const response = await this.routePty(executionId, "pty.write", [
			{ fd, data: toUint8Array(data) },
		]);
		if (response.kind !== SYNC_BRIDGE_KIND_JSON) {
			throw new Error(
				`Expected JSON response from pty.write, received ${response.kind}`,
			);
		}
		return Number((response.value as { written?: unknown }).written ?? 0);
	}

	async readPty(
		executionId: string,
		fd: number,
		options: { maxBytes?: number; timeoutMs?: number } = {},
	): Promise<Uint8Array | null> {
		const response = await this.routePty(executionId, "pty.read", [
			{
				fd,
				maxBytes: options.maxBytes,
				timeoutMs: options.timeoutMs,
			},
		]);
		if (response.kind !== SYNC_BRIDGE_KIND_JSON) {
			throw new Error(
				`Expected JSON response from pty.read, received ${response.kind}`,
			);
		}
		const data = (response.value as { data?: unknown }).data;
		return typeof data === "string" ? base64ToBytes(data) : null;
	}

	async resizePty(
		executionId: string,
		fd: number,
		size: { columns: number; rows: number },
	): Promise<void> {
		await this.routePty(executionId, "pty.resize", [
			{ fd, cols: size.columns, rows: size.rows },
		]);
		this.postControl({
			type: "resize-pty",
			executionId,
			columns: size.columns,
			rows: size.rows,
		});
	}

	async closePty(executionId: string, fd: number): Promise<void> {
		await this.routePty(executionId, "pty.close", [{ fd }]);
	}

	async dispatchExtensionRequest(
		namespace: string,
		payload: Uint8Array,
	): Promise<Uint8Array> {
		await this.ready;
		const response = await this.callWorker<BrowserWorkerExtensionResponse>(
			"extension",
			{ namespace, payload },
		);
		return response.payload;
	}

	dispose(): void {
		if (this.disposed) {
			return;
		}
		this.cleanup(new Error("Browser runtime has been disposed"), {
			terminateWorker: true,
		});
	}

	/**
	 * Snapshot the converged VM root filesystem (writable changes) so callers can
	 * persist them to host storage across runtimes. Returns null in legacy mode.
	 */
	async snapshotConvergedRootFilesystem(): Promise<ReturnType<
		ConvergedServicer["snapshotRootFilesystem"]
	> | null> {
		if (this.convergedReady === undefined) {
			return null;
		}
		await this.convergedReady;
		return this.convergedServicer?.snapshotRootFilesystem() ?? null;
	}

	async terminate(): Promise<void> {
		this.dispose();
	}

	signalPendingExecution(signal = 15): boolean {
		if (this.disposed) {
			return false;
		}
		const pending = Array.from(this.pending.values()).find(
			(entry) => entry.executionId,
		);
		if (!pending?.executionId) {
			return false;
		}
		const id = this.nextId++;
		const message: BrowserWorkerRequestMessage = {
			controlToken: this.controlToken,
			id,
			type: "signal",
			payload: {
				executionId: pending.executionId,
				signal,
			},
		};
		this.worker.postMessage(message);
		return true;
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
