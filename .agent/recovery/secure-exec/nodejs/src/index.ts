// Bridge compilation
export { getRawBridgeCode, getBridgeAttachCode } from "./bridge-loader.js";

// Stdlib polyfill bundling
export {
	bundlePolyfill,
	getAvailableStdlib,
	hasPolyfill,
	prebundleAllPolyfills,
} from "./polyfills.js";

// Node execution driver
export { NodeExecutionDriver } from "./execution-driver.js";
export type { NodeExecutionDriverOptions } from "./isolate-bootstrap.js";

// Node system driver
export {
	createDefaultNetworkAdapter,
	createNodeDriver,
	createNodeRuntimeDriverFactory,
	NodeFileSystem,
	filterEnv,
	isPrivateIp,
} from "./driver.js";
export type {
	DefaultNetworkAdapterOptions,
	NodeDriverOptions,
	NodeRuntimeDriverFactoryOptions,
} from "./driver.js";

// Module access filesystem
export { ModuleAccessFileSystem } from "./module-access.js";
export type { ModuleAccessOptions, PackageRootMapping } from "./module-access.js";

// Bridge handlers
export {
	emitConsoleEvent,
	stripDangerousEnv,
	createProcessConfigForExecution,
} from "./bridge-handlers.js";

// Custom bindings
export type { BindingTree, BindingFunction } from "./bindings.js";
export { BINDING_PREFIX, flattenBindingTree } from "./bindings.js";

// Kernel runtime driver (RuntimeDriver for kernel.mount())
export { createNodeRuntime } from "./kernel-runtime.js";
export type { NodeRuntimeOptions } from "./kernel-runtime.js";
export {
	createKernelCommandExecutor,
	createKernelVfsAdapter,
	createHostFallbackVfs,
} from "./kernel-runtime.js";

// OS platform adapters (host filesystem with root, worker threads)
export { HostNodeFileSystem } from "./os-filesystem.js";
export type { HostNodeFileSystemOptions } from "./os-filesystem.js";
export { NodeWorkerAdapter } from "./worker-adapter.js";
export type { WorkerHandle } from "./worker-adapter.js";

// Host command executor (CommandExecutor for standalone NodeRuntime)
export { createNodeHostCommandExecutor } from "./host-command-executor.js";

// Sandbox-native command executor (routes node commands through child V8 isolates)
export { createSandboxCommandExecutor } from "./sandbox-command-executor.js";

// Host network adapter (HostNetworkAdapter for kernel delegation)
export { createNodeHostNetworkAdapter } from "./host-network-adapter.js";

// Timeout utilities (re-exported from core)
export {
	TIMEOUT_EXIT_CODE,
	TIMEOUT_ERROR_MESSAGE,
} from "@secure-exec/core";
