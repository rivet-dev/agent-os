// AGENTOS_BROWSER_SUPPORT_DISABLED: retained for reference, but AgentOS is native-only.
/*
// @rivet-dev/agentos-browser — converged browser runtime for Agent OS.
//
// The browser runtime is @rivet-dev/agentos-runtime-browser's CONVERGED stack (worker,
// SharedArrayBuffer sync-bridge, fs/net/dns/module servicers, all enforced by the
// wasm kernel). Agent OS does not carry its own copy; it re-exports that runtime
// and adds only the ACP/wasm-sidecar layer (createAgentOsConvergedSidecar). The
// pre-convergence TS-kernel files (worker/runtime/driver/sync-bridge/permission
// eval) were deleted in the reconciliation — the kernel is the sole enforcement
// point, so guest-side permission eval no longer exists here.
//
// Per the converged model (kernel-owns-fs), per-runtime OPFS *namespace* helpers
// (listOpfsNamespaces/releaseOpfsNamespace) are gone: storage isolation is the
// kernel's responsibility, not a TS-layer concern.

// --- Converged runtime, re-exported from @rivet-dev/agentos-runtime-browser ---
export type {
	BrowserDriverOptions,
	BrowserRuntimeSystemOptions,
} from "@rivet-dev/agentos-runtime-browser";
export {
	createBrowserDriver,
	createBrowserNetworkAdapter,
	createOpfsFileSystem,
	InMemoryFileSystem,
} from "@rivet-dev/agentos-runtime-browser";
export type {
	ExecOptions,
	ExecResult,
	NodeRuntimeDriver,
	StdioChannel,
	StdioEvent,
	TimingMitigation,
} from "@rivet-dev/agentos-runtime-browser";
export {
	allowAll,
	allowAllChildProcess,
	allowAllEnv,
	allowAllFs,
	allowAllNetwork,
	createInMemoryFileSystem,
} from "@rivet-dev/agentos-runtime-browser";
export type {
	BrowserRuntimeDriverFactoryOptions,
	ConvergedSidecarFactoryOptions,
	ConvergedSidecarHandle,
} from "@rivet-dev/agentos-runtime-browser";
export { createBrowserRuntimeDriverFactory } from "@rivet-dev/agentos-runtime-browser";
export type { WorkerHandle } from "@rivet-dev/agentos-runtime-browser";
export { BrowserWorkerAdapter } from "@rivet-dev/agentos-runtime-browser";

// --- Agent OS converged layer: plug the ACP wasm sidecar into the runtime ---
export type { AgentOsConvergedSidecarOptions } from "./converged-sidecar.js";
export { createAgentOsConvergedSidecar } from "./converged-sidecar.js";
export type { ConvergedExecutionHostBridge } from "./converged-execution-host-bridge.js";
export { createConvergedExecutionHostBridge } from "./converged-execution-host-bridge.js";
*/

// Keep this file a module while exposing no browser entrypoint.
export {};
