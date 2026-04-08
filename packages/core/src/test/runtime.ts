/**
 * Internal test-only runtime exports for cross-package integration suites.
 *
 * This keeps repo-owned tests pointed at an Agent OS package surface even
 * while the public SDK removes the raw vm.kernel escape hatch.
 */

export {
	type AgentOsRuntimeAdmin,
	getAgentOsKernel,
	getAgentOsRuntimeAdmin,
} from "../agent-os.js";
export type {
	PermissionTier,
	WasmVmRuntimeOptions,
} from "../runtime.js";
export type {
	DriverProcess,
	Kernel,
	KernelInterface,
	KernelRuntimeDriver,
	ProcessContext,
	VirtualFileSystem,
} from "../runtime-compat.js";
export {
	AF_INET,
	AF_UNIX,
	allowAll,
	createInMemoryFileSystem,
	createKernel,
	createNodeHostNetworkAdapter,
	createNodeRuntime,
	createWasmVmRuntime,
	DEFAULT_FIRST_PARTY_TIERS,
	NodeFileSystem,
	SIGTERM,
	SOCK_DGRAM,
	SOCK_STREAM,
	WASMVM_COMMANDS,
} from "../runtime-compat.js";
export { TerminalHarness } from "./terminal-harness.js";
