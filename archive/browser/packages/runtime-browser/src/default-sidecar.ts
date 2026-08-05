// Default converged sidecar loader.
//
// Item 3b of the browser convergence: ship the web-target wasm kernel
// (archive/browser/crates/sidecar-browser, built into dist/sidecar-wasm-web/) plus a ready-made
// loader so consumers get the converged runtime out of the box without
// supplying their own wasm binding. The runtime driver is converged-only, so
// `createBrowserRuntimeDriverFactory({ convergedSidecar: createDefaultConvergedSidecar(config) })`
// is the supported zero-config path.
//
// The wasm-bindgen web output fetches its own `_bg.wasm`; we resolve both the
// glue module and the binary relative to this module's URL so a consumer's
// bundler (or the browser's native ESM loader) emits/serves them correctly.

import type { CreateVmConfig } from "@rivet-dev/agentos-runtime-core/vm-config";
import type { ProtocolFramePayloadCodec } from "@rivet-dev/agentos-runtime-core/protocol-frames";
import { createConvergedExecutionHostBridge } from "./converged-execution-host-bridge.js";
import type {
	ConvergedSidecarFactoryOptions,
	ConvergedSidecarHandle,
} from "./runtime-driver.js";

const WASM_MODULE_URL = new URL(
	"./sidecar-wasm-web/agentos_vm_browser.js",
	import.meta.url,
);
const WASM_BINARY_URL = new URL(
	"./sidecar-wasm-web/agentos_vm_browser_bg.wasm",
	import.meta.url,
);

interface BrowserSidecarWasmWebModule {
	default(input?: unknown): Promise<unknown>;
	BrowserSidecarWasm: new (hostBridge?: unknown) => {
		pushFrame(frame: Uint8Array): unknown;
	};
}

export interface DefaultConvergedSidecarOptions {
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
}

/**
 * Build the {@link ConvergedSidecarFactoryOptions} for the bundled web-target
 * wasm kernel. Pass the result to `createBrowserRuntimeDriverFactory`'s
 * `convergedSidecar` option.
 */
export function createDefaultConvergedSidecar(
	config: CreateVmConfig,
	options: DefaultConvergedSidecarOptions = {},
): ConvergedSidecarFactoryOptions {
	const moduleUrl = options.moduleUrl ?? WASM_MODULE_URL;
	const binaryUrl = options.binaryUrl ?? WASM_BINARY_URL;
	return {
		config,
		codec: options.codec ?? "bare",
		onFsReadDenied: options.onFsReadDenied,
		async loadSidecar(): Promise<ConvergedSidecarHandle> {
			const host = createConvergedExecutionHostBridge();
			const wasmModule = (await import(
				/* @vite-ignore */ String(moduleUrl)
			)) as BrowserSidecarWasmWebModule;
			await wasmModule.default(String(binaryUrl));
			const sidecar = new wasmModule.BrowserSidecarWasm(host.bridge);
			return {
				pushFrame: (frame: Uint8Array) => {
					const response = sidecar.pushFrame(frame);
					if (!(response instanceof Uint8Array)) {
						throw new Error("wasm sidecar returned no response frame");
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
