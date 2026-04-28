import { createRequire } from "node:module";
import { dirname } from "node:path";
import { Worker } from "node:worker_threads";
import {
	TIMEOUT_ERROR_MESSAGE,
	TIMEOUT_EXIT_CODE,
	createFsStub,
	createNetworkStub,
	filterEnv,
	wrapFileSystem,
	wrapNetworkAdapter,
} from "@secure-exec/core";
import type {
	ExecOptions,
	ExecResult,
	PythonRunOptions,
	PythonRunResult,
	StdioHook,
	NetworkAdapter,
	PythonRuntimeDriver,
	PythonRuntimeDriverFactory,
	RuntimeDriverOptions,
	VirtualFileSystem,
} from "@secure-exec/core";
import {
	WORKER_WHEEL_PRELOAD_JS,
	checkPackageInstallAllowed,
	normalizeWheelPreload,
	type WheelPreloadOptions,
	type WheelPreloadPayload,
} from "./wheel-preload.js";
import { WORKER_SAB_FETCH_JS } from "./sab-fetch-bootstrap.js";

const MAX_SERIALIZED_VALUE_BYTES = 4 * 1024 * 1024;

/**
 * Extra options accepted by the standalone Pyodide driver beyond the base
 * `RuntimeDriverOptions` shape from @secure-exec/core. Wheel preloading
 * lets callers vendor a curated set of `.whl` files into the runtime via
 * NODEFS + micropip, gated by an explicit allowlist.
 */
export interface PyodideRuntimeDriverExtraOptions {
	wheelPreload?: WheelPreloadOptions;
}

type WorkerRequestType = "init" | "exec" | "run";

type WorkerRequestMessage = {
	id: number;
	type: WorkerRequestType;
	payload?: unknown;
};

type WorkerResponseMessage = {
	type: "response";
	id: number;
	ok: boolean;
	result?: unknown;
	error?: {
		message: string;
		stack?: string;
	};
};

type WorkerStdioMessage = {
	type: "stdio";
	requestId: number;
	channel: "stdout" | "stderr";
	message: string;
};

type WorkerRpcMessage = {
	type: "rpc";
	id: number;
	method: "fsReadTextFile" | "networkFetch";
	params: Record<string, unknown>;
};

type WorkerOutboundMessage =
	| WorkerResponseMessage
	| WorkerStdioMessage
	| WorkerRpcMessage;

type WorkerRpcResultMessage = {
	type: "rpcResult";
	id: number;
	ok: boolean;
	result?: unknown;
	error?: {
		message: string;
	};
};

type PendingRequest = {
	resolve(value: unknown): void;
	reject(reason: unknown): void;
	hook?: StdioHook;
};

function normalizeCpuTimeLimitMs(timeoutMs?: number): number | undefined {
	if (timeoutMs === undefined) {
		return undefined;
	}
	if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) {
		throw new RangeError("cpuTimeLimitMs must be a positive finite number");
	}
	return Math.floor(timeoutMs);
}

function getPyodideIndexPath(): string {
	const requireFromRuntime = createRequire(import.meta.url);
	const pyodideModulePath = requireFromRuntime.resolve("pyodide/pyodide.mjs");
	return `${dirname(pyodideModulePath)}/`;
}

/**
 * Resolve the absolute file:// URL for `pyodide.mjs`. The Worker is
 * created with eval:true and has no module-resolution cwd, so bare
 * specifiers like "pyodide" fail when this driver is consumed from a
 * different package. Pass this URL through the init payload so the
 * worker can `await import(pyodideModuleUrl)` reliably.
 */
function getPyodideModuleUrl(): string {
	const requireFromRuntime = createRequire(import.meta.url);
	const pyodideModulePath = requireFromRuntime.resolve("pyodide/pyodide.mjs");
	return `file://${pyodideModulePath}`;
}

const WORKER_SOURCE = String.raw`
const { parentPort } = require("node:worker_threads");

let pyodide = null;
let currentRequestId = null;
let nextRpcId = 1;
const pendingRpc = new Map();
let sabFetch = null;
${WORKER_WHEEL_PRELOAD_JS}
${WORKER_SAB_FETCH_JS}

function serializeError(error) {
	if (error instanceof Error) {
		return {
			message: error.message,
			stack: error.stack,
		};
	}
	return {
		message: String(error),
	};
}

function isPlainObject(value) {
	if (value === null || typeof value !== "object") {
		return false;
	}
	const proto = Object.getPrototypeOf(value);
	return proto === Object.prototype || proto === null;
}

function serializeValue(value, depth = 0, seen = new WeakSet()) {
	if (
		value === null ||
		typeof value === "boolean" ||
		typeof value === "number" ||
		typeof value === "string"
	) {
		return value;
	}
	if (value === undefined) {
		return null;
	}
	if (depth >= 8) {
		return "[TruncatedDepth]";
	}
	if (typeof value === "bigint") {
		return Number(value);
	}
	if (Array.isArray(value)) {
		const limit = Math.min(value.length, 1024);
		return value.slice(0, limit).map((entry) =>
			serializeValue(entry, depth + 1, seen),
		);
	}
	if (value && typeof value === "object") {
		if (seen.has(value)) {
			return "[Circular]";
		}
		seen.add(value);

		if (typeof value.destroy === "function") {
			let repr = null;
			try {
				repr = String(value);
			} catch {
				repr = "[PyProxy]";
			}
			try {
				value.destroy();
			} catch {}
			return repr;
		}

		if (!isPlainObject(value)) {
			return String(value);
		}

		const out = {};
		const entries = Object.entries(value).slice(0, 1024);
		for (const [key, entryValue] of entries) {
			out[key] = serializeValue(entryValue, depth + 1, seen);
		}
		return out;
	}
	return String(value);
}

function postStdio(channel, message) {
	if (currentRequestId === null) {
		return;
	}
	parentPort.postMessage({
		type: "stdio",
		requestId: currentRequestId,
		channel,
		message: String(message),
	});
}

function callHost(method, params) {
	return new Promise((resolve, reject) => {
		const id = nextRpcId++;
		pendingRpc.set(id, { resolve, reject });
		parentPort.postMessage({ type: "rpc", id, method, params });
	});
}

async function ensurePyodide(payload) {
	if (pyodide) {
		return pyodide;
	}
	// Pyodide must be loaded by absolute file: URL when the host resolves it,
	// because the Worker (eval:true) has no module-resolution cwd. Falls back
	// to bare specifier so existing tests that don't pass pyodideModuleUrl
	// still work.
	const pyodideUrl = payload?.pyodideModuleUrl
		? payload.pyodideModuleUrl
		: "pyodide";
	const { loadPyodide } = await import(pyodideUrl);
	pyodide = await loadPyodide({
		indexURL: payload?.indexPath,
		env: payload?.env || {},
		stdout: (message) => postStdio("stdout", message),
		stderr: (message) => postStdio("stderr", message),
	});

	pyodide.registerJsModule("secure_exec", {
		read_text_file: async (path) => callHost("fsReadTextFile", { path }),
		fetch: async (url, options) =>
			callHost("networkFetch", { url, options: options || {} }),
	});

	// Synchronous fetch for libraries that can't await JS Promises from
	// wasm side modules (DuckDB's parquet reader is the canonical case).
	// Backed by SharedArrayBuffer + side-worker. Side worker uses Node's
	// native fetch directly — does NOT route through the NetworkAdapter
	// permission gate. Safe for the playground's full-network Pi VM;
	// stricter deployments should pass a MessagePort to the side worker
	// to gate each fetch through callHost("networkFetch", ...).
	if (sabFetch === null) {
		sabFetch = startSabFetch();
	}
	registerSabFetchModule(pyodide, sabFetch);

	// Preload vendored wheels (mount + micropip + bootstrap) BEFORE locking
	// down access to pyodide_js. micropip itself depends on pyodide_js for
	// installation; if we install the importer block first, micropip cannot
	// load. The trusted preload code runs first, then we engage the sandbox
	// for any subsequent agent code execution.
	await applyWheelPreload(pyodide, payload?.wheelPreload);

	// Block import js / pyodide_js — prevents sandbox escape via host JS runtime
	await pyodide.runPythonAsync([
		"import sys",
		"import importlib.abc",
		"import importlib.machinery",
		"class _BlockHostJsImporter(importlib.abc.MetaPathFinder):",
		"    _BLOCKED = frozenset(('js', 'pyodide_js'))",
		"    def find_spec(self, fullname, path, target=None):",
		"        if fullname in self._BLOCKED or fullname.startswith('js.') or fullname.startswith('pyodide_js.'):",
		"            raise ImportError('module ' + repr(fullname) + ' is blocked in sandbox')",
		"        return None",
		"    def find_module(self, fullname, path=None):",
		"        if fullname in self._BLOCKED or fullname.startswith('js.') or fullname.startswith('pyodide_js.'):",
		"            raise ImportError('module ' + repr(fullname) + ' is blocked in sandbox')",
		"        return None",
		"sys.meta_path.insert(0, _BlockHostJsImporter())",
		"for _m in list(sys.modules):",
		"    if _m == 'js' or _m == 'pyodide_js' or _m.startswith('js.') or _m.startswith('pyodide_js.'):",
		"        del sys.modules[_m]",
		"del _m, _BlockHostJsImporter",
	].join("\n"));

	return pyodide;
}

async function applyExecOverrides(py, options) {
	if (!options) {
		py.setStdin();
		return async () => {};
	}
	if (typeof options.stdin === "string") {
		const lines = options.stdin.split(/\r?\n/);
		let cursor = 0;
		py.setStdin({
			stdin: () => {
				if (cursor >= lines.length) {
					return undefined;
				}
				const line = lines[cursor];
				cursor += 1;
				return line;
			},
			autoEOF: true,
		});
	} else {
		py.setStdin();
	}

	const cleanup = [];
	const runCleanup = async () => {
		for (let index = cleanup.length - 1; index >= 0; index -= 1) {
			try {
				await cleanup[index]();
			} catch {}
		}
	};

	try {
		if (options.env && typeof options.env === "object") {
			py.globals.set(
				"__secure_exec_env_overrides_json__",
				JSON.stringify(options.env),
			);
			try {
				await py.runPythonAsync(
					"import json\nimport os\n__secure_exec_env_restore__ = {}\nfor _k, _v in json.loads(__secure_exec_env_overrides_json__).items():\n    _key = str(_k)\n    __secure_exec_env_restore__[_key] = os.environ.get(_key)\n    os.environ[_key] = str(_v)",
				);
				cleanup.push(async () => {
					try {
						await py.runPythonAsync(
							"import os\nfor _k, _v in __secure_exec_env_restore__.items():\n    if _v is None:\n        os.environ.pop(_k, None)\n    else:\n        os.environ[_k] = str(_v)\ntry:\n    del __secure_exec_env_restore__\nexcept NameError:\n    pass",
						);
					} catch {}
				});
			} finally {
				try {
					py.globals.delete("__secure_exec_env_overrides_json__");
				} catch {}
			}
		}

		if (typeof options.cwd === "string") {
			py.globals.set("__secure_exec_cwd_override__", options.cwd);
			try {
				await py.runPythonAsync(
					"import os\n__secure_exec_previous_cwd__ = os.getcwd()\nos.chdir(str(__secure_exec_cwd_override__))",
				);
				cleanup.push(async () => {
					try {
						await py.runPythonAsync(
							"import os\nos.chdir(__secure_exec_previous_cwd__)\ntry:\n    del __secure_exec_previous_cwd__\nexcept NameError:\n    pass",
						);
					} catch {}
				});
			} finally {
				try {
					py.globals.delete("__secure_exec_cwd_override__");
				} catch {}
			}
		}

		return runCleanup;
	} catch (error) {
		await runCleanup();
		throw error;
	}
}

function collectGlobals(py, names) {
	if (!Array.isArray(names) || names.length === 0) {
		return undefined;
	}
	const out = {};
	for (const name of names) {
		if (typeof name !== "string") {
			continue;
		}
		let value;
		try {
			value = py.globals.get(name);
		} catch {
			continue;
		}
		out[name] = serializeValue(value);
		if (value && typeof value.destroy === "function") {
			try {
				value.destroy();
			} catch {}
		}
	}
	return out;
}

function assertSerializedSize(value, maxBytes) {
	const json = JSON.stringify(value);
	const bytes = Buffer.byteLength(json, "utf8");
	if (bytes > maxBytes) {
		throw new Error(
			"ERR_SANDBOX_PAYLOAD_TOO_LARGE: python.run value exceeds " +
				String(maxBytes) +
				" bytes",
		);
	}
}

parentPort.on("message", async (message) => {
	if (!message || typeof message !== "object") {
		return;
	}

	if (message.type === "rpcResult") {
		const pending = pendingRpc.get(message.id);
		if (!pending) {
			return;
		}
		pendingRpc.delete(message.id);
		if (message.ok) {
			pending.resolve(message.result);
			return;
		}
		pending.reject(new Error(message.error?.message || "Host RPC failed"));
		return;
	}

	if (message.type !== "init" && message.type !== "exec" && message.type !== "run") {
		return;
	}

	currentRequestId = message.id;
	try {
		const py = await ensurePyodide(message.type === "init" ? message.payload : undefined);

		if (message.type === "init") {
			parentPort.postMessage({ type: "response", id: message.id, ok: true, result: {} });
			return;
		}

		const payload = message.payload || {};
		const cleanup = await applyExecOverrides(py, payload.options);
		try {
			if (message.type === "exec") {
				await py.runPythonAsync(payload.code, {
					filename: payload.options?.filePath || "<exec>",
				});
				parentPort.postMessage({
					type: "response",
					id: message.id,
					ok: true,
					result: { code: 0 },
				});
				return;
			}

			const rawValue = await py.runPythonAsync(payload.code, {
				filename: payload.options?.filePath || "<run>",
			});
			const serializedValue = serializeValue(rawValue);
			if (rawValue && typeof rawValue.destroy === "function") {
				try {
					rawValue.destroy();
				} catch {}
			}
			const globals = collectGlobals(py, payload.options?.globals);
			const result = {
				code: 0,
				value: serializedValue,
				globals,
			};
			assertSerializedSize(result, payload.maxSerializedBytes || 4194304);
			parentPort.postMessage({ type: "response", id: message.id, ok: true, result });
		} finally {
			await cleanup();
		}
	} catch (error) {
		parentPort.postMessage({
			type: "response",
			id: message.id,
			ok: false,
			error: serializeError(error),
		});
	} finally {
		currentRequestId = null;
	}
});
`;

export class PyodideRuntimeDriver implements PythonRuntimeDriver {
	private worker: Worker | null = null;
	private readonly pending = new Map<number, PendingRequest>();
	private readonly defaultOnStdio?: StdioHook;
	private readonly filesystem: VirtualFileSystem;
	private readonly networkAdapter: NetworkAdapter;
	private readonly defaultCpuTimeLimitMs?: number;
	private readonly runtimeEnv: Record<string, string>;
	private readonly indexPath: string;
	private readonly wheelPreload?: WheelPreloadPayload;
	private readonly allowRuntimeInstalls: boolean;
	private nextRequestId = 1;
	private readyPromise: Promise<void> | null = null;
	private disposed = false;

	constructor(
		private readonly options: RuntimeDriverOptions &
			PyodideRuntimeDriverExtraOptions,
	) {
		this.defaultOnStdio = options.onStdio;
		const permissions = options.system.permissions;
		this.filesystem = options.system.filesystem
			? wrapFileSystem(options.system.filesystem, permissions)
			: createFsStub();
		this.networkAdapter = options.system.network
			? wrapNetworkAdapter(options.system.network, permissions)
			: createNetworkStub();
		this.runtimeEnv = filterEnv(options.runtime.process.env, permissions);
		this.defaultCpuTimeLimitMs = normalizeCpuTimeLimitMs(options.cpuTimeLimitMs);
		this.indexPath = getPyodideIndexPath();
		this.wheelPreload = normalizeWheelPreload(options.wheelPreload);
		this.allowRuntimeInstalls =
			options.wheelPreload?.allowRuntimeInstalls ?? false;
	}

	private ensureNotDisposed(): void {
		if (this.disposed) {
			throw new Error("PythonRuntime has been disposed");
		}
	}

	private handleWorkerMessage = (message: WorkerOutboundMessage): void => {
		if (message.type === "stdio") {
			const pending = this.pending.get(message.requestId);
			const hook = pending?.hook ?? this.defaultOnStdio;
			if (!hook) {
				return;
			}
			try {
				hook({ channel: message.channel, message: message.message });
			} catch {
				// Keep runtime execution deterministic if host hooks fail.
			}
			return;
		}

		if (message.type === "rpc") {
			void this.handleWorkerRpc(message);
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
		const error = new Error(message.error?.message ?? "Pyodide worker request failed");
		if (message.error?.stack) {
			error.stack = message.error.stack;
		}
		pending.reject(error);
	};

	private handleWorkerError = (error: Error): void => {
		this.rejectAllPending(error);
	};

	private handleWorkerExit = (): void => {
		if (!this.disposed) {
			this.rejectAllPending(new Error("Pyodide worker exited unexpectedly"));
		}
		this.worker = null;
		this.readyPromise = null;
	};

	private async handleWorkerRpc(message: WorkerRpcMessage): Promise<void> {
		let result: unknown;
		let error: Error | null = null;
		try {
			switch (message.method) {
				case "fsReadTextFile": {
					const path = String(message.params.path ?? "");
					result = await this.filesystem.readTextFile(path);
					break;
				}
				case "networkFetch": {
					const url = String(message.params.url ?? "");
					const options =
						typeof message.params.options === "object" && message.params.options !== null
							? (message.params.options as {
									method?: string;
									headers?: Record<string, string>;
									body?: string | null;
							  })
							: {};
					result = await this.networkAdapter.fetch(url, options);
					break;
				}
				default:
					throw new Error(`Unsupported worker RPC method: ${message.method}`);
			}
		} catch (rpcError) {
			error = rpcError instanceof Error ? rpcError : new Error(String(rpcError));
		}

		if (!this.worker) {
			return;
		}
		const response: WorkerRpcResultMessage = error
			? {
					type: "rpcResult",
					id: message.id,
					ok: false,
					error: { message: error.message },
			  }
			: {
					type: "rpcResult",
					id: message.id,
					ok: true,
					result,
			  };
		this.worker.postMessage(response);
	}

	private rejectAllPending(error: Error): void {
		const pendingRequests = Array.from(this.pending.values());
		this.pending.clear();
		for (const pending of pendingRequests) {
			pending.reject(error);
		}
	}

	private createWorker(): Worker {
		const worker = new Worker(WORKER_SOURCE, { eval: true });
		worker.on("message", this.handleWorkerMessage as (message: unknown) => void);
		worker.on("error", this.handleWorkerError);
		worker.on("exit", this.handleWorkerExit);
		return worker;
	}

	private async ensureWorkerReady(): Promise<void> {
		this.ensureNotDisposed();
		if (this.readyPromise) {
			await this.readyPromise;
			return;
		}

		this.worker = this.createWorker();
		this.readyPromise = this.callWorker<void>("init", {
			indexPath: this.indexPath,
			pyodideModuleUrl: getPyodideModuleUrl(),
			env: this.runtimeEnv,
			wheelPreload: this.wheelPreload,
		}).then(() => undefined);
		await this.readyPromise;
	}

	private async restartWorkerAfterTimeout(): Promise<void> {
		const worker = this.worker;
		this.worker = null;
		this.readyPromise = null;
		if (worker) {
			worker.removeAllListeners();
			await worker.terminate();
		}
		this.rejectAllPending(new Error(TIMEOUT_ERROR_MESSAGE));
	}

	private callWorker<T>(
		type: WorkerRequestType,
		payload?: unknown,
		hook?: StdioHook,
	): Promise<T> {
		this.ensureNotDisposed();
		if (!this.worker) {
			return Promise.reject(new Error("Pyodide worker is not initialized"));
		}

		const id = this.nextRequestId++;
		const message: WorkerRequestMessage =
			payload === undefined ? { id, type } : { id, type, payload };

		return new Promise<T>((resolve, reject) => {
			this.pending.set(id, { resolve, reject, hook });
			this.worker!.postMessage(message);
		});
	}

	private async runWithTimeout<T>(
		requestFactory: () => Promise<T>,
		timeoutMs: number | undefined,
	): Promise<{ timedOut: boolean; value?: T }> {
		if (timeoutMs === undefined) {
			return {
				timedOut: false,
				value: await requestFactory(),
			};
		}

		return new Promise((resolve, reject) => {
			let settled = false;
			const timer = setTimeout(async () => {
				if (settled) {
					return;
				}
				settled = true;
				try {
					await this.restartWorkerAfterTimeout();
					resolve({ timedOut: true });
				} catch (error) {
					reject(error);
				}
			}, timeoutMs);

			void requestFactory().then(
				(value) => {
					if (settled) {
						return;
					}
					settled = true;
					clearTimeout(timer);
					resolve({ timedOut: false, value });
				},
				(error) => {
					if (settled) {
						return;
					}
					settled = true;
					clearTimeout(timer);
					reject(error);
				},
			);
		});
	}

	async run<T = unknown>(
		code: string,
		options: PythonRunOptions = {},
	): Promise<PythonRunResult<T>> {
		try {
			checkPackageInstallAllowed(code, {
				allowRuntimeInstalls: this.allowRuntimeInstalls,
			});
			await this.ensureWorkerReady();
			const timeoutMs = normalizeCpuTimeLimitMs(
				options.cpuTimeLimitMs ?? this.defaultCpuTimeLimitMs,
			);
			const hook = options.onStdio ?? this.defaultOnStdio;
			const envOverrides =
				options.env === undefined
					? undefined
					: filterEnv(options.env, this.options.system.permissions);
			const result = await this.runWithTimeout(
				() =>
					this.callWorker<PythonRunResult<T>>(
						"run",
						{
							code,
							options: {
								filePath: options.filePath,
								globals: options.globals,
								cwd: options.cwd,
								env: envOverrides,
								stdin: options.stdin,
							},
							maxSerializedBytes: MAX_SERIALIZED_VALUE_BYTES,
						},
						hook,
					),
				timeoutMs,
			);

			if (result.timedOut) {
				return {
					code: TIMEOUT_EXIT_CODE,
					errorMessage: TIMEOUT_ERROR_MESSAGE,
				};
			}

			return result.value ?? { code: 0 };
		} catch (error) {
			return {
				code: 1,
				errorMessage: error instanceof Error ? error.message : String(error),
			};
		}
	}

	async exec(code: string, options?: ExecOptions): Promise<ExecResult> {
		try {
			checkPackageInstallAllowed(code, {
				allowRuntimeInstalls: this.allowRuntimeInstalls,
			});
			await this.ensureWorkerReady();
			const timeoutMs = normalizeCpuTimeLimitMs(
				options?.cpuTimeLimitMs ?? this.defaultCpuTimeLimitMs,
			);
			const hook = options?.onStdio ?? this.defaultOnStdio;
			const envOverrides =
				options?.env === undefined
					? undefined
					: filterEnv(options.env, this.options.system.permissions);
			const result = await this.runWithTimeout(
				() =>
					this.callWorker<ExecResult>(
						"exec",
						{
							code,
							options: {
								cwd: options?.cwd,
								env: envOverrides,
								stdin: options?.stdin,
								filePath: options?.filePath,
							},
						},
						hook,
					),
				timeoutMs,
			);

			if (result.timedOut) {
				return {
					code: TIMEOUT_EXIT_CODE,
					errorMessage: TIMEOUT_ERROR_MESSAGE,
				};
			}
			return result.value ?? { code: 0 };
		} catch (error) {
			return {
				code: 1,
				errorMessage: error instanceof Error ? error.message : String(error),
			};
		}
	}

	dispose(): void {
		if (this.disposed) {
			return;
		}
		this.disposed = true;
		const worker = this.worker;
		this.worker = null;
		this.readyPromise = null;
		if (worker) {
			worker.removeAllListeners();
			void worker.terminate();
		}
		this.rejectAllPending(new Error("PythonRuntime has been disposed"));
	}

	async terminate(): Promise<void> {
		if (this.disposed) {
			return;
		}
		this.disposed = true;
		const worker = this.worker;
		this.worker = null;
		this.readyPromise = null;
		if (worker) {
			worker.removeAllListeners();
			await worker.terminate();
		}
		this.rejectAllPending(new Error("PythonRuntime has been disposed"));
	}
}

/**
 * Factory accepts an optional `factoryDefaults` argument so callers can
 * preconfigure wheel preloading (or any other extra option) once and have
 * every runtime created by the factory inherit it. Per-call options on
 * `createRuntimeDriver` always win when they overlap.
 */
export function createPyodideRuntimeDriverFactory(
	factoryDefaults?: PyodideRuntimeDriverExtraOptions,
): PythonRuntimeDriverFactory {
	return {
		createRuntimeDriver(
			options: RuntimeDriverOptions & PyodideRuntimeDriverExtraOptions,
		): PythonRuntimeDriver {
			const merged: RuntimeDriverOptions & PyodideRuntimeDriverExtraOptions = {
				...options,
				wheelPreload: options.wheelPreload ?? factoryDefaults?.wheelPreload,
			};
			return new PyodideRuntimeDriver(merged);
		},
	};
}
