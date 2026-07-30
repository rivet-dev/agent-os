// AGENTOS_BROWSER_SUPPORT_DISABLED: retained for reference, but AgentOS is native-only.
/*
export type {
	BrowserDriverOptions,
	BrowserRuntimeSystemOptions,
} from "./driver.js";
export {
	createBrowserDriver,
	createBrowserNetworkAdapter,
	createOpfsFileSystem,
} from "./driver.js";
export { InMemoryFileSystem } from "./os-filesystem.js";
export type {
	ExecOptions,
	ExecResult,
	NetworkAdapter,
	NodeRuntimeDriver,
	PtyOpenResult,
	StdioChannel,
	StdioEvent,
	TimingMitigation,
} from "./runtime.js";
export {
	allowAll,
	allowAllChildProcess,
	allowAllEnv,
	allowAllFs,
	allowAllNetwork,
	createInMemoryFileSystem,
} from "./runtime.js";
export type { WasiCommandBootstrapOptions } from "./wasi-command-bootstrap.js";
export { createWasiCommandBootstrapScript } from "./wasi-command-bootstrap.js";
export type {
	BrowserRuntimeDriverFactoryOptions,
	ConvergedSidecarFactoryOptions,
	ConvergedSidecarHandle,
} from "./runtime-driver.js";
export { createBrowserRuntimeDriverFactory } from "./runtime-driver.js";
export type { DefaultConvergedSidecarOptions } from "./default-sidecar.js";
export { createDefaultConvergedSidecar } from "./default-sidecar.js";
export type { WorkerHandle } from "./worker-adapter.js";
export { BrowserWorkerAdapter } from "./worker-adapter.js";

// Async-agent executor primitives (AGENTOS-WEB-ASYNC-AGENTS.md): the SAB ring,
// the kernel-worker reactor, and the execution-worker endpoint. Generic,
// model-agnostic; the Agent OS ACP layer composes them.
export {
	SabRing,
	SabRingProtocolError,
	sabRingByteLength,
	sabRingMaxFrameBytes,
} from "./sab-ring.js";
export type { SabRingLayout } from "./sab-ring.js";
export {
	KernelReactor,
	REACTOR_CONTROL_BYTES,
	FRAME_SYSCALL,
	FRAME_STDOUT,
	FRAME_STDERR,
	FRAME_EXIT,
	FRAME_RESULT,
	FRAME_POISON,
	DEFERRED,
	encodeSyscallCompletion,
} from "./sab-reactor.js";
export type { OutputFrame, OutputKind, ServiceSyscall } from "./sab-reactor.js";
export {
	SabExecutionEndpoint,
	ExecutionKilledError,
} from "./sab-execution-endpoint.js";

// The converged guest-syscall handler, reusable to service an agent execution's
// syscalls over the in-worker pushFrame (the kernel reactor's serviceSyscall).
export {
	ConvergedSyncBridgeHandler,
	PushFrameSidecarTransport,
} from "./converged-sync-bridge-handler.js";
export type { ConvergedSidecarRequestTransport } from "./converged-sync-bridge-handler.js";
export type { ConvergedSyncResponse } from "./converged-fs-bridge.js";
*/

// Keep this file a module while exposing no browser entrypoint.
export {};
