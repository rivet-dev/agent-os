// Register the sidecar spawn factory (side effect). After the
// @rivet-dev/agentos-core SidecarProcess refactor, process spawning is provided
// by a separately registered factory; importing stdio-client wires it up.
import "../stdio-client.js";

export {
	SidecarEventBufferOverflow,
	SidecarProcess,
	SidecarProcessError,
	SidecarProcessExited,
} from "../sidecar-process.js";

export type {
	AuthenticatedSession,
	CreatedVm,
	ExtEnvelope,
	GuestFilesystemStat,
	RootFilesystemEntry,
	RootFilesystemLowerDescriptor,
	SidecarEventSelector,
	SidecarFsPermissionRule,
	SidecarLinkPackageResult,
	SidecarMountDescriptor,
	SidecarMountPluginDescriptor,
	SidecarPatternPermissionRule,
	SidecarPermissionMode,
	SidecarPermissionScope,
	SidecarPermissionsPolicy,
	SidecarProcessSnapshotEntry,
	SidecarProjectedAgent,
	SidecarProjectedModuleDescriptor,
	SidecarRegisteredHostCallbackDefinition,
	SidecarRegisteredHostCallbackExample,
	SidecarRequestFrame,
	SidecarRequestHandler,
	SidecarRequestPayload,
	SidecarResponseFrame,
	SidecarResponsePayload,
	SidecarRulePermissions,
	SidecarSessionState,
	SidecarSignalHandlerRegistration,
	SidecarSignalState,
	SidecarSocketStateEntry,
	SidecarSoftwareDescriptor,
	SidecarSpawnOptions,
	SidecarZombieTimerCount,
} from "../sidecar-process.js";

export type {
	SidecarVmConfiguredResponse as SidecarConfigureVmResult,
} from "../sidecar-process.js";
