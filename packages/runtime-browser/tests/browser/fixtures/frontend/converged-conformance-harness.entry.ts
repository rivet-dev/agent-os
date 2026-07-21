// Bundled converged conformance harness.
//
// Exposes the same window.__agentOsBrowserHarness API the conformance specs
// drive (via harness.ts), but every runtime is created with the converged
// sidecar so guest syscalls run against the wasm kernel instead of the legacy
// in-process TS kernel. This is the vehicle for running the conformance suite
// against the converged path (item 2). It is esbuild-bundled (the converged
// modules import @rivet-dev/agentos-runtime-core, which can't load unbundled from /dist).

import {
	allowAll,
	createBrowserDriver,
	createBrowserRuntimeDriverFactory,
} from "../../../../src/index.js";
import { createConvergedExecutionHostBridge } from "../../../../src/converged-execution-host-bridge.js";
import { convergedPermissionsPolicy } from "../../../../src/converged-permissions.js";
import { rootFilesystemConfigFromVfs } from "../../../../src/root-filesystem-from-vfs.js";
import { decodeBase64 } from "../../../../src/converged-base64.js";

const WASM_MODULE_URL = "/sidecar-wasm-web/agentos_native_sidecar_browser.js";
const WASM_BINARY_URL = "/sidecar-wasm-web/agentos_native_sidecar_browser_bg.wasm";

type StdioEvent = { channel?: string; message?: unknown; data?: unknown };

interface ConvergedDriver {
	exec(code: string, options?: unknown): Promise<{ code?: number }>;
	dispose(): Promise<void> | void;
	disposed?: boolean;
	ready?: Promise<void>;
	signalPendingExecution?(signal: number): boolean;
	syncBridge?: { signalBuffer?: ArrayBufferLike };
	pending?: Map<unknown, unknown>;
	signalStates?: Map<string, Map<number, unknown>>;
	worker?: { onmessage?: unknown; onerror?: unknown };
	snapshotConvergedRootFilesystem?(): Promise<
		Array<{
			path: string;
			kind: string;
			content?: string;
			encoding?: string;
			target?: string;
		}> | null
	>;
}

interface PersistFs {
	writeFile(path: string, content: string | Uint8Array): Promise<void>;
	mkdir(path: string, options?: { recursive?: boolean }): Promise<void>;
	symlink(target: string, linkPath: string): Promise<void>;
}

interface ConvergedRuntimeRecord {
	driver: ConvergedDriver;
	decisions: { deniedFsReads: number };
	persistTo?: PersistFs;
}

function debugRuntime(driver: ConvergedDriver) {
	const signalState = driver.syncBridge?.signalBuffer
		? Array.from(new Int32Array(driver.syncBridge.signalBuffer))
		: [];
	const signalHandlers = Array.from(driver.signalStates?.entries?.() ?? []).map(
		([executionId, handlers]) => ({
			executionId,
			handlers: Array.from(handlers.entries()).map(([signal, registration]) => ({
				signal,
				...(registration as object),
			})),
		}),
	);
	return {
		disposed: Boolean(driver.disposed),
		pendingCount: driver.pending?.size ?? 0,
		signalState,
		signalHandlers,
		workerOnmessage: driver.worker?.onmessage === null ? "null" : "set",
		workerOnerror: driver.worker?.onerror === null ? "null" : "set",
	};
}

declare global {
	interface Window {
		__agentOsBrowserHarness?: unknown;
	}
}

let nextRuntimeId = 1;
const runtimes = new Map<string, ConvergedRuntimeRecord>();

async function loadSidecar(): Promise<{
	pushFrame: (frame: Uint8Array) => Uint8Array;
	setNextExecutionId: (executionId: string) => void;
}> {
	const wasmModule = await import(/* @vite-ignore */ WASM_MODULE_URL);
	await wasmModule.default(WASM_BINARY_URL);
	const host = createConvergedExecutionHostBridge();
	const sidecar = new wasmModule.BrowserSidecarWasm(host.bridge);
	return {
		pushFrame: (frame: Uint8Array) => {
			const response = sidecar.pushFrame(frame);
			if (!(response instanceof Uint8Array)) {
				throw new Error("wasm sidecar returned no response frame");
			}
			return response;
		},
		setNextExecutionId: host.setNextExecutionId,
	};
}

function permissionDenials(options: Record<string, unknown>) {
	return {
		denyFsRead: Boolean(options.denyFsRead),
		denyChildProcess: Boolean(options.denyChildProcess),
		denyNetwork: Boolean(options.denyNetwork),
		denyNetworkPort:
			typeof options.denyNetworkPort === "number"
				? (options.denyNetworkPort as number)
				: undefined,
	};
}

// Mirrors the legacy harness echo executor so child_process conformance scenarios
// (which run via the legacy commandExecutor host capability, not the kernel) work
// against the converged harness too.
function createEchoCommandExecutor() {
	return {
		spawn(
			command: string,
			args: string[],
			options: {
				onStdout?: (data: Uint8Array) => void;
				onStderr?: (data: Uint8Array) => void;
			} = {},
		) {
			let stdout = "";
			let stderr = "";
			let exitCode = 0;
			let exitSignal: number | null = null;
			let stdin = "";
			let resolveWait: (() => void) | undefined;
			let waitPromise: Promise<void> = Promise.resolve();
			if (command === "echo") {
				stdout = `${args.join(" ")}\n`;
			} else if (command === "wait-signal") {
				waitPromise = new Promise((resolve) => {
					resolveWait = resolve;
				});
			} else {
				stderr = `command not found: ${command}\n`;
				exitCode = 127;
			}
			queueMicrotask(() => {
				if (stdout) options.onStdout?.(new TextEncoder().encode(stdout));
				if (stderr) options.onStderr?.(new TextEncoder().encode(stderr));
			});
			return {
				async wait() {
					await waitPromise;
					return exitSignal === null
						? exitCode
						: { exitCode: null, signal: exitSignal };
				},
				writeStdin(data: string | Uint8Array) {
					stdin += typeof data === "string" ? data : new TextDecoder().decode(data);
				},
				closeStdin() {
					if (command === "cat" && stdin) {
						options.onStdout?.(new TextEncoder().encode(stdin));
					}
				},
				kill(signal = 15) {
					const signalNumber = Number(signal) || 15;
					exitSignal = signalNumber;
					options.onStdout?.(
						new TextEncoder().encode(`signal:${signalNumber}\n`),
					);
					resolveWait?.();
					return true;
				},
			};
		},
	};
}

async function createRuntime(options: Record<string, unknown> = {}) {
	// child_process is a host capability (the commandExecutor), gated by the
	// driver's permission policy rather than the wasm kernel; keep its denial on
	// the driver while fs/network denials go to the kernel config below.
	const driverPermissions = options.denyChildProcess
		? {
				...allowAll,
				childProcess: () => ({ allow: false, reason: "harness deny" }),
			}
		: allowAll;
	const system = await createBrowserDriver({
		filesystem: (options.filesystem as "memory" | "opfs") ?? "memory",
		permissions: driverPermissions as never,
		useDefaultNetwork: Boolean(options.useDefaultNetwork),
	});
	if (options.commandExecutor === "echo") {
		(system as { commandExecutor?: unknown }).commandExecutor =
			createEchoCommandExecutor();
	}
	(system as { runtime?: unknown }).runtime = {
		process: options.processConfig ?? {},
		os: options.osConfig ?? {},
	};

	const config = {
		rootFilesystem: await rootFilesystemConfigFromVfs(
			(system as { filesystem: Parameters<typeof rootFilesystemConfigFromVfs>[0] })
				.filesystem,
		),
		permissions: convergedPermissionsPolicy(permissionDenials(options)),
	} as never;

	const decisions = { deniedFsReads: 0 };
	const factory = createBrowserRuntimeDriverFactory({
		workerUrl: new URL("/agentos-worker.js", window.location.href),
		convergedSidecar: {
			loadSidecar,
			config,
			onFsReadDenied: () => {
				decisions.deniedFsReads += 1;
			},
		},
	});
	const driver = factory.createRuntimeDriver({
		system,
		runtime: (system as { runtime: unknown }).runtime,
	} as never) as ConvergedRuntimeRecord["driver"];

	const runtimeId = `converged-runtime-${nextRuntimeId++}`;
	runtimes.set(runtimeId, {
		driver,
		decisions,
		// Persist the converged VM fs back to the host filesystem (e.g. OPFS) on
		// dispose so data survives across runtimes, matching the legacy model.
		persistTo:
			options.filesystem === "opfs"
				? ((system as { filesystem: PersistFs }).filesystem)
				: undefined,
	});
	return {
		crossOriginIsolated: window.crossOriginIsolated,
		runtimeId,
		workerUrl: "/agentos-worker.js",
	};
}

function getRuntime(runtimeId: string): ConvergedRuntimeRecord {
	const runtime = runtimes.get(runtimeId);
	if (!runtime) {
		throw new Error(`unknown converged runtime: ${runtimeId}`);
	}
	return runtime;
}

async function exec(runtimeId: string, code: string, options: Record<string, unknown> = {}) {
	const runtime = getRuntime(runtimeId);
	const stdio: StdioEvent[] = [];
	const result = await runtime.driver.exec(code, {
		...options,
		onStdio: (event: unknown) => stdio.push(event as StdioEvent),
	});
	return {
		crossOriginIsolated: window.crossOriginIsolated,
		result,
		stdio,
		permissionDecisions: { deniedFsReads: runtime.decisions.deniedFsReads },
	};
}

async function disposeRuntime(runtimeId: string) {
	const runtime = runtimes.get(runtimeId);
	if (!runtime) {
		return;
	}
	if (runtime.persistTo && runtime.driver.snapshotConvergedRootFilesystem) {
		const entries = await runtime.driver.snapshotConvergedRootFilesystem();
		if (entries) {
			await persistEntries(entries, runtime.persistTo);
		}
	}
	await runtime.driver.dispose();
	runtimes.delete(runtimeId);
}

async function persistEntries(
	entries: Array<{
		path: string;
		kind: string;
		content?: string;
		encoding?: string;
		target?: string;
	}>,
	fs: PersistFs,
) {
	for (const entry of entries) {
		if (entry.path === "/" || entry.path.startsWith("/dev") || entry.path.startsWith("/proc")) {
			continue;
		}
		if (entry.kind === "directory") {
			await fs.mkdir(entry.path, { recursive: true }).catch(() => undefined);
		} else if (entry.kind === "symlink" && entry.target) {
			await fs.symlink(entry.target, entry.path).catch(() => undefined);
		} else if (entry.kind === "file") {
			const content =
				entry.encoding === "base64"
					? decodeBase64(entry.content ?? "")
					: (entry.content ?? "");
			await fs.writeFile(entry.path, content).catch(() => undefined);
		}
	}
}

async function disposeAllRuntimes() {
	for (const runtimeId of Array.from(runtimes.keys())) {
		await disposeRuntime(runtimeId);
	}
}

async function runPending(
	runtimeId: string,
	code: string,
	delayMs: number,
	act: (driver: ConvergedDriver) => unknown,
) {
	const { driver } = getRuntime(runtimeId);
	await driver.ready;
	let outcome = "resolved";
	let resultCode: number | null = null;
	let errorMessage: string | null = null;
	const pending = driver.exec(code).then(
		(result) => {
			resultCode = result.code ?? null;
		},
		(error: unknown) => {
			outcome = "rejected";
			errorMessage = error instanceof Error ? error.message : String(error);
		},
	);
	await new Promise((resolve) => setTimeout(resolve, delayMs));
	const acted = act(driver);
	await pending;
	return { outcome, resultCode, errorMessage, acted, debug: debugRuntime(driver) };
}

async function terminatePendingExec(runtimeId: string, code: string, delayMs = 25) {
	return runPending(runtimeId, code, delayMs, (driver) => driver.dispose());
}

async function signalPendingExec(
	runtimeId: string,
	code: string,
	signal = 15,
	delayMs = 25,
) {
	const result = await runPending(runtimeId, code, delayMs, (driver) =>
		driver.signalPendingExecution?.(signal),
	);
	return { ...result, signaled: result.acted };
}

async function debugPendingExec(runtimeId: string, code: string, delayMs = 25) {
	const { driver } = getRuntime(runtimeId);
	const pendingResult = await runPending(runtimeId, code, delayMs, (d) => {
		const snapshot = debugRuntime(d);
		d.dispose();
		return snapshot;
	});
	return { ...pendingResult, debug: pendingResult.acted };
}

function runtimeDebug(runtimeId: string) {
	return debugRuntime(getRuntime(runtimeId).driver);
}

async function smoke() {
	const { runtimeId } = await createRuntime({});
	const result = await exec(
		runtimeId,
		"process.stdout.write('harness-ready\\n');",
	);
	await disposeRuntime(runtimeId);
	return {
		crossOriginIsolated: window.crossOriginIsolated,
		workerUrl: "/agentos-worker.js",
		result: result.result,
		stdio: result.stdio,
	};
}

window.__agentOsBrowserHarness = {
	createRuntime,
	exec,
	disposeRuntime,
	disposeAllRuntimes,
	terminatePendingExec,
	signalPendingExec,
	debugPendingExec,
	runtimeDebug,
	smoke,
};

const status = document.getElementById("harness-status");
if (status) {
	status.textContent = "ready";
}
