import { execFileSync, spawn as spawnChildProcess } from "node:child_process";
import { randomUUID } from "node:crypto";
import { existsSync, mkdirSync, readdirSync, statSync } from "node:fs";
import {
	join,
	posix as posixPath,
	resolve as resolveHostPath,
} from "node:path";
import { fileURLToPath } from "node:url";
import type {
	MountConfigJsonObject,
	MountConfigJsonValue,
	NativeMountPluginDescriptor,
} from "@rivet-dev/agentos-runtime-core/descriptors";
import * as executionProtocol from "@rivet-dev/agentos-runtime-core/protocol";
import { SidecarRejectedError } from "@rivet-dev/agentos-runtime-core/sidecar-errors";
import type {
	CreateVmConfig,
	VmUserConfig,
} from "@rivet-dev/agentos-runtime-core/vm-config";
import { type Binding, type Bindings, validateBindings } from "./bindings.js";
import { zodToJsonSchema } from "./bindings-zod.js";
import type {
	JsonRpcNotification,
	JsonRpcRequest,
	JsonRpcResponse,
} from "./json-rpc.js";
import type {
	CodeEvaluationResult,
	CodeExecutionResult,
	ContextDescriptor,
	ExecutionSignal,
	InlineExecutionOptions,
	JavaScriptEvaluationOptions,
	JavaScriptExecutionOptions,
	LanguageExecutionOptions,
	LanguageSpawnOptions,
	NpmPackageInstallOptions,
	NpmProjectInstallOptions,
	OutputReplay,
	ProcessDescriptor,
	ProcessExit,
	ProcessOutputEvent,
	PythonInstallOptions,
	SpawnOptions,
	TypeScriptCheckOptions,
	TypeScriptCheckResult,
	TypeScriptDiagnostic,
	TypeScriptEvaluationOptions,
	TypeScriptExecutionOptions,
	TypeScriptFileExecutionOptions,
} from "./language-execution.js";
import { parseAgentOsOptions } from "./options-schema.js";
import type {
	ConnectTerminalOptions,
	Kernel,
	KernelExecOptions,
	KernelExecResult,
	ProcessInfo as KernelProcessInfo,
	KernelSpawnOptions,
	ManagedProcess,
	OpenShellOptions,
	Permissions,
	ShellHandle,
	VirtualFileSystem,
	VirtualStat,
} from "./runtime-compat.js";
import {
	type AgentOsSandboxInput,
	getSandboxDisposeHooks,
	resolveSandboxOptions,
} from "./sandbox.js";
import type {
	AcpSessionEvent,
	CancelPromptResult,
	PromptResult as DurablePromptResult,
	DurableSessionEventEntry,
	SessionInfo as DurableSessionInfo,
	EphemeralSessionEventEntry,
	HistoryPage,
	JsonValue,
	ListSessionsInput,
	OpenSessionInput,
	PermissionResponse,
	PermissionResponseResult,
	PermissionTerminalReason,
	PromptInput,
	ReadHistoryInput,
	SessionAgentInfo,
	SessionCapabilities,
	SessionConfig,
	SessionPage,
	SessionStreamEntry,
	SessionTarget,
	SetSessionConfigOptionInput,
} from "./session-api.js";
import { resolvePublishedSidecarBinary } from "./sidecar/binary.js";
import { findCargoBinary, resolveCargoBinary } from "./sidecar/cargo.js";

export type {
	MountConfigJsonObject,
	MountConfigJsonPrimitive,
	MountConfigJsonValue,
	NativeMountPluginDescriptor,
} from "@rivet-dev/agentos-runtime-core/descriptors";
export type { ConnectTerminalOptions } from "./runtime-compat.js";
export type * from "./session-api.js";

const ACP_PROTOCOL_VERSION = 1;
const ACP_EXTENSION_NAMESPACE = "dev.rivet.agent-os.acp";
const SHELL_DISPOSE_TIMEOUT_MS = 5_000;
const PROCESS_OUTPUT_EVENT_LIMIT = 1_024;

function safeWireU64(value: bigint): number {
	const result = Number(value);
	if (!Number.isSafeInteger(result) || result < 0) {
		throw new RangeError(
			`wire integer ${value} exceeds JavaScript's safe range`,
		);
	}
	return result;
}

function decodeDurableSessionInfo(
	value: AcpDurableSessionInfo,
): DurableSessionInfo {
	return {
		sessionId: value.sessionId,
		agent: value.agent,
		cwd: value.cwd,
		additionalDirectories: JSON.parse(value.additionalDirectories),
		state: JSON.parse(value.state),
		latestSequence: safeWireU64(value.latestSequence),
		title: value.title ?? undefined,
		metadata: value.metadata === null ? undefined : JSON.parse(value.metadata),
		createdAt: value.createdAt,
		updatedAt: value.updatedAt,
	};
}

const PERMISSION_TERMINAL_REASONS = new Set<PermissionTerminalReason>([
	"already_resolved",
	"prompt_cancelled",
	"adapter_exited",
	"session_deleted",
	"vm_shutdown",
	"request_not_found",
]);

function permissionTerminalReason(
	value: string | null,
): PermissionTerminalReason {
	if (
		value !== null &&
		PERMISSION_TERMINAL_REASONS.has(value as PermissionTerminalReason)
	) {
		return value as PermissionTerminalReason;
	}
	throw new Error(`invalid permission terminal reason: ${value ?? "missing"}`);
}

function decodeDurableSessionEvent(value: {
	sessionId: string;
	sequence: bigint;
	timestamp: string;
	event: AcpDurableEvent;
}): DurableSessionEventEntry {
	const envelope = {
		durability: "durable" as const,
		sessionId: value.sessionId,
		sequence: safeWireU64(value.sequence),
		timestamp: value.timestamp,
	};
	switch (value.event.tag) {
		case "AcpDurableSessionUpdate": {
			const update = JSON.parse(value.event.val.update) as {
				sessionUpdate: AcpSessionEvent["type"];
			} & Record<string, unknown>;
			const { sessionUpdate: type, ...payload } = update;
			return {
				...envelope,
				type,
				...payload,
			} as DurableSessionEventEntry;
		}
		case "AcpDurablePermissionRequest": {
			const request = JSON.parse(value.event.val.request) as {
				sessionId: string;
			} & Record<string, unknown>;
			const { sessionId: _sessionId, ...payload } = request;
			return {
				...envelope,
				type: "permission_request",
				requestId: value.event.val.requestId,
				...payload,
			} as DurableSessionEventEntry;
		}
		case "AcpDurablePermissionResponse": {
			const status = value.event.val.status;
			if (status !== "accepted" && status !== "not_pending") {
				throw new Error(`invalid permission response event status: ${status}`);
			}
			const response = JSON.parse(value.event.val.response) as Record<
				string,
				unknown
			>;
			return {
				...envelope,
				type: "permission_response",
				requestId: value.event.val.requestId,
				...response,
				status,
				...(value.event.val.reason === null
					? {}
					: { reason: permissionTerminalReason(value.event.val.reason) }),
			} as DurableSessionEventEntry;
		}
	}
}

function normalizeSessionCapabilities(value: unknown): SessionCapabilities {
	const capabilities = toRecord(value);
	const prompt = toRecord(capabilities.promptCapabilities);
	const mcp = toRecord(capabilities.mcpCapabilities);
	const session = toRecord(capabilities.sessionCapabilities);
	const extensions = Object.fromEntries(
		Object.entries(capabilities).filter(
			([key]) =>
				!(
					[
						"loadSession",
						"promptCapabilities",
						"mcpCapabilities",
						"sessionCapabilities",
					] as const
				).includes(key as never),
		),
	) as Record<string, JsonValue>;
	return {
		protocolVersion: ACP_PROTOCOL_VERSION,
		loadSession: capabilities.loadSession === true,
		...(Object.keys(prompt).length > 0
			? {
					prompt: {
						audio: prompt.audio === true || undefined,
						embeddedContext: prompt.embeddedContext === true || undefined,
						image: prompt.image === true || undefined,
					},
				}
			: {}),
		...(Object.keys(mcp).length > 0
			? {
					mcp: {
						http: mcp.http === true || undefined,
						sse: mcp.sse === true || undefined,
					},
				}
			: {}),
		...(Object.keys(session).length > 0
			? {
					session: {
						list: typeof session.list === "object" || undefined,
						resume: typeof session.resume === "object" || undefined,
						close: typeof session.close === "object" || undefined,
						delete: typeof session.delete === "object" || undefined,
						additionalDirectories:
							typeof session.additionalDirectories === "object" || undefined,
					},
				}
			: {}),
		...(Object.keys(extensions).length > 0 ? { extensions } : {}),
	};
}

async function waitForTrackedExitPromises(
	promises: Promise<unknown>[],
	timeoutMs: number,
): Promise<void> {
	if (promises.length === 0) {
		return;
	}
	await Promise.race([
		Promise.allSettled(promises).then(() => undefined),
		new Promise<void>((resolve) => {
			setTimeout(resolve, timeoutMs);
		}),
	]);
}

/** Process tree node using the same descriptor shape as process.get/list. */
export interface ProcessTreeNode extends ProcessDescriptor {
	ppid?: number;
	children: ProcessTreeNode[];
}

/** A directory entry with metadata. */
export interface DirEntry {
	/** Absolute path to the entry. */
	path: string;
	type: "file" | "directory" | "symlink";
	size: number;
}

/** One immediate child returned by a single readdirEntries operation. */
export interface ReaddirEntry {
	name: string;
	isDirectory: boolean;
	isSymbolicLink: boolean;
}

/** Fully buffered request to an HTTP service listening inside the VM. */
export interface HttpRequest {
	port: number;
	path: string;
	method?: string;
	headers?: Record<string, string>;
	body?: string | Uint8Array;
}

/** Fully buffered HTTP response from a service inside the VM. */
export interface HttpResponse {
	status: number;
	statusText: string;
	headers: Record<string, string>;
	body: Uint8Array;
}

function headersToRecord(headers: Headers): Record<string, string> {
	const result: Record<string, string> = {};
	headers.forEach((value, name) => {
		result[name] = value;
	});
	return result;
}

export interface ProcessOutput {
	pid: number;
	stream: "stdout" | "stderr";
	data: Uint8Array;
}

export interface ShellData {
	shellId: string;
	data: Uint8Array;
}

export interface ShellExit {
	shellId: string;
	exitCode: number;
}

/** Sanitized live mount metadata. Plugin configuration is never exposed. */
export interface MountInfo {
	path: string;
	kind: string;
	readOnly: boolean;
}

export interface ExportRootFilesystemOptions {
	/** Maximum serialized snapshot bytes returned to this caller. */
	maxBytes: number;
}

/** Portable, sidecar-owned dynamic mount descriptor. */
export interface DynamicMountDescriptor {
	path: string;
	plugin: NativeMountPluginDescriptor;
	readOnly?: boolean;
}

/** Callback-free options accepted by the portable openShell API. */
export type ShellOptions = Omit<OpenShellOptions, "onStderr">;

/** Options for readdirRecursive(). */
export interface ReaddirRecursiveOptions {
	/** Maximum depth to recurse (0 = only immediate children). */
	maxDepth?: number;
	/** Directory names to skip. */
	exclude?: string[];
}

/** Entry for batch write operations. */
export interface BatchWriteEntry {
	path: string;
	content: string | Uint8Array;
}

/** Result of a single file in a batch write. */
export interface BatchWriteResult {
	path: string;
	success: boolean;
	error?: string;
}

/** Result of a single file in a batch read. */
export interface BatchReadResult {
	path: string;
	content: Uint8Array | null;
	error?: string;
}

/** Entry in the agent registry, describing an available agent type. */
export interface AgentRegistryEntry {
	id: string;
	installed: boolean;
}

import {
	OPT_AGENTOS_ROOT,
	type PackageDescriptor,
	tryReadAgentosPackageManifest,
} from "./agentos-package.js";
import { getBaseEnvironment } from "./base-filesystem.js";
import { CronManager } from "./cron/cron-manager.js";
import type { ScheduleDriver } from "./cron/schedule-driver.js";
import { TimerScheduleDriver } from "./cron/timer-driver.js";
import type {
	CronEvent,
	CronEventHandler,
	CronJob,
	CronJobInfo,
	CronJobOptions,
} from "./cron/types.js";
import { resolveDefaultSoftware } from "./default-software.js";
import {
	type FilesystemEntry,
	snapshotVirtualFilesystem,
	sortFilesystemEntries,
} from "./filesystem-snapshot.js";
import { createHostDirBackend } from "./host-dir-mount.js";
import {
	type LocalCompatMount,
	serializeMountConfigForSidecar,
} from "./js-bridge.js";
import {
	createSnapshotExport,
	type LayerStore,
	type OverlayFilesystemMode,
	type RootSnapshotExport,
	type SnapshotLayerHandle,
} from "./layers.js";
import type { SoftwareInput, SoftwareRoot } from "./packages.js";
import type { PermissionTier } from "./runtime.js";
import { allowAll, createNodeHostNetworkAdapter } from "./runtime-compat.js";
import {
	type AcpDurableEvent,
	type AcpDurableSessionInfo,
	type AcpRequest,
	type AcpResponse,
	AcpRuntimeKind,
	decodeAcpCallback,
	decodeAcpEvent,
	decodeAcpResponse,
	encodeAcpCallbackResponse,
	encodeAcpRequest,
} from "./sidecar/agentos-protocol.js";
import { serializePermissionsForSidecar } from "./sidecar/permissions.js";
import {
	type AgentOsSidecarClient,
	type AgentOsSidecarPlacement,
	type AgentOsSidecarSessionBootstrap,
	type AgentOsSidecarSessionHandle,
	type AgentOsSidecarTransport,
	type AgentOsSidecarVmBootstrap,
	type AgentOsSidecarVmHandle,
	type AuthenticatedSession,
	type CreatedVm,
	createAgentOsSidecarClient,
	NativeSidecarKernelProxy,
	type RootFilesystemEntry,
	type SidecarMountDescriptor,
	type SidecarPermissionsPolicy,
	SidecarProcess,
	type SidecarRegisteredHostCallbackDefinition,
	type SidecarRequestFrame,
	type SidecarResponsePayload,
	serializeRootFilesystemForSidecar,
} from "./sidecar/rpc-client.js";

export interface AgentOsSharedSidecarOptions {
	pool?: string;
	runtime?: AgentOsSidecarRuntimeConfig;
}

export interface AgentOsCreateSidecarOptions {
	sidecarId?: string;
	runtime?: AgentOsSidecarRuntimeConfig;
}

/** Process-wide runtime settings applied when the native sidecar starts. */
export interface AgentOsSidecarRuntimeConfig {
	executor?: {
		/** Optional ceiling for concurrent V8 executors. Omit to leave admission uncapped. */
		maxActiveVms?: number;
	};
}

export type AgentOsSidecarConfig =
	| {
			kind: "shared";
			pool?: string;
			runtime?: AgentOsSidecarRuntimeConfig;
	  }
	| { kind: "explicit"; handle: AgentOsSidecar };

export interface AgentOsSidecarDescription {
	sidecarId: string;
	placement: AgentOsSidecarPlacement;
	state: "ready" | "disposing" | "disposed";
	activeVmCount: number;
}

interface InProcessSidecarVmAdmin {
	dispose(): Promise<void>;
}

interface AgentOsSidecarVmLease<TVmAdmin extends InProcessSidecarVmAdmin> {
	sidecar: AgentOsSidecar;
	session: AgentOsSidecarSessionHandle;
	vm: AgentOsSidecarVmHandle;
	admin: TVmAdmin;
	dispose(): Promise<void>;
}

interface HostMountInfo {
	vmPath: string;
	hostPath: string;
	readOnly: boolean;
}

interface AgentOsVmAdmin extends InProcessSidecarVmAdmin {
	kernel: Kernel;
	rootView: VirtualFileSystem;
	hostMounts: HostMountInfo[];
	env: Record<string, string>;
	permissions: Permissions;
	sidecarMounts: SidecarMountDescriptor[];
	sidecarPermissions: SidecarPermissionsPolicy | undefined;
	commandPermissions: Record<string, PermissionTier>;
	loopbackExemptPorts: number[] | undefined;
	sidecarClient: SidecarProcess;
	sidecarSession: AuthenticatedSession;
	sidecarVm: CreatedVm;
	snapshotRootFilesystem?: (maxBytes: number) => Promise<RootSnapshotExport>;
	bindings: Bindings[];
	bindingReference: string;
}

interface AcpTerminalEntry {
	handle: ShellHandle;
	output: string;
	truncated: boolean;
	outputByteLimit: number;
	exitCode: number | null;
	waitPromise: Promise<number>;
}

interface ShellEntry {
	handle: ShellHandle;
	dataHandlers: Set<(event: ShellData) => void>;
	stderrHandlers: Set<(event: ShellData) => void>;
	exitHandlers: Set<(event: ShellExit) => void>;
	exitPromise: Promise<number>;
}

export type RootLowerInput =
	| { kind: "bundled-base-filesystem" }
	| RootSnapshotExport;

export interface OverlayRootFilesystemConfig {
	type?: "overlay";
	mode?: OverlayFilesystemMode;
	disableDefaultBaseLayer?: boolean;
	lowers?: RootLowerInput[];
}

/** Declarative sidecar-native root filesystem. */
export interface NativeRootFilesystemConfig {
	type: "native";
	plugin: NativeMountPluginDescriptor;
	readOnly?: boolean;
}

export type RootFilesystemConfig =
	| OverlayRootFilesystemConfig
	| NativeRootFilesystemConfig;

/** VM-scoped SQLite storage shared by VFS and AgentOS durable state. */
export type VmSqliteConfig =
	| { type: "actor_uds"; path: string }
	| { type: "sqlite_file"; path: string };

/**
 * Compatibility path for arbitrary caller-supplied filesystems.
 * This maps to the sidecar `js_bridge` plugin during the migration.
 */
export interface PlainMountConfig {
	/** Path inside the VM to mount at. */
	path: string;
	/** The filesystem driver to mount. */
	driver: VirtualFileSystem;
	/** Filesystem type exposed through guest mount discovery. */
	guestFstype?: string;
	/** Source name exposed through guest mount discovery. */
	guestSource?: string;
	/** If true, write operations throw EROFS. */
	readOnly?: boolean;
}

/** Declarative native mount configuration that the sidecar can serialize. */
export interface NativeMountConfig {
	path: string;
	plugin: NativeMountPluginDescriptor;
	/** Filesystem type exposed through guest mount discovery. */
	guestFstype?: string;
	/** Source name exposed through guest mount discovery. */
	guestSource?: string;
	readOnly?: boolean;
}

export interface OverlayMountConfig {
	path: string;
	filesystem: {
		type: "overlay";
		store: LayerStore;
		mode?: OverlayFilesystemMode;
		lowers: SnapshotLayerHandle[];
	};
}

export type MountConfig =
	| PlainMountConfig
	| NativeMountConfig
	| OverlayMountConfig;

/**
 * Operator-tunable runtime limits for a VM. Every field is optional; unset fields fall back to
 * built-in defaults that match the runtime's historical hardcoded constants, so behavior is
 * unchanged unless a value is overridden. All values are JSON-serializable integers and are
 * forwarded to the native sidecar in the typed create-VM JSON config. Unknown, negative, or
 * non-integer values are rejected by the sidecar before VM construction.
 */
export interface AgentOsLimits {
	/** Kernel resource limits (processes, FDs, sockets, filesystem bytes, WASM caps, etc.). */
	resources?: {
		cpuCount?: number;
		maxProcesses?: number;
		maxOpenFds?: number;
		maxPipes?: number;
		maxPtys?: number;
		maxSockets?: number;
		maxConnections?: number;
		maxSocketBufferedBytes?: number;
		maxSocketDatagramQueueLen?: number;
		maxFilesystemBytes?: number;
		maxInodeCount?: number;
		maxBlockingReadMs?: number;
		maxPreadBytes?: number;
		maxFdWriteBytes?: number;
		maxProcessArgvBytes?: number;
		maxProcessEnvBytes?: number;
		maxReaddirEntries?: number;
		maxWasmFuel?: number;
		maxWasmMemoryBytes?: number;
		maxWasmStackBytes?: number;
	};
	/** HTTP body buffering limits. */
	http?: {
		/** Cap on `vm.httpRequest()` buffered response bodies. Must be <= the sidecar wire frame cap. */
		maxFetchResponseBytes?: number;
	};
	/** TLS plaintext buffering limits. */
	tls?: {
		maxBufferedBytes?: number;
	};
	/** Host binding registration and invocation limits. */
	bindings?: {
		defaultBindingTimeoutMs?: number;
		maxBindingTimeoutMs?: number;
		maxRegisteredCollections?: number;
		maxRegisteredBindingsPerVm?: number;
		maxBindingsPerCollection?: number;
		maxBindingSchemaBytes?: number;
		maxExamplesPerBinding?: number;
		maxBindingExampleInputBytes?: number;
	};
	/** Mount plugin manifest size limits. */
	plugins?: {
		maxPersistedManifestBytes?: number;
		maxPersistedManifestFileBytes?: number;
	};
	/** ACP adapter, active-turn, history-retention, and page limits. */
	acp?: {
		maxReadLineBytes?: number;
		stdoutBufferByteLimit?: number;
		maxCompletedMessageBytes?: number;
		maxTurnOutputBytes?: number;
		maxPromptBytes?: number;
		maxPromptBlocks?: number;
		maxFallbackContinuationBytes?: number;
		maxSessionHistoryBytes?: number;
		maxSessionHistoryEvents?: number;
		maxHistoryPageEntries?: number;
		maxSessionListEntries?: number;
	};
	/** Shared SQLite and actor-UDS request limits. */
	sqlite?: {
		maxResultBytes?: number;
		maxInFlightRequests?: number;
		maxQueuedRequestBytes?: number;
	};
	/** Guest JavaScript runtime buffering limits. */
	jsRuntime?: {
		v8HeapLimitMb?: number;
		syncRpcWaitTimeoutMs?: number;
		cpuTimeLimitMs?: number;
		wallClockLimitMs?: number;
		importCacheMaterializeTimeoutMs?: number;
		capturedOutputLimitBytes?: number;
		stdinBufferLimitBytes?: number;
		eventPayloadLimitBytes?: number;
		v8IpcMaxFrameBytes?: number;
	};
	/** Guest Python runtime limits. */
	python?: {
		outputBufferMaxBytes?: number;
		executionTimeoutMs?: number;
		maxOldSpaceMb?: number;
		vfsRpcTimeoutMs?: number;
	};
	/** Guest WASM runtime limits. */
	wasm?: {
		maxModuleFileBytes?: number;
		capturedOutputLimitBytes?: number;
		syncReadLimitBytes?: number;
		prewarmTimeoutMs?: number;
		runnerHeapLimitMb?: number;
		runnerCpuTimeLimitMs?: number;
	};
	/** Process spawn, I/O, and lifecycle-event backlog limits. */
	process?: {
		maxSpawnFileActions?: number;
		maxSpawnFileActionBytes?: number;
		pendingStdinBytes?: number;
		pendingEventCount?: number;
		pendingEventBytes?: number;
	};
}

export interface AgentStderrEvent {
	sessionId: string;
	agentType: string;
	processId: string;
	pid: number | null;
	chunk: Uint8Array;
}

export type AgentStderrHandler = (event: AgentStderrEvent) => void;

function defaultAgentStderrHandler(event: AgentStderrEvent): void {
	process.stderr.write(event.chunk);
}

/**
 * Restart disposition reported on an {@link AgentExitEvent}. AgentOS never
 * respawns an adapter or replays an interrupted request implicitly.
 */
export type AgentRestartOutcome = "not_attempted";

/**
 * An unexpected ACP adapter process exit — a crash from the host's
 * perspective (any spontaneous exit before `unloadSession()`, including exit
 * code 0). The live route is evicted and must be restored explicitly.
 */
export interface AgentExitEvent {
	sessionId: string;
	agentType: string;
	/** Sidecar process id of the adapter that exited. */
	processId: string;
	pid: number | null;
	/** Adapter exit code; `null` when the exit was observed indirectly. */
	exitCode: number | null;
	/** Always `"not_attempted"`; AgentOS does not restart adapters implicitly. */
	restart: AgentRestartOutcome;
	/** Always zero. */
	restartCount: number;
	/** Always zero. */
	maxRestarts: number;
}

export type AgentExitHandler = (event: AgentExitEvent) => void;

function defaultAgentExitHandler(event: AgentExitEvent): void {
	process.stderr.write(
		`[agentos] agent adapter exited unexpectedly: session=${event.sessionId} agent=${event.agentType} exitCode=${event.exitCode ?? "unknown"}; restore explicitly before retrying\n`,
	);
}

/**
 * A near-capacity warning for one bounded limit (a queue/buffer, a saturating
 * resource cap, or a memory envelope) inside the VM runtime. Delivered the moment
 * usage crosses the runtime's warning threshold (~80%), once per crossing — the
 * runtime applies edge-triggering + hysteresis, so this never spams.
 */
export interface LimitWarning {
	/** Stable limit name, e.g. `"javascript_event_channel"` or `"vm_open_fds"`. */
	limit: string;
	/** Limit class: `"queue"`, `"resource"`, or `"memory"`. */
	category: string;
	/** Current observed usage. */
	observed: number;
	/** Configured capacity. */
	capacity: number;
	/** Observed fill as a percentage of capacity (0–100). */
	fillPercent: number;
}

export type LimitWarningHandler = (warning: LimitWarning) => void;

/**
 * Public core VM options.
 *
 * Keep this interface in sync with
 * `packages/core/src/options-schema.ts::agentOsOptionsSchema`. The TypeScript
 * Rivet actor accepts this surface directly alongside ordinary actor options.
 */
export interface AgentOsOptions {
	/** Initial virtual Linux credentials and account record. Defaults to `1000:1000` (`agentos`). */
	user?: VmUserConfig;
	/**
	 * Software to install in the VM. Each entry is a package-dir ref. Arrays are
	 * flattened, so meta-packages that export arrays of sub-packages work directly.
	 */
	software?: SoftwareInput[];
	/**
	 * Whether to auto-include the default software bundle (`@agentos-software/common`
	 * — `sh` + coreutils + the standard CLI tools agents rely on) in addition to
	 * any `software` you pass. Defaults to `true`; set `false` for a bare VM with
	 * only the software you list explicitly. Entries already present in `software`
	 * are not duplicated.
	 */
	defaultSoftware?: boolean;
	/** Loopback ports to exempt from SSRF checks (for testing with host-side mock servers). */
	loopbackExemptPorts?: number[];
	/**
	 * Allowed Node.js builtins for guest Node processes.
	 * Defaults to the hardened builtin set used by the native sidecar bridge.
	 */
	allowedNodeBuiltins?: string[];
	/**
	 * Opt in to a high-resolution monotonic guest clock (microsecond class)
	 * for guest Node processes. Default `false` keeps the security-oriented
	 * 1ms timer resolution — untrusted guest code should not get a precise
	 * timer (timing side channels). Enable only for trusted benchmarking or
	 * profiling workloads.
	 */
	highResolutionTime?: boolean;
	/** Durable SQLite storage for VM-owned filesystem and session state. */
	database?: VmSqliteConfig;
	/** Root filesystem configuration. Defaults to an overlay with the bundled base snapshot as its deepest lower. */
	rootFilesystem?: RootFilesystemConfig;
	/** Filesystems to mount at boot time. */
	mounts?: MountConfig[];
	/** External sandbox mounted into this VM with process bindings. */
	sandbox?: AgentOsSandboxInput;
	/** Custom schedule driver for cron jobs. Defaults to TimerScheduleDriver. */
	scheduleDriver?: ScheduleDriver;
	/** Host-side bindings available to agents inside the VM. */
	bindings?: Bindings[];
	/**
	 * Custom permission policy for the kernel. Controls access to filesystem,
	 * network, child process, and environment operations. Defaults to allowAll.
	 */
	permissions?: Permissions;
	/**
	 * Sidecar placement for the VM. Defaults to the shared `default` pool.
	 * Pass an explicit sidecar handle to pin the VM to a caller-managed sidecar.
	 */
	sidecar?: AgentOsSidecarConfig;
	/**
	 * Operator-tunable runtime limits. Unset fields use built-in defaults that match the
	 * runtime's historical constants, so omitting this leaves behavior unchanged.
	 */
	limits?: AgentOsLimits;
	/**
	 * Called with stderr chunks from the top-level ACP-speaking agent process.
	 * The agent process uses stdout for ACP JSON-RPC protocol traffic, so only
	 * stderr is forwarded through this hook. Defaults to writing chunks to
	 * `process.stderr`.
	 */
	onAgentStderr?: AgentStderrHandler;
	/**
	 * Called when the ACP adapter process behind a session exits unexpectedly.
	 * The sidecar evicts the live
	 * route and never retries the adapter or interrupted request implicitly.
	 * Defaults to writing a warning line to `process.stderr`.
	 */
	onAgentExit?: AgentExitHandler;
	/**
	 * Called when a bounded limit inside the VM runtime approaches capacity
	 * (~80%, edge-triggered with hysteresis so it does not spam). Use it to alert
	 * on a slow consumer or a runaway guest before the limit is actually hit.
	 */
	onLimitWarning?: LimitWarningHandler;
}

export interface AgentOsRuntimeAdmin {
	kernel: Kernel;
	rootView: VirtualFileSystem;
	env: Record<string, string>;
	sidecar: AgentOsSidecar;
}

class AcpDispatchError extends Error {
	readonly code: number;
	readonly data?: Record<string, unknown>;

	constructor(code: number, message: string, data?: Record<string, unknown>) {
		super(message);
		this.name = "AcpDispatchError";
		this.code = code;
		this.data = data;
	}
}

function toJsonRpcNotification(value: unknown): JsonRpcNotification {
	if (
		!value ||
		typeof value !== "object" ||
		Array.isArray(value) ||
		(value as { jsonrpc?: unknown }).jsonrpc !== "2.0" ||
		typeof (value as { method?: unknown }).method !== "string"
	) {
		throw new Error("Invalid JSON-RPC notification from sidecar");
	}
	return value as JsonRpcNotification;
}

function toJsonRpcResponse(value: unknown): JsonRpcResponse {
	if (
		!value ||
		typeof value !== "object" ||
		Array.isArray(value) ||
		(value as { jsonrpc?: unknown }).jsonrpc !== "2.0" ||
		!(
			typeof (value as { id?: unknown }).id === "number" ||
			typeof (value as { id?: unknown }).id === "string" ||
			(value as { id?: unknown }).id === null
		)
	) {
		throw new Error("Invalid JSON-RPC response from sidecar");
	}
	return value as JsonRpcResponse;
}

function toJsonRpcRequest(value: unknown): JsonRpcRequest {
	if (
		!value ||
		typeof value !== "object" ||
		Array.isArray(value) ||
		(value as { jsonrpc?: unknown }).jsonrpc !== "2.0" ||
		!(
			typeof (value as { id?: unknown }).id === "number" ||
			typeof (value as { id?: unknown }).id === "string" ||
			(value as { id?: unknown }).id === null
		) ||
		typeof (value as { method?: unknown }).method !== "string"
	) {
		throw new Error("Invalid JSON-RPC request from ACP callback");
	}
	return value as JsonRpcRequest;
}

function toRecord(value: unknown): Record<string, unknown> {
	return value && typeof value === "object" && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: {};
}

interface NormalizedPackageRef {
	path: string;
}

function normalizePackageRef(value: unknown): NormalizedPackageRef | undefined {
	// The single package reference is `packagePath`: the packed `.aospkg` file
	// (registry-built packages export `{ packagePath }`), or a package dir for
	// local transition fixtures. A raw string is shorthand for the same path.
	if (typeof value === "string") {
		return { path: value };
	}
	const record = toRecord(value);
	if (typeof record.packagePath === "string") {
		return { path: record.packagePath };
	}
	// Recognizably-legacy shapes fail loudly: silently dropping a software
	// entry boots a VM with missing packages and no diagnostic.
	for (const legacy of ["packageTar", "packageDir", "dir"]) {
		if (typeof record[legacy] === "string") {
			throw new Error(
				`agentOS package ref uses removed field "${legacy}" (value: ${JSON.stringify(record[legacy])}); ` +
					"packages are referenced by a single `packagePath` — update the package " +
					"(rebuild @agentos-software/* dependencies) or pass { packagePath }",
			);
		}
	}
	return undefined;
}

const CLOSED_SHELL_ID_RETENTION_LIMIT = 2048;

class BoundedSet<T, V = undefined> {
	readonly limit: number;
	#entries = new Map<T, V | undefined>();

	constructor(limit: number) {
		if (!Number.isInteger(limit) || limit <= 0) {
			throw new Error(`BoundedSet limit must be a positive integer: ${limit}`);
		}
		this.limit = limit;
	}

	add(value: T, associated?: V): void {
		if (this.#entries.has(value)) {
			this.#entries.delete(value);
		}
		this.#entries.set(value, associated);
		if (this.#entries.size <= this.limit) {
			return;
		}
		const oldest = this.#entries.keys().next();
		if (!oldest.done) {
			this.#entries.delete(oldest.value);
		}
	}

	has(value: T): boolean {
		return this.#entries.has(value);
	}

	get(value: T): V | undefined {
		return this.#entries.get(value);
	}

	delete(value: T): boolean {
		return this.#entries.delete(value);
	}

	get size(): number {
		return this.#entries.size;
	}
}

function isOverlayMountConfig(
	config: MountConfig,
): config is OverlayMountConfig {
	return "filesystem" in config;
}

function isNativeMountConfig(config: MountConfig): config is NativeMountConfig {
	return "plugin" in config;
}

interface HostDirMountPluginConfig {
	hostPath: string;
	readOnly?: boolean;
}

interface SandboxAgentMountPluginConfig {
	baseUrl: string;
	token?: string;
	headers?: Record<string, string>;
	basePath?: string;
	timeoutMs?: number;
	maxFullReadBytes?: number;
}

interface S3MountPluginCredentials {
	accessKeyId: string;
	secretAccessKey: string;
}

interface GoogleDriveMountPluginCredentials {
	clientEmail: string;
	privateKey: string;
}

interface S3MountPluginConfig {
	bucket: string;
	prefix?: string;
	region?: string;
	credentials?: S3MountPluginCredentials;
	endpoint?: string;
	chunkSize?: number;
	inlineThreshold?: number;
}

interface GoogleDriveMountPluginConfig {
	credentials: GoogleDriveMountPluginCredentials;
	folderId: string;
	keyPrefix?: string;
	chunkSize?: number;
	inlineThreshold?: number;
}

function asMountConfigJsonObject(
	value: MountConfigJsonValue | undefined,
): MountConfigJsonObject {
	if (value && typeof value === "object" && !Array.isArray(value)) {
		return value as MountConfigJsonObject;
	}
	return {};
}

function getHostDirMountPluginConfig(
	config: MountConfigJsonValue | undefined,
): HostDirMountPluginConfig | null {
	const object = asMountConfigJsonObject(config);
	if (typeof object.hostPath !== "string") {
		return null;
	}

	const hostPathConfig: HostDirMountPluginConfig = {
		hostPath: object.hostPath,
	};
	if (typeof object.readOnly === "boolean") {
		hostPathConfig.readOnly = object.readOnly;
	}
	return hostPathConfig;
}

function getSandboxAgentMountPluginConfig(
	config: MountConfigJsonValue | undefined,
): SandboxAgentMountPluginConfig | null {
	const object = asMountConfigJsonObject(config);
	if (typeof object.baseUrl !== "string") {
		return null;
	}

	const sandboxConfig: SandboxAgentMountPluginConfig = {
		baseUrl: object.baseUrl,
	};
	if (typeof object.token === "string") {
		sandboxConfig.token = object.token;
	}
	if (typeof object.basePath === "string") {
		sandboxConfig.basePath = object.basePath;
	}
	if (typeof object.timeoutMs === "number") {
		sandboxConfig.timeoutMs = object.timeoutMs;
	}
	if (typeof object.maxFullReadBytes === "number") {
		sandboxConfig.maxFullReadBytes = object.maxFullReadBytes;
	}
	if (
		object.headers &&
		typeof object.headers === "object" &&
		!Array.isArray(object.headers)
	) {
		const headers = Object.entries(object.headers)
			.filter(([, value]) => typeof value === "string")
			.map(([name, value]) => [name, value as string]);
		if (headers.length > 0) {
			sandboxConfig.headers = Object.fromEntries(headers);
		}
	}

	return sandboxConfig;
}

function getS3MountPluginConfig(
	config: MountConfigJsonValue | undefined,
): S3MountPluginConfig | null {
	const object = asMountConfigJsonObject(config);
	if (typeof object.bucket !== "string") {
		return null;
	}

	const s3Config: S3MountPluginConfig = {
		bucket: object.bucket,
	};
	if (typeof object.prefix === "string") {
		s3Config.prefix = object.prefix;
	}
	if (typeof object.region === "string") {
		s3Config.region = object.region;
	}
	if (typeof object.endpoint === "string") {
		s3Config.endpoint = object.endpoint;
	}
	if (typeof object.chunkSize === "number") {
		s3Config.chunkSize = object.chunkSize;
	}
	if (typeof object.inlineThreshold === "number") {
		s3Config.inlineThreshold = object.inlineThreshold;
	}
	if (
		object.credentials &&
		typeof object.credentials === "object" &&
		!Array.isArray(object.credentials) &&
		typeof object.credentials.accessKeyId === "string" &&
		typeof object.credentials.secretAccessKey === "string"
	) {
		s3Config.credentials = {
			accessKeyId: object.credentials.accessKeyId,
			secretAccessKey: object.credentials.secretAccessKey,
		};
	}

	return s3Config;
}

function getGoogleDriveMountPluginConfig(
	config: MountConfigJsonValue | undefined,
): GoogleDriveMountPluginConfig | null {
	const object = asMountConfigJsonObject(config);
	if (typeof object.folderId !== "string") {
		return null;
	}
	if (
		!object.credentials ||
		typeof object.credentials !== "object" ||
		Array.isArray(object.credentials) ||
		typeof object.credentials.clientEmail !== "string" ||
		typeof object.credentials.privateKey !== "string"
	) {
		return null;
	}

	const googleDriveConfig: GoogleDriveMountPluginConfig = {
		credentials: {
			clientEmail: object.credentials.clientEmail,
			privateKey: object.credentials.privateKey,
		},
		folderId: object.folderId,
	};
	if (typeof object.keyPrefix === "string") {
		googleDriveConfig.keyPrefix = object.keyPrefix;
	}
	if (typeof object.chunkSize === "number") {
		googleDriveConfig.chunkSize = object.chunkSize;
	}
	if (typeof object.inlineThreshold === "number") {
		googleDriveConfig.inlineThreshold = object.inlineThreshold;
	}

	return googleDriveConfig;
}

const KERNEL_POSIX_BOOTSTRAP_DIRS = [
	"/dev",
	"/proc",
	"/tmp",
	"/bin",
	"/lib",
	"/sbin",
	"/boot",
	"/etc",
	"/root",
	"/run",
	"/srv",
	"/sys",
	"/opt",
	"/mnt",
	"/media",
	"/home",
	"/home/agentos",
	"/workspace",
	"/usr",
	"/usr/bin",
	"/usr/games",
	"/usr/include",
	"/usr/lib",
	"/usr/libexec",
	"/usr/man",
	"/usr/local",
	"/usr/local/bin",
	"/usr/sbin",
	"/usr/share",
	"/usr/share/man",
	"/var",
	"/var/cache",
	"/var/empty",
	"/var/lib",
	"/var/lock",
	"/var/log",
	"/var/run",
	"/var/spool",
	"/var/tmp",
	"/etc/agentos",
] as const;

// Standard POSIX metadata for the bootstrap dirs whose mode/owner is NOT the
// default (`755`, root:root). Replicated as a constant so building the no-base
// bootstrap layer needs no `base-filesystem.json` read — when a base IS present
// the sidecar's embedded base layer is authoritative and these are never emitted.
const KERNEL_POSIX_BOOTSTRAP_DIR_METADATA: Record<
	string,
	{ mode: string; uid: number; gid: number }
> = {
	"/tmp": { mode: "1777", uid: 0, gid: 0 },
	"/root": { mode: "700", uid: 0, gid: 0 },
	"/sys": { mode: "555", uid: 0, gid: 0 },
	"/home/agentos": { mode: "2755", uid: 1000, gid: 1000 },
	"/workspace": { mode: "755", uid: 1000, gid: 1000 },
	"/var/empty": { mode: "555", uid: 0, gid: 0 },
	"/var/lock": { mode: "777", uid: 0, gid: 0 },
	"/var/run": { mode: "777", uid: 0, gid: 0 },
	"/var/tmp": { mode: "1777", uid: 0, gid: 0 },
};

// Runtime commands that get a `/bin/<cmd>` stub at bootstrap so the guest shell
// resolves them on PATH (e.g. `sh -c "python ..."`, pipelines). The sidecar
// intercepts these by name and routes them to the embedded V8 / Pyodide runtime.
const RUNTIME_BOOTSTRAP_COMMANDS = [
	"node",
	"npm",
	"npx",
	"python",
	"python3",
] as const;
const REPO_ROOT = fileURLToPath(new URL("../../..", import.meta.url));
const SIDECAR_BINARY = join(REPO_ROOT, "target/debug/agentos-sidecar");
const SIDECAR_BUILD_INPUTS = [
	join(REPO_ROOT, "Cargo.toml"),
	join(REPO_ROOT, "Cargo.lock"),
	join(REPO_ROOT, "crates/bridge"),
	join(REPO_ROOT, "crates/build-support"),
	join(REPO_ROOT, "crates/execution"),
	join(REPO_ROOT, "crates/kernel"),
	join(REPO_ROOT, "crates/agentos-protocol"),
	join(REPO_ROOT, "crates/agentos-sidecar"),
	join(REPO_ROOT, "crates/native-sidecar"),
	join(REPO_ROOT, "crates/native-sidecar-core"),
	join(REPO_ROOT, "crates/sidecar-protocol"),
	join(REPO_ROOT, "crates/v8-runtime"),
	join(REPO_ROOT, "crates/vfs"),
	join(REPO_ROOT, "crates/vm-config"),
	join(REPO_ROOT, "packages/build-tools/bridge-src"),
	join(REPO_ROOT, "packages/build-tools/package.json"),
	join(REPO_ROOT, "packages/build-tools/scripts/build-v8-bridge.mjs"),
	join(REPO_ROOT, "packages/core/fixtures/base-filesystem.json"),
	join(REPO_ROOT, "packages/runtime-core/fixtures/base-filesystem.json"),
	join(REPO_ROOT, "pnpm-lock.yaml"),
] as const;
let ensuredSidecarBinary: string | null = null;

function collectConfiguredLowerPaths(
	config?: RootFilesystemConfig,
): Set<string> {
	const paths = new Set<string>();

	for (const lower of config?.type === "native" ? [] : (config?.lowers ?? [])) {
		if (lower.kind !== "snapshot-export") {
			continue;
		}
		for (const entry of lower.source.filesystem.entries) {
			paths.add(entry.path);
		}
	}

	return paths;
}

function findBootstrapSeedEntry(
	config: RootFilesystemConfig | undefined,
	path: string,
): FilesystemEntry | undefined {
	for (const lower of config?.type === "native" ? [] : (config?.lowers ?? [])) {
		if (lower.kind !== "snapshot-export") {
			continue;
		}
		const entry = lower.source.filesystem.entries.find(
			(candidate) => candidate.path === path,
		);
		if (entry) {
			return entry;
		}
	}

	// No base-filesystem JSON read: standard non-default dir metadata comes from
	// the constant table. When a base layer IS present these dirs are never
	// emitted (see createKernelBootstrapLower), so this only seeds the no-base case.
	const meta = KERNEL_POSIX_BOOTSTRAP_DIR_METADATA[path];
	return meta ? { path, type: "directory", ...meta } : undefined;
}

function createKernelBootstrapLower(
	config: RootFilesystemConfig | undefined,
	extraEntries: FilesystemEntry[] = [],
): RootSnapshotExport | null {
	if (config?.type === "native") {
		return null;
	}
	const includesBundledBaseLayer = !(config?.disableDefaultBaseLayer ?? false);
	const existingPaths = collectConfiguredLowerPaths(config);
	const entries: FilesystemEntry[] = [
		{
			path: "/",
			type: "directory",
			mode: "755",
			uid: 0,
			gid: 0,
		},
	];

	// Only run the FS bootstrap (creating the POSIX dir tree) when there is NO
	// base layer. When the bundled base IS present, the sidecar's embedded base
	// layer already provides every POSIX dir with the correct mode/owner, so we
	// emit nothing here and never read its filesystem table.
	if (!includesBundledBaseLayer) {
		for (const dir of KERNEL_POSIX_BOOTSTRAP_DIRS) {
			if (existingPaths.has(dir)) {
				continue;
			}
			const seed = findBootstrapSeedEntry(config, dir);
			entries.push({
				path: dir,
				type: "directory",
				mode: seed?.type === "directory" ? seed.mode : "755",
				uid: seed?.uid ?? 0,
				gid: seed?.gid ?? 0,
			});
		}
	}

	if (!includesBundledBaseLayer && !existingPaths.has("/usr/bin/env")) {
		entries.push({
			path: "/usr/bin/env",
			type: "file",
			mode: "644",
			uid: 0,
			gid: 0,
			content: "AA==",
			encoding: "base64",
		});
	}

	for (const entry of sortFilesystemEntries(extraEntries)) {
		if (existingPaths.has(entry.path)) {
			continue;
		}
		entries.push(entry);
	}

	return entries.length > 1 ? createSnapshotExport(entries) : null;
}

function buildLiveBootstrapDirectoryEntries(
	existingPaths: ReadonlySet<string>,
	config: RootFilesystemConfig | undefined,
): RootFilesystemEntry[] {
	const entries: RootFilesystemEntry[] = [];
	for (const dir of KERNEL_POSIX_BOOTSTRAP_DIRS) {
		if (existingPaths.has(dir)) {
			continue;
		}
		const seed = findBootstrapSeedEntry(config, dir);
		entries.push({
			path: dir,
			kind: "directory",
			mode: Number.parseInt(seed?.type === "directory" ? seed.mode : "755", 8),
			uid: seed?.uid ?? 0,
			gid: seed?.gid ?? 0,
			executable: true,
		});
	}
	return entries;
}

async function bootstrapLiveBootstrapDirectories(
	client: SidecarProcess,
	session: AuthenticatedSession,
	vm: CreatedVm,
	config: RootFilesystemConfig | undefined,
): Promise<void> {
	const existingPaths = new Set(
		(
			await client.snapshotRootFilesystem(session, vm, Number.MAX_SAFE_INTEGER)
		).map((entry) => entry.path),
	);
	const entries = buildLiveBootstrapDirectoryEntries(existingPaths, config);
	if (entries.length === 0) {
		return;
	}
	await client.bootstrapRootFilesystem(session, vm, entries);
}

function toSnapshotModeString(
	mode: number | undefined,
	kind: RootFilesystemEntry["kind"],
): string {
	const fallback =
		kind === "directory" ? 0o755 : kind === "symlink" ? 0o777 : 0o644;
	return `0${((mode ?? fallback) & 0o7777).toString(8)}`;
}

function convertSidecarRootSnapshotEntries(
	entries: RootFilesystemEntry[],
): FilesystemEntry[] {
	return entries.map((entry) => {
		const baseEntry: FilesystemEntry = {
			path: entry.path,
			type: entry.kind,
			mode: toSnapshotModeString(entry.mode, entry.kind),
			uid: entry.uid ?? 0,
			gid: entry.gid ?? 0,
		};

		if (entry.kind === "file") {
			return {
				...baseEntry,
				content: entry.content ?? "",
				encoding: entry.encoding ?? "utf8",
			};
		}

		if (entry.kind === "symlink") {
			if (entry.target === undefined) {
				throw new Error(
					`sidecar root snapshot for ${entry.path} is missing a symlink target`,
				);
			}
			return {
				...baseEntry,
				target: entry.target,
			};
		}

		return baseEntry;
	});
}

function ensureNativeSidecarBinary(): string {
	// A published install has no in-repo Cargo workspace to build from: resolve
	// the prebuilt platform binary (or the AGENTOS_SIDECAR_BIN override).
	if (
		process.env.AGENTOS_SIDECAR_BIN ||
		!existsSync(join(REPO_ROOT, "Cargo.toml"))
	) {
		return resolvePublishedSidecarBinary();
	}
	if (
		ensuredSidecarBinary &&
		existsSync(ensuredSidecarBinary) &&
		!sidecarBinaryNeedsBuild()
	) {
		return ensuredSidecarBinary;
	}

	if (sidecarBinaryNeedsBuild()) {
		const cargoBinary = findCargoBinary();
		if (cargoBinary) {
			execFileSync(cargoBinary, ["build", "-q", "-p", "agentos-sidecar"], {
				cwd: REPO_ROOT,
				stdio: "pipe",
			});
		} else if (!existsSync(SIDECAR_BINARY)) {
			execFileSync(
				resolveCargoBinary(),
				["build", "-q", "-p", "agentos-sidecar"],
				{
					cwd: REPO_ROOT,
					stdio: "pipe",
				},
			);
		}
	}

	ensuredSidecarBinary = SIDECAR_BINARY;
	return ensuredSidecarBinary;
}

function sidecarBinaryNeedsBuild(): boolean {
	if (!existsSync(SIDECAR_BINARY)) {
		return true;
	}

	const binaryMtimeMs = statSync(SIDECAR_BINARY).mtimeMs;
	return SIDECAR_BUILD_INPUTS.some(
		(path) => existsSync(path) && latestMtimeMs(path) > binaryMtimeMs,
	);
}

function latestMtimeMs(path: string): number {
	const stats = statSync(path);
	if (!stats.isDirectory()) {
		return stats.mtimeMs;
	}

	let latest = stats.mtimeMs;
	for (const entry of readdirSync(path)) {
		latest = Math.max(latest, latestMtimeMs(join(path, entry)));
	}
	return latest;
}

async function resolveCompatLocalMounts(
	mounts?: MountConfig[],
): Promise<LocalCompatMount[]> {
	if (!mounts) {
		return [];
	}

	const resolved: LocalCompatMount[] = [];
	for (const mount of mounts) {
		if (isNativeMountConfig(mount)) {
			continue;
		}

		if (!isOverlayMountConfig(mount)) {
			resolved.push({
				path: posixPath.normalize(mount.path),
				fs: mount.driver,
				readOnly: mount.readOnly ?? false,
			});
			continue;
		}

		const mode = mount.filesystem.mode ?? "ephemeral";
		const fs =
			mode === "read-only"
				? mount.filesystem.store.createOverlayFilesystem({
						mode: "read-only",
						lowers: mount.filesystem.lowers,
					})
				: mount.filesystem.store.createOverlayFilesystem({
						upper: await mount.filesystem.store.createWritableLayer(),
						lowers: mount.filesystem.lowers,
					});

		resolved.push({
			path: posixPath.normalize(mount.path),
			fs,
			readOnly: mode === "read-only",
		});
	}

	return resolved;
}

function collectSidecarMountPlan(options: { mounts?: MountConfig[] }): {
	sidecarMounts: Array<ReturnType<typeof serializeMountConfigForSidecar>>;
	hostMounts: HostMountInfo[];
	hostPathMappings: HostMountInfo[];
} {
	const sidecarMounts: Array<
		ReturnType<typeof serializeMountConfigForSidecar>
	> = [];
	const hostMounts: HostMountInfo[] = [];
	const hostPathMappings: HostMountInfo[] = [];
	const seenMounts = new Set<string>();

	function pushMount(mount: NativeMountConfig): void {
		const serialized = serializeMountConfigForSidecar(mount);
		const key = `${serialized.guestPath}\0${serialized.plugin.id}\0${JSON.stringify(
			serialized.plugin.config,
		)}`;
		if (seenMounts.has(key)) {
			return;
		}
		seenMounts.add(key);
		sidecarMounts.push(serialized);

		if (mount.plugin.id === "host_dir") {
			const config = getHostDirMountPluginConfig(mount.plugin.config);
			if (config) {
				hostPathMappings.push({
					vmPath: posixPath.normalize(mount.path),
					hostPath: resolveHostPath(config.hostPath),
					readOnly: mount.readOnly ?? config.readOnly ?? true,
				});
			}
			if (config && options.mounts?.some((candidate) => candidate === mount)) {
				hostMounts.push({
					vmPath: posixPath.normalize(mount.path),
					hostPath: resolveHostPath(config.hostPath),
					readOnly: mount.readOnly ?? config.readOnly ?? true,
				});
			}
		}
	}

	for (const mount of options.mounts ?? []) {
		if (!isNativeMountConfig(mount)) {
			sidecarMounts.push({
				guestPath: mount.path,
				readOnly: isOverlayMountConfig(mount)
					? (mount.filesystem.mode ?? "ephemeral") === "read-only"
					: (mount.readOnly ?? false),
				plugin: {
					id: "js_bridge",
					config: {},
				},
			});
			continue;
		}
		pushMount(mount);
	}

	hostMounts.sort((left, right) => right.vmPath.length - left.vmPath.length);
	hostPathMappings.sort(
		(left, right) => right.vmPath.length - left.vmPath.length,
	);
	return { sidecarMounts, hostMounts, hostPathMappings };
}

function collectBindingBootstrapCommands(bindings: Bindings[]): string[] {
	if (bindings.length === 0) {
		return [];
	}

	return [
		"agentos",
		...bindings.map((bindingCollection) => `agentos-${bindingCollection.name}`),
	];
}

function validationMessage(error: unknown): string {
	if (
		typeof error === "object" &&
		error !== null &&
		"issues" in error &&
		Array.isArray((error as { issues?: unknown[] }).issues)
	) {
		return (
			error as { issues: Array<{ message: string; path?: unknown[] }> }
		).issues
			.map((issue) => {
				const path =
					Array.isArray(issue.path) && issue.path.length > 0
						? ` at "${issue.path.join(".")}"`
						: "";
				return `${issue.message}${path}`;
			})
			.join("; ");
	}
	return error instanceof Error ? error.message : String(error);
}

function bindingToSidecarDefinition(
	definition: Binding,
): SidecarRegisteredHostCallbackDefinition {
	return {
		description: definition.description,
		inputSchema: zodToJsonSchema(definition.inputSchema),
		...(definition.timeout !== undefined
			? { timeoutMs: definition.timeout }
			: {}),
		...(definition.examples && definition.examples.length > 0
			? {
					examples: definition.examples.map((example) => ({
						description: example.description,
						input: example.input,
					})),
				}
			: {}),
	};
}

function combineInstructions(
	additionalInstructions: string | undefined,
	bindingReference: string,
): string | null {
	const parts = [additionalInstructions, bindingReference]
		.map((part) => part?.trim())
		.filter((part): part is string => Boolean(part));
	if (parts.length === 0) {
		return null;
	}
	return parts.join("\n\n");
}

function buildBindingReference(bindings: Bindings[]): string {
	if (bindings.length === 0) {
		return "";
	}

	const lines = [
		"## Available Host Bindings",
		"",
		"Run `agentos list-bindings` to see all available bindings.",
		"",
	];

	for (const bindingCollection of bindings) {
		lines.push(`### ${bindingCollection.name}`);
		lines.push("");
		lines.push(bindingCollection.description);
		lines.push("");
		for (const [bindingName, definition] of Object.entries(
			bindingCollection.bindings,
		)) {
			const sidecarBinding = bindingToSidecarDefinition(definition);
			const signature = buildBindingFlagSignature(sidecarBinding.inputSchema);
			const suffix = signature.length > 0 ? ` ${signature}` : "";
			lines.push(
				`- \`agentos-${bindingCollection.name} ${bindingName}${suffix}\` — ${definition.description}`,
			);
		}
		lines.push("");

		const bindingsWithExamples = Object.entries(
			bindingCollection.bindings,
		).filter(
			([, definition]) => definition.examples && definition.examples.length > 0,
		);
		if (bindingsWithExamples.length > 0) {
			lines.push("**Examples:**");
			lines.push("");
			for (const [bindingName, definition] of bindingsWithExamples) {
				for (const example of definition.examples ?? []) {
					const args = inputToBindingFlags(example.input);
					const suffix = args.length > 0 ? ` ${args}` : "";
					lines.push(
						`- ${example.description}: \`agentos-${bindingCollection.name} ${bindingName}${suffix}\``,
					);
				}
			}
			lines.push("");
		}

		lines.push(
			`Run \`agentos-${bindingCollection.name} <binding> --help\` for details.`,
		);
		lines.push("");
	}

	return lines.join("\n");
}

function buildBindingFlagSignature(schema: unknown): string {
	return describeBindingFlags(schema)
		.map((flag) => {
			if (flag.required) {
				return `${flag.name} <${flag.type}>`;
			}
			return `[${flag.name} <${flag.type}>]`;
		})
		.join(" ");
}

function describeBindingFlags(
	schema: unknown,
): Array<{ name: string; type: string; required: boolean }> {
	const schemaObject = asRecord(schema);
	const properties = asRecord(schemaObject.properties);
	const required = Array.isArray(schemaObject.required)
		? new Set(
				schemaObject.required.filter(
					(item): item is string => typeof item === "string",
				),
			)
		: new Set<string>();

	return Object.entries(properties).map(([fieldName, fieldSchema]) => ({
		name: `--${camelToKebab(fieldName)}`,
		type: describeBindingFlagType(fieldSchema),
		required: required.has(fieldName),
	}));
}

function describeBindingFlagType(schema: unknown): string {
	const schemaObject = asRecord(schema);
	const type =
		typeof schemaObject.type === "string" ? schemaObject.type : undefined;
	if (type === "array") {
		const itemType = describeJsonSchemaScalarType(schemaObject.items);
		return `${itemType}[]`;
	}
	if (type === "string") {
		const enumValues = Array.isArray(schemaObject.enum)
			? schemaObject.enum.filter(
					(item): item is string => typeof item === "string",
				)
			: [];
		return enumValues.length > 0 ? enumValues.join("|") : "string";
	}
	return type ?? "string";
}

function describeJsonSchemaScalarType(schema: unknown): string {
	const schemaObject = asRecord(schema);
	return typeof schemaObject.type === "string" ? schemaObject.type : "string";
}

function inputToBindingFlags(input: unknown): string {
	const inputObject = asRecord(input);
	return Object.entries(inputObject)
		.flatMap(([key, value]) => {
			const flag = `--${camelToKebab(key)}`;
			if (value === true) {
				return [flag];
			}
			if (value === false) {
				return [`--no-${camelToKebab(key)}`];
			}
			if (Array.isArray(value)) {
				return value.map((item) => `${flag} ${bindingCliString(item)}`);
			}
			return [`${flag} ${bindingCliString(value)}`];
		})
		.join(" ");
}

function bindingCliString(value: unknown): string {
	return typeof value === "string" ? value : (JSON.stringify(value) ?? "null");
}

function camelToKebab(value: string): string {
	return value.replace(
		/[A-Z]/g,
		(ch, index) => `${index > 0 ? "-" : ""}${ch.toLowerCase()}`,
	);
}

function asRecord(value: unknown): Record<string, unknown> {
	if (typeof value === "object" && value !== null && !Array.isArray(value)) {
		return value as Record<string, unknown>;
	}
	return {};
}

async function handleHostCallback(
	request: SidecarRequestFrame,
	context: HostCallbackContext,
): Promise<SidecarResponsePayload> {
	const payload = request.payload;
	if (payload.type !== "host_callback") {
		return {
			type: "host_callback_result",
			invocation_id: "unknown",
			error: `unsupported sidecar request type: ${payload.type}`,
		};
	}

	const command = parseHostCommandCallbackInput(payload.input);
	if (command) {
		try {
			return {
				type: "host_callback_result",
				invocation_id: payload.invocation_id,
				result: await handleHostCommandCallback(command, context),
			};
		} catch (error) {
			return {
				type: "host_callback_result",
				invocation_id: payload.invocation_id,
				error: validationMessage(error),
			};
		}
	}

	const definition = context.bindingMap.get(payload.callback_key);
	if (!definition) {
		return {
			type: "host_callback_result",
			invocation_id: payload.invocation_id,
			error: `Unknown binding "${payload.callback_key}"`,
		};
	}

	const permissionMode = bindingPermissionMode(
		context.permissions,
		payload.callback_key,
	);
	if (permissionMode !== "allow") {
		return {
			type: "host_callback_result",
			invocation_id: payload.invocation_id,
			error: `EACCES: blocked by binding.invoke policy for ${payload.callback_key}`,
		};
	}

	const parsed = definition.inputSchema.safeParse(payload.input);
	if (!parsed.success) {
		return {
			type: "host_callback_result",
			invocation_id: payload.invocation_id,
			error: validationMessage(parsed.error),
		};
	}

	try {
		return {
			type: "host_callback_result",
			invocation_id: payload.invocation_id,
			result: await executeBinding(
				definition,
				payload.callback_key,
				parsed.data,
			),
		};
	} catch (error) {
		return {
			type: "host_callback_result",
			invocation_id: payload.invocation_id,
			error: validationMessage(error),
		};
	}
}

function buildBindingMap(bindings: Bindings[]): Map<string, Binding> {
	const bindingMap = new Map<string, Binding>();
	for (const bindingCollection of bindings) {
		for (const [bindingName, definition] of Object.entries(
			bindingCollection.bindings,
		)) {
			bindingMap.set(`${bindingCollection.name}:${bindingName}`, definition);
		}
	}
	return bindingMap;
}

interface HostCommandCallbackInput {
	type: "command";
	command: string;
	args: string[];
	cwd: string;
}

interface HostCallbackContext {
	bindings: Bindings[];
	bindingMap: ReadonlyMap<string, Binding>;
	permissions: Permissions;
	readFile(path: string): Promise<Uint8Array>;
}

interface JsBridgeContext {
	filesystem: VirtualFileSystem;
}

function bridgeErrorMessage(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}

function toBridgeArgs(value: unknown): Record<string, unknown> {
	if (!value || typeof value !== "object" || Array.isArray(value)) {
		throw new Error("js_bridge args must be an object");
	}
	return value as Record<string, unknown>;
}

function bridgePath(mountId: string, value: unknown): string {
	if (!mountId.startsWith("/")) {
		throw new Error(`Unsupported js_bridge mount id: ${mountId}`);
	}
	if (typeof value !== "string") {
		throw new Error("js_bridge path argument must be a string");
	}
	return posixPath.normalize(posixPath.join(mountId, value));
}

function requireBridgeNumber(value: unknown, field: string): number {
	if (typeof value !== "number" || !Number.isFinite(value)) {
		throw new Error(`js_bridge args.${field} must be a number`);
	}
	return value;
}

function decodeBridgeBytes(value: unknown, field: string): Uint8Array {
	if (typeof value === "string") {
		return new Uint8Array(Buffer.from(value, "base64"));
	}
	if (
		Array.isArray(value) &&
		value.every(
			(entry) => Number.isInteger(entry) && entry >= 0 && entry <= 255,
		)
	) {
		return new Uint8Array(value);
	}
	throw new Error(`js_bridge args.${field} must be base64 bytes`);
}

async function handleJsBridgeCall(
	request: Extract<SidecarRequestFrame["payload"], { type: "js_bridge_call" }>,
	context: JsBridgeContext,
): Promise<SidecarResponsePayload> {
	try {
		const args = toBridgeArgs(request.args);
		const fs = context.filesystem;
		const path = () => bridgePath(request.mount_id, args.path);
		let result: unknown;

		switch (request.operation) {
			case "readFile":
				result = Buffer.from(await fs.readFile(path())).toString("base64");
				break;
			case "readDir":
				result = await fs.readDir(path());
				break;
			case "readDirWithTypes":
				result = await fs.readDirWithTypes(path());
				break;
			case "writeFile":
				await fs.writeFile(path(), decodeBridgeBytes(args.content, "content"));
				break;
			case "createDir":
				await fs.createDir(path());
				break;
			case "mkdir":
				await fs.mkdir(path(), { recursive: args.recursive !== false });
				break;
			case "exists":
				result = await fs.exists(path());
				break;
			case "stat":
				result = await fs.stat(path());
				break;
			case "removeFile":
				await fs.removeFile(path());
				break;
			case "removeDir":
				await fs.removeDir(path());
				break;
			case "rename":
				await fs.rename(
					bridgePath(request.mount_id, args.oldPath),
					bridgePath(request.mount_id, args.newPath),
				);
				break;
			case "realpath":
				result = await fs.realpath(path());
				break;
			case "symlink": {
				if (typeof args.target !== "string") {
					throw new Error("js_bridge args.target must be a string");
				}
				await fs.symlink(
					args.target,
					bridgePath(request.mount_id, args.linkPath),
				);
				break;
			}
			case "readlink":
				result = await fs.readlink(path());
				break;
			case "lstat":
				result = await fs.lstat(path());
				break;
			case "link":
				await fs.link(
					bridgePath(request.mount_id, args.oldPath),
					bridgePath(request.mount_id, args.newPath),
				);
				break;
			case "chmod":
				await fs.chmod(path(), requireBridgeNumber(args.mode, "mode"));
				break;
			case "chown":
				await fs.chown(
					path(),
					requireBridgeNumber(args.uid, "uid"),
					requireBridgeNumber(args.gid, "gid"),
					{ followSymlinks: args.followSymlinks !== false },
				);
				break;
			case "utimes":
				await fs.utimes(
					path(),
					requireBridgeNumber(args.atimeMs, "atimeMs"),
					requireBridgeNumber(args.mtimeMs, "mtimeMs"),
				);
				break;
			case "truncate":
				await fs.truncate(path(), requireBridgeNumber(args.length, "length"));
				break;
			case "pread":
				result = Buffer.from(
					await fs.pread(
						path(),
						requireBridgeNumber(args.offset, "offset"),
						requireBridgeNumber(args.length, "length"),
					),
				).toString("base64");
				break;
			case "pwrite":
				await fs.pwrite(
					path(),
					requireBridgeNumber(args.offset, "offset"),
					decodeBridgeBytes(args.content, "content"),
				);
				break;
			default:
				throw new Error(
					`Unsupported js_bridge operation: ${request.operation}`,
				);
		}

		return {
			type: "js_bridge_result",
			call_id: request.call_id,
			...(result === undefined ? {} : { result }),
		};
	} catch (error) {
		return {
			type: "js_bridge_result",
			call_id: request.call_id,
			error: bridgeErrorMessage(error),
		};
	}
}

function parseHostCommandCallbackInput(
	input: unknown,
): HostCommandCallbackInput | null {
	const value = asRecord(input);
	if (
		value.type !== "command" ||
		typeof value.command !== "string" ||
		typeof value.cwd !== "string" ||
		!Array.isArray(value.args) ||
		!value.args.every((arg): arg is string => typeof arg === "string")
	) {
		return null;
	}
	return {
		type: "command",
		command: value.command,
		args: value.args,
		cwd: value.cwd,
	};
}

async function handleHostCommandCallback(
	command: HostCommandCallbackInput,
	context: HostCallbackContext,
): Promise<unknown> {
	const directBindings = context.bindings.find(
		(bindingCollection) =>
			`agentos-${bindingCollection.name}` === command.command,
	);
	if (command.command === "agentos") {
		return handleAgentOsRegistryCommand(command, context);
	}
	if (directBindings) {
		return handleAgentOsBindingCommand(command, context, directBindings);
	}
	throw new Error(`Unknown host callback command "${command.command}"`);
}

async function handleAgentOsRegistryCommand(
	command: HostCommandCallbackInput,
	context: HostCallbackContext,
): Promise<unknown> {
	const [subcommand, collectionName, bindingName, ...bindingArgs] =
		command.args;
	if (!subcommand || isHelpFlag(subcommand)) {
		return {
			usage:
				"agentos <command>: list-bindings [collection], <collection> --help, or <collection> <binding> ...",
		};
	}
	if (subcommand === "list-bindings") {
		return collectionName
			? describeBindingsPayload(context.bindings, collectionName)
			: listBindingsPayload(context.bindings);
	}
	const bindingCollection = context.bindings.find(
		(collection) => collection.name === subcommand,
	);
	if (!bindingCollection) {
		throw new Error(
			`No binding collection "${subcommand}". Available: ${bindingsNames(context.bindings)}`,
		);
	}
	if (!collectionName || isHelpFlag(collectionName)) {
		return describeBindingsPayload(context.bindings, subcommand);
	}
	if (bindingName && isHelpFlag(bindingName)) {
		return describeBindingPayload(bindingCollection, collectionName);
	}
	return invokeBinding({
		bindingCollection,
		bindingName: collectionName,
		args: [bindingName, ...bindingArgs].filter(
			(value): value is string => typeof value === "string",
		),
		cwd: command.cwd,
		context,
	});
}

async function handleAgentOsBindingCommand(
	command: HostCommandCallbackInput,
	context: HostCallbackContext,
	bindingCollection: Bindings,
): Promise<unknown> {
	const [bindingName, helpOrFirstArg, ...rest] = command.args;
	if (!bindingName || isHelpFlag(bindingName)) {
		return describeBindingsPayload(context.bindings, bindingCollection.name);
	}
	if (helpOrFirstArg && isHelpFlag(helpOrFirstArg)) {
		return describeBindingPayload(bindingCollection, bindingName);
	}
	return invokeBinding({
		bindingCollection,
		bindingName,
		args: [helpOrFirstArg, ...rest].filter(
			(value): value is string => typeof value === "string",
		),
		cwd: command.cwd,
		context,
	});
}

async function invokeBinding({
	bindingCollection,
	bindingName,
	args,
	cwd,
	context,
}: {
	bindingCollection: Bindings;
	bindingName: string;
	args: string[];
	cwd: string;
	context: HostCallbackContext;
}): Promise<unknown> {
	const definition = bindingCollection.bindings[bindingName];
	if (!definition) {
		throw new Error(
			`No binding "${bindingName}" in collection "${bindingCollection.name}". Available: ${bindingNames(bindingCollection)}`,
		);
	}
	const callbackKey = `${bindingCollection.name}:${bindingName}`;
	const permissionMode = bindingPermissionMode(
		context.permissions,
		callbackKey,
	);
	if (permissionMode !== "allow") {
		throw new Error(
			`EACCES: blocked by binding.invoke policy for ${callbackKey}`,
		);
	}
	const input = await parseBindingInput(
		definition,
		args,
		cwd,
		context.readFile,
	);
	return executeBinding(definition, callbackKey, input);
}

async function executeBinding(
	definition: Binding,
	callbackKey: string,
	input: unknown,
): Promise<unknown> {
	const parsed = definition.inputSchema.safeParse(input);
	if (!parsed.success) {
		throw new Error(validationMessage(parsed.error));
	}

	return Promise.race([
		Promise.resolve(definition.execute(parsed.data)),
		new Promise<never>((_, reject) => {
			if (definition.timeout === undefined) {
				return;
			}
			setTimeout(
				() =>
					reject(
						new Error(
							`Binding "${callbackKey}" timed out after ${definition.timeout}ms`,
						),
					),
				definition.timeout,
			);
		}),
	]);
}

async function parseBindingInput(
	definition: Binding,
	args: string[],
	cwd: string,
	readFile: (path: string) => Promise<Uint8Array>,
): Promise<unknown> {
	if (args[0] === "--json") {
		const value = args[1];
		if (value === undefined) {
			throw new Error("Flag --json requires a value");
		}
		return JSON.parse(value);
	}
	if (args[0] === "--json-file") {
		const value = args[1];
		if (value === undefined) {
			throw new Error("Flag --json-file requires a value");
		}
		const guestPath = value.startsWith("/")
			? posixPath.normalize(value)
			: posixPath.normalize(`${cwd}/${value}`);
		const text = new TextDecoder().decode(await readFile(guestPath));
		return JSON.parse(text);
	}
	return parseBindingArgv(
		bindingToSidecarDefinition(definition).inputSchema,
		args,
	);
}

function parseBindingArgv(
	schema: unknown,
	argv: string[],
): Record<string, unknown> {
	const schemaObject = asRecord(schema);
	const properties = asRecord(schemaObject.properties);
	const required = Array.isArray(schemaObject.required)
		? new Set(
				schemaObject.required.filter(
					(value): value is string => typeof value === "string",
				),
			)
		: new Set<string>();
	const flagToField = new Map<string, [string, unknown]>();
	for (const [fieldName, fieldSchema] of Object.entries(properties)) {
		flagToField.set(camelToKebab(fieldName), [fieldName, fieldSchema]);
	}

	const input: Record<string, unknown> = {};
	for (let index = 0; index < argv.length; ) {
		const arg = argv[index];
		if (!arg?.startsWith("--")) {
			throw new Error(`Unexpected positional argument: "${arg}"`);
		}
		const rawFlag = arg.slice(2);
		const negated = rawFlag.startsWith("no-");
		const flagName = negated ? rawFlag.slice(3) : rawFlag;
		const entry = flagToField.get(flagName);
		if (!entry) {
			throw new Error(`Unknown flag: --${rawFlag}`);
		}
		const [fieldName, fieldSchema] = entry;
		const fieldType = jsonSchemaType(fieldSchema);
		if (negated) {
			if (fieldType !== "boolean") {
				throw new Error(`Unknown flag: --${rawFlag}`);
			}
			input[fieldName] = false;
			index += 1;
			continue;
		}
		if (fieldType === "boolean") {
			input[fieldName] = true;
			index += 1;
			continue;
		}
		const value = argv[index + 1];
		if (value === undefined) {
			throw new Error(`Flag --${rawFlag} requires a value`);
		}
		if (fieldType === "number" || fieldType === "integer") {
			const number = Number(value);
			if (!Number.isFinite(number)) {
				throw new Error(`Flag --${rawFlag} expects a number, got "${value}"`);
			}
			input[fieldName] = number;
			index += 2;
			continue;
		}
		if (fieldType === "array") {
			const current = Array.isArray(input[fieldName])
				? (input[fieldName] as unknown[])
				: [];
			const itemSchema = asRecord(fieldSchema).items;
			const itemType = jsonSchemaType(itemSchema);
			current.push(
				itemType === "number" || itemType === "integer" ? Number(value) : value,
			);
			input[fieldName] = current;
			index += 2;
			continue;
		}
		input[fieldName] = value;
		index += 2;
	}

	for (const fieldName of required) {
		if (!(fieldName in input)) {
			throw new Error(`Missing required flag: --${camelToKebab(fieldName)}`);
		}
	}
	return input;
}

function listBindingsPayload(bindings: Bindings[]): unknown {
	return {
		bindings: bindings.map((bindingCollection) => ({
			name: bindingCollection.name,
			description: bindingCollection.description,
			bindings: Object.keys(bindingCollection.bindings),
		})),
	};
}

function describeBindingsPayload(
	bindings: Bindings[],
	collectionName: string,
): unknown {
	const bindingCollection = bindings.find(
		(collection) => collection.name === collectionName,
	);
	if (!bindingCollection) {
		throw new Error(
			`No binding collection "${collectionName}". Available: ${bindingsNames(bindings)}`,
		);
	}
	return {
		name: bindingCollection.name,
		description: bindingCollection.description,
		bindings: Object.fromEntries(
			Object.entries(bindingCollection.bindings).map(
				([bindingName, definition]) => [
					bindingName,
					{
						description: definition.description,
						flags: describeBindingFlags(
							bindingToSidecarDefinition(definition).inputSchema,
						),
					},
				],
			),
		),
	};
}

function describeBindingPayload(
	bindingCollection: Bindings,
	bindingName: string,
): unknown {
	const definition = bindingCollection.bindings[bindingName];
	if (!definition) {
		throw new Error(
			`No binding "${bindingName}" in collection "${bindingCollection.name}". Available: ${bindingNames(bindingCollection)}`,
		);
	}
	return {
		collection: bindingCollection.name,
		binding: bindingName,
		description: definition.description,
		flags: describeBindingFlags(
			bindingToSidecarDefinition(definition).inputSchema,
		),
		examples:
			definition.examples?.map((example) => ({
				description: example.description,
				input: example.input,
			})) ?? [],
	};
}

function bindingPermissionMode(
	permissions: Permissions,
	callbackKey: string,
): "allow" | "deny" {
	const scope = permissions.binding;
	if (!scope) {
		return "deny";
	}
	if (typeof scope === "string") {
		return scope;
	}
	let mode: "allow" | "deny" = scope.default ?? "deny";
	for (const rule of scope.rules) {
		const operations = rule.operations ?? ["*"];
		const patterns = rule.patterns ?? ["**"];
		if (
			operations.some(
				(operation) => operation === "*" || operation === "invoke",
			) &&
			patterns.some((pattern) => permissionPatternMatches(pattern, callbackKey))
		) {
			mode = rule.mode;
		}
	}
	return mode;
}

function permissionPatternMatches(pattern: string, value: string): boolean {
	if (pattern === "*" || pattern === "**" || pattern === value) {
		return true;
	}
	const parts = pattern.split(/(\*\*|\*)/u);
	const source = parts
		.map((part) => {
			if (part === "**") return ".*";
			if (part === "*") return "[^:]*";
			return part.replace(/[.+?^${}()|[\]\\]/g, "\\$&");
		})
		.join("");
	return new RegExp(`^${source}$`).test(value);
}

function bindingsNames(bindings: Bindings[]): string {
	return bindings.map((bindingCollection) => bindingCollection.name).join(", ");
}

function bindingNames(bindingCollection: Bindings): string {
	return Object.keys(bindingCollection.bindings).join(", ");
}

function isHelpFlag(value: string): boolean {
	return value === "--help" || value === "-h";
}

function jsonSchemaType(schema: unknown): string | undefined {
	const schemaObject = asRecord(schema);
	return typeof schemaObject.type === "string" ? schemaObject.type : undefined;
}

async function registerBindingsOnSidecar(
	client: SidecarProcess,
	session: AuthenticatedSession,
	vm: CreatedVm,
	bindings: Bindings[],
): Promise<string> {
	if (bindings.length === 0) {
		return "";
	}

	for (const bindingCollection of bindings) {
		await client.registerHostCallbacks(session, vm, {
			name: bindingCollection.name,
			description: bindingCollection.description,
			commandAliases: [`agentos-${bindingCollection.name}`],
			registryCommandAliases: ["agentos"],
			callbacks: Object.fromEntries(
				Object.entries(bindingCollection.bindings).map(
					([bindingName, definition]) => [
						bindingName,
						bindingToSidecarDefinition(definition),
					],
				),
			),
		});
	}

	return buildBindingReference(bindings);
}

function executionIdentity(options: {
	contextId?: string;
}): executionProtocol.ExecutionIdentityOptions {
	return {
		contextId: options.contextId ?? null,
	};
}

function executionOutput(
	options: Pick<LanguageExecutionOptions, "contextId" | "output">,
	background = false,
): executionProtocol.ExecutionOutputOptions {
	if (options.output?.retainEvents && !options.contextId && !background) {
		throw new Error(
			"retainEvents is available only for spawned processes; use output.capture for attached runs",
		);
	}
	const capture = options.output?.capture;
	return {
		capture:
			capture === "all"
				? executionProtocol.ExecutionOutputCapture.All
				: capture === "stderr"
					? executionProtocol.ExecutionOutputCapture.Stderr
					: capture === "none"
						? executionProtocol.ExecutionOutputCapture.None
						: null,
		retainEvents: options.output?.retainEvents ?? null,
	};
}

function executionBytes(
	data: string | Uint8Array | undefined,
): ArrayBuffer | null {
	if (data === undefined) return null;
	const bytes =
		typeof data === "string" ? new TextEncoder().encode(data) : data;
	return bytes.slice().buffer;
}

function processExecutionOptions(
	options: LanguageExecutionOptions = {},
	internal?: { background?: boolean; executionId?: string },
): executionProtocol.ProcessExecutionOptions {
	const background = internal?.background ?? false;
	return {
		identity: executionIdentity(options),
		output: executionOutput(options, background),
		operationId: internal?.executionId ?? null,
		background,
		cwd: options.cwd ?? null,
		env: options.env ? new Map(Object.entries(options.env)) : null,
		args: options.args ?? [],
		stdin: executionBytes(options.stdin),
		timeoutMs: protocolTimeout(options.timeoutMs),
		pty: options.pty
			? {
					cols:
						options.pty.cols === undefined
							? null
							: protocolUnsigned(options.pty.cols, 0xffff, "pty.cols"),
					rows:
						options.pty.rows === undefined
							? null
							: protocolUnsigned(options.pty.rows, 0xffff, "pty.rows"),
				}
			: null,
	};
}

function protocolUnsigned(
	value: number,
	maximum: number,
	name: string,
): number {
	if (!Number.isSafeInteger(value) || value < 0 || value > maximum) {
		throw new RangeError(`${name} must be an integer between 0 and ${maximum}`);
	}
	return value;
}

function protocolTimeout(value: number | undefined): bigint | null {
	if (value === undefined) return null;
	if (!Number.isSafeInteger(value) || value < 0) {
		throw new RangeError("timeoutMs must be a non-negative safe integer");
	}
	return BigInt(value);
}

function safeProtocolNumber(value: bigint, name: string): number {
	const result = Number(value);
	if (!Number.isSafeInteger(result)) {
		throw new RangeError(`${name} exceeds JavaScript's safe integer range`);
	}
	return result;
}

function mapExecutionState(
	state: executionProtocol.ExecutionState,
): ContextDescriptor["state"] {
	const mapped = state.toLowerCase();
	return (
		mapped === "creating" ? "idle" : mapped
	) as ContextDescriptor["state"];
}

function mapExecutionOutcome(
	outcome: executionProtocol.ExecutionOutcome,
): CodeExecutionResult["outcome"] {
	return outcome === executionProtocol.ExecutionOutcome.TimedOut
		? "timed_out"
		: (outcome.toLowerCase() as CodeExecutionResult["outcome"]);
}

function mapExecutionDescriptor(
	descriptor: executionProtocol.ExecutionDescriptor,
): ContextDescriptor {
	return {
		contextId: descriptor.executionId,
		state: mapExecutionState(descriptor.state),
		...(descriptor.retainedLanguage
			? {
					language:
						descriptor.retainedLanguage ===
						executionProtocol.RetainedExecutionLanguage.JavaScript
							? ("javascript" as const)
							: ("python" as const),
				}
			: {}),
		createdAtMs: safeProtocolNumber(descriptor.createdAtMs, "createdAtMs"),
		...(descriptor.lastStartedAtMs !== null
			? {
					lastStartedAtMs: safeProtocolNumber(
						descriptor.lastStartedAtMs,
						"lastStartedAtMs",
					),
				}
			: {}),
		...(descriptor.lastCompletedAtMs !== null
			? {
					lastCompletedAtMs: safeProtocolNumber(
						descriptor.lastCompletedAtMs,
						"lastCompletedAtMs",
					),
				}
			: {}),
	};
}

function mapExecutionError(
	error: executionProtocol.ExecutionErrorData | null,
): CodeExecutionResult["error"] {
	if (!error) return undefined;
	return {
		code: error.code,
		name: error.name,
		message: error.message,
		...(error.stack ? { stack: error.stack } : {}),
		...(error.details
			? { details: JSON.parse(error.details) as JsonValue }
			: {}),
	};
}

function mapExecutionResult(
	result: executionProtocol.ExecutionCompletedResponse,
): MappedExecutionResult {
	const base = {
		...(result.exitCode !== null ? { exitCode: result.exitCode } : {}),
		...(result.stdout !== null
			? { stdout: new TextDecoder().decode(result.stdout) }
			: {}),
		...(result.stderr !== null
			? { stderr: new TextDecoder().decode(result.stderr) }
			: {}),
		...(result.stdoutTruncated !== null
			? { stdoutTruncated: result.stdoutTruncated }
			: {}),
		...(result.stderrTruncated !== null
			? { stderrTruncated: result.stderrTruncated }
			: {}),
		...(result.evaluationValue
			? { evaluationValue: JSON.parse(result.evaluationValue) as JsonValue }
			: {}),
		...(result.typeScriptCheckResult
			? {
					typeScriptCheckResult: JSON.parse(
						result.typeScriptCheckResult,
					) as JsonValue,
				}
			: {}),
	};
	const outcome = mapExecutionOutcome(result.outcome);
	if (outcome === "succeeded") {
		return { ...base, outcome } as MappedExecutionResult;
	}
	return {
		...base,
		outcome,
		error: mapExecutionError(result.error) ?? {
			code: "execution_failed",
			name: "ExecutionError",
			message: `execution completed with ${outcome}`,
		},
	} as MappedExecutionResult;
}

type MappedExecutionResult = CodeExecutionResult & {
	evaluationValue?: JsonValue;
	typeScriptCheckResult?: JsonValue;
};

function mapTypeScriptCheckResult(
	result: MappedExecutionResult,
): TypeScriptCheckResult {
	if (result.outcome !== "succeeded") {
		return { ...result, diagnostics: [] };
	}
	const data = result.typeScriptCheckResult;
	if (!data || Array.isArray(data) || typeof data !== "object") {
		throw new Error("TypeScript checker returned no diagnostic result");
	}
	const hasErrors = data.hasErrors;
	const diagnostics = data.diagnostics;
	if (typeof hasErrors !== "boolean" || !Array.isArray(diagnostics)) {
		throw new Error("TypeScript checker returned an invalid diagnostic result");
	}
	return {
		...result,
		hasErrors,
		diagnostics: diagnostics as unknown as TypeScriptDiagnostic[],
	};
}

interface InternalExecutionOutputEvent {
	executionId: string;
	generation: number;
	processId?: string;
	sequence: number;
	channel: "stdout" | "stderr" | "pty";
	chunk: Uint8Array;
	timestampMs: number;
}

interface InternalExecutionCompletedEvent {
	executionId: string;
	generation: number;
	outcome: CodeExecutionResult["outcome"];
	exitCode?: number;
	error?: CodeExecutionResult["error"];
}

interface InternalExecutionOutputPage {
	events: InternalExecutionOutputEvent[];
	nextCursor: string;
	hasMore: boolean;
	truncated: boolean;
}

interface InternalBackgroundExecution {
	executionId: string;
	pid: number;
	createdAtMs: number;
	completed: boolean;
	completion: Promise<void>;
}

function mapExecutionOutputEvent(
	event: executionProtocol.ExecutionOutputEvent,
): InternalExecutionOutputEvent {
	return {
		executionId: event.executionId,
		generation: safeProtocolNumber(event.generation, "execution generation"),
		...(event.processId ? { processId: event.processId } : {}),
		sequence: safeProtocolNumber(event.sequence, "execution output sequence"),
		channel:
			event.channel.toLowerCase() as InternalExecutionOutputEvent["channel"],
		chunk: new Uint8Array(event.chunk),
		timestampMs: safeProtocolNumber(event.timestampMs, "execution timestamp"),
	};
}

function mapExecutionCompletedEvent(
	event: executionProtocol.ExecutionCompletedEvent,
): InternalExecutionCompletedEvent {
	return {
		executionId: event.executionId,
		generation: safeProtocolNumber(event.generation, "execution generation"),
		outcome: mapExecutionOutcome(event.outcome),
		...(event.exitCode !== null ? { exitCode: event.exitCode } : {}),
		...(event.error ? { error: mapExecutionError(event.error) } : {}),
	};
}

export class AgentOs {
	#kernel: Kernel;
	readonly sidecar: AgentOsSidecar;
	private _durableSessionEventHandlers = new Map<
		string,
		Set<(entry: SessionStreamEntry) => void>
	>();
	private _agentExitHandlers = new Map<string, Set<AgentExitHandler>>();
	private _processes = new Map<
		number,
		{
			proc: ManagedProcess;
			command: string;
			args: string[];
			startedAtMs: number;
			retainEvents: boolean;
			events: ProcessOutputEvent[];
			nextSequence: number;
			signal?: ExecutionSignal;
			exit?: ProcessExit;
			outputHandlers: Set<(event: ProcessOutput) => void>;
			exitHandlers: Set<(event: ProcessExit) => void>;
		}
	>();
	private _languageProcesses = new Map<
		number,
		{
			executionId: string;
			descriptor: ProcessDescriptor;
			signal?: ExecutionSignal;
			exit?: ProcessExit;
			outputHandlers: Set<(event: ProcessOutput) => void>;
			exitHandlers: Set<(event: ProcessExit) => void>;
		}
	>();
	private _languageProcessIds = new Map<string, number>();
	private _executionOutputHandlers = new Map<
		string,
		Set<(event: InternalExecutionOutputEvent) => void>
	>();
	private _executionCompletedHandlers = new Map<
		string,
		Set<(event: InternalExecutionCompletedEvent) => void>
	>();
	private _shells = new Map<string, ShellEntry>();
	// Value is the recorded exit code (undefined until/unless the exit
	// resolves) so waitShell can still report it after the entry is dropped.
	private _closedShellIds = new BoundedSet<string, number>(
		CLOSED_SHELL_ID_RETENTION_LIMIT,
	);
	private _pendingShellExitPromises = new Set<Promise<number>>();
	private _shellCounter = 0;
	private _acpTerminals = new Map<string, AcpTerminalEntry>();
	private _acpTerminalCounter = 0;
	private _softwareRoots: SoftwareRoot[];
	private _cronManager!: CronManager;
	private _bindings: Bindings[] = [];
	private _bindingReference = "";
	private _permissions: Permissions = allowAll;
	private _hostMounts: HostMountInfo[];
	private _env: Record<string, string>;
	private _rootFilesystem: VirtualFileSystem;
	private _sidecarLease: AgentOsSidecarVmLease<AgentOsVmAdmin> | null = null;
	private readonly _sidecarClient: SidecarProcess;
	private readonly _sidecarSession: AuthenticatedSession;
	private readonly _sidecarVm: CreatedVm;
	private readonly _disposeSidecarEventListener: () => void;
	private readonly _agentStderrHandler?: AgentStderrHandler;
	private readonly _agentExitHandler?: AgentExitHandler;
	private readonly _limitWarningHandler?: LimitWarningHandler;
	private readonly _disposeHooks: Array<() => void | Promise<void>> = [];

	/**
	 * Execute commands and manage child processes in the VM.
	 *
	 * `exec` evaluates a command with the configured shell, while `execFile`
	 * invokes an executable directly with an argv array.
	 */
	readonly process = {
		exec: this._exec.bind(this),
		execFile: this._execFile.bind(this),
		spawn: this._spawnProcess.bind(this),
		get: this._getProcess.bind(this),
		list: this._listProcesses.bind(this),
		tree: this._processTree.bind(this),
		wait: this._waitProcess.bind(this),
		signal: this._signalProcess.bind(this),
		kill: this._killProcess.bind(this),
		writeStdin: this._writeProcessStdin.bind(this),
		closeStdin: this._closeProcessStdin.bind(this),
		resizePty: this._resizeProcessPty.bind(this),
		readOutput: this._readProcessOutput.bind(this),
	};

	readonly javascript = {
		execute: this._executeJavaScript.bind(this),
		evaluate: this._evaluateJavaScript.bind(this),
		executeFile: this._executeJavaScriptFile.bind(this),
		spawn: this._spawnJavaScript.bind(this),
		spawnFile: this._spawnJavaScriptFile.bind(this),
		npm: {
			install: this._installNpmPackages.bind(this),
			runScript: this._executeNpmScript.bind(this),
			runPackage: this._executeNpmPackage.bind(this),
		},
	};

	readonly typescript = {
		execute: this._executeTypeScript.bind(this),
		evaluate: this._evaluateTypeScript.bind(this),
		executeFile: this._executeTypeScriptFile.bind(this),
		spawn: this._spawnTypeScript.bind(this),
		spawnFile: this._spawnTypeScriptFile.bind(this),
		check: this._checkTypeScript.bind(this),
		checkProject: this._checkTypeScriptProject.bind(this),
	};

	readonly python = {
		execute: this._executePython.bind(this),
		evaluate: this._evaluatePython.bind(this),
		executeFile: this._executePythonFile.bind(this),
		executeModule: this._executePythonModule.bind(this),
		spawn: this._spawnPython.bind(this),
		spawnFile: this._spawnPythonFile.bind(this),
		spawnModule: this._spawnPythonModule.bind(this),
		install: this._installPythonPackages.bind(this),
	};

	readonly contexts = {
		get: this._getContext.bind(this),
		list: this._listContexts.bind(this),
		reset: this._resetContext.bind(this),
		delete: this._deleteContext.bind(this),
	};

	readonly terminal = {
		open: this._openTerminal.bind(this),
		write: this._writeTerminal.bind(this),
		resize: this._resizeTerminal.bind(this),
		wait: this._waitTerminal.bind(this),
		close: this._closeTerminal.bind(this),
	};

	readonly filesystem = {
		readFile: this._readFile.bind(this),
		writeFile: this._writeFile.bind(this),
		readFiles: this._readFiles.bind(this),
		writeFiles: this._writeFiles.bind(this),
		stat: this._stat.bind(this),
		mkdir: this._mkdir.bind(this),
		readdir: this._readdir.bind(this),
		readdirEntries: this._readdirEntries.bind(this),
		readdirRecursive: this._readdirRecursive.bind(this),
		exists: this._exists.bind(this),
		move: this._move.bind(this),
		remove: this._remove.bind(this),
		export: this._exportRootFilesystem.bind(this),
		mount: this._mountFs.bind(this),
		unmount: this._unmountFs.bind(this),
		listMounts: this._listMounts.bind(this),
	};

	readonly network = {
		httpRequest: this._httpRequest.bind(this),
	};

	readonly software = {
		list: this._listSoftware.bind(this),
		link: this._linkSoftware.bind(this),
	};

	readonly agents = {
		list: this._listAgents.bind(this),
	};

	readonly sessions = {
		open: this._openSession.bind(this),
		get: this._getSession.bind(this),
		list: this._listSessions.bind(this),
		delete: this._deleteSession.bind(this),
		unload: this._unloadSession.bind(this),
		prompt: this._prompt.bind(this),
		cancelPrompt: this._cancelPrompt.bind(this),
		respondPermission: this._respondPermission.bind(this),
		readHistory: this._readHistory.bind(this),
		getConfig: this._getSessionConfig.bind(this),
		setConfigOption: this._setSessionConfigOption.bind(this),
		getCapabilities: this._getSessionCapabilities.bind(this),
		getAgentInfo: this._getSessionAgentInfo.bind(this),
	};

	readonly cron = {
		schedule: this._scheduleCron.bind(this),
		list: this._listCronJobs.bind(this),
		cancel: this._cancelCronJob.bind(this),
	};

	private constructor(
		kernel: Kernel,
		sidecar: AgentOsSidecar,
		softwareRoots: SoftwareRoot[],
		hostMounts: HostMountInfo[],
		env: Record<string, string>,
		rootFilesystem: VirtualFileSystem,
		sidecarClient: SidecarProcess,
		sidecarSession: AuthenticatedSession,
		sidecarVm: CreatedVm,
		agentStderrHandler?: AgentStderrHandler,
		agentExitHandler?: AgentExitHandler,
		limitWarningHandler?: LimitWarningHandler,
	) {
		this.#kernel = kernel;
		this.sidecar = sidecar;
		this._softwareRoots = softwareRoots;
		this._hostMounts = hostMounts;
		this._env = env;
		this._rootFilesystem = rootFilesystem;
		this._sidecarClient = sidecarClient;
		this._sidecarSession = sidecarSession;
		this._sidecarVm = sidecarVm;
		this._agentStderrHandler = agentStderrHandler;
		this._agentExitHandler = agentExitHandler;
		this._limitWarningHandler = limitWarningHandler;
		this._disposeSidecarEventListener = this._sidecarClient.onEvent((event) => {
			this._handleSidecarEvent(event);
		});
		agentOsRuntimeAdmins.set(this, {
			kernel,
			rootView: rootFilesystem,
			env,
			sidecar,
		});
	}

	static async createSidecar(
		options: AgentOsCreateSidecarOptions = {},
	): Promise<AgentOsSidecar> {
		return createAgentOsSidecarInternal(options);
	}

	static async getSharedSidecar(
		options: AgentOsSharedSidecarOptions = {},
	): Promise<AgentOsSidecar> {
		return getSharedAgentOsSidecarInternal(options);
	}

	static async create(options?: AgentOsOptions): Promise<AgentOs> {
		options = parseAgentOsOptions(options);
		// Default software is FULLY DYNAMIC: this package's own NON-agent
		// @agentos-software/* dependencies (e.g. common), each default-exporting
		// its registry-built descriptor. Agent packages are NOT projected here —
		// openSession({ agent: id }) links the matching agent dependency into the running
		// VM on first use, so agent closures (and pi's V8 snapshot bundle) only
		// enter VMs that run them. Unbuilt packages throw with build
		// instructions; opt out via defaultSoftware: false.
		const defaultSoftware =
			options?.defaultSoftware === false ? [] : resolveDefaultSoftware();
		const software: unknown[] =
			options?.defaultSoftware === false
				? (options.software ?? [])
				: [...defaultSoftware, ...(options?.software ?? [])];
		// Packages are projected by the SIDECAR: the client forwards only the
		// package `path` over `configureVm` and the sidecar reads metadata from
		// the packed vbare manifest (chunk1 of the `.aospkg`).
		const flatSoftware = software.flat();
		// Honor the AgentOsOptions.defaultSoftware contract ("entries already present
		// in `software` are not duplicated"): the default bundle and an explicitly
		// passed one resolve to the same package paths, so dedup by path. Without
		// this the sidecar rejects the second projection with a duplicate-command
		// error (e.g. coreutils' `[`).
		const seenPackagePaths = new Set<string>();
		const sidecarPackages = flatSoftware.flatMap((entry) => {
			const ref = normalizePackageRef(entry);
			if (!ref || seenPackagePaths.has(ref.path)) {
				return [];
			}
			seenPackagePaths.add(ref.path);
			return [{ path: ref.path }];
		});
		// All package software is projected into `/opt/agentos` by the sidecar. The
		// client stages nothing host-side and parses NO package manifests: the
		// sidecar owns agent resolution, agent enumeration, and agent snapshot
		// bundle loading from the projected package dirs.
		const localMounts = await resolveCompatLocalMounts(options?.mounts);
		if (options?.bindings && options.bindings.length > 0) {
			validateBindings(options.bindings);
		}

		// Resolve the sidecar handle before starting an external sandbox so option
		// validation failures cannot leak provider resources.
		const sidecar = resolveAgentOsSidecar(options?.sidecar);
		options = await resolveSandboxOptions(options);
		const sandboxDisposeHooks = getSandboxDisposeHooks(options);
		const bindings = options.bindings;

		const createVmAdmin = async (): Promise<AgentOsVmAdmin> => {
			// The `/opt/agentos` projection is built by the sidecar from the
			// forwarded `packages` (it owns the staging dir + read-only mount, and
			// runtime `linkSoftware` appends to that live dir). The client no longer
			// stages packages host-side.
			const bindingBootstrapCommands = collectBindingBootstrapCommands(
				bindings ?? [],
			);
			const bootstrapCommands = [
				...RUNTIME_BOOTSTRAP_COMMANDS,
				...bindingBootstrapCommands,
			];
			const bootstrapLower = createKernelBootstrapLower(
				options?.rootFilesystem,
			);
			let bindingReference = "";
			let rootBridge: NativeSidecarKernelProxy | null = null;
			let kernel: Kernel | null = null;
			let client: SidecarProcess | null = null;
			let createdNativeVm: CreatedVm | null = null;
			let nativeSession: AuthenticatedSession | null = null;
			let cleanedUp = false;

			const cleanup = async (): Promise<void> => {
				if (cleanedUp) {
					return;
				}
				cleanedUp = true;
			};

			try {
				const env: Record<string, string> = getBaseEnvironment();
				// Guest command paths. The sidecar owns the `/opt/agentos` projection and
				// reports the exact projected package commands after `configureVm`.
				// Binding-shim commands are added below.
				const commandGuestPaths = new Map<string, string>();
				const { sidecarMounts, hostMounts, hostPathMappings } =
					collectSidecarMountPlan({
						mounts: options?.mounts,
					});
				// Reuse the sidecar handle's single shared native process; this VM
				// becomes another tenant of it rather than spawning its own process.
				const shared = await ensureSharedSidecarNativeProcess(sidecar);
				client = shared.client;
				const session = shared.session;
				nativeSession = session;
				const hostPermissions = options?.permissions ?? {
					...allowAll,
					binding: "allow",
				};
				const sidecarPermissions =
					serializePermissionsForSidecar(hostPermissions);
				const createVmConfig: CreateVmConfig = {
					env,
					database: options?.database,
					...(options?.user ? { user: options.user } : {}),
					rootFilesystem: serializeRootFilesystemForSidecar(
						options?.rootFilesystem,
						bootstrapLower,
					),
					permissions: sidecarPermissions,
					limits: options?.limits,
					loopbackExemptPorts: options?.loopbackExemptPorts ?? [],
					bootstrapCommands,
					...(options?.rootFilesystem?.type === "native"
						? {
								nativeRoot: {
									plugin: {
										id: options.rootFilesystem.plugin.id,
										config: options.rootFilesystem.plugin.config ?? {},
									},
									readOnly: options.rootFilesystem.readOnly ?? false,
								},
							}
						: {}),
					// 0.3: the Node builtin allow-list moved from configureVm to
					// VM creation. `undefined` => engine default allow-list;
					// `[]` => deny all; `[..]` => exactly those. Platform and
					// module resolution keep their engine defaults (full Node
					// emulation), matching the prior behavior where Agent OS only
					// constrained the builtin allow-list.
					...(options?.allowedNodeBuiltins !== undefined ||
					options?.highResolutionTime !== undefined
						? {
								jsRuntime: {
									platform: "node" as const,
									moduleResolution: "node" as const,
									...(options?.allowedNodeBuiltins !== undefined
										? { allowedBuiltins: options.allowedNodeBuiltins }
										: {}),
									...(options?.highResolutionTime !== undefined
										? { highResolutionTime: options.highResolutionTime }
										: {}),
								},
							}
						: {}),
				};
				const nativeVm = await client.createVm(session, {
					runtime: "java_script",
					config: createVmConfig,
				});
				createdNativeVm = nativeVm;
				// Scope the readiness wait to THIS VM's ownership; on a shared process
				// other VMs are emitting their own lifecycle events concurrently.
				await client.waitForEvent(
					(event) =>
						event.payload.type === "vm_lifecycle" &&
						event.payload.state === "ready" &&
						event.ownership.scope === "vm" &&
						event.ownership.vm_id === nativeVm.vmId,
					10_000,
				);
				const configuredVm = await client.configureVm(session, nativeVm, {
					mounts: sidecarMounts,
					permissions: sidecarPermissions,
					commandPermissions: {},
					loopbackExemptPorts: options?.loopbackExemptPorts,
					packages: sidecarPackages,
					packagesMountAt: OPT_AGENTOS_ROOT,
					bindingShimCommands: bindingBootstrapCommands,
				});
				for (const command of configuredVm.projectedCommands) {
					commandGuestPaths.set(command.name, command.guestPath);
				}
				if (bindings && bindings.length > 0) {
					bindingReference = await registerBindingsOnSidecar(
						client,
						session,
						nativeVm,
						bindings,
					);
					commandGuestPaths.set("agentos", "/bin/agentos");
					for (const bindingCollection of bindings) {
						commandGuestPaths.set(
							`agentos-${bindingCollection.name}`,
							`/bin/agentos-${bindingCollection.name}`,
						);
					}
				}

				rootBridge = new NativeSidecarKernelProxy({
					client,
					session,
					vm: nativeVm,
					env,
					cwd: "/workspace",
					localMounts,
					sidecarMounts,
					permissions: sidecarPermissions,
					commandPermissions: {},
					loopbackExemptPorts: options?.loopbackExemptPorts,
					// Retained for runtime mount reconfigures: `configure_vm` is
					// replace-on-write for the whole payload, so post-boot mountFs
					// must resend the boot packages and binding shims.
					packages: sidecarPackages,
					packagesMountAt: OPT_AGENTOS_ROOT,
					bindingShimCommands: bindingBootstrapCommands,
					commandGuestPaths,
					onDispose: cleanup,
					// The native process is owned by the AgentOsSidecar handle and
					// shared across VMs; disposing this VM must not kill the process.
					ownsClient: false,
				});
				if (options?.rootFilesystem?.type !== "native") {
					await bootstrapLiveBootstrapDirectories(
						client,
						session,
						nativeVm,
						options?.rootFilesystem,
					);
				}

				kernel = rootBridge as unknown as Kernel;
				const snapshotClient = client;

				return {
					env,
					hostMounts,
					kernel,
					rootView: rootBridge.createRootView(),
					sidecarMounts,
					sidecarPermissions,
					commandPermissions: {},
					loopbackExemptPorts: options?.loopbackExemptPorts,
					sidecarClient: client,
					sidecarSession: session,
					sidecarVm: nativeVm,
					permissions: hostPermissions,
					snapshotRootFilesystem: async (maxBytes) =>
						createSnapshotExport(
							convertSidecarRootSnapshotEntries(
								await snapshotClient.snapshotRootFilesystem(
									session,
									nativeVm,
									maxBytes,
								),
							),
						),
					bindings: bindings ?? [],
					bindingReference,
					async dispose() {
						if (kernel) {
							const currentKernel = kernel;
							kernel = null;
							await currentKernel.dispose();
						}
						if (rootBridge) {
							const currentRootBridge = rootBridge;
							rootBridge = null;
							await currentRootBridge.dispose();
							return;
						}
						await cleanup();
					},
				};
			} catch (error) {
				// The native process is shared and owned by the sidecar handle, so
				// never dispose the client here — only tear down this VM's resources.
				if (kernel) {
					await kernel.dispose().catch(() => {});
				}
				if (rootBridge) {
					await rootBridge.dispose().catch(() => {});
				} else {
					if (createdNativeVm && nativeSession && client) {
						await client
							.disposeVm(nativeSession, createdNativeVm)
							.catch(() => {});
					}
					await cleanup();
				}
				throw error;
			}
		};

		let sidecarLease: AgentOsSidecarVmLease<AgentOsVmAdmin> | null = null;

		try {
			sidecarLease = await leaseAgentOsSidecarVm(sidecar, {
				createVm: async () => createVmAdmin(),
			});
			const vmAdmin = sidecarLease.admin;

			const vm = new AgentOs(
				vmAdmin.kernel,
				sidecar,
				[],
				vmAdmin.hostMounts,
				vmAdmin.env,
				vmAdmin.rootView,
				vmAdmin.sidecarClient,
				vmAdmin.sidecarSession,
				vmAdmin.sidecarVm,
				options?.onAgentStderr ?? defaultAgentStderrHandler,
				options?.onAgentExit ?? defaultAgentExitHandler,
				options?.onLimitWarning,
			);
			vm._sidecarLease = sidecarLease;
			vm._bindings = vmAdmin.bindings;
			vm._bindingReference = vmAdmin.bindingReference;
			vm._permissions = vmAdmin.permissions;
			vm._disposeHooks.push(...sandboxDisposeHooks);
			vm._installSidecarRequestHandler();
			vm._cronManager = new CronManager(
				vm,
				options?.scheduleDriver ?? new TimerScheduleDriver(),
			);

			return vm;
		} catch (error) {
			const cleanupErrors: unknown[] = [];
			try {
				await sidecarLease?.dispose();
			} catch (disposeError) {
				cleanupErrors.push(disposeError);
			}
			const sandboxCleanupResults = await Promise.allSettled(
				sandboxDisposeHooks.map((hook) => hook()),
			);
			cleanupErrors.push(
				...sandboxCleanupResults.flatMap((result) =>
					result.status === "rejected" ? [result.reason] : [],
				),
			);
			if (cleanupErrors.length > 0) {
				throw new AggregateError(
					[error, ...cleanupErrors],
					"AgentOS VM creation and cleanup failed",
				);
			}
			throw error;
		}
	}

	async createContext(contextId: string): Promise<void> {
		const response = await this._sidecarClient.sendVmRequest(
			this._sidecarSession,
			this._sidecarVm,
			{ type: "create_context", request: { contextId } },
		);
		if (response.type !== "execution_descriptor") {
			throw new Error(`unexpected createContext response: ${response.type}`);
		}
	}

	private async _executionOperation(
		payload: Parameters<SidecarProcess["sendVmRequest"]>[2],
		options: LanguageExecutionOptions,
		background = false,
	): Promise<CodeExecutionResult | InternalBackgroundExecution> {
		if (options.signal?.aborted) {
			throw options.signal.reason ?? new DOMException("Aborted", "AbortError");
		}
		let executionId: string | undefined;
		const earlyOutput: InternalExecutionOutputEvent[] = [];
		const completedBeforeAdmission = new Set<string>();
		let resolveCompletion: (() => void) | undefined;
		const completion = new Promise<void>((resolve) => {
			resolveCompletion = resolve;
		});
		let warnedAboutEarlyEvents = false;
		const warnAboutEarlyEvents = () => {
			if (warnedAboutEarlyEvents) return;
			warnedAboutEarlyEvents = true;
			console.warn(
				"[agentos] execution admission received more than 1024 early events; use process.readOutput() to replay any omitted output",
			);
		};
		const deliverOutput = (event: InternalExecutionOutputEvent) => {
			if (event.channel === "stderr") options.onStderr?.(event.chunk);
			else options.onStdout?.(event.chunk);
		};
		const unsubscribeOutput = this._onExecutionOutput("*", (event) => {
			if (executionId === undefined) {
				if (earlyOutput.length < 1_024) earlyOutput.push(event);
				else warnAboutEarlyEvents();
				return;
			}
			if (event.executionId === executionId) deliverOutput(event);
		});
		let completed = false;
		const unsubscribeCompleted = this._onExecutionCompleted("*", (event) => {
			if (executionId === undefined) {
				if (completedBeforeAdmission.size < 1_024) {
					completedBeforeAdmission.add(event.executionId);
				} else warnAboutEarlyEvents();
				return;
			}
			if (event.executionId === executionId) {
				completed = true;
				resolveCompletion?.();
			}
		});
		let response: Awaited<ReturnType<SidecarProcess["sendVmRequest"]>>;
		try {
			response = await this._sidecarClient.sendVmRequest(
				this._sidecarSession,
				this._sidecarVm,
				payload,
			);
		} catch (error) {
			unsubscribeOutput();
			unsubscribeCompleted();
			throw error;
		}
		if (response.type !== "execution_accepted") {
			unsubscribeOutput();
			unsubscribeCompleted();
			throw new Error(`unexpected execution response: ${response.type}`);
		}
		const descriptor = response.response.execution;
		executionId = response.response.operationId;
		for (const event of earlyOutput) {
			if (event.executionId === executionId) deliverOutput(event);
		}
		completed = completedBeforeAdmission.has(executionId);
		if (completed) resolveCompletion?.();
		const abort = () => {
			// Admitted executions report cancellation through their structured result.
			void this._cancelExecution(response.response.operationId).catch(
				(error) => {
					console.error("[agentos] failed to cancel aborted execution", error);
				},
			);
		};
		options.signal?.addEventListener("abort", abort, { once: true });
		if (options.signal?.aborted) abort();
		const cleanup = () => {
			unsubscribeOutput();
			unsubscribeCompleted();
			options.signal?.removeEventListener("abort", abort);
		};
		if (background) {
			if (!descriptor || descriptor.pid === null) {
				cleanup();
				throw new Error("spawned execution admission returned no process id");
			}
			if (completed) cleanup();
			else {
				const unsubscribeBackgroundCompletion = this._onExecutionCompleted(
					descriptor.executionId,
					() => {
						unsubscribeBackgroundCompletion();
						cleanup();
					},
				);
			}
			return {
				executionId: descriptor.executionId,
				pid: descriptor.pid,
				completed,
				completion,
				createdAtMs: safeProtocolNumber(
					descriptor.createdAtMs,
					"process startedAtMs",
				),
			};
		}
		try {
			await completion;
			return await this._waitExecutionResult(response.response.operationId);
		} finally {
			cleanup();
		}
	}

	private async _spawnLanguageOperation(
		buildPayload: (
			options: LanguageExecutionOptions,
			executionId: string,
		) => Parameters<SidecarProcess["sendVmRequest"]>[2],
		options: LanguageSpawnOptions,
		language: "javascript" | "python",
	): Promise<ProcessDescriptor> {
		if ("contextId" in options) {
			throw new Error(
				"contextId is not supported for spawned processes; contexts are for attached operations",
			);
		}
		const executionId = `process-${randomUUID()}`;
		const internalOptions: LanguageExecutionOptions = {
			...options,
			output: options.output,
		};
		const admitted = (await this._executionOperation(
			buildPayload(internalOptions, executionId),
			internalOptions,
			true,
		)) as InternalBackgroundExecution;
		const descriptor: ProcessDescriptor = {
			pid: admitted.pid,
			state: "running",
			language,
			startedAtMs: admitted.createdAtMs,
		};
		this._languageProcesses.set(admitted.pid, {
			executionId: admitted.executionId,
			descriptor,
			outputHandlers: new Set(),
			exitHandlers: new Set(),
		});
		this._languageProcessIds.set(admitted.executionId, admitted.pid);
		const reconcileCompletion = async () => {
			if (!this._languageProcesses.get(admitted.pid)?.exit) {
				await this._waitProcess(admitted.pid);
			}
		};
		if (admitted.completed) await reconcileCompletion();
		else {
			void admitted.completion
				.then(reconcileCompletion)
				.catch((error) =>
					console.error(
						"[agentos] failed to reconcile spawned process completion",
						error,
					),
				);
		}
		if (options.signal) {
			const abort = () => {
				void this._killProcess(admitted.pid);
			};
			options.signal.addEventListener("abort", abort, { once: true });
		}
		return this._languageProcesses.get(admitted.pid)?.descriptor ?? descriptor;
	}

	private async _exec(
		command: string,
		options: LanguageExecutionOptions = {},
	): Promise<CodeExecutionResult> {
		return (await this._executionOperation(
			{
				type: "shell_execution",
				request: { process: processExecutionOptions(options), command },
			},
			options,
		)) as CodeExecutionResult;
	}

	private async _execFile(
		command: string,
		args: readonly string[] = [],
		options: Omit<LanguageExecutionOptions, "args"> = {},
	): Promise<CodeExecutionResult> {
		const operationOptions = { ...options, args: [...args] };
		return (await this._executionOperation(
			{
				type: "argv_execution",
				request: {
					process: processExecutionOptions(operationOptions),
					command,
				},
			},
			operationOptions,
		)) as CodeExecutionResult;
	}

	/** @deprecated Use `process.exec()` for lifecycle-aware execution. */
	async exec(
		command: string,
		options?: KernelExecOptions,
	): Promise<KernelExecResult> {
		return this.#kernel.exec(command, options);
	}

	/** @deprecated Use `process.execFile()` for lifecycle-aware execution. */
	async execArgv(
		command: string,
		args: readonly string[] = [],
		options?: KernelExecOptions,
	): Promise<KernelExecResult> {
		const kernel = this.#kernel as unknown as {
			execArgv(
				command: string,
				args?: readonly string[],
				options?: KernelExecOptions,
			): Promise<KernelExecResult>;
		};
		return kernel.execArgv(command, args, options);
	}

	private async _executeJavaScript(
		source: string,
		options: JavaScriptExecutionOptions = {},
	): Promise<CodeExecutionResult> {
		return (await this._executionOperation(
			{
				type: "javascript_execution",
				request: {
					process: processExecutionOptions(options),
					source,
					format:
						options.format === "commonjs"
							? executionProtocol.JavaScriptModuleFormat.CommonJs
							: executionProtocol.JavaScriptModuleFormat.Module,
					filePath: options.filePath ?? null,
					inputs: options.inputs ? JSON.stringify(options.inputs) : null,
				},
			},
			options,
		)) as CodeExecutionResult;
	}

	private async _evaluationOperation<T>(
		payload: Parameters<SidecarProcess["sendVmRequest"]>[2],
		options: LanguageExecutionOptions,
	): Promise<CodeEvaluationResult<T>> {
		const result = (await this._executionOperation(
			payload,
			options,
		)) as CodeExecutionResult;
		if (result.outcome !== "succeeded") return result;
		const value = (result as MappedExecutionResult).evaluationValue;
		if (value === undefined) {
			throw new Error(
				"successful evaluation did not return a JSON-serializable value",
			);
		}
		return { ...result, value: value as T } as CodeEvaluationResult<T>;
	}

	private _evaluateJavaScript<T = JsonValue>(
		expression: string,
		options?: JavaScriptEvaluationOptions,
	): Promise<CodeEvaluationResult<T>>;
	private async _evaluateJavaScript<T = JsonValue>(
		expression: string,
		options: JavaScriptEvaluationOptions = {},
	): Promise<CodeEvaluationResult<T>> {
		return this._evaluationOperation<T>(
			{
				type: "javascript_evaluation",
				request: {
					process: processExecutionOptions(options),
					expression,
					format:
						options.format === "commonjs"
							? executionProtocol.JavaScriptModuleFormat.CommonJs
							: executionProtocol.JavaScriptModuleFormat.Module,
					filePath: options.filePath ?? null,
					inputs: options.inputs ? JSON.stringify(options.inputs) : null,
				},
			},
			options,
		);
	}

	private async _executeJavaScriptFile(
		path: string,
		options: LanguageExecutionOptions = {},
	): Promise<CodeExecutionResult> {
		return (await this._executionOperation(
			{
				type: "javascript_file_execution",
				request: { process: processExecutionOptions(options), path },
			},
			options,
		)) as CodeExecutionResult;
	}

	private _spawnJavaScript(
		source: string,
		options: LanguageSpawnOptions = {},
	): Promise<ProcessDescriptor> {
		return this._spawnLanguageOperation(
			(operationOptions, executionId) => ({
				type: "javascript_execution",
				request: {
					process: processExecutionOptions(operationOptions, {
						background: true,
						executionId,
					}),
					source,
					format: executionProtocol.JavaScriptModuleFormat.Module,
					filePath: null,
					inputs: null,
				},
			}),
			options,
			"javascript",
		);
	}

	private _spawnJavaScriptFile(
		path: string,
		options: LanguageSpawnOptions = {},
	): Promise<ProcessDescriptor> {
		return this._spawnLanguageOperation(
			(operationOptions, executionId) => ({
				type: "javascript_file_execution",
				request: {
					process: processExecutionOptions(operationOptions, {
						background: true,
						executionId,
					}),
					path,
				},
			}),
			options,
			"javascript",
		);
	}

	private async _executeTypeScript(
		source: string,
		options: TypeScriptExecutionOptions = {},
	): Promise<CodeExecutionResult> {
		return (await this._executionOperation(
			{
				type: "typescript_execution",
				request: {
					process: processExecutionOptions(options),
					source,
					filePath: options.filePath ?? null,
					tsconfigPath: options.tsconfigPath ?? null,
					compilerOptions: options.compilerOptions
						? JSON.stringify(options.compilerOptions)
						: null,
					inputs: options.inputs ? JSON.stringify(options.inputs) : null,
				},
			},
			options,
		)) as CodeExecutionResult;
	}

	private _evaluateTypeScript<T = JsonValue>(
		expression: string,
		options?: TypeScriptEvaluationOptions,
	): Promise<CodeEvaluationResult<T>>;
	private async _evaluateTypeScript<T = JsonValue>(
		expression: string,
		options: TypeScriptEvaluationOptions = {},
	): Promise<CodeEvaluationResult<T>> {
		return this._evaluationOperation<T>(
			{
				type: "typescript_evaluation",
				request: {
					process: processExecutionOptions(options),
					expression,
					filePath: options.filePath ?? null,
					tsconfigPath: options.tsconfigPath ?? null,
					compilerOptions: options.compilerOptions
						? JSON.stringify(options.compilerOptions)
						: null,
					inputs: options.inputs ? JSON.stringify(options.inputs) : null,
				},
			},
			options,
		);
	}

	private async _executeTypeScriptFile(
		path: string,
		options: TypeScriptFileExecutionOptions = {},
	): Promise<CodeExecutionResult> {
		return (await this._executionOperation(
			{
				type: "typescript_file_execution",
				request: {
					process: processExecutionOptions(options),
					path,
					tsconfigPath: options.tsconfigPath ?? null,
					compilerOptions: options.compilerOptions
						? JSON.stringify(options.compilerOptions)
						: null,
				},
			},
			options,
		)) as CodeExecutionResult;
	}

	private _spawnTypeScript(
		source: string,
		options: LanguageSpawnOptions = {},
	): Promise<ProcessDescriptor> {
		return this._spawnLanguageOperation(
			(operationOptions, executionId) => ({
				type: "typescript_execution",
				request: {
					process: processExecutionOptions(operationOptions, {
						background: true,
						executionId,
					}),
					source,
					filePath: null,
					tsconfigPath: null,
					compilerOptions: null,
					inputs: null,
				},
			}),
			options,
			"javascript",
		);
	}

	private _spawnTypeScriptFile(
		path: string,
		options: LanguageSpawnOptions = {},
	): Promise<ProcessDescriptor> {
		return this._spawnLanguageOperation(
			(operationOptions, executionId) => ({
				type: "typescript_file_execution",
				request: {
					process: processExecutionOptions(operationOptions, {
						background: true,
						executionId,
					}),
					path,
					tsconfigPath: null,
					compilerOptions: null,
				},
			}),
			options,
			"javascript",
		);
	}

	private async _checkTypeScript(
		source: string,
		options: TypeScriptCheckOptions = {},
	): Promise<TypeScriptCheckResult> {
		const result = (await this._executionOperation(
			{
				type: "typescript_check",
				request: {
					identity: executionIdentity(options),
					output: executionOutput(options),
					source,
					cwd: options.cwd ?? null,
					filePath: options.filePath ?? null,
					tsconfigPath: options.tsconfigPath ?? null,
					compilerOptions: options.compilerOptions
						? JSON.stringify(options.compilerOptions)
						: null,
					timeoutMs: protocolTimeout(options.timeoutMs),
				},
			},
			options,
		)) as MappedExecutionResult;
		return mapTypeScriptCheckResult(result);
	}

	private async _checkTypeScriptProject(
		options: Omit<TypeScriptCheckOptions, "filePath" | "compilerOptions"> = {},
	): Promise<TypeScriptCheckResult> {
		const result = (await this._executionOperation(
			{
				type: "typescript_project_check",
				request: {
					identity: executionIdentity(options),
					output: executionOutput(options),
					cwd: options.cwd ?? null,
					tsconfigPath: options.tsconfigPath ?? null,
					timeoutMs: protocolTimeout(options.timeoutMs),
				},
			},
			options,
		)) as MappedExecutionResult;
		return mapTypeScriptCheckResult(result);
	}

	private async _installNpmPackages(
		options?: NpmProjectInstallOptions,
	): Promise<CodeExecutionResult>;
	private async _installNpmPackages(
		packages: string | string[],
		options?: NpmPackageInstallOptions,
	): Promise<CodeExecutionResult>;
	private async _installNpmPackages(
		packagesOrOptions: string | string[] | NpmProjectInstallOptions = {},
		maybeOptions: NpmPackageInstallOptions = {},
	): Promise<CodeExecutionResult> {
		if (
			typeof packagesOrOptions === "string" ||
			Array.isArray(packagesOrOptions)
		) {
			const packages = Array.isArray(packagesOrOptions)
				? packagesOrOptions
				: [packagesOrOptions];
			return (await this._executionOperation(
				{
					type: "npm_package_install",
					request: {
						identity: executionIdentity(maybeOptions),
						output: executionOutput(maybeOptions),
						cwd: maybeOptions.cwd ?? null,
						env: maybeOptions.env
							? new Map(Object.entries(maybeOptions.env))
							: null,
						timeoutMs: protocolTimeout(maybeOptions.timeoutMs),
						packages,
						dev: maybeOptions.dev ?? null,
						global: maybeOptions.global ?? null,
					},
				},
				maybeOptions,
			)) as CodeExecutionResult;
		}
		const options = packagesOrOptions;
		return (await this._executionOperation(
			{
				type: "npm_project_install",
				request: {
					identity: executionIdentity(options),
					output: executionOutput(options),
					cwd: options.cwd ?? null,
					env: options.env ? new Map(Object.entries(options.env)) : null,
					timeoutMs: protocolTimeout(options.timeoutMs),
					frozen: options.frozen ?? null,
				},
			},
			options,
		)) as CodeExecutionResult;
	}

	private async _executeNpmScript(
		script: string,
		options: LanguageExecutionOptions = {},
	): Promise<CodeExecutionResult> {
		return (await this._executionOperation(
			{
				type: "npm_script_execution",
				request: { process: processExecutionOptions(options), script },
			},
			options,
		)) as CodeExecutionResult;
	}

	private async _executeNpmPackage(
		packageSpec: string,
		options: LanguageExecutionOptions & { binary?: string } = {},
	): Promise<CodeExecutionResult> {
		return (await this._executionOperation(
			{
				type: "npm_package_execution",
				request: {
					process: processExecutionOptions(options),
					packageSpec,
					binary: options.binary ?? null,
				},
			},
			options,
		)) as CodeExecutionResult;
	}

	private async _executePython(
		source: string,
		options: InlineExecutionOptions = {},
	): Promise<CodeExecutionResult> {
		return (await this._executionOperation(
			{
				type: "python_execution",
				request: {
					process: processExecutionOptions(options),
					source,
					inputs: options.inputs ? JSON.stringify(options.inputs) : null,
				},
			},
			options,
		)) as CodeExecutionResult;
	}

	private _evaluatePython<T = JsonValue>(
		expression: string,
		options?: InlineExecutionOptions,
	): Promise<CodeEvaluationResult<T>>;
	private async _evaluatePython<T = JsonValue>(
		expression: string,
		options: InlineExecutionOptions = {},
	): Promise<CodeEvaluationResult<T>> {
		return this._evaluationOperation<T>(
			{
				type: "python_evaluation",
				request: {
					process: processExecutionOptions(options),
					expression,
					inputs: options.inputs ? JSON.stringify(options.inputs) : null,
				},
			},
			options,
		);
	}

	private async _executePythonFile(
		path: string,
		options: LanguageExecutionOptions = {},
	): Promise<CodeExecutionResult> {
		return (await this._executionOperation(
			{
				type: "python_file_execution",
				request: { process: processExecutionOptions(options), path },
			},
			options,
		)) as CodeExecutionResult;
	}

	private async _executePythonModule(
		module: string,
		options: LanguageExecutionOptions = {},
	): Promise<CodeExecutionResult> {
		return (await this._executionOperation(
			{
				type: "python_module_execution",
				request: { process: processExecutionOptions(options), module },
			},
			options,
		)) as CodeExecutionResult;
	}

	private _spawnPython(
		source: string,
		options: LanguageSpawnOptions = {},
	): Promise<ProcessDescriptor> {
		return this._spawnLanguageOperation(
			(operationOptions, executionId) => ({
				type: "python_execution",
				request: {
					process: processExecutionOptions(operationOptions, {
						background: true,
						executionId,
					}),
					source,
					inputs: null,
				},
			}),
			options,
			"python",
		);
	}

	private _spawnPythonFile(
		path: string,
		options: LanguageSpawnOptions = {},
	): Promise<ProcessDescriptor> {
		return this._spawnLanguageOperation(
			(operationOptions, executionId) => ({
				type: "python_file_execution",
				request: {
					process: processExecutionOptions(operationOptions, {
						background: true,
						executionId,
					}),
					path,
				},
			}),
			options,
			"python",
		);
	}

	private _spawnPythonModule(
		module: string,
		options: LanguageSpawnOptions = {},
	): Promise<ProcessDescriptor> {
		return this._spawnLanguageOperation(
			(operationOptions, executionId) => ({
				type: "python_module_execution",
				request: {
					process: processExecutionOptions(operationOptions, {
						background: true,
						executionId,
					}),
					module,
				},
			}),
			options,
			"python",
		);
	}

	private async _installPythonPackages(
		options?: PythonInstallOptions,
	): Promise<CodeExecutionResult>;
	private async _installPythonPackages(
		packages: string | string[],
		options?: PythonInstallOptions,
	): Promise<CodeExecutionResult>;
	private async _installPythonPackages(
		packagesOrOptions: string | string[] | PythonInstallOptions = {},
		maybeOptions: PythonInstallOptions = {},
	): Promise<CodeExecutionResult> {
		const packages =
			typeof packagesOrOptions === "string"
				? [packagesOrOptions]
				: Array.isArray(packagesOrOptions)
					? packagesOrOptions
					: [];
		const options =
			typeof packagesOrOptions === "string" || Array.isArray(packagesOrOptions)
				? maybeOptions
				: packagesOrOptions;
		return (await this._executionOperation(
			{
				type: "python_install",
				request: {
					identity: executionIdentity(options),
					output: executionOutput(options),
					cwd: options.cwd ?? null,
					env: options.env ? new Map(Object.entries(options.env)) : null,
					timeoutMs: protocolTimeout(options.timeoutMs),
					packages,
					upgrade: options.upgrade ?? null,
					requirementsFile: options.requirementsFile ?? null,
					indexUrl: options.indexUrl ?? null,
					extraIndexUrls: options.extraIndexUrls ?? [],
				},
			},
			options,
		)) as CodeExecutionResult;
	}

	private async _getContext(contextId: string): Promise<ContextDescriptor> {
		const response = await this._sidecarClient.sendVmRequest(
			this._sidecarSession,
			this._sidecarVm,
			{ type: "get_execution", request: { executionId: contextId } },
		);
		if (response.type !== "execution_descriptor") {
			throw new Error(`unexpected getContext response: ${response.type}`);
		}
		return mapExecutionDescriptor(response.response.execution);
	}

	private async _listContexts(): Promise<ContextDescriptor[]> {
		const response = await this._sidecarClient.sendVmRequest(
			this._sidecarSession,
			this._sidecarVm,
			{ type: "list_executions" },
		);
		if (response.type !== "execution_list") {
			throw new Error(`unexpected listContexts response: ${response.type}`);
		}
		return response.response.executions.map(mapExecutionDescriptor);
	}

	private async _waitExecutionResult(
		executionId: string,
	): Promise<MappedExecutionResult> {
		let resolveCompletion: (() => void) | undefined;
		const completion = new Promise<void>((resolve) => {
			resolveCompletion = resolve;
		});
		const unsubscribe = this._onExecutionCompleted(executionId, () => {
			resolveCompletion?.();
		});
		let response: Awaited<ReturnType<SidecarProcess["sendVmRequest"]>>;
		try {
			try {
				response = await this._sidecarClient.sendVmRequest(
					this._sidecarSession,
					this._sidecarVm,
					{ type: "wait_execution", request: { executionId } },
				);
			} catch (error) {
				if (
					!(error instanceof SidecarRejectedError) ||
					error.detail.code !== "execution_busy"
				) {
					throw error;
				}
				await completion;
				response = await this._sidecarClient.sendVmRequest(
					this._sidecarSession,
					this._sidecarVm,
					{ type: "wait_execution", request: { executionId } },
				);
			}
		} finally {
			unsubscribe();
		}
		if (response.type !== "execution_completed") {
			throw new Error(`unexpected waitExecution response: ${response.type}`);
		}
		return mapExecutionResult(response.response);
	}

	private async _cancelExecution(executionId: string): Promise<void> {
		await this._descriptorLifecycleRequest("cancel_execution", executionId);
	}

	private async _signalExecution(
		executionId: string,
		signal: ExecutionSignal,
	): Promise<void> {
		const response = await this._sidecarClient.sendVmRequest(
			this._sidecarSession,
			this._sidecarVm,
			{ type: "signal_execution", request: { executionId, signal } },
		);
		if (response.type !== "execution_descriptor") {
			throw new Error(`unexpected signalExecution response: ${response.type}`);
		}
	}

	private async _resetContext(contextId: string): Promise<void> {
		await this._descriptorLifecycleRequest("reset_execution", contextId);
	}

	private async _descriptorLifecycleRequest(
		type: "cancel_execution" | "reset_execution",
		executionId: string,
	): Promise<ContextDescriptor> {
		const payload =
			type === "cancel_execution"
				? ({ type, request: { executionId } } as const)
				: ({ type, request: { executionId } } as const);
		const response = await this._sidecarClient.sendVmRequest(
			this._sidecarSession,
			this._sidecarVm,
			payload,
		);
		if (response.type !== "execution_descriptor") {
			throw new Error(`unexpected ${type} response: ${response.type}`);
		}
		return mapExecutionDescriptor(response.response.execution);
	}

	private async _deleteContext(contextId: string): Promise<void> {
		const response = await this._sidecarClient.sendVmRequest(
			this._sidecarSession,
			this._sidecarVm,
			{ type: "delete_execution", request: { executionId: contextId } },
		);
		if (response.type !== "execution_deleted") {
			throw new Error(`unexpected deleteContext response: ${response.type}`);
		}
	}

	private async _writeExecutionStdin(
		executionId: string,
		data: string | Uint8Array,
	): Promise<void> {
		const response = await this._sidecarClient.sendVmRequest(
			this._sidecarSession,
			this._sidecarVm,
			{
				type: "write_execution_stdin",
				request: { executionId, chunk: executionBytes(data) as ArrayBuffer },
			},
		);
		if (response.type !== "execution_io") {
			throw new Error(
				`unexpected writeExecutionStdin response: ${response.type}`,
			);
		}
	}

	private async _closeExecutionStdin(executionId: string): Promise<void> {
		const response = await this._sidecarClient.sendVmRequest(
			this._sidecarSession,
			this._sidecarVm,
			{ type: "close_execution_stdin", request: { executionId } },
		);
		if (response.type !== "execution_io") {
			throw new Error(
				`unexpected closeExecutionStdin response: ${response.type}`,
			);
		}
	}

	private async _resizeExecutionPty(
		executionId: string,
		size: { cols: number; rows: number },
	): Promise<void> {
		const cols = protocolUnsigned(size.cols, 0xffff, "size.cols");
		const rows = protocolUnsigned(size.rows, 0xffff, "size.rows");
		const response = await this._sidecarClient.sendVmRequest(
			this._sidecarSession,
			this._sidecarVm,
			{
				type: "resize_execution_pty",
				request: { executionId, cols, rows },
			},
		);
		if (response.type !== "execution_io") {
			throw new Error(
				`unexpected resizeExecutionPty response: ${response.type}`,
			);
		}
	}

	private async _readExecutionOutput(
		executionId: string,
		options: { cursor?: string; limit?: number } = {},
	): Promise<InternalExecutionOutputPage> {
		const limit =
			options.limit === undefined
				? null
				: protocolUnsigned(options.limit, 0xffff_ffff, "limit");
		const response = await this._sidecarClient.sendVmRequest(
			this._sidecarSession,
			this._sidecarVm,
			{
				type: "read_execution_output",
				request: {
					executionId,
					cursor: options.cursor ?? null,
					limit,
				},
			},
		);
		if (response.type !== "execution_output_page") {
			throw new Error(
				`unexpected readExecutionOutput response: ${response.type}`,
			);
		}
		return {
			events: response.response.events.map(mapExecutionOutputEvent),
			nextCursor: response.response.nextCursor,
			hasMore: response.response.hasMore,
			truncated: response.response.truncated,
		};
	}

	private _onExecutionOutput(
		executionId: string,
		handler: (event: InternalExecutionOutputEvent) => void,
	): () => void {
		const handlers =
			this._executionOutputHandlers.get(executionId) ?? new Set();
		handlers.add(handler);
		this._executionOutputHandlers.set(executionId, handlers);
		return () => {
			handlers.delete(handler);
			if (handlers.size === 0)
				this._executionOutputHandlers.delete(executionId);
		};
	}

	private _onExecutionCompleted(
		executionId: string,
		handler: (event: InternalExecutionCompletedEvent) => void,
	): () => void {
		const handlers =
			this._executionCompletedHandlers.get(executionId) ?? new Set();
		handlers.add(handler);
		this._executionCompletedHandlers.set(executionId, handlers);
		return () => {
			handlers.delete(handler);
			if (handlers.size === 0)
				this._executionCompletedHandlers.delete(executionId);
		};
	}

	private _trackProcess(
		proc: ManagedProcess,
		command: string,
		args: string[],
		retainEvents: boolean,
		outputHandlers: Set<(event: ProcessOutput) => void>,
		exitHandlers: Set<(event: ProcessExit) => void>,
	): ProcessDescriptor {
		const entry = {
			proc,
			command,
			args,
			startedAtMs: Date.now(),
			retainEvents,
			events: [] as ProcessOutputEvent[],
			nextSequence: 0,
			signal: undefined as ExecutionSignal | undefined,
			exit: undefined as ProcessExit | undefined,
			outputHandlers,
			exitHandlers,
		};
		this._processes.set(proc.pid, entry);

		// NOTE: do NOT delete from `_processes` on exit — the public API contract
		// (getProcess/listProcesses/stopProcess, see process-management.test.ts)
		// requires exited processes to stay queryable (running:false, exitCode set).
		// `_processes` is a process table for this VM's lifetime; it is freed wholesale
		// in dispose(). (H5: the leak was that dispose() never cleared it.)
		void proc.wait().then((code) => {
			const exit: ProcessExit = {
				pid: proc.pid,
				outcome: entry.signal ? "signalled" : "exited",
				exitCode: code,
				...(entry.signal ? { signal: entry.signal } : {}),
			};
			entry.exit = exit;
			for (const h of exitHandlers) h(exit);
		});

		return {
			pid: proc.pid,
			state: "running",
			command,
			startedAtMs: entry.startedAtMs,
		};
	}

	private async _spawnProcess(
		command: string,
		args: string[] = [],
		options: SpawnOptions = {},
	): Promise<ProcessDescriptor> {
		const outputHandlers = new Set<(event: ProcessOutput) => void>();
		const exitHandlers = new Set<(event: ProcessExit) => void>();
		const recordOutput = (
			channel: "stdout" | "stderr",
			data: Uint8Array,
		): void => {
			const entry = this._processes.get(proc.pid);
			if (!entry?.retainEvents) return;
			if (entry.events.length >= PROCESS_OUTPUT_EVENT_LIMIT) {
				entry.events.shift();
			}
			entry.events.push({
				pid: proc.pid,
				sequence: entry.nextSequence++,
				channel,
				chunk: data,
				timestampMs: Date.now(),
			});
		};

		const proc = this.#kernel.spawn(command, args, {
			cwd: options.cwd,
			env: options.env,
			stdin: options.stdin,
			timeout: options.timeoutMs,
			streamStdin: true,
			onStdout: (data) => {
				recordOutput("stdout", data);
				options?.onStdout?.(data);
				for (const h of outputHandlers) {
					h({ pid: proc.pid, stream: "stdout", data });
				}
			},
			onStderr: (data) => {
				recordOutput("stderr", data);
				options?.onStderr?.(data);
				for (const h of outputHandlers) {
					h({ pid: proc.pid, stream: "stderr", data });
				}
			},
		});

		return this._trackProcess(
			proc,
			command,
			args,
			options.output?.retainEvents ?? false,
			outputHandlers,
			exitHandlers,
		);
	}

	spawn(
		command: string,
		args: string[] = [],
		options?: SpawnOptions,
	): Promise<ProcessDescriptor> {
		return this.process.spawn(command, args, options);
	}

	/** Write data to a process's stdin. */
	private _writeProcessStdin(
		pid: number,
		data: string | Uint8Array,
	): Promise<void> {
		const language = this._languageProcesses.get(pid);
		if (language) {
			return this._writeExecutionStdin(language.executionId, data);
		}
		const entry = this._processes.get(pid);
		if (!entry) throw new Error(`Process not found: ${pid}`);
		return entry.proc.writeStdin(data);
	}

	/** Close a process's stdin stream. */
	private _closeProcessStdin(pid: number): Promise<void> {
		const language = this._languageProcesses.get(pid);
		if (language) {
			return this._closeExecutionStdin(language.executionId);
		}
		const entry = this._processes.get(pid);
		if (!entry) throw new Error(`Process not found: ${pid}`);
		return entry.proc.closeStdin();
	}

	/** Subscribe to stdout and stderr from a process. */
	onProcessOutput(
		pid: number,
		handler: (event: ProcessOutput) => void,
	): () => void {
		const entry = this._processes.get(pid) ?? this._languageProcesses.get(pid);
		if (!entry) throw new Error(`Process not found: ${pid}`);
		entry.outputHandlers.add(handler);
		return () => {
			entry.outputHandlers.delete(handler);
		};
	}

	/** Subscribe to process exit. Returns an unsubscribe function. */
	onProcessExit(
		pid: number,
		handler: (event: ProcessExit) => void,
	): () => void {
		const entry = this._processes.get(pid) ?? this._languageProcesses.get(pid);
		if (!entry) throw new Error(`Process not found: ${pid}`);
		// If already exited, call immediately.
		if ("exit" in entry && entry.exit) {
			handler(entry.exit);
			return () => {};
		}
		entry.exitHandlers.add(handler);
		return () => {
			entry.exitHandlers.delete(handler);
		};
	}

	/** Wait for a process to exit. Returns the exit code. */
	private async _waitProcess(pid: number): Promise<ProcessExit> {
		const language = this._languageProcesses.get(pid);
		if (language) {
			if (language.exit) return language.exit;
			const result = await this._waitExecutionResult(language.executionId);
			const exit: ProcessExit = {
				pid,
				outcome:
					result.outcome === "timed_out"
						? "timed_out"
						: result.outcome === "cancelled"
							? "signalled"
							: "exited",
				...(result.exitCode !== undefined ? { exitCode: result.exitCode } : {}),
				...(language.signal ? { signal: language.signal } : {}),
			};
			language.exit = exit;
			language.descriptor = { ...language.descriptor, state: "exited" };
			for (const handler of language.exitHandlers) handler(exit);
			return exit;
		}
		const entry = this._processes.get(pid);
		if (!entry) throw new Error(`Process not found: ${pid}`);
		if (entry.exit) return entry.exit;
		const exitCode = await entry.proc.wait();
		return (
			entry.exit ?? {
				pid,
				outcome: "exited",
				exitCode,
			}
		);
	}

	private _assertSafeAbsolutePath(path: string): void {
		if (!path.startsWith("/")) {
			throw new Error(`Path must be absolute: ${path}`);
		}
		if (posixPath.normalize(path) !== path) {
			throw new Error(`Path must be normalized: ${path}`);
		}
	}

	private _assertWritableAbsolutePath(path: string): void {
		this._assertSafeAbsolutePath(path);
		if (path === "/proc" || path.startsWith("/proc/")) {
			throw new Error(`Path is read-only: ${path}`);
		}
	}

	private _vfs(): VirtualFileSystem {
		return (this.#kernel as unknown as { vfs: VirtualFileSystem }).vfs;
	}

	private async _readFile(path: string): Promise<Uint8Array> {
		this._assertSafeAbsolutePath(path);
		return this.#kernel.readFile(path);
	}

	private async _writeFile(
		path: string,
		content: string | Uint8Array,
	): Promise<void> {
		this._assertWritableAbsolutePath(path);
		return this.#kernel.writeFile(path, content);
	}

	private async _writeFiles(
		entries: BatchWriteEntry[],
	): Promise<BatchWriteResult[]> {
		const results: BatchWriteResult[] = [];
		for (const entry of entries) {
			try {
				this._assertWritableAbsolutePath(entry.path);
				// Create parent directories as needed
				const parentDir = entry.path.substring(0, entry.path.lastIndexOf("/"));
				if (parentDir) {
					await this._mkdirp(parentDir);
				}
				await this.#kernel.writeFile(entry.path, entry.content);
				results.push({ path: entry.path, success: true });
			} catch (err: unknown) {
				results.push({
					path: entry.path,
					success: false,
					error: err instanceof Error ? err.message : String(err),
				});
			}
		}
		return results;
	}

	private async _readFiles(paths: string[]): Promise<BatchReadResult[]> {
		const results: BatchReadResult[] = [];
		for (const path of paths) {
			try {
				this._assertSafeAbsolutePath(path);
				const content = await this.#kernel.readFile(path);
				results.push({ path, content });
			} catch (err: unknown) {
				results.push({
					path,
					content: null,
					error: err instanceof Error ? err.message : String(err),
				});
			}
		}
		return results;
	}

	/** Recursively create directories (mkdir -p). */
	private async _mkdirp(path: string): Promise<void> {
		this._assertWritableAbsolutePath(path);
		// `kernel.mkdir` is already recursive (it defaults to recursive=true on both
		// the native sidecar and compat kernels) and creating an existing directory is
		// a no-op, so a single call is sufficient. Do NOT probe each ancestor with
		// `exists()` first: on the native sidecar every read-side op
		// (exists/stat/readFile) triggers a full shadow-tree walk, so a per-component
		// exists() loop makes `mkdir -p` cost O(components * tree).
		await this.#kernel.mkdir(path);
	}

	private async _mkdir(
		path: string,
		options?: { recursive?: boolean },
	): Promise<void> {
		if (options?.recursive) {
			return this._mkdirp(path);
		}
		this._assertSafeAbsolutePath(path);
		return this.#kernel.mkdir(path);
	}

	private async _readdir(path: string): Promise<string[]> {
		this._assertSafeAbsolutePath(path);
		return this.#kernel.readdir(path);
	}

	private async _readdirEntries(path: string): Promise<ReaddirEntry[]> {
		this._assertSafeAbsolutePath(path);
		const entries = await this.#kernel.readdirRecursive(path, { maxDepth: 0 });
		return entries.map((entry) => ({
			name: entry.name,
			isDirectory: entry.isDirectory,
			isSymbolicLink: entry.isSymbolicLink,
		}));
	}

	private async _readdirRecursive(
		path: string,
		options?: ReaddirRecursiveOptions,
	): Promise<DirEntry[]> {
		this._assertSafeAbsolutePath(path);
		const exclude = options?.exclude ? new Set(options.exclude) : undefined;
		const entries = await this.#kernel.readdirRecursive(path, {
			maxDepth: options?.maxDepth,
		});
		const excludedPrefixes: string[] = [];
		const results: DirEntry[] = [];

		for (const entry of entries) {
			if (
				excludedPrefixes.some(
					(prefix) =>
						entry.path === prefix || entry.path.startsWith(`${prefix}/`),
				)
			) {
				continue;
			}
			if (exclude?.has(entry.name)) {
				if (entry.isDirectory && !entry.isSymbolicLink) {
					excludedPrefixes.push(entry.path);
				}
				continue;
			}
			results.push({
				path: entry.path,
				type: entry.isSymbolicLink
					? "symlink"
					: entry.isDirectory
						? "directory"
						: "file",
				size: entry.size,
			});
		}

		return results;
	}

	private async _stat(path: string): Promise<VirtualStat> {
		this._assertSafeAbsolutePath(path);
		return this.#kernel.stat(path);
	}

	private async _exists(path: string): Promise<boolean> {
		this._assertSafeAbsolutePath(path);
		return this.#kernel.exists(path);
	}

	private async _exportRootFilesystem(
		options: ExportRootFilesystemOptions,
	): Promise<RootSnapshotExport> {
		if (!Number.isSafeInteger(options.maxBytes) || options.maxBytes <= 0) {
			throw new RangeError("maxBytes must be a positive safe integer");
		}
		const nativeSnapshot = this._sidecarLease?.admin.snapshotRootFilesystem;
		const snapshot = nativeSnapshot
			? await nativeSnapshot(options.maxBytes)
			: createSnapshotExport(
					await snapshotVirtualFilesystem(this._rootFilesystem),
				);
		const size = Buffer.byteLength(JSON.stringify(snapshot));
		if (size > options.maxBytes) {
			throw new RangeError(
				`root filesystem export is ${size} bytes, limit is ${options.maxBytes}; raise maxBytes to export this filesystem`,
			);
		}
		return snapshot;
	}

	/**
	 * Mount a filesystem into the running VM. Resolves once the mount has been
	 * delivered to the native sidecar, so guest code can use it immediately
	 * after the returned promise settles; a delivery failure rejects instead of
	 * leaving the mount silently host-only.
	 */
	private async _mountFs(descriptor: DynamicMountDescriptor): Promise<void> {
		this._assertSafeAbsolutePath(descriptor.path);
		if (!(this.#kernel instanceof NativeSidecarKernelProxy)) {
			throw new Error("portable dynamic mounts require the native sidecar");
		}
		await this.#kernel.mountDescriptor({
			guestPath: descriptor.path,
			readOnly: descriptor.readOnly ?? false,
			plugin: {
				id: descriptor.plugin.id,
				config: descriptor.plugin.config ?? {},
			},
		});
	}

	private async _unmountFs(path: string): Promise<void> {
		this._assertSafeAbsolutePath(path);
		if (!(this.#kernel instanceof NativeSidecarKernelProxy)) {
			throw new Error("portable dynamic mounts require the native sidecar");
		}
		await this.#kernel.unmountDescriptor(path);
	}

	private async _listMounts(): Promise<MountInfo[]> {
		if (!(this.#kernel instanceof NativeSidecarKernelProxy)) return [];
		return this.#kernel.listMounts();
	}

	private async _move(from: string, to: string): Promise<void> {
		this._assertWritableAbsolutePath(from);
		this._assertWritableAbsolutePath(to);
		await this.#kernel.movePath(from, to);
	}

	private async _remove(
		path: string,
		options?: { recursive?: boolean },
	): Promise<void> {
		this._assertWritableAbsolutePath(path);
		await this.#kernel.removePath(path, {
			recursive: options?.recursive ?? false,
		});
	}

	/** @deprecated Use `filesystem.readFile()`. */
	readFile(path: string): Promise<Uint8Array> {
		return this.filesystem.readFile(path);
	}

	/** @deprecated Use `filesystem.writeFile()`. */
	writeFile(path: string, content: string | Uint8Array): Promise<void> {
		return this.filesystem.writeFile(path, content);
	}

	/** @deprecated Use `filesystem.writeFiles()`. */
	writeFiles(entries: BatchWriteEntry[]): Promise<BatchWriteResult[]> {
		return this.filesystem.writeFiles(entries);
	}

	/** @deprecated Use `filesystem.readFiles()`. */
	readFiles(paths: string[]): Promise<BatchReadResult[]> {
		return this.filesystem.readFiles(paths);
	}

	/** @deprecated Use `filesystem.mkdir()`. */
	mkdir(path: string, options?: { recursive?: boolean }): Promise<void> {
		return this.filesystem.mkdir(path, options);
	}

	/** @deprecated Use `filesystem.readdir()`. */
	readdir(path: string): Promise<string[]> {
		return this.filesystem.readdir(path);
	}

	/** @deprecated Use `filesystem.readdirEntries()`. */
	readdirEntries(path: string): Promise<ReaddirEntry[]> {
		return this.filesystem.readdirEntries(path);
	}

	/** @deprecated Use `filesystem.readdirRecursive()`. */
	readdirRecursive(
		path: string,
		options?: ReaddirRecursiveOptions,
	): Promise<DirEntry[]> {
		return this.filesystem.readdirRecursive(path, options);
	}

	/** @deprecated Use `filesystem.stat()`. */
	stat(path: string): Promise<VirtualStat> {
		return this.filesystem.stat(path);
	}

	/** @deprecated Use `filesystem.exists()`. */
	exists(path: string): Promise<boolean> {
		return this.filesystem.exists(path);
	}

	/** @deprecated Use `filesystem.export()`. */
	exportRootFilesystem(
		options: ExportRootFilesystemOptions,
	): Promise<RootSnapshotExport> {
		return this.filesystem.export(options);
	}

	/** @deprecated Use `filesystem.mount()`. */
	mountFs(descriptor: DynamicMountDescriptor): Promise<void> {
		return this.filesystem.mount(descriptor);
	}

	/** @deprecated Use `filesystem.unmount()`. */
	unmountFs(path: string): Promise<void> {
		return this.filesystem.unmount(path);
	}

	/** @deprecated Use `filesystem.listMounts()`. */
	listMounts(): Promise<MountInfo[]> {
		return this.filesystem.listMounts();
	}

	/** @deprecated Use `filesystem.move()`. */
	move(from: string, to: string): Promise<void> {
		return this.filesystem.move(from, to);
	}

	/** @deprecated Use `filesystem.remove()`. */
	remove(path: string, options?: { recursive?: boolean }): Promise<void> {
		return this.filesystem.remove(path, options);
	}

	private async _httpRequest(request: HttpRequest): Promise<HttpResponse> {
		this._assertSafeAbsolutePath(request.path.split("?")[0] || "/");
		const method = request.method?.toUpperCase() ?? "GET";
		const body =
			typeof request.body === "string"
				? request.body
				: request.body === undefined
					? undefined
					: new TextDecoder().decode(request.body);
		const responsePayload = JSON.parse(
			await this._sidecarClient.vmFetch(this._sidecarSession, this._sidecarVm, {
				port: request.port,
				method,
				path: request.path,
				headersJson: JSON.stringify(request.headers ?? {}),
				...(method !== "GET" && method !== "HEAD" && body !== undefined
					? { body }
					: {}),
			}),
		) as {
			status: number;
			statusText?: string;
			headers?: Array<[string, string]>;
			body?: string;
		};
		const headers: Record<string, string> = {};
		for (const [key, value] of responsePayload.headers ?? []) {
			headers[key] = value;
		}
		return {
			status: responsePayload.status,
			statusText: responsePayload.statusText ?? "",
			headers,
			body: new Uint8Array(Buffer.from(responsePayload.body ?? "", "base64")),
		};
	}

	/** @deprecated Use `network.httpRequest()`. */
	httpRequest(request: HttpRequest): Promise<HttpResponse> {
		return this.network.httpRequest(request);
	}

	/**
	 * Fetch an HTTP endpoint inside the VM while preserving duplicate response
	 * headers and arbitrary binary bodies. agentOS Apps uses this compatibility
	 * surface for its direct actor API.
	 */
	async fetch(port: number, request: Request): Promise<Response> {
		const url = new URL(request.url);
		const responsePayload = JSON.parse(
			await this._sidecarClient.vmFetch(this._sidecarSession, this._sidecarVm, {
				port,
				method: request.method,
				path: `${url.pathname}${url.search}`,
				headersJson: JSON.stringify(headersToRecord(request.headers)),
				...(request.method !== "GET" && request.method !== "HEAD"
					? {
							bodyBase64: Buffer.from(await request.arrayBuffer()).toString(
								"base64",
							),
						}
					: {}),
			}),
		) as {
			status: number;
			statusText?: string;
			headers?: Array<[string, string]>;
			body?: string;
		};
		const headers = new Headers();
		for (const [key, value] of responsePayload.headers ?? []) {
			headers.append(key, value);
		}
		return new Response(Buffer.from(responsePayload.body ?? "", "base64"), {
			status: responsePayload.status,
			statusText: responsePayload.statusText ?? "",
			headers,
		});
	}

	async fetchStreamStart(
		port: number,
		request: Request,
	): Promise<{
		streamId: string;
		status: number;
		statusText: string;
		headers: Array<[string, string]>;
	}> {
		const url = new URL(request.url);
		return JSON.parse(
			await this._sidecarClient.vmFetch(this._sidecarSession, this._sidecarVm, {
				port,
				method: request.method,
				path: `${url.pathname}${url.search}`,
				headersJson: JSON.stringify(headersToRecord(request.headers)),
				...(request.method !== "GET" && request.method !== "HEAD"
					? {
							bodyBase64: Buffer.from(await request.arrayBuffer()).toString(
								"base64",
							),
						}
					: {}),
				streamOperation: "start",
			}),
		) as {
			streamId: string;
			status: number;
			statusText: string;
			headers: Array<[string, string]>;
		};
	}

	async fetchStreamRead(
		streamId: string,
		maxBytes = 64 * 1024,
	): Promise<{ body: Uint8Array; done: boolean }> {
		const response = JSON.parse(
			await this._sidecarClient.vmFetch(this._sidecarSession, this._sidecarVm, {
				port: 0,
				method: "GET",
				path: "/",
				headersJson: "{}",
				streamOperation: "read",
				streamId,
				maxBytes,
			}),
		) as { body: string; done: boolean };
		return {
			body: new Uint8Array(Buffer.from(response.body, "base64")),
			done: response.done,
		};
	}

	async fetchStreamCancel(streamId: string): Promise<void> {
		await this._sidecarClient.vmFetch(this._sidecarSession, this._sidecarVm, {
			port: 0,
			method: "GET",
			path: "/",
			headersJson: "{}",
			streamOperation: "cancel",
			streamId,
		});
	}

	private _openTerminal(options?: ShellOptions): { shellId: string } {
		const shellId = `shell-${++this._shellCounter}`;
		this._closedShellIds.delete(shellId);
		const dataHandlers = new Set<(event: ShellData) => void>();
		const stderrHandlers = new Set<(event: ShellData) => void>();
		const exitHandlers = new Set<(event: ShellExit) => void>();

		const handle = this.#kernel.openShell({
			...options,
			onStderr: (data) => {
				for (const handler of stderrHandlers) handler({ shellId, data });
			},
		});
		handle.onData = (data) => {
			for (const handler of dataHandlers) handler({ shellId, data });
		};

		const entry: ShellEntry = {
			handle,
			dataHandlers,
			stderrHandlers,
			exitHandlers,
			exitPromise: Promise.resolve(0),
		};
		const exitPromise = handle.wait();
		const finalize = (exitCode?: number) => {
			this._pendingShellExitPromises.delete(entry.exitPromise);
			if (this._shells.get(shellId) === entry) {
				this._shells.delete(shellId);
			}
			// Record the exit code even when closeShell already dropped the
			// entry, so a waitShell issued after exit still resolves with it.
			this._closedShellIds.add(shellId, exitCode);
		};
		entry.exitPromise = exitPromise.then(
			(exitCode) => {
				finalize(exitCode);
				for (const handler of exitHandlers) handler({ shellId, exitCode });
				return exitCode;
			},
			(error) => {
				finalize();
				throw error;
			},
		);
		this._pendingShellExitPromises.add(entry.exitPromise);
		this._shells.set(shellId, entry);
		return { shellId };
	}

	async connectTerminal(options?: ConnectTerminalOptions): Promise<number> {
		return this.#kernel.connectTerminal(options);
	}

	/** Write data to a shell's PTY input. */
	private _writeTerminal(
		shellId: string,
		data: string | Uint8Array,
	): Promise<void> {
		const entry = this._shells.get(shellId);
		if (!entry) throw new Error(`Shell not found: ${shellId}`);
		return entry.handle.write(data);
	}

	/**
	 * Subscribe to ordered PTY output (stdout and stderr). Returns an unsubscribe
	 * function. `OpenShellOptions.onStderr` is a diagnostic tap for callers that
	 * need channel identity; do not render both surfaces.
	 */
	onShellData(
		shellId: string,
		handler: (event: ShellData) => void,
	): () => void {
		const entry = this._shells.get(shellId);
		if (!entry) throw new Error(`Shell not found: ${shellId}`);
		entry.dataHandlers.add(handler);
		return () => {
			entry.dataHandlers.delete(handler);
		};
	}

	/** Subscribe to the stderr-only diagnostic stream for a shell. */
	onShellStderr(
		shellId: string,
		handler: (event: ShellData) => void,
	): () => void {
		const entry = this._shells.get(shellId);
		if (!entry) throw new Error(`Shell not found: ${shellId}`);
		entry.stderrHandlers.add(handler);
		return () => entry.stderrHandlers.delete(handler);
	}

	/** Subscribe to shell exit. */
	onShellExit(
		shellId: string,
		handler: (event: ShellExit) => void,
	): () => void {
		const entry = this._shells.get(shellId);
		if (!entry) {
			const exitCode = this._closedShellIds.get(shellId);
			if (exitCode !== undefined) {
				handler({ shellId, exitCode });
				return () => {};
			}
			throw new Error(`Shell not found: ${shellId}`);
		}
		entry.exitHandlers.add(handler);
		return () => entry.exitHandlers.delete(handler);
	}

	/** Notify a shell of terminal resize. */
	private _resizeTerminal(shellId: string, cols: number, rows: number): void {
		const entry = this._shells.get(shellId);
		if (!entry) throw new Error(`Shell not found: ${shellId}`);
		entry.handle.resize(cols, rows);
	}

	/**
	 * Wait for a shell to exit and return its process exit code. Resolves
	 * immediately for a shell that has already exited (within the closed-shell
	 * retention window).
	 */
	private _waitTerminal(shellId: string): Promise<number> {
		const entry = this._shells.get(shellId);
		if (!entry) {
			const exitCode = this._closedShellIds.get(shellId);
			if (exitCode !== undefined) return Promise.resolve(exitCode);
			throw new Error(`Shell not found: ${shellId}`);
		}
		return entry.exitPromise;
	}

	/** Kill a shell process and remove it from tracking. */
	private _closeTerminal(shellId: string): void {
		const entry = this._shells.get(shellId);
		if (!entry) {
			if (this._closedShellIds.has(shellId)) {
				return;
			}
			throw new Error(`Shell not found: ${shellId}`);
		}
		entry.handle.kill();
		this._shells.delete(shellId);
		this._closedShellIds.add(shellId);
	}

	/** @deprecated Use `terminal.open()`. */
	openShell(options?: ShellOptions): { shellId: string } {
		return this.terminal.open(options);
	}

	/** @deprecated Use `terminal.write()`. */
	writeShell(shellId: string, data: string | Uint8Array): Promise<void> {
		return this.terminal.write(shellId, data);
	}

	/** @deprecated Use `terminal.resize()`. */
	resizeShell(shellId: string, cols: number, rows: number): void {
		this.terminal.resize(shellId, cols, rows);
	}

	/** @deprecated Use `terminal.wait()`. */
	waitShell(shellId: string): Promise<number> {
		return this.terminal.wait(shellId);
	}

	/** @deprecated Use `terminal.close()`. */
	closeShell(shellId: string): void {
		this.terminal.close(shellId);
	}

	private _resolveVmPathToHostPath(vmPath: string): string | null {
		const normalizedVmPath = posixPath.normalize(vmPath);
		for (const mount of this._hostMounts) {
			if (
				normalizedVmPath === mount.vmPath ||
				normalizedVmPath.startsWith(`${mount.vmPath}/`)
			) {
				const relativePath = posixPath.relative(mount.vmPath, normalizedVmPath);
				if (!relativePath) {
					return mount.hostPath;
				}
				return join(mount.hostPath, ...relativePath.split("/").filter(Boolean));
			}
		}
		return null;
	}

	/** Returns info about all processes spawned via spawn(). */
	private async _listProcesses(): Promise<ProcessDescriptor[]> {
		return [
			...[...this._processes.values()].map(
				({ proc, command, startedAtMs }): ProcessDescriptor => ({
					pid: proc.pid,
					command,
					state: proc.exitCode === null ? "running" : "exited",
					startedAtMs,
				}),
			),
			...[...this._languageProcesses.values()].map((entry) => entry.descriptor),
		];
	}

	/** Returns all kernel processes across all active runtimes (WASM and Node). */
	private _listAllProcesses(): KernelProcessInfo[] {
		if (this.#kernel instanceof NativeSidecarKernelProxy) {
			return this.#kernel.snapshotProcesses();
		}
		return [...this.#kernel.processes.values()];
	}

	/** Returns processes organized as a tree using ppid relationships. */
	private async _processTree(): Promise<ProcessTreeNode[]> {
		const all = this._listAllProcesses();
		const nodeMap = new Map<number, ProcessTreeNode>();

		// Index: create a tree node for each process
		for (const proc of all) {
			nodeMap.set(proc.pid, {
				pid: proc.pid,
				ppid: proc.ppid,
				command: proc.command,
				state: proc.status,
				startedAtMs: proc.startTime,
				children: [],
			});
		}
		for (const process of this._languageProcesses.values()) {
			if (!nodeMap.has(process.descriptor.pid)) {
				nodeMap.set(process.descriptor.pid, {
					...process.descriptor,
					children: [],
				});
			}
		}

		// Wire: attach each node to its parent
		const roots: ProcessTreeNode[] = [];
		for (const node of nodeMap.values()) {
			const parent =
				node.ppid === undefined ? undefined : nodeMap.get(node.ppid);
			if (parent) {
				parent.children.push(node);
			} else {
				roots.push(node);
			}
		}

		return roots;
	}

	/** Returns info about a specific process by PID. Throws if not found. */
	private async _getProcess(pid: number): Promise<ProcessDescriptor> {
		const language = this._languageProcesses.get(pid);
		if (language) return language.descriptor;
		const entry = this._processes.get(pid);
		if (!entry) {
			throw new Error(`Process not found: ${pid}`);
		}
		return {
			pid: entry.proc.pid,
			command: entry.command,
			state: entry.proc.exitCode === null ? "running" : "exited",
			startedAtMs: entry.startedAtMs,
		};
	}

	private async _signalProcess(
		pid: number,
		signal: ExecutionSignal,
	): Promise<void> {
		const language = this._languageProcesses.get(pid);
		if (language) {
			language.signal = signal;
			await this._signalExecution(language.executionId, signal);
			return;
		}
		const entry = this._processes.get(pid);
		if (!entry) {
			throw new Error(`Process not found: ${pid}`);
		}
		if (entry.proc.exitCode !== null) return;
		entry.signal = signal;
		const number = signal === "SIGKILL" ? 9 : signal === "SIGINT" ? 2 : 15;
		entry.proc.kill(number);
	}

	/** Send SIGKILL to force-kill a process. No-op if already exited. */
	private async _killProcess(pid: number): Promise<void> {
		await this._signalProcess(pid, "SIGKILL");
	}

	private async _resizeProcessPty(
		pid: number,
		size: { cols: number; rows: number },
	): Promise<void> {
		const language = this._languageProcesses.get(pid);
		if (!language) {
			throw new Error(`Process ${pid} does not have a managed PTY`);
		}
		await this._resizeExecutionPty(language.executionId, size);
	}

	private async _readProcessOutput(
		pid: number,
		options: { after?: number } = {},
	): Promise<OutputReplay> {
		const language = this._languageProcesses.get(pid);
		if (language) {
			const page = await this._readExecutionOutput(language.executionId, {
				cursor:
					options.after === undefined ? undefined : `1:${options.after + 1}`,
			});
			return {
				pid,
				events: page.events.map((event) => ({
					pid,
					sequence: event.sequence,
					channel: event.channel,
					chunk: event.chunk,
					timestampMs: event.timestampMs,
				})),
				nextCursor: page.nextCursor,
				hasMore: page.hasMore,
				truncated: page.truncated,
			};
		}
		const entry = this._processes.get(pid);
		if (!entry) throw new Error(`Process not found: ${pid}`);
		if (!entry.retainEvents) {
			throw new Error(
				`Process ${pid} was not spawned with output.retainEvents enabled`,
			);
		}
		const after = options.after ?? -1;
		const events = entry.events.filter((event) => event.sequence > after);
		return {
			pid,
			events,
			nextCursor: String(events.at(-1)?.sequence ?? after),
			hasMore: false,
			truncated:
				entry.events.length === PROCESS_OUTPUT_EVENT_LIMIT &&
				after < (entry.events[0]?.sequence ?? 0) - 1,
		};
	}

	private async _openSession(input: OpenSessionInput): Promise<void> {
		const response = await this._sendAcpRequest({
			tag: "AcpOpenSessionRequest",
			val: {
				sessionId: input.sessionId ?? null,
				agent: input.agent,
				cwd: input.cwd ?? null,
				additionalDirectories:
					input.additionalDirectories === undefined
						? null
						: JSON.stringify(input.additionalDirectories),
				env: input.env === undefined ? null : JSON.stringify(input.env),
				mcpServers:
					input.mcpServers === undefined
						? null
						: JSON.stringify(input.mcpServers),
				permissionPolicy: input.permissionPolicy ?? null,
				skipOsInstructions: input.skipOsInstructions ?? null,
				additionalInstructions:
					combineInstructions(
						input.additionalInstructions,
						this._bindingReference,
					) ?? null,
			},
		});
		if (response.tag !== "AcpOpenSessionResponse") {
			throw new Error(`unexpected openSession response: ${response.tag}`);
		}
	}

	private async _getSession(
		input?: SessionTarget,
	): Promise<DurableSessionInfo> {
		const response = await this._sendAcpRequest({
			tag: "AcpGetDurableSessionRequest",
			val: { sessionId: input?.sessionId ?? null },
		});
		if (response.tag !== "AcpGetDurableSessionResponse") {
			throw new Error(`unexpected getSession response: ${response.tag}`);
		}
		return decodeDurableSessionInfo(response.val.session);
	}

	private async _listSessions(input?: ListSessionsInput): Promise<SessionPage> {
		const response = await this._sendAcpRequest({
			tag: "AcpListDurableSessionsRequest",
			val: { cursor: input?.cursor ?? null, limit: input?.limit ?? null },
		});
		if (response.tag !== "AcpListDurableSessionsResponse") {
			throw new Error(`unexpected listSessions response: ${response.tag}`);
		}
		return {
			sessions: response.val.sessions.map(decodeDurableSessionInfo),
			nextCursor: response.val.nextCursor,
		};
	}

	private async _deleteSession(input: SessionTarget = {}): Promise<void> {
		const response = await this._sendAcpRequest({
			tag: "AcpDeleteSessionRequest",
			val: { sessionId: input.sessionId ?? null },
		});
		if (response.tag !== "AcpDeleteSessionResponse") {
			throw new Error(`unexpected deleteSession response: ${response.tag}`);
		}
	}

	private async _unloadSession(input?: SessionTarget): Promise<void> {
		const response = await this._sendAcpRequest({
			tag: "AcpUnloadSessionRequest",
			val: { sessionId: input?.sessionId ?? null },
		});
		if (response.tag !== "AcpUnloadSessionResponse") {
			throw new Error(`unexpected unloadSession response: ${response.tag}`);
		}
	}

	private async _prompt(input: PromptInput): Promise<DurablePromptResult> {
		const response = await this._sendAcpRequest({
			tag: "AcpPromptRequest",
			val: {
				sessionId: input.sessionId ?? null,
				idempotencyKey: input.idempotencyKey ?? null,
				content: JSON.stringify(input.content),
			},
		});
		if (response.tag !== "AcpPromptResponse") {
			throw new Error(`unexpected prompt response: ${response.tag}`);
		}
		return {
			sessionId: response.val.sessionId,
			message:
				response.val.message === null ? null : JSON.parse(response.val.message),
			stopReason: response.val.stopReason as DurablePromptResult["stopReason"],
		};
	}

	private async _cancelPrompt(
		input?: SessionTarget,
	): Promise<CancelPromptResult> {
		const response = await this._sendAcpRequest({
			tag: "AcpCancelPromptRequest",
			val: { sessionId: input?.sessionId ?? null },
		});
		if (response.tag !== "AcpCancelPromptResponse") {
			throw new Error(`unexpected cancelPrompt response: ${response.tag}`);
		}
		if (
			response.val.status !== "cancelled" &&
			response.val.status !== "no_active_prompt"
		) {
			throw new Error(`invalid cancelPrompt status: ${response.val.status}`);
		}
		return { status: response.val.status };
	}

	private async _respondPermission(
		input: PermissionResponse,
	): Promise<PermissionResponseResult> {
		const response = await this._sendAcpRequest({
			tag: "AcpRespondPermissionRequest",
			val: {
				sessionId: input.sessionId,
				requestId: input.requestId,
				optionId: input.optionId,
			},
		});
		if (response.tag !== "AcpRespondPermissionResponse") {
			throw new Error(`unexpected respondPermission response: ${response.tag}`);
		}
		if (
			response.val.status !== "accepted" &&
			response.val.status !== "not_pending"
		) {
			throw new Error(
				`invalid respondPermission status: ${response.val.status}`,
			);
		}
		if (response.val.status === "accepted") {
			if (response.val.reason !== null) {
				throw new Error(
					"accepted permission response must not include a reason",
				);
			}
			return { status: "accepted" };
		}
		return {
			status: "not_pending",
			reason: permissionTerminalReason(response.val.reason),
		};
	}

	private async _readHistory(input?: ReadHistoryInput): Promise<HistoryPage> {
		const response = await this._sendAcpRequest({
			tag: "AcpReadHistoryRequest",
			val: {
				sessionId: input?.sessionId ?? null,
				before: input?.before === undefined ? null : BigInt(input.before),
				after: input?.after === undefined ? null : BigInt(input.after),
				limit: input?.limit ?? null,
			},
		});
		if (response.tag !== "AcpHistoryPageResponse") {
			throw new Error(`unexpected readHistory response: ${response.tag}`);
		}
		return {
			events: response.val.events.map(decodeDurableSessionEvent),
			hasMoreBefore: response.val.hasMoreBefore,
			hasMoreAfter: response.val.hasMoreAfter,
		};
	}

	private async _getSessionConfig(
		input?: SessionTarget,
	): Promise<SessionConfig> {
		const response = await this._sendAcpRequest({
			tag: "AcpGetSessionConfigRequest",
			val: { sessionId: input?.sessionId ?? null },
		});
		if (response.tag !== "AcpSessionConfigResponse") {
			throw new Error(`unexpected getSessionConfig response: ${response.tag}`);
		}
		return {
			revision: safeWireU64(response.val.revision),
			options: JSON.parse(response.val.options),
		};
	}

	private async _setSessionConfigOption(
		input: SetSessionConfigOptionInput,
	): Promise<SessionConfig> {
		const response = await this._sendAcpRequest({
			tag: "AcpSetSessionConfigOptionRequest",
			val: {
				sessionId: input.sessionId ?? null,
				configId: input.configId,
				value: JSON.stringify(input.value),
			},
		});
		if (response.tag !== "AcpSessionConfigResponse") {
			throw new Error(
				`unexpected setSessionConfigOption response: ${response.tag}`,
			);
		}
		return {
			revision: safeWireU64(response.val.revision),
			options: JSON.parse(response.val.options),
		};
	}

	private async _getSessionCapabilities(
		input?: SessionTarget,
	): Promise<SessionCapabilities | null> {
		const response = await this._sendAcpRequest({
			tag: "AcpGetSessionCapabilitiesRequest",
			val: { sessionId: input?.sessionId ?? null },
		});
		if (response.tag !== "AcpSessionCapabilitiesResponse") {
			throw new Error(
				`unexpected getSessionCapabilities response: ${response.tag}`,
			);
		}
		return response.val.capabilities === null
			? null
			: normalizeSessionCapabilities(JSON.parse(response.val.capabilities));
	}

	private async _getSessionAgentInfo(
		input?: SessionTarget,
	): Promise<SessionAgentInfo | null> {
		const response = await this._sendAcpRequest({
			tag: "AcpGetSessionAgentInfoRequest",
			val: { sessionId: input?.sessionId ?? null },
		});
		if (response.tag !== "AcpSessionAgentInfoResponse") {
			throw new Error(
				`unexpected getSessionAgentInfo response: ${response.tag}`,
			);
		}
		return response.val.agentInfo === null
			? null
			: JSON.parse(response.val.agentInfo);
	}

	/** @deprecated Use `sessions.open()`. */
	openSession(input: OpenSessionInput): Promise<void> {
		return this.sessions.open(input);
	}

	/** @deprecated Use `sessions.get()`. */
	getSession(input?: SessionTarget): Promise<DurableSessionInfo> {
		return this.sessions.get(input);
	}

	/** @deprecated Use `sessions.list()`. */
	listSessions(input?: ListSessionsInput): Promise<SessionPage> {
		return this.sessions.list(input);
	}

	/** @deprecated Use `sessions.delete()`. */
	deleteSession(input: SessionTarget = {}): Promise<void> {
		return this.sessions.delete(input);
	}

	/** @deprecated Use `sessions.unload()`. */
	unloadSession(input?: SessionTarget): Promise<void> {
		return this.sessions.unload(input);
	}

	/** @deprecated Use `sessions.prompt()`. */
	prompt(input: PromptInput): Promise<DurablePromptResult> {
		return this.sessions.prompt(input);
	}

	/** @deprecated Use `sessions.cancelPrompt()`. */
	cancelPrompt(input?: SessionTarget): Promise<CancelPromptResult> {
		return this.sessions.cancelPrompt(input);
	}

	/** @deprecated Use `sessions.respondPermission()`. */
	respondPermission(
		input: PermissionResponse,
	): Promise<PermissionResponseResult> {
		return this.sessions.respondPermission(input);
	}

	/** @deprecated Use `sessions.readHistory()`. */
	readHistory(input?: ReadHistoryInput): Promise<HistoryPage> {
		return this.sessions.readHistory(input);
	}

	/** @deprecated Use `sessions.getConfig()`. */
	getSessionConfig(input?: SessionTarget): Promise<SessionConfig> {
		return this.sessions.getConfig(input);
	}

	/** @deprecated Use `sessions.setConfigOption()`. */
	setSessionConfigOption(
		input: SetSessionConfigOptionInput,
	): Promise<SessionConfig> {
		return this.sessions.setConfigOption(input);
	}

	/** @deprecated Use `sessions.getCapabilities()`. */
	getSessionCapabilities(
		input?: SessionTarget,
	): Promise<SessionCapabilities | null> {
		return this.sessions.getCapabilities(input);
	}

	/** @deprecated Use `sessions.getAgentInfo()`. */
	getSessionAgentInfo(input?: SessionTarget): Promise<SessionAgentInfo | null> {
		return this.sessions.getAgentInfo(input);
	}

	/**
	 * Dynamically link a software package into the RUNNING VM. The package's
	 * `bin/` commands appear under `/opt/agentos/bin` (on `$PATH`) and its `share/man`
	 * pages under MANPATH immediately — the `/opt/agentos` mount is host-backed, so
	 * writing into its staging dir is reflected live with no reboot. An `agent`
	 * block registers the package for `openSession({ agent: name })`. Persists for the VM's
	 * lifetime (and across a snapshot iff the volume persists).
	 */
	private async _linkSoftware(descriptor: PackageDescriptor): Promise<void> {
		// Forward to the sidecar, which owns the `/opt/agentos` projection and
		// appends the package to its live host-backed staging dir; the commands
		// appear under `/opt/agentos/bin` immediately. The sidecar rejects a
		// duplicate command, surfaced here as a thrown error.
		const commands = await this._sidecarClient.linkPackage(
			this._sidecarSession,
			this._sidecarVm,
			descriptor,
		);
		if (this.#kernel instanceof NativeSidecarKernelProxy) {
			this.#kernel.registerCommandGuestPaths(
				new Map(
					commands.projectedCommands.map((command) => [
						command.name,
						command.guestPath,
					]),
				),
			);
			// Retain the linked package for runtime mount reconfigures:
			// `configure_vm` is replace-on-write, so a later `mountFs` that
			// resent only the boot packages would unproject this one.
			this.#kernel.registerLinkedPackage(descriptor);
		}
		// The client parses no manifests: an `agent` block in the linked package is
		// picked up by the sidecar (it owns the projected `/opt/agentos` and answers
		// openSession/listAgents from it). Nothing to record client-side.
	}

	private async _listSoftware(): Promise<
		{ packageName: string; commands: string[] }[]
	> {
		return this._sidecarClient.providedCommands(
			this._sidecarSession,
			this._sidecarVm,
		);
	}

	/**
	 * Returns all registered agents with their installation status. Thin forwarder:
	 * sends `AcpListAgentsRequest` and maps the response. The sidecar enumerates the
	 * projected `/opt/agentos` packages (the client parses no manifests). Every such
	 * agent is a package materialized into the VM, so `installed` is always `true`.
	 */
	private async _listAgents(): Promise<AgentRegistryEntry[]> {
		const response = await this._sendAcpRequest({
			tag: "AcpListAgentsRequest",
			val: { reserved: false },
		});
		if (response.tag !== "AcpListAgentsResponse") {
			throw new Error(`unexpected list_agents response: ${response.tag}`);
		}
		return response.val.agents.map((agent) => ({
			id: agent.id,
			installed: agent.installed,
		}));
	}

	/** @deprecated Use `software.link()`. */
	linkSoftware(descriptor: PackageDescriptor): Promise<void> {
		return this.software.link(descriptor);
	}

	/** @deprecated Use `software.list()`. */
	listSoftware(): Promise<{ packageName: string; commands: string[] }[]> {
		return this.software.list();
	}

	/** @deprecated Use `agents.list()`. */
	listAgents(): Promise<AgentRegistryEntry[]> {
		return this.agents.list();
	}

	private _recordAgentStderr(event: {
		sessionId: string;
		agentType: string;
		processId: string;
		chunk: ArrayBuffer;
	}): void {
		if (!event.sessionId) {
			return;
		}
		const handler = this._agentStderrHandler;
		if (!handler) {
			return;
		}
		try {
			handler({
				sessionId: event.sessionId,
				agentType: event.agentType,
				processId: event.processId,
				pid: null,
				chunk: new Uint8Array(event.chunk),
			});
		} catch (error) {
			console.error("AgentOS stderr handler failed", error);
		}
	}

	private _recordAgentExit(event: {
		sessionId: string;
		agentType: string;
		processId: string;
		pid: number | null;
		exitCode: number | null;
		restart: string;
		restartCount: number;
		maxRestarts: number;
	}): void {
		const publicEvent: AgentExitEvent = {
			sessionId: event.sessionId,
			agentType: event.agentType,
			processId: event.processId,
			pid: event.pid,
			exitCode: event.exitCode,
			restart: event.restart as AgentRestartOutcome,
			restartCount: event.restartCount,
			maxRestarts: event.maxRestarts,
		};
		const handler = this._agentExitHandler;
		if (handler) {
			try {
				handler(publicEvent);
			} catch (error) {
				console.error("AgentOS agent-exit handler failed", error);
			}
		}
		for (const key of ["*", event.sessionId]) {
			for (const subscription of this._agentExitHandlers.get(key) ?? []) {
				try {
					subscription(publicEvent);
				} catch (error) {
					console.error("AgentOS agent-exit subscription failed", error);
				}
			}
		}
	}

	private _handleSidecarEvent(
		event: Parameters<SidecarProcess["onEvent"]>[0] extends (
			event: infer T,
		) => void
			? T
			: never,
	): void {
		if (event.payload.type === "execution_output") {
			const output = mapExecutionOutputEvent(event.payload.event);
			const pid = this._languageProcessIds.get(output.executionId);
			if (pid !== undefined) {
				const process = this._languageProcesses.get(pid);
				if (process) {
					const publicOutput: ProcessOutput = {
						pid,
						stream: output.channel === "stderr" ? "stderr" : "stdout",
						data: output.chunk,
					};
					for (const handler of process.outputHandlers) {
						try {
							handler(publicOutput);
						} catch (error) {
							console.error("AgentOS process output handler failed", error);
						}
					}
				}
			}
			for (const key of ["*", output.executionId]) {
				for (const handler of this._executionOutputHandlers.get(key) ?? []) {
					try {
						handler(output);
					} catch (error) {
						console.error("AgentOS execution output handler failed", error);
					}
				}
			}
			return;
		}
		if (event.payload.type === "execution_completed") {
			const completed = mapExecutionCompletedEvent(event.payload.event);
			const pid = this._languageProcessIds.get(completed.executionId);
			if (pid !== undefined) {
				const process = this._languageProcesses.get(pid);
				if (process && !process.exit) {
					const exit: ProcessExit = {
						pid,
						outcome:
							completed.outcome === "timed_out"
								? "timed_out"
								: completed.outcome === "cancelled"
									? "signalled"
									: "exited",
						...(completed.exitCode !== undefined
							? { exitCode: completed.exitCode }
							: {}),
						...(process.signal ? { signal: process.signal } : {}),
					};
					process.exit = exit;
					process.descriptor = { ...process.descriptor, state: "exited" };
					for (const handler of process.exitHandlers) {
						try {
							handler(exit);
						} catch (error) {
							console.error("AgentOS process exit handler failed", error);
						}
					}
				}
			}
			for (const key of ["*", completed.executionId]) {
				for (const handler of this._executionCompletedHandlers.get(key) ?? []) {
					try {
						handler(completed);
					} catch (error) {
						console.error("AgentOS execution completion handler failed", error);
					}
				}
			}
			return;
		}
		if (event.payload.type === "ext") {
			this._handleAcpExtEvent(event.payload.envelope);
			return;
		}
		if (event.payload.type !== "structured") {
			return;
		}
		if (event.payload.name === "limit_warning") {
			this._handleLimitWarning(event.payload.detail);
		}
	}

	private _handleLimitWarning(detail: Record<string, string>): void {
		if (!this._limitWarningHandler) {
			return;
		}
		const toNumber = (value: string | undefined): number => {
			const parsed = Number(value);
			return Number.isFinite(parsed) ? parsed : 0;
		};
		try {
			this._limitWarningHandler({
				limit: detail.limit ?? "",
				category: detail.category ?? "",
				observed: toNumber(detail.observed),
				capacity: toNumber(detail.capacity),
				fillPercent: toNumber(detail.fillPercent),
			});
		} catch (error) {
			console.error("AgentOS limit-warning handler failed", error);
		}
	}

	private _handleAcpExtEvent(envelope: {
		namespace: string;
		payload: Uint8Array;
	}): void {
		if (envelope.namespace !== ACP_EXTENSION_NAMESPACE) {
			return;
		}
		try {
			const event = decodeAcpEvent(envelope.payload);
			switch (event.tag) {
				case "AcpDurableSessionEvent": {
					this._emitDurableSessionEvent(decodeDurableSessionEvent(event.val));
					return;
				}
				case "AcpEphemeralSessionUpdateEvent": {
					const update = JSON.parse(event.val.update) as {
						sessionUpdate: EphemeralSessionEventEntry["type"];
					} & Record<string, unknown>;
					const { sessionUpdate: type, ...payload } = update;
					this._emitDurableSessionEvent({
						durability: "ephemeral",
						type,
						sessionId: event.val.sessionId,
						afterSequence: safeWireU64(event.val.afterSequence),
						...payload,
					} as EphemeralSessionEventEntry);
					return;
				}
				case "AcpSessionEvent":
					return;
				case "AcpAgentStderrEvent": {
					this._recordAgentStderr(event.val);
					return;
				}
				case "AcpAgentExitedEvent": {
					this._recordAgentExit(event.val);
					return;
				}
			}
		} catch (error) {
			console.error("AgentOS failed to decode an ACP sidecar event", error);
		}
	}

	private _emitDurableSessionEvent(entry: SessionStreamEntry): void {
		for (const handler of this._durableSessionEventHandlers.get(
			entry.sessionId,
		) ?? []) {
			try {
				handler(entry);
			} catch (error) {
				console.error("AgentOS session event handler failed", error);
			}
		}
	}

	private async _sendAcpRequest(request: AcpRequest): Promise<AcpResponse> {
		const envelope = await this._sidecarClient.extensionRequest(
			this._sidecarSession,
			this._sidecarVm,
			{
				namespace: ACP_EXTENSION_NAMESPACE,
				payload: encodeAcpRequest(request),
			},
		);
		if (envelope.namespace !== ACP_EXTENSION_NAMESPACE) {
			throw new Error(`unexpected ACP Ext namespace: ${envelope.namespace}`);
		}
		const response = decodeAcpResponse(envelope.payload);
		if (response.tag === "AcpErrorResponse") {
			const error = new Error(response.val.message) as Error & {
				code?: string;
			};
			error.code = response.val.code;
			throw error;
		}
		return response;
	}

	private _installSidecarRequestHandler(): void {
		const context: HostCallbackContext = {
			bindings: this._bindings,
			bindingMap: buildBindingMap(this._bindings),
			permissions: this._permissions,
			readFile: (path) => this.readFile(path),
		};
		this._sidecarClient.setSidecarRequestHandler((request) => {
			switch (request.payload.type) {
				case "host_callback":
					return handleHostCallback(request, context);
				case "js_bridge_call":
					return handleJsBridgeCall(request.payload, {
						filesystem: this.#kernel.vfs,
					});
				case "ext":
					return this._handleAcpExtSidecarRequest(request.payload.envelope);
			}
		});
	}

	private async _handleAcpExtSidecarRequest(envelope: {
		namespace: string;
		payload: Uint8Array;
	}): Promise<SidecarResponsePayload> {
		if (envelope.namespace !== ACP_EXTENSION_NAMESPACE) {
			return {
				type: "ext_result",
				envelope: {
					namespace: envelope.namespace,
					payload: Buffer.from("unknown extension namespace", "utf8"),
				},
			};
		}
		const callback = decodeAcpCallback(envelope.payload);
		switch (callback.tag) {
			case "AcpHostRequestCallback": {
				const response = await this._dispatchAcpSidecarRequest(
					toJsonRpcRequest(JSON.parse(callback.val.request)),
				);
				return {
					type: "ext_result",
					envelope: {
						namespace: ACP_EXTENSION_NAMESPACE,
						payload: encodeAcpCallbackResponse({
							tag: "AcpHostRequestCallbackResponse",
							val: {
								response: JSON.stringify(response),
							},
						}),
					},
				};
			}
		}
	}

	private async _dispatchAcpSidecarRequest(
		request: JsonRpcRequest,
	): Promise<JsonRpcResponse> {
		try {
			const result = await this._handleSupportedAcpSidecarRequest(request);
			return {
				jsonrpc: "2.0",
				id: request.id,
				result,
			};
		} catch (error) {
			if (error instanceof AcpDispatchError) {
				return {
					jsonrpc: "2.0",
					id: request.id,
					error: {
						code: error.code,
						message: error.message,
						...(error.data ? { data: error.data } : {}),
					},
				};
			}
			return {
				jsonrpc: "2.0",
				id: request.id,
				error: {
					code: -32603,
					message: error instanceof Error ? error.message : String(error),
				},
			};
		}
	}

	private async _handleSupportedAcpSidecarRequest(
		request: JsonRpcRequest,
	): Promise<unknown> {
		const params = this._acpParams(request);
		switch (request.method) {
			case "fs/read":
			case "fs/read_text_file":
				return this._handleAcpReadFile(params);
			case "fs/write":
			case "fs/write_text_file":
				return this._handleAcpWriteFile(params);
			case "fs/readDir":
			case "fs/read_dir":
				return this._handleAcpReadDir(params);
			case "terminal/create":
				return this._handleAcpCreateTerminal(params);
			case "terminal/write":
				return this._handleAcpWriteTerminal(params);
			case "terminal/output":
			case "terminal/read":
				return this._handleAcpReadTerminal(params);
			case "terminal/wait_for_exit":
			case "terminal/waitForExit":
				return this._handleAcpWaitForTerminalExit(params);
			case "terminal/kill":
				return this._handleAcpKillTerminal(params);
			case "terminal/release":
			case "terminal/close":
				return this._handleAcpReleaseTerminal(params);
			case "terminal/resize":
				return this._handleAcpResizeTerminal(params);
			default:
				throw new AcpDispatchError(
					-32601,
					`Method not found: ${request.method}`,
					{
						method: request.method,
					},
				);
		}
	}

	private _acpParams(request: JsonRpcRequest): Record<string, unknown> {
		if (!request.params) {
			return {};
		}
		if (
			typeof request.params !== "object" ||
			request.params === null ||
			Array.isArray(request.params)
		) {
			throw new AcpDispatchError(
				-32602,
				`${request.method} requires object params`,
			);
		}
		return request.params as Record<string, unknown>;
	}

	private _requireAcpStringParam(
		params: Record<string, unknown>,
		name: string,
		method: string,
	): string {
		const value = params[name];
		if (typeof value !== "string") {
			throw new AcpDispatchError(-32602, `${method} requires a string ${name}`);
		}
		return value;
	}

	private _optionalAcpStringParam(
		params: Record<string, unknown>,
		name: string,
		method: string,
	): string | undefined {
		const value = params[name];
		if (value === undefined || value === null) {
			return undefined;
		}
		if (typeof value !== "string") {
			throw new AcpDispatchError(
				-32602,
				`${method} requires ${name} to be a string when provided`,
			);
		}
		return value;
	}

	private _optionalAcpNumberParam(
		params: Record<string, unknown>,
		name: string,
		method: string,
	): number | undefined {
		const value = params[name];
		if (value === undefined || value === null) {
			return undefined;
		}
		if (typeof value !== "number" || !Number.isFinite(value)) {
			throw new AcpDispatchError(
				-32602,
				`${method} requires ${name} to be a number when provided`,
			);
		}
		return value;
	}

	private _optionalAcpStringArrayParam(
		params: Record<string, unknown>,
		name: string,
		method: string,
	): string[] | undefined {
		const value = params[name];
		if (value === undefined || value === null) {
			return undefined;
		}
		if (
			!Array.isArray(value) ||
			value.some((entry) => typeof entry !== "string")
		) {
			throw new AcpDispatchError(
				-32602,
				`${method} requires ${name} to be an array of strings when provided`,
			);
		}
		return [...value];
	}

	private _optionalAcpEnvParam(
		params: Record<string, unknown>,
		name: string,
		method: string,
	): Record<string, string> | undefined {
		const value = params[name];
		if (value === undefined || value === null) {
			return undefined;
		}
		if (Array.isArray(value)) {
			const env: Record<string, string> = {};
			for (const entry of value) {
				if (!entry || typeof entry !== "object" || Array.isArray(entry)) {
					throw new AcpDispatchError(
						-32602,
						`${method} requires ${name} entries to be { name, value } objects`,
					);
				}
				const record = entry as Record<string, unknown>;
				if (
					typeof record.name !== "string" ||
					typeof record.value !== "string"
				) {
					throw new AcpDispatchError(
						-32602,
						`${method} requires ${name} entries to be { name, value } objects`,
					);
				}
				env[record.name] = record.value;
			}
			return env;
		}
		if (typeof value !== "object") {
			throw new AcpDispatchError(
				-32602,
				`${method} requires ${name} to be an object or name/value array`,
			);
		}
		const env: Record<string, string> = {};
		for (const [key, entryValue] of Object.entries(
			value as Record<string, unknown>,
		)) {
			if (typeof entryValue !== "string") {
				throw new AcpDispatchError(
					-32602,
					`${method} requires ${name} values to be strings`,
				);
			}
			env[key] = entryValue;
		}
		return env;
	}

	private _requireAcpTerminal(
		params: Record<string, unknown>,
		method: string,
	): AcpTerminalEntry {
		const terminalId = this._requireAcpStringParam(
			params,
			"terminalId",
			method,
		);
		const terminal = this._acpTerminals.get(terminalId);
		if (!terminal) {
			throw new AcpDispatchError(
				-32602,
				`ACP terminal not found: ${terminalId}`,
			);
		}
		return terminal;
	}

	private _appendAcpTerminalOutput(
		terminal: AcpTerminalEntry,
		data: Uint8Array,
	): void {
		const chunk = Buffer.from(data).toString("utf8");
		if (!chunk) {
			return;
		}
		terminal.output += chunk;
		if (
			Number.isFinite(terminal.outputByteLimit) &&
			terminal.outputByteLimit >= 0 &&
			terminal.output.length > terminal.outputByteLimit
		) {
			terminal.output = terminal.output.slice(
				terminal.output.length - terminal.outputByteLimit,
			);
			terminal.truncated = true;
		}
	}

	private async _handleAcpReadFile(
		params: Record<string, unknown>,
	): Promise<{ content: string }> {
		const method = "fs/read";
		const path = this._requireAcpStringParam(params, "path", method);
		const line = this._optionalAcpNumberParam(params, "line", method);
		const limit = this._optionalAcpNumberParam(params, "limit", method);
		const encoding = this._optionalAcpStringParam(params, "encoding", method);
		const bytes = await this.readFile(path);
		if (encoding === "base64") {
			return { content: Buffer.from(bytes).toString("base64") };
		}
		const text = new TextDecoder().decode(bytes);
		if (line === undefined && limit === undefined) {
			return { content: text };
		}
		const startLine = Math.max(1, Math.trunc(line ?? 1));
		const lineLimit =
			limit === undefined
				? Number.POSITIVE_INFINITY
				: Math.max(0, Math.trunc(limit));
		return {
			content: text
				.split("\n")
				.slice(startLine - 1, startLine - 1 + lineLimit)
				.join("\n"),
		};
	}

	private async _handleAcpWriteFile(
		params: Record<string, unknown>,
	): Promise<null> {
		const method = "fs/write";
		const path = this._requireAcpStringParam(params, "path", method);
		const content = this._requireAcpStringParam(params, "content", method);
		const encoding = this._optionalAcpStringParam(params, "encoding", method);
		await this.writeFile(
			path,
			encoding === "base64" ? Buffer.from(content, "base64") : content,
		);
		return null;
	}

	private async _handleAcpReadDir(params: Record<string, unknown>): Promise<{
		entries: Array<{
			name: string;
			path: string;
			type: "file" | "directory" | "symlink";
		}>;
	}> {
		const method = "fs/readDir";
		const path = this._requireAcpStringParam(params, "path", method);
		const entries = await this._vfs().readDirWithTypes(path);
		return {
			entries: entries
				.filter((entry) => entry.name !== "." && entry.name !== "..")
				.map((entry) => ({
					name: entry.name,
					path: path === "/" ? `/${entry.name}` : `${path}/${entry.name}`,
					type: entry.isSymbolicLink
						? "symlink"
						: entry.isDirectory
							? "directory"
							: "file",
				})),
		};
	}

	private _handleAcpCreateTerminal(params: Record<string, unknown>): {
		terminalId: string;
	} {
		const method = "terminal/create";
		const command = this._requireAcpStringParam(params, "command", method);
		const args = this._optionalAcpStringArrayParam(params, "args", method);
		const env = this._optionalAcpEnvParam(params, "env", method);
		const cwd = this._optionalAcpStringParam(params, "cwd", method);
		const cols = this._optionalAcpNumberParam(params, "cols", method);
		const rows = this._optionalAcpNumberParam(params, "rows", method);
		const outputByteLimit = Math.max(
			0,
			Math.trunc(
				this._optionalAcpNumberParam(params, "outputByteLimit", method) ??
					1_048_576,
			),
		);
		const terminalId = `acp-terminal-${++this._acpTerminalCounter}`;
		const terminal: AcpTerminalEntry = {
			handle: this.#kernel.openShell({
				command,
				...(args ? { args } : {}),
				...(env ? { env } : {}),
				...(cwd ? { cwd } : {}),
				...(cols !== undefined ? { cols: Math.trunc(cols) } : {}),
				...(rows !== undefined ? { rows: Math.trunc(rows) } : {}),
			}),
			output: "",
			truncated: false,
			outputByteLimit,
			exitCode: null,
			waitPromise: Promise.resolve(0),
		};
		terminal.handle.onData = (data) => {
			this._appendAcpTerminalOutput(terminal, data);
		};
		terminal.waitPromise = terminal.handle.wait().then((exitCode) => {
			terminal.exitCode = exitCode;
			return exitCode;
		});
		this._acpTerminals.set(terminalId, terminal);
		return { terminalId };
	}

	private async _handleAcpWriteTerminal(
		params: Record<string, unknown>,
	): Promise<null> {
		const method = "terminal/write";
		const terminal = this._requireAcpTerminal(params, method);
		const data = this._requireAcpStringParam(params, "data", method);
		const encoding = this._optionalAcpStringParam(params, "encoding", method);
		await terminal.handle.write(
			encoding === "base64" ? Buffer.from(data, "base64") : data,
		);
		return null;
	}

	private _handleAcpReadTerminal(params: Record<string, unknown>): {
		output: string;
		truncated: boolean;
		exitStatus?: { exitCode: number; signal: null };
	} {
		const terminal = this._requireAcpTerminal(params, "terminal/output");
		return {
			output: terminal.output,
			truncated: terminal.truncated,
			...(terminal.exitCode !== null
				? {
						exitStatus: {
							exitCode: terminal.exitCode,
							signal: null,
						},
					}
				: {}),
		};
	}

	private async _handleAcpWaitForTerminalExit(
		params: Record<string, unknown>,
	): Promise<{ exitCode: number; signal: null }> {
		const terminal = this._requireAcpTerminal(params, "terminal/wait_for_exit");
		const exitCode = await terminal.waitPromise;
		return { exitCode, signal: null };
	}

	private _handleAcpKillTerminal(params: Record<string, unknown>): null {
		const method = "terminal/kill";
		const terminal = this._requireAcpTerminal(params, method);
		const signal = this._optionalAcpNumberParam(params, "signal", method) ?? 15;
		terminal.handle.kill(Math.trunc(signal));
		return null;
	}

	private _handleAcpReleaseTerminal(params: Record<string, unknown>): null {
		const method = "terminal/release";
		const terminalId = this._requireAcpStringParam(
			params,
			"terminalId",
			method,
		);
		const terminal = this._acpTerminals.get(terminalId);
		if (!terminal) {
			throw new AcpDispatchError(
				-32602,
				`ACP terminal not found: ${terminalId}`,
			);
		}
		if (terminal.exitCode === null) {
			terminal.handle.kill();
		}
		this._acpTerminals.delete(terminalId);
		return null;
	}

	private _handleAcpResizeTerminal(params: Record<string, unknown>): null {
		const method = "terminal/resize";
		const terminal = this._requireAcpTerminal(params, method);
		const cols = this._optionalAcpNumberParam(params, "cols", method);
		const rows = this._optionalAcpNumberParam(params, "rows", method);
		if (cols === undefined || rows === undefined) {
			throw new AcpDispatchError(
				-32602,
				`${method} requires numeric cols and rows`,
			);
		}
		terminal.handle.resize(Math.trunc(cols), Math.trunc(rows));
		return null;
	}

	onSessionEvent(handler: (entry: SessionStreamEntry) => void): () => void;
	onSessionEvent(
		sessionId: string | undefined,
		handler: (entry: SessionStreamEntry) => void,
	): () => void;
	onSessionEvent(
		sessionIdOrHandler:
			| string
			| ((entry: SessionStreamEntry) => void)
			| undefined,
		maybeHandler?: (entry: SessionStreamEntry) => void,
	): () => void {
		const sessionId =
			typeof sessionIdOrHandler === "string" ? sessionIdOrHandler : "main";
		const handler =
			typeof sessionIdOrHandler === "function"
				? sessionIdOrHandler
				: maybeHandler;
		if (!handler) {
			throw new TypeError("onSessionEvent requires a handler");
		}
		const handlers =
			this._durableSessionEventHandlers.get(sessionId) ?? new Set();
		handlers.add(handler);
		this._durableSessionEventHandlers.set(sessionId, handlers);
		return () => {
			handlers.delete(handler);
			if (handlers.size === 0) {
				this._durableSessionEventHandlers.delete(sessionId);
			}
		};
	}

	/** Subscribe to unexpected adapter exits without changing session liveness. */
	onAgentExit(handler: AgentExitHandler): () => void;
	onAgentExit(
		sessionId: string | undefined,
		handler: AgentExitHandler,
	): () => void;
	onAgentExit(
		sessionIdOrHandler: string | AgentExitHandler | undefined,
		maybeHandler?: AgentExitHandler,
	): () => void {
		const sessionId =
			typeof sessionIdOrHandler === "string" ? sessionIdOrHandler : "*";
		const handler =
			typeof sessionIdOrHandler === "function"
				? sessionIdOrHandler
				: maybeHandler;
		if (!handler) throw new TypeError("onAgentExit requires a handler");
		const handlers = this._agentExitHandlers.get(sessionId) ?? new Set();
		handlers.add(handler);
		this._agentExitHandlers.set(sessionId, handlers);
		return () => {
			handlers.delete(handler);
			if (handlers.size === 0) this._agentExitHandlers.delete(sessionId);
		};
	}

	// ── Cron ────────────────────────────────────────────────────

	/** Schedule a cron job. Returns a handle with the job ID and a cancel method. */
	private _scheduleCron(options: CronJobOptions): CronJob {
		return this._cronManager.schedule(options);
	}

	/** List all registered cron jobs. */
	private _listCronJobs(): CronJobInfo[] {
		return this._cronManager.list();
	}

	/** Cancel a cron job by ID. */
	private _cancelCronJob(id: string): void {
		this._cronManager.cancel(id);
	}

	/** @deprecated Use `cron.schedule()`. */
	scheduleCron(options: CronJobOptions): CronJob {
		return this.cron.schedule(options);
	}

	/** @deprecated Use `cron.list()`. */
	listCronJobs(): CronJobInfo[] {
		return this.cron.list();
	}

	/** @deprecated Use `cron.cancel()`. */
	cancelCronJob(id: string): void {
		this.cron.cancel(id);
	}

	/** Subscribe to cron lifecycle events (fire, complete, error). */
	onCronEvent(handler: CronEventHandler): void {
		this._cronManager.onEvent(handler);
	}

	async dispose(): Promise<void> {
		this._cronManager.dispose();

		for (const [id, entry] of this._shells) {
			entry.handle.kill();
		}
		const shellExitPromises = [...this._pendingShellExitPromises];
		this._shells.clear();
		const terminalExitPromises: Promise<unknown>[] = [];
		for (const terminal of this._acpTerminals.values()) {
			terminal.handle.kill();
			terminalExitPromises.push(
				terminal.waitPromise.then(
					() => undefined,
					() => undefined,
				),
			);
		}
		this._acpTerminals.clear();
		this._processes.clear();
		this._executionOutputHandlers.clear();
		this._executionCompletedHandlers.clear();
		await waitForTrackedExitPromises(
			[...shellExitPromises, ...terminalExitPromises],
			SHELL_DISPOSE_TIMEOUT_MS,
		);

		this._disposeSidecarEventListener();

		const sidecarLease = this._sidecarLease;
		this._sidecarLease = null;
		const disposeVm = sidecarLease
			? sidecarLease.dispose()
			: this.#kernel.dispose();
		const hooks = this._disposeHooks.splice(0);
		const errors: unknown[] = [];
		try {
			await disposeVm;
		} catch (error) {
			errors.push(error);
		}
		const hookResults = await Promise.allSettled(hooks.map((hook) => hook()));
		errors.push(
			...hookResults.flatMap((result) =>
				result.status === "rejected" ? [result.reason] : [],
			),
		);
		if (errors.length === 1) throw errors[0];
		if (errors.length > 1) {
			throw new AggregateError(errors, "AgentOS VM disposal failed");
		}
	}
}

const agentOsRuntimeAdmins = new WeakMap<AgentOs, AgentOsRuntimeAdmin>();

export function getAgentOsRuntimeAdmin(vm: AgentOs): AgentOsRuntimeAdmin {
	const admin = agentOsRuntimeAdmins.get(vm);
	if (!admin) {
		throw new Error("Agent OS runtime admin is not available for this VM");
	}
	return admin;
}

export function getAgentOsKernel(vm: AgentOs): Kernel {
	return getAgentOsRuntimeAdmin(vm).kernel;
}

function resolveAgentOsSidecar(
	config: AgentOsSidecarConfig | undefined,
): AgentOsSidecar {
	if (!config || config.kind === "shared") {
		return getSharedAgentOsSidecarInternal(
			config?.kind === "shared"
				? { pool: config.pool, runtime: config.runtime }
				: undefined,
		);
	}

	return config.handle;
}

interface CreateInProcessSidecarTransportOptions<
	TVmAdmin extends InProcessSidecarVmAdmin,
> {
	createVm(
		sessionBootstrap: AgentOsSidecarSessionBootstrap,
		vmBootstrap: AgentOsSidecarVmBootstrap,
	): Promise<TVmAdmin>;
}

interface InProcessSidecarTransport<TVmAdmin extends InProcessSidecarVmAdmin>
	extends AgentOsSidecarTransport {
	getVmAdmin(vmId: string): TVmAdmin | undefined;
}

interface AgentOsSidecarLeaseRecord {
	dispose(): Promise<void>;
}

interface SharedSidecarNativeProcess {
	client: SidecarProcess;
	session: AuthenticatedSession;
}

interface AgentOsSidecarState {
	description: AgentOsSidecarDescription;
	runtime: AgentOsSidecarRuntimeConfig;
	activeLeases: Set<AgentOsSidecarLeaseRecord>;
	sharedPool?: string;
	/**
	 * The single native sidecar process shared by every VM leased from this
	 * handle. Spawned lazily on first VM creation and reused thereafter so VMs
	 * are cheap incremental tenants of one process rather than one-process-each.
	 */
	nativeProcess?: Promise<SharedSidecarNativeProcess>;
	/**
	 * The shared sidecar's child process + stdio, cached for synchronous
	 * ref/unref. Unref'd when no VM leases are active so a one-shot host process
	 * can exit after `dispose()`; re-ref'd while leases are live.
	 */
	sharedChild?: SidecarEventLoopHandle;
	/**
	 * Number of live "holds" on the shared sidecar's event-loop reference. A hold
	 * is taken for the WHOLE create→use→dispose lifetime of every VM lease (not
	 * just while it sits in `activeLeases`), so a VM that is still mid-creation
	 * still counts. The child + stdio are ref'd while this is >0 and unref'd at 0.
	 * A counter (not a boolean) so concurrent create/dispose cannot clobber each
	 * other — Node ref/unref is not itself counted.
	 */
	eventLoopHolds?: number;
}

const sidecarStates = new WeakMap<AgentOsSidecar, AgentOsSidecarState>();
const sharedSidecars = new Map<string, AgentOsSidecar>();

interface RefCountableHandle {
	ref?(): unknown;
	unref?(): unknown;
}

interface SidecarEventLoopHandle extends RefCountableHandle {
	stdin?: RefCountableHandle | null;
	stdout?: RefCountableHandle | null;
	stderr?: RefCountableHandle | null;
	stdio?: ReadonlyArray<RefCountableHandle | null>;
	kill?(signal?: string | number): unknown;
}

let sidecarProcessExitHookInstalled = false;

/**
 * Install a one-time, synchronous `process.on("exit")` hook that SIGKILLs any
 * pooled shared sidecar child. Once a one-shot host process is allowed to exit
 * (its sidecar handles are unref'd at 0 leases), this reaps the sidecar
 * immediately instead of waiting for its stdin-EOF grace window — no orphan, no
 * delay. We deliberately do NOT install SIGINT/SIGTERM handlers: a library
 * should not hijack the host's signal handling. SIGINT still reaches the sidecar
 * via the process group, and SIGTERM-driven exit still closes its stdin.
 */
function ensureSidecarProcessExitCleanup(): void {
	if (sidecarProcessExitHookInstalled) return;
	sidecarProcessExitHookInstalled = true;
	process.on("exit", () => {
		for (const sidecar of sharedSidecars.values()) {
			try {
				sidecarStates.get(sidecar)?.sharedChild?.kill?.("SIGKILL");
			} catch {
				// best-effort reap; the process is exiting regardless
			}
		}
	});
}

function sidecarChildHandle(
	client: unknown,
): SidecarEventLoopHandle | undefined {
	// SidecarProcess -> StdioSidecarProtocolClient.child (the spawned ChildProcess).
	const protocolClient = (
		client as
			| { protocolClient?: { child?: SidecarEventLoopHandle } }
			| undefined
	)?.protocolClient;
	return protocolClient?.child ?? undefined;
}

/**
 * Apply the current hold state to the shared sidecar's child + stdio: ref them
 * while ≥1 hold is live so in-flight VM work keeps the host process alive; unref
 * them at 0 so a one-shot script exits on its own after `dispose()`. The sidecar
 * process itself stays running (reusable) and self-exits on stdin EOF when the
 * host finally goes away. Best-effort: never let ref/unref break VM lifecycle.
 */
function applySharedSidecarHold(state: AgentOsSidecarState): void {
	const child = state.sharedChild;
	if (!child) return;
	const hold = (state.eventLoopHolds ?? 0) > 0;
	// Include the complete stdio array, not just the named fd0-fd2 aliases.
	// Protocol v8 carries responses over fd3; leaving that control socket
	// referenced pins Node's event loop after the final VM lease is disposed.
	const handles = new Set<RefCountableHandle | null | undefined>([
		child,
		child.stdin,
		child.stdout,
		child.stderr,
		...(child.stdio ?? []),
	]);
	for (const handle of handles) {
		if (!handle) continue;
		try {
			if (hold) handle.ref?.();
			else handle.unref?.();
		} catch {
			// ref/unref is an optimization, not correctness-critical
		}
	}
}

/**
 * Take a hold for the entire create→use→dispose lifetime of one VM lease. Taken
 * BEFORE VM creation starts (not when the lease lands in `activeLeases`) so a VM
 * that is still mid-creation keeps the sidecar ref'd and a concurrent dispose
 * cannot unref it out from under the in-flight create.
 */
function acquireSharedSidecarHold(state: AgentOsSidecarState): void {
	state.eventLoopHolds = (state.eventLoopHolds ?? 0) + 1;
	if (state.eventLoopHolds === 1) applySharedSidecarHold(state);
}

/** Release a hold taken by {@link acquireSharedSidecarHold}; unref at 0. */
function releaseSharedSidecarHold(state: AgentOsSidecarState): void {
	const current = state.eventLoopHolds ?? 0;
	if (current <= 0) {
		// The `holdReleased` guard makes each lease release exactly once, so this
		// should be unreachable. Warn rather than silently floor, per the repo's
		// no-silent-masking rule, so an accounting bug surfaces instead of hiding.
		state.eventLoopHolds = 0;
		console.warn(
			"[agentos] shared sidecar event-loop hold released more than acquired",
		);
		return;
	}
	state.eventLoopHolds = current - 1;
	if (state.eventLoopHolds === 0) applySharedSidecarHold(state);
}

/**
 * Spawn-once accessor for a sidecar handle's shared native process. Concurrent
 * callers await the same promise, so one `AgentOsSidecar` maps to exactly one
 * `agent-os-sidecar` OS process for its whole lifetime.
 */
function ensureSharedSidecarNativeProcess(
	sidecar: AgentOsSidecar,
): Promise<SharedSidecarNativeProcess> {
	const state = getSidecarState(sidecar);
	if (!state.nativeProcess) {
		ensureSidecarProcessExitCleanup();
		state.nativeProcess = (async () => {
			const client = SidecarProcess.spawn({
				cwd: REPO_ROOT,
				command: ensureNativeSidecarBinary(),
				args: sidecarRuntimeArgs(state.runtime),
			});
			// Track the child immediately — BEFORE the handshake await — so a
			// failed `authenticateAndOpenSession()` can still reap it (otherwise
			// the spawned child is untracked, unreapable, and pins the loop).
			state.sharedChild = sidecarChildHandle(client);
			if (!state.sharedChild) {
				// We reached into @rivet-dev/agentos-runtime-core internals to get the child for
				// idle-unref. If that shape ever changes this returns undefined and
				// the optimization silently stops working (one-shot scripts would
				// hang again). Make it loud rather than a silent regression.
				console.warn(
					"[agentos] could not resolve the shared sidecar child handle; " +
						"standalone scripts may not exit cleanly after dispose(). " +
						"This usually means @rivet-dev/agentos-runtime-core internals changed.",
				);
			}
			// Apply the current hold state to the just-spawned child.
			applySharedSidecarHold(state);
			try {
				const session = await client.authenticateAndOpenSession();
				return { client, session };
			} catch (error) {
				// Spawn/handshake failed: reap the child, drop the cached handle,
				// and CLEAR the rejected promise so the next create() retries
				// instead of permanently wedging on a rejected `nativeProcess`.
				try {
					state.sharedChild?.kill?.("SIGKILL");
				} catch {
					// already gone
				}
				state.sharedChild = undefined;
				state.nativeProcess = undefined;
				throw error;
			}
		})();
	}
	return state.nativeProcess;
}

/** Dispose a sidecar handle's shared native process, if one was spawned. */
async function disposeSharedSidecarNativeProcess(
	state: AgentOsSidecarState,
): Promise<void> {
	const pending = state.nativeProcess;
	if (!pending) {
		return;
	}
	state.nativeProcess = undefined;
	// The cached child is now dead; drop it (symmetric with the assignment in
	// ensureSharedSidecarNativeProcess). We deliberately do NOT zero
	// `eventLoopHolds` here: this runs only from `AgentOsSidecar.dispose()`, which
	// has already set the handle to `disposing` (so no new lease can acquire) and
	// drained `activeLeases`; the disposed handle's state is then abandoned. Force-
	// zeroing a shared counter could clobber a hold on a freshly re-acquired
	// process generation, so it is left to the balanced acquire/release pairs.
	state.sharedChild = undefined;
	try {
		const { client } = await pending;
		await client.dispose();
	} catch {
		// Process may have already exited; nothing to reclaim.
	}
}

export class AgentOsSidecar {
	constructor(
		sidecarId: string,
		placement: AgentOsSidecarPlacement,
		sharedPool?: string,
		runtime?: AgentOsSidecarRuntimeConfig,
	) {
		sidecarStates.set(this, {
			description: {
				sidecarId,
				placement: cloneSidecarPlacement(placement),
				state: "ready",
				activeVmCount: 0,
			},
			activeLeases: new Set(),
			sharedPool,
			runtime: normalizeSidecarRuntimeConfig(runtime),
		});
	}

	describe(): AgentOsSidecarDescription {
		const state = getSidecarState(this);
		return cloneSidecarDescription(state.description);
	}

	async dispose(): Promise<void> {
		const state = getSidecarState(this);
		if (state.description.state === "disposed") {
			return;
		}

		state.description.state = "disposing";
		const errors: Error[] = [];
		for (const lease of [...state.activeLeases]) {
			try {
				await lease.dispose();
			} catch (error) {
				errors.push(error instanceof Error ? error : new Error(String(error)));
			}
		}
		state.activeLeases.clear();
		state.description.activeVmCount = 0;
		// Tear down the shared native process after all leased VMs are gone.
		await disposeSharedSidecarNativeProcess(state);
		state.description.state = "disposed";
		if (state.sharedPool && sharedSidecars.get(state.sharedPool) === this) {
			sharedSidecars.delete(state.sharedPool);
		}
		if (errors.length > 0) {
			throw new Error(errors.map((error) => error.message).join("; "));
		}
	}
}

function createAgentOsSidecarInternal(
	options: AgentOsCreateSidecarOptions = {},
): AgentOsSidecar {
	const sidecarId = options.sidecarId ?? `agentos-sidecar-${randomUUID()}`;
	return new AgentOsSidecar(
		sidecarId,
		{
			kind: "explicit",
			sidecarId,
		},
		undefined,
		options.runtime,
	);
}

/**
 * Test-only escape hatch: dispose every cached shared sidecar so vitest
 * workers can exit cleanly. The shared sidecar is normally process-global and
 * keeps its native subprocess alive across `AgentOs.create()` calls; without
 * this hook the vitest worker can hold open piped stdio handles after the
 * test suite finishes and stall `pnpm test` indefinitely.
 */
export async function __disposeAllSharedSidecarsForTesting(): Promise<void> {
	const sidecars = Array.from(sharedSidecars.values());
	sharedSidecars.clear();
	const errors: Error[] = [];
	for (const sidecar of sidecars) {
		try {
			await sidecar.dispose();
		} catch (error) {
			errors.push(error instanceof Error ? error : new Error(String(error)));
		}
	}
	if (errors.length > 0) {
		throw new Error(
			`failed to dispose shared sidecars: ${errors.map((error) => error.message).join("; ")}`,
		);
	}
}

function getSharedAgentOsSidecarInternal(
	options: AgentOsSharedSidecarOptions = {},
): AgentOsSidecar {
	const pool = options.pool ?? "default";
	const existing = sharedSidecars.get(pool);
	if (existing && existing.describe().state !== "disposed") {
		if (options.runtime !== undefined) {
			const requested = normalizeSidecarRuntimeConfig(options.runtime);
			const configured = getSidecarState(existing).runtime;
			if (!sidecarRuntimeConfigsEqual(requested, configured)) {
				throw new Error(
					`Shared sidecar pool ${JSON.stringify(pool)} already exists with different runtime settings`,
				);
			}
		}
		return existing;
	}

	const sidecar = new AgentOsSidecar(
		`agentos-shared-sidecar:${pool}`,
		{ kind: "shared", ...(pool ? { pool } : {}) },
		pool,
		options.runtime,
	);
	sharedSidecars.set(pool, sidecar);
	return sidecar;
}

function normalizeSidecarRuntimeConfig(
	runtime: AgentOsSidecarRuntimeConfig | undefined,
): AgentOsSidecarRuntimeConfig {
	const maxActiveVms = runtime?.executor?.maxActiveVms;
	if (maxActiveVms === undefined) return {};
	if (!Number.isSafeInteger(maxActiveVms) || maxActiveVms <= 0) {
		throw new Error(
			"runtime.executor.maxActiveVms must be a positive safe integer",
		);
	}
	return { executor: { maxActiveVms } };
}

function sidecarRuntimeConfigsEqual(
	left: AgentOsSidecarRuntimeConfig,
	right: AgentOsSidecarRuntimeConfig,
): boolean {
	return left.executor?.maxActiveVms === right.executor?.maxActiveVms;
}

function sidecarRuntimeArgs(runtime: AgentOsSidecarRuntimeConfig): string[] {
	const maxActiveVms = runtime.executor?.maxActiveVms;
	return maxActiveVms === undefined
		? []
		: ["--max-active-vms", String(maxActiveVms)];
}

async function leaseAgentOsSidecarVm<TVmAdmin extends InProcessSidecarVmAdmin>(
	sidecar: AgentOsSidecar,
	options: CreateInProcessSidecarTransportOptions<TVmAdmin>,
): Promise<AgentOsSidecarVmLease<TVmAdmin>> {
	const state = getSidecarState(sidecar);
	if (state.description.state !== "ready") {
		throw new Error(
			`Cannot lease VM from sidecar ${state.description.sidecarId} while it is ${state.description.state}`,
		);
	}

	let transport: InProcessSidecarTransport<TVmAdmin> | undefined;
	const client: AgentOsSidecarClient = createAgentOsSidecarClient({
		async createOwnershipTransport(sessionBootstrap) {
			transport = await createInProcessSidecarTransport(
				sessionBootstrap,
				options,
			);
			return transport;
		},
	});

	// Hold the shared sidecar's event-loop ref for this lease's WHOLE lifetime —
	// taken now, before VM creation, so a concurrent dispose cannot unref the
	// sidecar while this create is still in flight. Released exactly once on
	// dispose or on a failed create.
	acquireSharedSidecarHold(state);
	let holdReleased = false;
	const releaseHold = () => {
		if (holdReleased) return;
		holdReleased = true;
		releaseSharedSidecarHold(state);
	};

	let disposed = false;
	let leaseRecord: AgentOsSidecarLeaseRecord | undefined;

	try {
		const session = await client.createOwnershipSession({
			placement: cloneSidecarPlacement(state.description.placement),
		});
		const vm = await session.createVm();
		const admin = transport?.getVmAdmin(vm.vmId);
		if (!admin) {
			throw new Error(`Sidecar VM admin was not registered for ${vm.vmId}`);
		}

		const lease: AgentOsSidecarVmLease<TVmAdmin> = {
			sidecar,
			session,
			vm,
			admin,
			async dispose() {
				if (disposed) {
					return;
				}
				disposed = true;
				state.activeLeases.delete(leaseRecord!);
				state.description.activeVmCount = state.activeLeases.size;
				await client.dispose();
				// Release this lease's hold; the shared sidecar is unref'd only
				// once the last hold (across all in-flight + active leases) drops,
				// so a one-shot host process can then exit on its own.
				releaseHold();
			},
		};

		leaseRecord = {
			dispose: () => lease.dispose(),
		};
		state.activeLeases.add(leaseRecord);
		state.description.activeVmCount = state.activeLeases.size;
		return lease;
	} catch (error) {
		await client.dispose().catch(() => {});
		releaseHold();
		throw error;
	}
}

async function createInProcessSidecarTransport<
	TVmAdmin extends InProcessSidecarVmAdmin,
>(
	sessionBootstrap: AgentOsSidecarSessionBootstrap,
	options: CreateInProcessSidecarTransportOptions<TVmAdmin>,
): Promise<InProcessSidecarTransport<TVmAdmin>> {
	const vmAdmins = new Map<string, TVmAdmin>();
	let disposed = false;

	async function disposeVmAdmin(vmId: string): Promise<void> {
		const admin = vmAdmins.get(vmId);
		if (!admin) {
			return;
		}

		vmAdmins.delete(vmId);
		await admin.dispose();
	}

	return {
		async createVm(vmBootstrap) {
			if (disposed) {
				throw new Error(
					`Cannot create VM ${vmBootstrap.vmId} for disposed sidecar session ${sessionBootstrap.sessionId}`,
				);
			}

			const admin = await options.createVm(sessionBootstrap, vmBootstrap);
			vmAdmins.set(vmBootstrap.vmId, admin);
		},

		async disposeVm(vmId) {
			await disposeVmAdmin(vmId);
		},

		async dispose() {
			if (disposed) {
				return;
			}
			disposed = true;

			const errors: Error[] = [];
			for (const vmId of [...vmAdmins.keys()]) {
				try {
					await disposeVmAdmin(vmId);
				} catch (error) {
					errors.push(
						error instanceof Error ? error : new Error(String(error)),
					);
				}
			}

			if (errors.length > 0) {
				throw new Error(errors.map((error) => error.message).join("; "));
			}
		},

		getVmAdmin(vmId) {
			return vmAdmins.get(vmId);
		},
	};
}

function getSidecarState(sidecar: AgentOsSidecar): AgentOsSidecarState {
	const state = sidecarStates.get(sidecar);
	if (!state) {
		throw new Error("Unknown Agent OS sidecar handle");
	}
	return state;
}

function cloneSidecarDescription(
	description: AgentOsSidecarDescription,
): AgentOsSidecarDescription {
	return {
		...description,
		placement: cloneSidecarPlacement(description.placement),
	};
}

function cloneSidecarPlacement(
	placement: AgentOsSidecarPlacement,
): AgentOsSidecarPlacement {
	if (placement.kind === "shared") {
		return {
			kind: "shared",
			...(placement.pool ? { pool: placement.pool } : {}),
		};
	}

	return {
		kind: "explicit",
		sidecarId: placement.sidecarId,
	};
}
