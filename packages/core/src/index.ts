// @rivet-dev/agentos

export { AgentOs, AgentOsSidecar } from "./agent-os.js";
export {
	isPackageDescriptor,
	OPT_AGENTOS_BIN,
	OPT_AGENTOS_ROOT,
	tryReadAgentosPackageManifest,
} from "./agentos-package.js";
export type { Binding, BindingExample, Bindings } from "./bindings.js";
export {
	binding,
	bindings,
	MAX_BINDING_DESCRIPTION_LENGTH,
	validateBindings,
} from "./bindings.js";
export {
	CronManager,
	InvalidScheduleError,
	PastScheduleError,
	TimerScheduleDriver,
} from "./cron/index.js";
export { createHostDirBackend, nodeModulesMount } from "./host-dir-mount.js";
export type * from "./language-execution.js";
export { createSnapshotExport } from "./layers.js";
export {
	agentOsLimitsSchema,
	agentOsOptionFieldSchemas,
	agentOsOptionsSchema,
	bindingSchema,
	bindingsSchema,
	mountConfigSchema,
	nativeMountConfigSchema,
	parseAgentOsOptions,
	permissionsSchema,
	rootFilesystemConfigSchema,
	sharedSidecarConfigSchema,
	sidecarConfigSchema,
} from "./options-schema.js";
export { defineSoftware } from "./packages.js";
export type {
	ExecOptions,
	ExecResult,
	ManagedProcess,
	ProcessInfo,
	ShellHandle,
	VirtualDirEntry,
	VirtualStat,
} from "./runtime.js";
export { KernelError } from "./runtime-compat.js";
export {
	createSandboxBindings,
	createSandboxFs,
	getSandboxDisposeHooks,
	resolveSandboxOptions,
	SandboxStartupError,
} from "./sandbox.js";
export type * from "./types.js";
