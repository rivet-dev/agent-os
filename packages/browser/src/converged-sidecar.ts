// Agent OS converged sidecar loader.
//
// The converged browser runtime lives in `@rivet-dev/agentos-runtime-browser`: the worker, the
// SharedArrayBuffer sync-bridge, and the fs/net/dns/module servicers are all
// reused verbatim. Agent OS plugs in its OWN wasm sidecar — the one that registers
// `BrowserAcpExtension` — via `createBrowserRuntimeDriverFactory({ convergedSidecar })`.
// This is the Agent OS analogue of agentos's `createDefaultConvergedSidecar`:
// same `ConvergedSidecarFactoryOptions` contract, different (ACP-bearing) wasm.
//
// The kernel (wasm) remains the sole enforcement point; no guest-side permission
// eval, one transport. The agentos wasm-bindgen web output fetches its own
// `_bg.wasm`; both URLs resolve relative to this module so a consumer's bundler
// (or native ESM loader) emits/serves them.

import type { CreateVmConfig } from "@rivet-dev/agentos-runtime-core/vm-config";
import type { ProtocolFramePayloadCodec } from "@rivet-dev/agentos-runtime-core/protocol-frames";
import type {
	ConvergedSidecarFactoryOptions,
	ConvergedSidecarHandle,
} from "@rivet-dev/agentos-runtime-browser";
import {
	createConvergedExecutionHostBridge,
	type SyncAgentExecutor,
} from "./converged-execution-host-bridge.js";

const WASM_MODULE_URL = new URL(
	"./sidecar-wasm-web/agentos_sidecar_browser.js",
	import.meta.url,
);
const WASM_BINARY_URL = new URL(
	"./sidecar-wasm-web/agentos_sidecar_browser_bg.wasm",
	import.meta.url,
);

interface AgentOsSidecarWasmWebModule {
	default(input?: unknown): Promise<unknown>;
	AgentOsBrowserSidecarWasm: new (hostBridge?: unknown) => {
		pushFrame(frame: Uint8Array): unknown;
	};
}

export interface AgentOsConvergedSidecarOptions {
	/** Wire codec; defaults to the same-version BARE codec. */
	codec?: ProtocolFramePayloadCodec;
	/** Invoked when the kernel denies a guest fs read with EACCES. */
	onFsReadDenied?: () => void;
	/**
	 * Override the wasm glue-module URL (advanced; defaults to the bundled
	 * dist/sidecar-wasm-web output resolved relative to this module).
	 */
	moduleUrl?: URL | string;
	/** Override the wasm binary URL (advanced; see `moduleUrl`). */
	binaryUrl?: URL | string;
	/**
	 * Run a SYNCHRONOUS in-process ACP agent for `create_session` (e.g. an ACP
	 * echo/test adapter). Required to drive an agent process in-browser, because
	 * the synchronous main-thread `AcpCore` cannot block-wait on an async agent
	 * worker (see AGENTOS-WEB-CONVERGENCE.md Step 7). Omit for guest-only use.
	 */
	agentExecutor?: SyncAgentExecutor;
}

/**
 * Build the {@link ConvergedSidecarFactoryOptions} for the Agent OS web-target
 * wasm kernel (with the ACP extension). Pass the result to
 * `createBrowserRuntimeDriverFactory`'s `convergedSidecar` option:
 *
 * ```ts
 * import { createBrowserRuntimeDriverFactory } from "@rivet-dev/agentos-runtime-browser";
 * import { createAgentOsConvergedSidecar } from "@rivet-dev/agentos-browser";
 *
 * const factory = createBrowserRuntimeDriverFactory({
 *   convergedSidecar: createAgentOsConvergedSidecar(config),
 * });
 * ```
 */
export function createAgentOsConvergedSidecar(
	config: CreateVmConfig,
	options: AgentOsConvergedSidecarOptions = {},
): ConvergedSidecarFactoryOptions {
	const moduleUrl = options.moduleUrl ?? WASM_MODULE_URL;
	const binaryUrl = options.binaryUrl ?? WASM_BINARY_URL;
	return {
		config,
		codec: options.codec ?? "bare",
		onFsReadDenied: options.onFsReadDenied,
		async loadSidecar(): Promise<ConvergedSidecarHandle> {
			const host = createConvergedExecutionHostBridge({
				agentExecutor: options.agentExecutor,
			});
			const wasmModule = (await import(
				/* @vite-ignore */ String(moduleUrl)
			)) as AgentOsSidecarWasmWebModule;
			await wasmModule.default(String(binaryUrl));
			const sidecar = new wasmModule.AgentOsBrowserSidecarWasm(host.bridge);
			return {
				pushFrame: (frame: Uint8Array) => {
					const response = sidecar.pushFrame(frame);
					if (!(response instanceof Uint8Array)) {
						throw new Error("agentos wasm sidecar returned no response frame");
					}
					return response;
				},
				setNextExecutionId: (executionId: string) => {
					host.setNextExecutionId(executionId);
				},
			};
		},
	};
}
