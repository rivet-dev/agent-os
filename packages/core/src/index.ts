// @rivet-dev/agent-os

export {
	createInMemoryFileSystem,
	KernelError,
} from "./runtime-compat.js";
export type {
	NetworkAccessRequest,
	OpenShellOptions,
	PermissionDecision,
	PermissionMode,
	Permissions,
	FsPermissionRule,
	PatternPermissionRule,
	RulePermissions,
	FsPermissions,
	NetworkPermissions,
	ChildProcessPermissions,
	EnvPermissions,
	ProcessInfo,
	VirtualDirEntry,
	VirtualFileSystem,
	VirtualStat,
} from "./runtime-compat.js";
export type {
	AgentCapabilities,
	AgentInfo,
	AgentOsOptions,
	AgentRegistryEntry,
	AgentOsSidecarConfig,
	AgentOsCreateSidecarOptions,
	AgentOsSharedSidecarOptions,
	BatchReadResult,
	BatchWriteEntry,
	BatchWriteResult,
	ConnectTerminalOptions,
	CreateSessionOptions,
	DirEntry,
	GetEventsOptions,
	JsonRpcError,
	JsonRpcNotification,
	JsonRpcRequest,
	JsonRpcResponse,
	PermissionReply,
	PermissionRequest,
	PermissionRequestHandler,
	OverlayMountConfig,
	McpServerConfig,
	McpServerConfigLocal,
	McpServerConfigRemote,
	MountConfigJsonObject,
	MountConfigJsonValue,
	MountConfig,
	NativeMountConfig,
	NativeMountPluginDescriptor,
	PlainMountConfig,
	ProcessTreeNode,
	ReaddirRecursiveOptions,
	RootFilesystemConfig,
	RootLowerInput,
	SequencedEvent,
	SessionConfigOption,
	SessionEventHandler,
	SessionInitData,
	SessionMode,
	SessionModeState,
	SessionInfo,
	SpawnedProcessInfo,
} from "./agent-os.js";
export { AgentOs } from "./agent-os.js";
export type { AgentOsSidecarDescription } from "./agent-os.js";
export { AgentOsSidecar } from "./agent-os.js";
export type {
	AgentConfig,
	AgentType,
	PrepareInstructionsOptions,
} from "./agents.js";
export { AGENT_CONFIGS } from "./agents.js";
export type {
	AgentSoftwareDescriptor,
	AnySoftwareDescriptor,
	SoftwareContext,
	SoftwareDescriptor,
	SoftwareInput,
	SoftwareRoot,
	ToolSoftwareDescriptor,
	WasmCommandDirDescriptor,
	WasmCommandSoftwareDescriptor,
} from "./packages.js";
export { defineSoftware } from "./packages.js";
export type { HostDirBackendOptions } from "./host-dir-mount.js";
export { createHostDirBackend } from "./host-dir-mount.js";
export type {
	FilesystemSnapshotExport,
	LayerHandle,
	LayerStore,
	OverlayFilesystemMode,
	RootSnapshotExport,
	SnapshotImportSource,
	SnapshotLayerHandle,
	WritableLayerHandle,
} from "./layers.js";
export {
	createInMemoryLayerStore,
	createSnapshotExport,
} from "./layers.js";
export type {
	CronAction,
	CronEvent,
	CronEventHandler,
	CronJob,
	CronJobInfo,
	CronJobOptions,
	ScheduleDriver,
	ScheduleEntry,
	ScheduleHandle,
} from "./cron/index.js";
export { CronManager, TimerScheduleDriver } from "./cron/index.js";
export type { HostTool, ToolExample, ToolKit } from "./host-tools.js";
export {
	hostTool,
	toolKit,
	validateToolkits,
	MAX_TOOL_DESCRIPTION_LENGTH,
} from "./host-tools.js";
export { getOsInstructions } from "./os-instructions.js";
