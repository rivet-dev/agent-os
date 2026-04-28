import { execFileSync, spawn as spawnChildProcess } from "node:child_process";
import { randomUUID } from "node:crypto";
import {
	existsSync,
	mkdtempSync,
	readdirSync,
	readFileSync,
	rmSync,
	statSync,
	writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import {
	join,
	posix as posixPath,
	resolve as resolveHostPath,
} from "node:path";
import { fileURLToPath } from "node:url";
import type {
	AgentCapabilities,
	AgentInfo,
	GetEventsOptions,
	PermissionReply,
	PermissionRequest,
	PermissionRequestHandler,
	SequencedEvent,
	SessionConfigOption,
	SessionEventHandler,
	SessionInitData,
	SessionModeState,
} from "./agent-session-types.js";
import { type HostTool, type ToolKit, validateToolkits } from "./host-tools.js";
import { zodToJsonSchema } from "./host-tools-zod.js";
import type {
	JsonRpcNotification,
	JsonRpcRequest,
	JsonRpcResponse,
} from "./json-rpc.js";
import {
	type ConnectTerminalOptions,
	type Kernel,
	type KernelExecOptions,
	type KernelExecResult,
	type ProcessInfo as KernelProcessInfo,
	type KernelSpawnOptions,
	type ManagedProcess,
	type OpenShellOptions,
	type Permissions,
	type ShellHandle,
	type VirtualFileSystem,
	type VirtualStat,
} from "./runtime-compat.js";
import { findCargoBinary, resolveCargoBinary } from "./sidecar/cargo.js";

export type {
	AgentCapabilities,
	AgentInfo,
	GetEventsOptions,
	PermissionReply,
	PermissionRequest,
	PermissionRequestHandler,
	SequencedEvent,
	SessionConfigOption,
	SessionEventHandler,
	SessionInitData,
	SessionMode,
	SessionModeState,
} from "./agent-session-types.js";
export type {
	AcpTimeoutErrorData,
	JsonRpcError,
	JsonRpcErrorData,
	JsonRpcNotification,
	JsonRpcRequest,
	JsonRpcResponse,
} from "./json-rpc.js";
export { isAcpTimeoutErrorData } from "./json-rpc.js";
export type { ConnectTerminalOptions } from "./runtime-compat.js";

const ACP_PROTOCOL_VERSION = 1;
const SHELL_DISPOSE_TIMEOUT_MS = 5_000;

function defaultAcpClientCapabilities(): Record<string, unknown> {
	return {
		fs: {
			readTextFile: true,
			writeTextFile: true,
		},
		terminal: true,
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

/** Process tree node: extends kernel ProcessInfo with child references. */
export interface ProcessTreeNode extends KernelProcessInfo {
	children: ProcessTreeNode[];
}

/** A directory entry with metadata. */
export interface DirEntry {
	/** Absolute path to the entry. */
	path: string;
	type: "file" | "directory" | "symlink";
	size: number;
}

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
	acpAdapter: string;
	agentPackage: string;
	installed: boolean;
}

import { AGENT_CONFIGS, type AgentConfig, type AgentType } from "./agents.js";
import {
	type BaseFilesystemEntry,
	getBaseEnvironment,
	getBaseFilesystemEntries,
} from "./base-filesystem.js";
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
import {
	type FilesystemEntry,
	sortFilesystemEntries,
	snapshotVirtualFilesystem,
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
import {
	type CommandPackageMetadata,
	processSoftware,
	resolvePackageDir,
	type SoftwareInput,
	type SoftwareRoot,
} from "./packages.js";
import { allowAll, createNodeHostNetworkAdapter } from "./runtime-compat.js";
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
	NativeSidecarProcessClient,
	type RootFilesystemEntry,
	type SidecarRegisteredToolDefinition,
	type SidecarRequestFrame,
	type SidecarResponsePayload,
	type SidecarSessionState,
	serializeRootFilesystemForSidecar,
} from "./sidecar/rpc-client.js";

const OS_INSTRUCTIONS_FIXTURE = fileURLToPath(
	new URL("../fixtures/AGENTOS_SYSTEM_PROMPT.md", import.meta.url),
);

function buildOsInstructions(additional?: string): string {
	const base = readFileSync(OS_INSTRUCTIONS_FIXTURE, "utf-8");
	if (!additional) {
		return base;
	}
	return `${base}\n${additional}`;
}

export interface AgentOsSharedSidecarOptions {
	pool?: string;
}

export interface AgentOsCreateSidecarOptions {
	sidecarId?: string;
}

export type AgentOsSidecarConfig =
	| { kind: "shared"; pool?: string }
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
	sidecarClient: NativeSidecarProcessClient;
	sidecarSession: AuthenticatedSession;
	sidecarVm: CreatedVm;
	snapshotRootFilesystem?: () => Promise<RootSnapshotExport>;
	toolKits: ToolKit[];
	toolReference: string;
}

interface SessionEventSubscriber {
	handler: SessionEventHandler;
	lastDeliveredSequenceNumber: number | null;
}

interface AgentSessionEntry {
	sessionId: string;
	agentType: string;
	processId: string;
	pid: number | null;
	closed: boolean;
	modes: SessionModeState | null;
	configOptions: SessionConfigOption[];
	capabilities: AgentCapabilities;
	agentInfo: AgentInfo | null;
	highestSequenceNumber: number | null;
	events: SequencedEvent[];
	eventHandlers: Set<SessionEventSubscriber>;
	sessionEventDispatchScheduled: boolean;
	permissionHandlers: Set<PermissionRequestHandler>;
	configOverrides: Map<string, string>;
	pendingPermissionReplies: Map<
		string,
		{
			resolve: (reply: PermissionReply) => void;
			reject: (error: Error) => void;
			timer: ReturnType<typeof setTimeout>;
		}
	>;
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
	dataHandlers: Set<(data: Uint8Array) => void>;
	exitPromise: Promise<void>;
}

export type RootLowerInput =
	| { kind: "bundled-base-filesystem" }
	| RootSnapshotExport;

export interface RootFilesystemConfig {
	type?: "overlay";
	mode?: OverlayFilesystemMode;
	disableDefaultBaseLayer?: boolean;
	lowers?: RootLowerInput[];
}

export type MountConfigJsonPrimitive = string | number | boolean | null;
export type MountConfigJsonValue =
	| MountConfigJsonPrimitive
	| MountConfigJsonObject
	| MountConfigJsonValue[];

export interface MountConfigJsonObject {
	[key: string]: MountConfigJsonValue;
}

export interface NativeMountPluginDescriptor<
	TConfig extends MountConfigJsonObject = MountConfigJsonObject,
> {
	id: string;
	config?: TConfig;
}

/**
 * Compatibility path for arbitrary caller-supplied filesystems.
 * This maps to the sidecar `js_bridge` plugin during the migration.
 */
export interface PlainMountConfig {
	/** Path inside the VM to mount at. */
	path: string;
	/** The filesystem driver to mount. */
	driver: VirtualFileSystem;
	/** If true, write operations throw EROFS. */
	readOnly?: boolean;
}

/** Declarative native mount configuration that the sidecar can serialize. */
export interface NativeMountConfig {
	path: string;
	plugin: NativeMountPluginDescriptor;
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

export interface AgentOsOptions {
	/**
	 * Software to install in the VM. Each entry provides agents, tools,
	 * or WASM commands. Any object with a `commandDir` property (e.g.,
	 * registry packages like @rivet-dev/agent-os-coreutils) is treated
	 * as a WASM command source automatically. Arrays are flattened, so
	 * meta-packages that export arrays of sub-packages work directly.
	 */
	software?: SoftwareInput[];
	/** Loopback ports to exempt from SSRF checks (for testing with host-side mock servers). */
	loopbackExemptPorts?: number[];
	/**
	 * Allowed Node.js builtins for guest Node processes.
	 * Defaults to the hardened builtin set used by the native sidecar bridge.
	 */
	allowedNodeBuiltins?: string[];
	/**
	 * Host-side CWD for module access resolution. Sets the directory whose
	 * node_modules are projected into the VM at /root/node_modules/.
	 * Defaults to process.cwd().
	 */
	moduleAccessCwd?: string;
	/** Root filesystem configuration. Defaults to an overlay with the bundled base snapshot as its deepest lower. */
	rootFilesystem?: RootFilesystemConfig;
	/** Filesystems to mount at boot time. */
	mounts?: MountConfig[];
	/** Additional instructions appended to the base OS instructions written to /etc/agentos/instructions.md. */
	additionalInstructions?: string;
	/** Custom schedule driver for cron jobs. Defaults to TimerScheduleDriver. */
	scheduleDriver?: ScheduleDriver;
	/** Host-side toolkits available to agents inside the VM. */
	toolKits?: ToolKit[];
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
}

/** Configuration for a local MCP server (spawned as a child process). */
export interface McpServerConfigLocal {
	type: "local";
	/** Command to launch the MCP server. */
	command: string;
	/** Arguments for the command. */
	args?: string[];
	/** Environment variables for the server process. */
	env?: Record<string, string>;
}

/** Configuration for a remote MCP server (connected via URL). */
export interface McpServerConfigRemote {
	type: "remote";
	/** URL of the remote MCP server. */
	url: string;
	/** HTTP headers to include in requests to the server. */
	headers?: Record<string, string>;
}

export type McpServerConfig = McpServerConfigLocal | McpServerConfigRemote;

export interface AgentOsRuntimeAdmin {
	kernel: Kernel;
	rootView: VirtualFileSystem;
	env: Record<string, string>;
	sidecar: AgentOsSidecar;
}

export interface CreateSessionOptions {
	/** Working directory for the agent session inside the VM. */
	cwd?: string;
	/** Environment variables to pass to the agent process. */
	env?: Record<string, string>;
	/** MCP servers to make available to the agent during the session. */
	mcpServers?: McpServerConfig[];
	/** Skip OS instructions injection entirely (default false). */
	skipOsInstructions?: boolean;
	/** Additional instructions appended to the base OS instructions. */
	additionalInstructions?: string;
}

export interface SessionInfo {
	sessionId: string;
	agentType: string;
}

/** Result from AgentOs.prompt(). */
export interface PromptResult {
	/** Raw JSON-RPC response from the ACP adapter. */
	response: JsonRpcResponse;
	/** Accumulated agent text output from streamed message chunks. */
	text: string;
}

/** Information about a process spawned via AgentOs.spawn(). */
export interface SpawnedProcessInfo {
	pid: number;
	command: string;
	args: string[];
	running: boolean;
	exitCode: number | null;
}

const LEGACY_PERMISSION_METHOD = "request/permission";
const ACP_PERMISSION_METHOD = "session/request_permission";

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

function toRecord(value: unknown): Record<string, unknown> {
	return value && typeof value === "object" && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: {};
}

const ACP_SESSION_EVENT_RETENTION_LIMIT = 1024;
const CLOSED_SESSION_ID_RETENTION_LIMIT = 2048;

class BoundedSet<T> {
	readonly limit: number;
	#entries = new Map<T, undefined>();

	constructor(limit: number) {
		if (!Number.isInteger(limit) || limit <= 0) {
			throw new Error(`BoundedSet limit must be a positive integer: ${limit}`);
		}
		this.limit = limit;
	}

	add(value: T): void {
		if (this.#entries.has(value)) {
			this.#entries.delete(value);
		}
		this.#entries.set(value, undefined);
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

	delete(value: T): boolean {
		return this.#entries.delete(value);
	}

	get size(): number {
		return this.#entries.size;
	}
}

function cloneSequencedEvents(events: SequencedEvent[]): SequencedEvent[] {
	return events.map((event) => ({
		sequenceNumber: event.sequenceNumber,
		notification: event.notification,
	}));
}

function mergeSequencedEvents(
	current: SequencedEvent[],
	incoming: SequencedEvent[],
): SequencedEvent[] {
	const bySequence = new Map<number, SequencedEvent>();
	for (const event of current) {
		bySequence.set(event.sequenceNumber, event);
	}
	for (const event of incoming) {
		bySequence.set(event.sequenceNumber, event);
	}
	const merged = [...bySequence.values()].sort(
		(left, right) => left.sequenceNumber - right.sequenceNumber,
	);
	return merged.length <= ACP_SESSION_EVENT_RETENTION_LIMIT
		? merged
		: merged.slice(-ACP_SESSION_EVENT_RETENTION_LIMIT);
}

function nextHighestSequenceNumber(
	current: number | null,
	events: SequencedEvent[],
): number | null {
	const latest = events.at(-1)?.sequenceNumber;
	if (latest === undefined) {
		return current;
	}
	return current === null ? latest : Math.max(current, latest);
}

function shouldDispatchToSessionEventHandlers(
	notification: JsonRpcNotification,
): boolean {
	return notification.method === "session/update";
}

function latestBufferedSessionEventSequence(
	events: SequencedEvent[],
): number | null {
	for (let index = events.length - 1; index >= 0; index -= 1) {
		const event = events[index];
		if (event && shouldDispatchToSessionEventHandlers(event.notification)) {
			return event.sequenceNumber;
		}
	}
	return null;
}

function toSessionModes(value: unknown): SessionModeState | null {
	if (!value || typeof value !== "object" || Array.isArray(value)) {
		return null;
	}
	return value as SessionModeState;
}

function toSessionConfigOptions(value: unknown): SessionConfigOption[] {
	return Array.isArray(value) ? (value as SessionConfigOption[]) : [];
}

function toAgentCapabilities(value: unknown): AgentCapabilities {
	if (!value || typeof value !== "object" || Array.isArray(value)) {
		return {};
	}
	return value as AgentCapabilities;
}

function toAgentInfo(value: unknown): AgentInfo | null {
	if (!value || typeof value !== "object" || Array.isArray(value)) {
		return null;
	}
	if (typeof (value as { name?: unknown }).name !== "string") {
		return null;
	}
	return value as AgentInfo;
}

function sessionEntryFromInit(
	sessionId: string,
	agentType: string,
	initData: SessionInitData,
): AgentSessionEntry {
	return {
		sessionId,
		agentType,
		processId: "",
		pid: null,
		closed: false,
		modes: initData.modes ?? null,
		configOptions: initData.configOptions ?? [],
		capabilities: initData.capabilities ?? {},
		agentInfo: initData.agentInfo ?? null,
		highestSequenceNumber: null,
		events: [],
		eventHandlers: new Set(),
		sessionEventDispatchScheduled: false,
		permissionHandlers: new Set(),
		configOverrides: new Map(),
		pendingPermissionReplies: new Map(),
	};
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
	"/home/user",
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

const NODE_RUNTIME_BOOTSTRAP_COMMANDS = ["node", "npm", "npx"] as const;
const KERNEL_COMMAND_STUB = "#!/bin/sh\n# kernel command stub\n";
const REPO_ROOT = fileURLToPath(new URL("../../..", import.meta.url));
const SIDECAR_BINARY = join(REPO_ROOT, "target/debug/agent-os-sidecar");
const SIDECAR_BUILD_INPUTS = [
	join(REPO_ROOT, "Cargo.toml"),
	join(REPO_ROOT, "Cargo.lock"),
	join(REPO_ROOT, "crates/bridge"),
	join(REPO_ROOT, "crates/execution"),
	join(REPO_ROOT, "crates/kernel"),
	join(REPO_ROOT, "crates/sidecar"),
] as const;
let ensuredSidecarBinary: string | null = null;

interface PreparedCommandDirs {
	commandDirs: string[];
	dispose(): void;
}

function isWasmBinaryFile(path: string): boolean {
	try {
		const header = readFileSync(path);
		return (
			header.length >= 4 &&
			header[0] === 0x00 &&
			header[1] === 0x61 &&
			header[2] === 0x73 &&
			header[3] === 0x6d
		);
	} catch {
		return false;
	}
}

function collectBootstrapWasmCommands(commandDirs: string[]): string[] {
	const commands: string[] = [];
	const seen = new Set<string>();

	for (const dir of commandDirs) {
		let entries: string[];
		try {
			entries = readdirSync(dir).sort((a, b) => a.localeCompare(b));
		} catch {
			continue;
		}

		for (const entry of entries) {
			if (entry.startsWith(".")) {
				continue;
			}

			const fullPath = join(dir, entry);
			try {
				if (statSync(fullPath).isDirectory()) {
					continue;
				}
			} catch {
				continue;
			}

			if (!isWasmBinaryFile(fullPath) || seen.has(entry)) {
				continue;
			}

			seen.add(entry);
			commands.push(entry);
		}
	}

	return commands;
}

function resolveDeclaredCommandSource(
	commandDir: string,
	commandName: string,
	aliases: Record<string, string>,
): string | null {
	let current = commandName;
	const visited = new Set<string>();

	while (!visited.has(current)) {
		visited.add(current);

		const candidatePath = join(commandDir, current);
		if (isWasmBinaryFile(candidatePath)) {
			return candidatePath;
		}

		const next = aliases[current];
		if (!next) {
			return null;
		}

		current = next;
	}

	return null;
}

function prepareCommandDirs(
	commandPackages: CommandPackageMetadata[],
): PreparedCommandDirs {
	const commandDirs: string[] = [];
	const tempDirs: string[] = [];

	try {
		for (const commandPackage of commandPackages) {
			commandDirs.push(commandPackage.commandDir);

			const aliasEntries = Object.entries(commandPackage.aliases)
				.sort(([leftAlias], [rightAlias]) =>
					leftAlias.localeCompare(rightAlias),
				)
				.flatMap(([aliasName]) => {
					const aliasPath = join(commandPackage.commandDir, aliasName);
					if (isWasmBinaryFile(aliasPath)) {
						return [];
					}

					const sourcePath = resolveDeclaredCommandSource(
						commandPackage.commandDir,
						aliasName,
						commandPackage.aliases,
					);
					if (!sourcePath) {
						return [];
					}

					return [[aliasName, sourcePath] as const];
				});

			if (aliasEntries.length === 0) {
				continue;
			}

			const aliasDir = mkdtempSync(join(tmpdir(), "agent-os-command-aliases-"));
			for (const [aliasName, sourcePath] of aliasEntries) {
				writeFileSync(join(aliasDir, aliasName), readFileSync(sourcePath));
			}

			tempDirs.push(aliasDir);
			commandDirs.push(aliasDir);
		}
	} catch (error) {
		for (const tempDir of tempDirs) {
			rmSync(tempDir, { recursive: true, force: true });
		}
		throw error;
	}

	return {
		commandDirs,
		dispose() {
			for (const tempDir of tempDirs) {
				rmSync(tempDir, { recursive: true, force: true });
			}
		},
	};
}

function collectConfiguredLowerPaths(
	config?: RootFilesystemConfig,
): Set<string> {
	const paths = new Set<string>();

	for (const lower of config?.lowers ?? []) {
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
): BaseFilesystemEntry | undefined {
	for (const lower of config?.lowers ?? []) {
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

	return getBaseFilesystemEntries().find((entry) => entry.path === path);
}

function createKernelBootstrapLower(
	config: RootFilesystemConfig | undefined,
	commandNames: string[],
	extraEntries: FilesystemEntry[] = [],
): RootSnapshotExport | null {
	const includesBundledBaseLayer = !(config?.disableDefaultBaseLayer ?? false);
	const existingPaths = collectConfiguredLowerPaths(config);
	if (includesBundledBaseLayer) {
		for (const entry of getBaseFilesystemEntries()) {
			existingPaths.add(entry.path);
		}
	}
	const entries: FilesystemEntry[] = [
		{
			path: "/",
			type: "directory",
			mode: "755",
			uid: 0,
			gid: 0,
		},
	];

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

	const uniqueCommands = [...new Set(commandNames)].sort((a, b) =>
		a.localeCompare(b),
	);
	for (const command of uniqueCommands) {
		const stubPath = `/bin/${command}`;
		if (existingPaths.has(stubPath)) {
			continue;
		}
		entries.push({
			path: stubPath,
			type: "file",
			mode: "755",
			uid: 0,
			gid: 0,
			content: KERNEL_COMMAND_STUB,
			encoding: "utf8",
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
	client: NativeSidecarProcessClient,
	session: AuthenticatedSession,
	vm: CreatedVm,
	config: RootFilesystemConfig | undefined,
): Promise<void> {
	const existingPaths = new Set(
		(await client.snapshotRootFilesystem(session, vm)).map(
			(entry) => entry.path,
		),
	);
	const entries = buildLiveBootstrapDirectoryEntries(existingPaths, config);
	if (entries.length === 0) {
		return;
	}
	await client.bootstrapRootFilesystem(session, vm, entries);
}

function buildOsInstructionsBootstrapEntries(
	additionalInstructions?: string,
): FilesystemEntry[] {
	return [
		{
			path: "/etc/agentos/instructions.md",
			type: "file",
			mode: "0644",
			uid: 0,
			gid: 0,
			content: buildOsInstructions(additionalInstructions),
			encoding: "utf8",
		},
	];
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
			execFileSync(cargoBinary, ["build", "-q", "-p", "agent-os-sidecar"], {
				cwd: REPO_ROOT,
				stdio: "pipe",
			});
		} else if (!existsSync(SIDECAR_BINARY)) {
			execFileSync(
				resolveCargoBinary(),
				["build", "-q", "-p", "agent-os-sidecar"],
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

function collectGuestCommandPaths(commandDirs: string[]): Map<string, string> {
	const guestPaths = new Map<string, string>();

	for (const [index, commandDir] of commandDirs.entries()) {
		let entries: string[];
		try {
			entries = readdirSync(commandDir).sort((left, right) =>
				left.localeCompare(right),
			);
		} catch {
			continue;
		}

		for (const entry of entries) {
			if (entry.startsWith(".")) {
				continue;
			}
			if (!isWasmBinaryFile(join(commandDir, entry)) || guestPaths.has(entry)) {
				continue;
			}
			guestPaths.set(entry, `/__agentos/commands/${index}/${entry}`);
		}
	}

	return guestPaths;
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

function collectSidecarMountPlan(options: {
	mounts?: MountConfig[];
	moduleAccessCwd: string;
	softwareRoots: SoftwareRoot[];
	commandDirs: string[];
	shimDir: string | null;
}): {
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
			continue;
		}
		pushMount(mount);
	}

	const moduleNodeModules = resolveHostPath(
		join(options.moduleAccessCwd, "node_modules"),
	);
	if (existsSync(moduleNodeModules)) {
		hostPathMappings.push({
			vmPath: "/root/node_modules",
			hostPath: moduleNodeModules,
			readOnly: true,
		});
	}

	for (const root of options.softwareRoots) {
		pushMount({
			path: root.vmPath,
			plugin: createHostDirBackend({
				hostPath: root.hostPath,
				readOnly: true,
			}),
			readOnly: true,
		});
	}

	for (const [index, commandDir] of options.commandDirs.entries()) {
		pushMount({
			path: `/__agentos/commands/${index}`,
			plugin: createHostDirBackend({
				hostPath: commandDir,
				readOnly: true,
			}),
			readOnly: true,
		});
	}

	if (options.shimDir) {
		pushMount({
			path: "/usr/local/bin",
			plugin: createHostDirBackend({
				hostPath: options.shimDir,
				readOnly: true,
			}),
			readOnly: true,
		});
	}

	hostMounts.sort((left, right) => right.vmPath.length - left.vmPath.length);
	hostPathMappings.sort(
		(left, right) => right.vmPath.length - left.vmPath.length,
	);
	return { sidecarMounts, hostMounts, hostPathMappings };
}

function materializeToolShimDir(toolKits: ToolKit[]): string {
	const shimDir = mkdtempSync(join(tmpdir(), "agent-os-host-tools-shims-"));
	writeFileSync(join(shimDir, "agentos"), KERNEL_COMMAND_STUB, { mode: 0o755 });

	for (const toolKit of toolKits) {
		writeFileSync(
			join(shimDir, `agentos-${toolKit.name}`),
			KERNEL_COMMAND_STUB,
			{ mode: 0o755 },
		);
	}

	return shimDir;
}

function collectToolkitBootstrapCommands(toolKits: ToolKit[]): string[] {
	if (toolKits.length === 0) {
		return [];
	}

	return ["agentos", ...toolKits.map((toolKit) => `agentos-${toolKit.name}`)];
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

function toolToSidecarDefinition(
	tool: HostTool,
): SidecarRegisteredToolDefinition {
	return {
		description: tool.description,
		inputSchema: zodToJsonSchema(tool.inputSchema),
		...(tool.timeout !== undefined ? { timeoutMs: tool.timeout } : {}),
		...(tool.examples && tool.examples.length > 0
			? {
					examples: tool.examples.map((example) => ({
						description: example.description,
						input: example.input,
					})),
				}
			: {}),
	};
}

async function handleToolInvocation(
	request: SidecarRequestFrame,
	toolMap: ReadonlyMap<string, HostTool>,
): Promise<SidecarResponsePayload> {
	const payload = request.payload;
	if (payload.type !== "tool_invocation") {
		return {
			type: "tool_invocation_result",
			invocation_id: "unknown",
			error: `unsupported sidecar request type: ${payload.type}`,
		};
	}

	const tool = toolMap.get(payload.tool_key);
	if (!tool) {
		return {
			type: "tool_invocation_result",
			invocation_id: payload.invocation_id,
			error: `Unknown tool "${payload.tool_key}"`,
		};
	}

	const parsed = tool.inputSchema.safeParse(payload.input);
	if (!parsed.success) {
		return {
			type: "tool_invocation_result",
			invocation_id: payload.invocation_id,
			error: validationMessage(parsed.error),
		};
	}

	try {
		const result = await Promise.race([
			Promise.resolve(tool.execute(parsed.data)),
			new Promise<never>((_, reject) =>
				setTimeout(
					() =>
						reject(
							new Error(
								`Tool "${payload.tool_key}" timed out after ${payload.timeout_ms}ms`,
							),
						),
					payload.timeout_ms,
				),
			),
		]);
		return {
			type: "tool_invocation_result",
			invocation_id: payload.invocation_id,
			result,
		};
	} catch (error) {
		return {
			type: "tool_invocation_result",
			invocation_id: payload.invocation_id,
			error: validationMessage(error),
		};
	}
}

function buildToolMap(toolKits: ToolKit[]): Map<string, HostTool> {
	const toolMap = new Map<string, HostTool>();
	for (const toolKit of toolKits) {
		for (const [toolName, tool] of Object.entries(toolKit.tools)) {
			toolMap.set(`${toolKit.name}:${toolName}`, tool);
		}
	}
	return toolMap;
}

async function registerToolkitsOnSidecar(
	client: NativeSidecarProcessClient,
	session: AuthenticatedSession,
	vm: CreatedVm,
	toolKits: ToolKit[],
): Promise<string> {
	if (toolKits.length === 0) {
		return "";
	}

	let promptMarkdown = "";
	for (const toolKit of toolKits) {
		const registered = await client.registerToolkit(session, vm, {
			name: toolKit.name,
			description: toolKit.description,
			tools: Object.fromEntries(
				Object.entries(toolKit.tools).map(([toolName, tool]) => [
					toolName,
					toolToSidecarDefinition(tool),
				]),
			),
		});
		promptMarkdown = registered.promptMarkdown;
	}

	return promptMarkdown;
}

export class AgentOs {
	#kernel: Kernel;
	readonly sidecar: AgentOsSidecar;
	private _sessions = new Map<string, AgentSessionEntry>();
	private _closedSessionIds = new BoundedSet<string>(
		CLOSED_SESSION_ID_RETENTION_LIMIT,
	);
	private _sessionClosePromises = new Map<string, Promise<void>>();
	private _pendingSessionRequestResolvers = new Map<
		string,
		Set<{
			method: string;
			resolve: (response: JsonRpcResponse) => void;
		}>
	>();
	private _processes = new Map<
		number,
		{
			proc: ManagedProcess;
			command: string;
			args: string[];
			stdoutHandlers: Set<(data: Uint8Array) => void>;
			stderrHandlers: Set<(data: Uint8Array) => void>;
			exitHandlers: Set<(exitCode: number) => void>;
		}
	>();
	private _shells = new Map<string, ShellEntry>();
	private _pendingShellExitPromises = new Set<Promise<void>>();
	private _shellCounter = 0;
	private _acpTerminals = new Map<string, AcpTerminalEntry>();
	private _acpTerminalCounter = 0;
	private _moduleAccessCwd: string;
	private _softwareRoots: SoftwareRoot[];
	private _softwareAgentConfigs: Map<string, AgentConfig>;
	private _cronManager!: CronManager;
	private _toolKits: ToolKit[] = [];
	private _toolReference = "";
	private _hostMounts: HostMountInfo[];
	private _env: Record<string, string>;
	private _rootFilesystem: VirtualFileSystem;
	private _sidecarLease: AgentOsSidecarVmLease<AgentOsVmAdmin> | null = null;
	private readonly _sidecarClient: NativeSidecarProcessClient;
	private readonly _sidecarSession: AuthenticatedSession;
	private readonly _sidecarVm: CreatedVm;
	private readonly _disposeSidecarEventListener: () => void;

	private constructor(
		kernel: Kernel,
		sidecar: AgentOsSidecar,
		moduleAccessCwd: string,
		softwareRoots: SoftwareRoot[],
		softwareAgentConfigs: Map<string, AgentConfig>,
		hostMounts: HostMountInfo[],
		env: Record<string, string>,
		rootFilesystem: VirtualFileSystem,
		sidecarClient: NativeSidecarProcessClient,
		sidecarSession: AuthenticatedSession,
		sidecarVm: CreatedVm,
	) {
		this.#kernel = kernel;
		this.sidecar = sidecar;
		this._moduleAccessCwd = moduleAccessCwd;
		this._softwareRoots = softwareRoots;
		this._softwareAgentConfigs = softwareAgentConfigs;
		this._hostMounts = hostMounts;
		this._env = env;
		this._rootFilesystem = rootFilesystem;
		this._sidecarClient = sidecarClient;
		this._sidecarSession = sidecarSession;
		this._sidecarVm = sidecarVm;
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
		const processed = processSoftware(options?.software ?? []);
		const moduleAccessCwd = options?.moduleAccessCwd ?? process.cwd();
		const localMounts = await resolveCompatLocalMounts(options?.mounts);
		const toolKits = options?.toolKits;
		if (toolKits && toolKits.length > 0) {
			validateToolkits(toolKits);
		}

		const createVmAdmin = async (): Promise<AgentOsVmAdmin> => {
			const preparedCommandDirs = prepareCommandDirs(processed.commandPackages);
			const toolBootstrapCommands = collectToolkitBootstrapCommands(
				toolKits ?? [],
			);
			const bootstrapLower = createKernelBootstrapLower(
				options?.rootFilesystem,
				[
					...collectBootstrapWasmCommands(preparedCommandDirs.commandDirs),
					...NODE_RUNTIME_BOOTSTRAP_COMMANDS,
					...toolBootstrapCommands,
				],
				buildOsInstructionsBootstrapEntries(options?.additionalInstructions),
			);
			let toolReference = "";
			let rootBridge: NativeSidecarKernelProxy | null = null;
			let kernel: Kernel | null = null;
			let client: NativeSidecarProcessClient | null = null;
			let toolShimDir: string | null = null;
			let cleanedUp = false;

			const cleanup = async (): Promise<void> => {
				if (cleanedUp) {
					return;
				}
				cleanedUp = true;
				if (toolShimDir) {
					rmSync(toolShimDir, { recursive: true, force: true });
					toolShimDir = null;
				}
				preparedCommandDirs.dispose();
			};

			try {
				const env: Record<string, string> = getBaseEnvironment();
				if (toolKits && toolKits.length > 0) {
					toolShimDir = materializeToolShimDir(toolKits);
				}
				const commandGuestPaths = collectGuestCommandPaths(
					preparedCommandDirs.commandDirs,
				);
				const { sidecarMounts, hostMounts, hostPathMappings } =
					collectSidecarMountPlan({
						mounts: options?.mounts,
						moduleAccessCwd,
						softwareRoots: processed.softwareRoots,
						commandDirs: preparedCommandDirs.commandDirs,
						shimDir: toolShimDir,
					});
				client = NativeSidecarProcessClient.spawn({
					cwd: REPO_ROOT,
					command: ensureNativeSidecarBinary(),
					args: [],
					frameTimeoutMs: 60_000,
				});
				const session = await client.authenticateAndOpenSession();
				const sidecarPermissions = serializePermissionsForSidecar(
					options?.permissions ?? allowAll,
				);
				const nativeVm = await client.createVm(session, {
					runtime: "java_script",
					metadata: {
						...Object.fromEntries(
							Object.entries(env).map(([key, value]) => [`env.${key}`, value]),
						),
					},
					rootFilesystem: serializeRootFilesystemForSidecar(
						options?.rootFilesystem,
						bootstrapLower,
					),
					permissions: sidecarPermissions,
				});
				await client.waitForEvent(
					(event) =>
						event.payload.type === "vm_lifecycle" &&
						event.payload.state === "ready",
					10_000,
				);
				await client.configureVm(session, nativeVm, {
					mounts: sidecarMounts,
					permissions: sidecarPermissions,
					moduleAccessCwd,
					commandPermissions: processed.commandPermissions,
					allowedNodeBuiltins: options?.allowedNodeBuiltins,
					loopbackExemptPorts: options?.loopbackExemptPorts,
				});
				if (toolKits && toolKits.length > 0) {
					toolReference = await registerToolkitsOnSidecar(
						client,
						session,
						nativeVm,
						toolKits,
					);
					commandGuestPaths.set("agentos", "/bin/agentos");
					for (const toolKit of toolKits) {
						commandGuestPaths.set(
							`agentos-${toolKit.name}`,
							`/bin/agentos-${toolKit.name}`,
						);
					}
				}

				rootBridge = new NativeSidecarKernelProxy({
					client,
					session,
					vm: nativeVm,
					env,
					cwd: "/home/user",
					localMounts,
					commandGuestPaths,
					onDispose: cleanup,
				});
				await bootstrapLiveBootstrapDirectories(
					client,
					session,
					nativeVm,
					options?.rootFilesystem,
				);

				kernel = rootBridge as unknown as Kernel;
				const snapshotClient = client;

				return {
					env,
					hostMounts,
					kernel,
					rootView: rootBridge.createRootView(),
					sidecarClient: client,
					sidecarSession: session,
					sidecarVm: nativeVm,
					snapshotRootFilesystem: async () =>
						createSnapshotExport(
							convertSidecarRootSnapshotEntries(
								await snapshotClient.snapshotRootFilesystem(session, nativeVm),
							),
						),
					toolKits: toolKits ?? [],
					toolReference,
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
				if (kernel) {
					await kernel.dispose().catch(() => {});
				}
				if (rootBridge) {
					await rootBridge.dispose().catch(() => {});
				} else {
					await client?.dispose().catch(() => {});
					await cleanup();
				}
				throw error;
			}
		};

		const sidecar = resolveAgentOsSidecar(options?.sidecar);
		let sidecarLease: AgentOsSidecarVmLease<AgentOsVmAdmin> | null = null;

		try {
			sidecarLease = await leaseAgentOsSidecarVm(sidecar, {
				createVm: async () => createVmAdmin(),
			});
			const vmAdmin = sidecarLease.admin;

			const vm = new AgentOs(
				vmAdmin.kernel,
				sidecar,
				moduleAccessCwd,
				processed.softwareRoots,
				processed.agentConfigs,
				vmAdmin.hostMounts,
				vmAdmin.env,
				vmAdmin.rootView,
				vmAdmin.sidecarClient,
				vmAdmin.sidecarSession,
				vmAdmin.sidecarVm,
			);
			vm._sidecarLease = sidecarLease;
			vm._toolKits = vmAdmin.toolKits;
			vm._toolReference = vmAdmin.toolReference;
			vm._installSidecarRequestHandler();
			vm._cronManager = new CronManager(
				vm,
				options?.scheduleDriver ?? new TimerScheduleDriver(),
			);

			return vm;
		} catch (error) {
			await sidecarLease?.dispose().catch(() => {});
			throw error;
		}
	}

	async exec(
		command: string,
		options?: KernelExecOptions,
	): Promise<KernelExecResult> {
		return this.#kernel.exec(command, options);
	}

	private _trackProcess(
		proc: ManagedProcess,
		command: string,
		args: string[],
		stdoutHandlers: Set<(data: Uint8Array) => void>,
		stderrHandlers: Set<(data: Uint8Array) => void>,
		exitHandlers: Set<(exitCode: number) => void>,
	): { pid: number } {
		const entry = {
			proc,
			command,
			args,
			stdoutHandlers,
			stderrHandlers,
			exitHandlers,
		};
		this._processes.set(proc.pid, entry);

		void proc.wait().then((code) => {
			for (const h of exitHandlers) h(code);
		});

		return { pid: proc.pid };
	}

	spawn(
		command: string,
		args: string[],
		options?: KernelSpawnOptions,
	): { pid: number } {
		const stdoutHandlers = new Set<(data: Uint8Array) => void>();
		const stderrHandlers = new Set<(data: Uint8Array) => void>();
		const exitHandlers = new Set<(exitCode: number) => void>();

		// Include caller-provided callbacks in the initial handler sets.
		if (options?.onStdout) stdoutHandlers.add(options.onStdout);
		if (options?.onStderr) stderrHandlers.add(options.onStderr);

		const proc = this.#kernel.spawn(command, args, {
			...options,
			onStdout: (data) => {
				for (const h of stdoutHandlers) h(data);
			},
			onStderr: (data) => {
				for (const h of stderrHandlers) h(data);
			},
		});

		return this._trackProcess(
			proc,
			command,
			args,
			stdoutHandlers,
			stderrHandlers,
			exitHandlers,
		);
	}

	/** Write data to a process's stdin. */
	writeProcessStdin(pid: number, data: string | Uint8Array): void {
		const entry = this._processes.get(pid);
		if (!entry) throw new Error(`Process not found: ${pid}`);
		entry.proc.writeStdin(data);
	}

	/** Close a process's stdin stream. */
	closeProcessStdin(pid: number): void {
		const entry = this._processes.get(pid);
		if (!entry) throw new Error(`Process not found: ${pid}`);
		entry.proc.closeStdin();
	}

	/** Subscribe to stdout data from a process. Returns an unsubscribe function. */
	onProcessStdout(
		pid: number,
		handler: (data: Uint8Array) => void,
	): () => void {
		const entry = this._processes.get(pid);
		if (!entry) throw new Error(`Process not found: ${pid}`);
		entry.stdoutHandlers.add(handler);
		return () => {
			entry.stdoutHandlers.delete(handler);
		};
	}

	/** Subscribe to stderr data from a process. Returns an unsubscribe function. */
	onProcessStderr(
		pid: number,
		handler: (data: Uint8Array) => void,
	): () => void {
		const entry = this._processes.get(pid);
		if (!entry) throw new Error(`Process not found: ${pid}`);
		entry.stderrHandlers.add(handler);
		return () => {
			entry.stderrHandlers.delete(handler);
		};
	}

	/** Subscribe to process exit. Returns an unsubscribe function. */
	onProcessExit(pid: number, handler: (exitCode: number) => void): () => void {
		const entry = this._processes.get(pid);
		if (!entry) throw new Error(`Process not found: ${pid}`);
		// If already exited, call immediately.
		if (entry.proc.exitCode !== null) {
			handler(entry.proc.exitCode);
			return () => {};
		}
		entry.exitHandlers.add(handler);
		return () => {
			entry.exitHandlers.delete(handler);
		};
	}

	/** Wait for a process to exit. Returns the exit code. */
	waitProcess(pid: number): Promise<number> {
		const entry = this._processes.get(pid);
		if (!entry) throw new Error(`Process not found: ${pid}`);
		return entry.proc.wait();
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

	private async _copyPath(from: string, to: string): Promise<void> {
		const stat = await this._vfs().lstat(from);
		if (stat.isSymbolicLink) {
			const target = await this._vfs().readlink(from);
			await this._vfs().symlink(target, to);
			return;
		}
		if (stat.isDirectory) {
			await this._mkdirp(posixPath.dirname(to));
			if (!(await this.#kernel.exists(to))) {
				await this.#kernel.mkdir(to);
			}
			await this._vfs().chmod(to, stat.mode);
			await this._vfs().chown(to, stat.uid, stat.gid);
			const entries = await this.#kernel.readdir(from);
			for (const entry of entries) {
				if (entry === "." || entry === "..") continue;
				const fromPath = from === "/" ? `/${entry}` : `${from}/${entry}`;
				const toPath = to === "/" ? `/${entry}` : `${to}/${entry}`;
				await this._copyPath(fromPath, toPath);
			}
			return;
		}
		const content = await this.#kernel.readFile(from);
		await this.writeFile(to, content);
		await this._vfs().chmod(to, stat.mode);
		await this._vfs().chown(to, stat.uid, stat.gid);
	}

	async readFile(path: string): Promise<Uint8Array> {
		this._assertSafeAbsolutePath(path);
		return this.#kernel.readFile(path);
	}

	async writeFile(path: string, content: string | Uint8Array): Promise<void> {
		this._assertWritableAbsolutePath(path);
		return this.#kernel.writeFile(path, content);
	}

	async writeFiles(entries: BatchWriteEntry[]): Promise<BatchWriteResult[]> {
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

	async readFiles(paths: string[]): Promise<BatchReadResult[]> {
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
		const parts = path.split("/").filter(Boolean);
		let current = "";
		for (const part of parts) {
			current += `/${part}`;
			if (!(await this.#kernel.exists(current))) {
				await this.#kernel.mkdir(current);
			}
		}
	}

	async mkdir(path: string, options?: { recursive?: boolean }): Promise<void> {
		if (options?.recursive) {
			return this._mkdirp(path);
		}
		this._assertSafeAbsolutePath(path);
		return this.#kernel.mkdir(path);
	}

	async readdir(path: string): Promise<string[]> {
		this._assertSafeAbsolutePath(path);
		return this.#kernel.readdir(path);
	}

	async readdirRecursive(
		path: string,
		options?: ReaddirRecursiveOptions,
	): Promise<DirEntry[]> {
		this._assertSafeAbsolutePath(path);
		const maxDepth = options?.maxDepth;
		const exclude = options?.exclude ? new Set(options.exclude) : undefined;
		const results: DirEntry[] = [];

		// BFS queue: [dirPath, currentDepth]
		const queue: [string, number][] = [[path, 0]];

		while (queue.length > 0) {
			const item = queue.shift();
			if (!item) break;
			const [dirPath, depth] = item;
			const entries = await this.#kernel.readdir(dirPath);

			for (const name of entries) {
				if (name === "." || name === "..") continue;
				if (exclude?.has(name)) continue;

				const fullPath = dirPath === "/" ? `/${name}` : `${dirPath}/${name}`;
				const s = await this.#kernel.stat(fullPath);

				if (s.isSymbolicLink) {
					results.push({
						path: fullPath,
						type: "symlink",
						size: s.size,
					});
				} else if (s.isDirectory) {
					results.push({
						path: fullPath,
						type: "directory",
						size: s.size,
					});
					if (maxDepth === undefined || depth < maxDepth) {
						queue.push([fullPath, depth + 1]);
					}
				} else {
					results.push({
						path: fullPath,
						type: "file",
						size: s.size,
					});
				}
			}
		}

		return results;
	}

	async stat(path: string): Promise<VirtualStat> {
		this._assertSafeAbsolutePath(path);
		return this.#kernel.stat(path);
	}

	async exists(path: string): Promise<boolean> {
		this._assertSafeAbsolutePath(path);
		return this.#kernel.exists(path);
	}

	async snapshotRootFilesystem(): Promise<RootSnapshotExport> {
		const nativeSnapshot = this._sidecarLease?.admin.snapshotRootFilesystem;
		if (nativeSnapshot) {
			return nativeSnapshot();
		}

		return createSnapshotExport(
			await snapshotVirtualFilesystem(this._rootFilesystem),
		);
	}

	mountFs(
		path: string,
		driver: VirtualFileSystem,
		options?: { readOnly?: boolean },
	): void {
		this._assertSafeAbsolutePath(path);
		this.#kernel.mountFs(path, driver, { readOnly: options?.readOnly });
	}

	unmountFs(path: string): void {
		this._assertSafeAbsolutePath(path);
		this.#kernel.unmountFs(path);
	}

	async move(from: string, to: string): Promise<void> {
		this._assertSafeAbsolutePath(from);
		this._assertSafeAbsolutePath(to);
		const sourceStat = await this._vfs().lstat(from);
		if (!sourceStat.isDirectory || sourceStat.isSymbolicLink) {
			return this.#kernel.rename(from, to);
		}
		await this._copyPath(from, to);
		await this.delete(from, { recursive: true });
	}

	async delete(path: string, options?: { recursive?: boolean }): Promise<void> {
		this._assertSafeAbsolutePath(path);
		const s = await this.#kernel.stat(path);
		if (s.isDirectory) {
			if (options?.recursive) {
				const entries = await this.#kernel.readdir(path);
				for (const entry of entries) {
					if (entry === "." || entry === "..") continue;
					await this.delete(`${path}/${entry}`, { recursive: true });
				}
			}
			return this.#kernel.removeDir(path);
		}
		return this.#kernel.removeFile(path);
	}

	async fetch(port: number, request: Request): Promise<Response> {
		const url = new URL(request.url);
		const responsePayload = JSON.parse(
			await this._sidecarClient.vmFetch(this._sidecarSession, this._sidecarVm, {
				port,
				method: request.method,
				path: `${url.pathname}${url.search}`,
				headersJson: JSON.stringify(
					Object.fromEntries(request.headers.entries()),
				),
				...(request.method !== "GET" && request.method !== "HEAD"
					? { body: await request.text() }
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
			statusText: responsePayload.statusText,
			headers,
		});
	}

	openShell(options?: OpenShellOptions): { shellId: string } {
		const shellId = `shell-${++this._shellCounter}`;
		const dataHandlers = new Set<(data: Uint8Array) => void>();

		const handle = this.#kernel.openShell(options);
		handle.onData = (data) => {
			for (const h of dataHandlers) h(data);
		};

		const entry: ShellEntry = {
			handle,
			dataHandlers,
			exitPromise: Promise.resolve(),
		};
		const exitPromise = handle.wait().then(
			() => undefined,
			() => undefined,
		);
		entry.exitPromise = exitPromise.finally(() => {
			this._pendingShellExitPromises.delete(entry.exitPromise);
			if (this._shells.get(shellId) === entry) {
				this._shells.delete(shellId);
			}
		});
		this._pendingShellExitPromises.add(entry.exitPromise);
		this._shells.set(shellId, entry);
		return { shellId };
	}

	async connectTerminal(options?: ConnectTerminalOptions): Promise<number> {
		return this.#kernel.connectTerminal(options);
	}

	/** Write data to a shell's PTY input. */
	writeShell(shellId: string, data: string | Uint8Array): void {
		const entry = this._shells.get(shellId);
		if (!entry) throw new Error(`Shell not found: ${shellId}`);
		entry.handle.write(data);
	}

	/** Subscribe to data output from a shell. Returns an unsubscribe function. */
	onShellData(
		shellId: string,
		handler: (data: Uint8Array) => void,
	): () => void {
		const entry = this._shells.get(shellId);
		if (!entry) throw new Error(`Shell not found: ${shellId}`);
		entry.dataHandlers.add(handler);
		return () => {
			entry.dataHandlers.delete(handler);
		};
	}

	/** Notify a shell of terminal resize. */
	resizeShell(shellId: string, cols: number, rows: number): void {
		const entry = this._shells.get(shellId);
		if (!entry) throw new Error(`Shell not found: ${shellId}`);
		entry.handle.resize(cols, rows);
	}

	/** Kill a shell process and remove it from tracking. */
	closeShell(shellId: string): void {
		const entry = this._shells.get(shellId);
		if (!entry) throw new Error(`Shell not found: ${shellId}`);
		entry.handle.kill();
		this._shells.delete(shellId);
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
	listProcesses(): SpawnedProcessInfo[] {
		return [...this._processes.values()].map(({ proc, command, args }) => ({
			pid: proc.pid,
			command,
			args,
			running: proc.exitCode === null,
			exitCode: proc.exitCode,
		}));
	}

	/** Returns all kernel processes across all active runtimes (WASM and Node). */
	allProcesses(): KernelProcessInfo[] {
		if (this.#kernel instanceof NativeSidecarKernelProxy) {
			return this.#kernel.snapshotProcesses();
		}
		return [...this.#kernel.processes.values()];
	}

	/** Returns processes organized as a tree using ppid relationships. */
	processTree(): ProcessTreeNode[] {
		const all = this.allProcesses();
		const nodeMap = new Map<number, ProcessTreeNode>();

		// Index: create a tree node for each process
		for (const proc of all) {
			nodeMap.set(proc.pid, { ...proc, children: [] });
		}

		// Wire: attach each node to its parent
		const roots: ProcessTreeNode[] = [];
		for (const node of nodeMap.values()) {
			const parent = nodeMap.get(node.ppid);
			if (parent) {
				parent.children.push(node);
			} else {
				roots.push(node);
			}
		}

		return roots;
	}

	/** Returns info about a specific process by PID. Throws if not found. */
	getProcess(pid: number): SpawnedProcessInfo {
		const entry = this._processes.get(pid);
		if (!entry) {
			throw new Error(`Process not found: ${pid}`);
		}
		return {
			pid: entry.proc.pid,
			command: entry.command,
			args: entry.args,
			running: entry.proc.exitCode === null,
			exitCode: entry.proc.exitCode,
		};
	}

	/** Send SIGTERM to gracefully stop a process. No-op if already exited. */
	stopProcess(pid: number): void {
		const entry = this._processes.get(pid);
		if (!entry) {
			throw new Error(`Process not found: ${pid}`);
		}
		if (entry.proc.exitCode !== null) return;
		entry.proc.kill();
	}

	/** Send SIGKILL to force-kill a process. No-op if already exited. */
	killProcess(pid: number): void {
		const entry = this._processes.get(pid);
		if (!entry) {
			throw new Error(`Process not found: ${pid}`);
		}
		if (entry.proc.exitCode !== null) return;
		entry.proc.kill(9);
	}

	/** Returns all active sessions with their IDs and agent types. */
	listSessions(): SessionInfo[] {
		return [...this._sessions.values()].map((s) => ({
			sessionId: s.sessionId,
			agentType: s.agentType,
		}));
	}

	/** Internal helper: retrieve a session or throw. */
	private _requireSession(sessionId: string): AgentSessionEntry {
		const session = this._sessions.get(sessionId);
		if (!session) {
			throw new Error(`Session not found: ${sessionId}`);
		}
		return session;
	}

	/** Returns all registered agents with their installation status. */
	listAgents(): AgentRegistryEntry[] {
		// Collect all agent IDs from both package configs and hardcoded configs.
		const allIds = new Set<string>([
			...this._softwareAgentConfigs.keys(),
			...Object.keys(AGENT_CONFIGS),
		]);

		return [...allIds]
			.map((id) => {
				const config = this._resolveAgentConfig(id);
				if (!config) return null;

				let installed = false;
				try {
					// Check package roots first, then CWD-based node_modules.
					const vmPrefix = `/root/node_modules/${config.acpAdapter}`;
					let hostPkgJsonPath: string | null = null;
					for (const root of this._softwareRoots) {
						if (root.vmPath === vmPrefix) {
							hostPkgJsonPath = join(root.hostPath, "package.json");
							break;
						}
					}
					if (!hostPkgJsonPath) {
						hostPkgJsonPath = join(
							resolvePackageDir(this._moduleAccessCwd, config.acpAdapter),
							"package.json",
						);
					}
					readFileSync(hostPkgJsonPath);
					installed = true;
				} catch {
					// Package not installed
				}
				return {
					id,
					acpAdapter: config.acpAdapter,
					agentPackage: config.agentPackage,
					installed,
				};
			})
			.filter((entry): entry is AgentRegistryEntry => entry !== null);
	}

	private _syncSessionState(
		session: AgentSessionEntry,
		state: Pick<
			SidecarSessionState,
			| "processId"
			| "pid"
			| "closed"
			| "modes"
			| "configOptions"
			| "agentCapabilities"
			| "agentInfo"
			| "events"
		>,
	): void {
		session.processId = state.processId;
		session.pid = state.pid ?? null;
		session.closed = state.closed;
		session.modes = toSessionModes(state.modes);
		session.configOptions = toSessionConfigOptions(state.configOptions);
		this._applySyntheticConfigOverrides(session);
		session.capabilities = toAgentCapabilities(state.agentCapabilities);
		session.agentInfo = toAgentInfo(state.agentInfo);
		session.events = mergeSequencedEvents(
			session.events,
			state.events.map((event) => ({
				sequenceNumber: event.sequenceNumber,
				notification: toJsonRpcNotification(event.notification),
			})),
		);
		session.highestSequenceNumber = nextHighestSequenceNumber(
			session.highestSequenceNumber,
			session.events,
		);
	}

	private _applySessionUpdate(
		session: AgentSessionEntry,
		notification: JsonRpcNotification,
	): void {
		if (notification.method !== "session/update") {
			return;
		}

		const params = toRecord(notification.params);
		const update = toRecord(params.update ?? params);
		const sessionUpdate = update.sessionUpdate;

		if (
			sessionUpdate === "current_mode_update" &&
			typeof update.currentModeId === "string" &&
			session.modes
		) {
			session.modes = {
				...session.modes,
				currentModeId: update.currentModeId,
			};
		}

		if (
			(sessionUpdate === "config_option_update" ||
				sessionUpdate === "config_options_update") &&
			Array.isArray(update.configOptions)
		) {
			session.configOptions = update.configOptions as SessionConfigOption[];
		}
	}

	private _recordSessionNotification(
		session: AgentSessionEntry,
		sequenceNumber: number,
		notification: JsonRpcNotification,
	): void {
		session.events = mergeSequencedEvents(session.events, [
			{ sequenceNumber, notification },
		]);
		session.highestSequenceNumber = nextHighestSequenceNumber(
			session.highestSequenceNumber,
			session.events,
		);
		this._applySessionUpdate(session, notification);

		if (shouldDispatchToSessionEventHandlers(notification)) {
			this._queueSessionEventDispatch(session);
		}

		if (
			notification.method === LEGACY_PERMISSION_METHOD ||
			notification.method === ACP_PERMISSION_METHOD
		) {
			const params = toRecord(notification.params);
			const permissionId = params.permissionId;
			if (
				typeof permissionId === "string" ||
				typeof permissionId === "number"
			) {
				const request: PermissionRequest = {
					permissionId: String(permissionId),
					description:
						typeof params.description === "string"
							? params.description
							: undefined,
					params,
				};
				for (const handler of session.permissionHandlers) {
					handler(request);
				}
			}
		}
	}

	private _queueSessionEventDispatch(session: AgentSessionEntry): void {
		if (
			session.sessionEventDispatchScheduled ||
			session.eventHandlers.size === 0
		) {
			return;
		}

		session.sessionEventDispatchScheduled = true;
		queueMicrotask(() => {
			session.sessionEventDispatchScheduled = false;
			this._flushSessionEventHandlers(session);
		});
	}

	private _flushSessionEventHandlers(session: AgentSessionEntry): void {
		if (session.eventHandlers.size === 0) {
			return;
		}

		const dispatchableEvents = session.events.filter((event) =>
			shouldDispatchToSessionEventHandlers(event.notification),
		);
		if (dispatchableEvents.length === 0) {
			return;
		}

		for (const subscriber of [...session.eventHandlers]) {
			const pendingEvents = dispatchableEvents.filter((event) =>
				subscriber.lastDeliveredSequenceNumber === null
					? true
					: event.sequenceNumber > subscriber.lastDeliveredSequenceNumber,
			);
			for (const event of pendingEvents) {
				try {
					subscriber.handler(event.notification);
				} catch {
					// Ignore subscriber callback failures and keep event delivery moving.
				}
				subscriber.lastDeliveredSequenceNumber = event.sequenceNumber;
			}
		}
	}

	private _subscribeSessionEvents(
		session: AgentSessionEntry,
		handler: SessionEventHandler,
		options?: { replayBuffered?: boolean },
	): () => void {
		const replayBuffered = options?.replayBuffered !== false;
		const subscriber: SessionEventSubscriber = {
			handler,
			lastDeliveredSequenceNumber: replayBuffered
				? null
				: latestBufferedSessionEventSequence(session.events),
		};

		if (replayBuffered) {
			for (const event of session.events) {
				if (!shouldDispatchToSessionEventHandlers(event.notification)) {
					continue;
				}
				handler(event.notification);
				subscriber.lastDeliveredSequenceNumber = event.sequenceNumber;
			}
		}

		session.eventHandlers.add(subscriber);
		return () => {
			session.eventHandlers.delete(subscriber);
		};
	}

	private _nextSyntheticSequenceNumber(session: AgentSessionEntry): number {
		return (
			Math.min(0, ...session.events.map((event) => event.sequenceNumber)) - 1
		);
	}

	private _applySyntheticConfigOverrides(session: AgentSessionEntry): void {
		if (session.configOverrides.size === 0) {
			return;
		}

		session.configOptions = session.configOptions.map((option) => {
			const override =
				session.configOverrides.get(option.id) ??
				(typeof option.category === "string"
					? session.configOverrides.get(option.category)
					: undefined);
			return override === undefined
				? option
				: { ...option, currentValue: override };
		});
	}

	private _recordSyntheticConfigUpdate(session: AgentSessionEntry): void {
		this._recordSessionNotification(
			session,
			this._nextSyntheticSequenceNumber(session),
			{
				jsonrpc: "2.0",
				method: "session/update",
				params: {
					update: {
						sessionUpdate: "config_option_update",
						configOptions: session.configOptions,
					},
				},
			},
		);
	}

	private _applyCodexConfigFallback(
		session: AgentSessionEntry,
		category: string,
		value: string,
	): JsonRpcResponse {
		const option = session.configOptions.find(
			(entry) => entry.category === category,
		);
		if (option) {
			session.configOverrides.set(option.id, value);
		}
		session.configOverrides.set(category, value);
		this._applySyntheticConfigOverrides(session);
		this._recordSyntheticConfigUpdate(session);
		return {
			jsonrpc: "2.0",
			id: null,
			result: {
				configOptions: session.configOptions,
				via: "codex-config-fallback",
			},
		};
	}

	private _augmentPromptParams(
		session: AgentSessionEntry,
		params?: Record<string, unknown>,
	): Record<string, unknown> | undefined {
		if (session.agentType !== "codex") {
			return params;
		}

		const model = session.configOptions.find(
			(option) => option.category === "model",
		)?.currentValue;
		const thoughtLevel = session.configOptions.find(
			(option) => option.category === "thought_level",
		)?.currentValue;
		if (!model && !thoughtLevel) {
			return params;
		}

		const meta =
			params?._meta &&
			typeof params._meta === "object" &&
			!Array.isArray(params._meta)
				? { ...(params._meta as Record<string, unknown>) }
				: {};
		meta.agentOsCodexConfig = {
			...(typeof model === "string" ? { model } : {}),
			...(typeof thoughtLevel === "string"
				? { thought_level: thoughtLevel }
				: {}),
		};
		return {
			...(params ?? {}),
			_meta: meta,
		};
	}

	private _handleSidecarEvent(
		event: Parameters<NativeSidecarProcessClient["onEvent"]>[0] extends (
			event: infer T,
		) => void
			? T
			: never,
	): void {
		if (event.payload.type !== "structured") {
			return;
		}
		if (event.payload.name !== "acp.session_event") {
			return;
		}

		const sessionId = event.payload.detail.session_id;
		const session = sessionId ? this._sessions.get(sessionId) : undefined;
		if (!session) {
			return;
		}

		const sequenceNumber = Number(event.payload.detail.sequence_number);
		if (!Number.isInteger(sequenceNumber)) {
			return;
		}

		const notificationText = event.payload.detail.notification;
		if (typeof notificationText !== "string") {
			return;
		}

		try {
			this._recordSessionNotification(
				session,
				sequenceNumber,
				toJsonRpcNotification(JSON.parse(notificationText)),
			);
		} catch {
			// Ignore malformed event payloads from the sidecar.
		}
	}

	private _unsupportedConfigResponse(
		agentType: string,
		category: string,
	): JsonRpcResponse {
		const message =
			agentType === "opencode" && category === "model"
				? "OpenCode reports available models, but model switching must be configured before createSession() because ACP session/set_config_option is not implemented."
				: `The ${category} config option is read-only for ${agentType} sessions.`;
		return {
			jsonrpc: "2.0",
			id: null,
			error: {
				code: -32601,
				message,
			},
		};
	}

	private async _sendSessionRequest(
		sessionId: string,
		method: string,
		params?: Record<string, unknown>,
	): Promise<JsonRpcResponse> {
		const session = this._requireSession(sessionId);
		const requestParams =
			method === "session/prompt"
				? this._augmentPromptParams(session, params)
				: params;
		const response = await new Promise<JsonRpcResponse>((resolve, reject) => {
			const resolvers =
				this._pendingSessionRequestResolvers.get(sessionId) ?? new Set();
			const resolver = {
				method,
				resolve: (response: JsonRpcResponse) => {
					resolve(response);
				},
			};
			resolvers.add(resolver);
			this._pendingSessionRequestResolvers.set(sessionId, resolvers);

			void this._sidecarClient
				.sessionRequest(this._sidecarSession, this._sidecarVm, {
					sessionId,
					method,
					params: requestParams,
				})
				.then(resolve, reject)
				.finally(() => {
					const nextResolvers =
						this._pendingSessionRequestResolvers.get(sessionId);
					if (!nextResolvers) {
						return;
					}
					nextResolvers.delete(resolver);
					if (nextResolvers.size === 0) {
						this._pendingSessionRequestResolvers.delete(sessionId);
					}
				});
		});
		const liveSession = this._sessions.get(sessionId);
		if (liveSession) {
			await this._hydrateSessionState(liveSession).catch(() => {});
		}
		if (!response.error) {
			if (
				method === "session/set_mode" &&
				typeof requestParams?.modeId === "string" &&
				session.modes
			) {
				session.modes = {
					...session.modes,
					currentModeId: requestParams.modeId,
				};
			}
			if (
				method === "session/set_config_option" &&
				typeof requestParams?.configId === "string" &&
				typeof requestParams?.value === "string"
			) {
				const nextValue = requestParams.value;
				session.configOptions = session.configOptions.map((option) =>
					option.id === requestParams.configId
						? { ...option, currentValue: nextValue }
						: option,
				);
			}
		}
		return response;
	}

	private async _setSessionConfigByCategory(
		sessionId: string,
		category: string,
		value: string,
	): Promise<JsonRpcResponse> {
		const session = this._requireSession(sessionId);
		const option = session.configOptions.find(
			(entry) => entry.category === category,
		);
		if (option?.readOnly) {
			return this._unsupportedConfigResponse(session.agentType, category);
		}
		const response = await this._sendSessionRequest(
			sessionId,
			"session/set_config_option",
			{
				configId: option?.id ?? category,
				value,
			},
		);
		if (
			session.agentType === "codex" &&
			response.error?.code === -32601 &&
			toRecord(response.error.data).method === "session/set_config_option"
		) {
			return this._applyCodexConfigFallback(session, category, value);
		}
		return response;
	}

	private _removeSession(sessionId: string): void {
		this._sessions.delete(sessionId);
	}

	private _abortPendingSessionRequests(sessionId: string): void {
		const resolvers = this._pendingSessionRequestResolvers.get(sessionId);
		if (!resolvers) {
			return;
		}
		this._pendingSessionRequestResolvers.delete(sessionId);
		const response: JsonRpcResponse = {
			jsonrpc: "2.0",
			id: null,
			error: {
				code: -32_000,
				message: `Session closed: ${sessionId}`,
			},
		};
		for (const resolver of resolvers) {
			resolver.resolve(response);
		}
	}

	private _cancelPendingPromptRequests(sessionId: string): boolean {
		const resolvers = this._pendingSessionRequestResolvers.get(sessionId);
		if (!resolvers) {
			return false;
		}

		const response: JsonRpcResponse = {
			jsonrpc: "2.0",
			id: null,
			result: {
				stopReason: "cancelled",
			},
		};

		let cancelledPrompt = false;
		for (const resolver of [...resolvers]) {
			if (resolver.method !== "session/prompt") {
				continue;
			}
			resolvers.delete(resolver);
			resolver.resolve(response);
			cancelledPrompt = true;
		}

		if (resolvers.size === 0) {
			this._pendingSessionRequestResolvers.delete(sessionId);
		}

		return cancelledPrompt;
	}

	private _rejectPendingPermissionReplies(sessionId: string): void {
		const session = this._sessions.get(sessionId);
		if (!session) {
			return;
		}
		for (const [
			permissionId,
			pendingReply,
		] of session.pendingPermissionReplies) {
			clearTimeout(pendingReply.timer);
			pendingReply.reject(
				new Error(`Session closed before permission reply: ${permissionId}`),
			);
		}
		session.pendingPermissionReplies.clear();
	}

	private _collectHostProcessTree(rootPid: number): number[] {
		let output = "";
		try {
			output = execFileSync("ps", ["-eo", "pid=,ppid="], {
				encoding: "utf8",
			});
		} catch {
			return [];
		}

		const rows = output
			.split("\n")
			.map((line) => line.trim())
			.filter(Boolean)
			.map((line) => {
				const [pid, ppid] = line.split(/\s+/);
				return {
					pid: Number(pid),
					ppid: Number(ppid),
				};
			})
			.filter((row) => Number.isFinite(row.pid) && Number.isFinite(row.ppid));
		if (!rows.some((row) => row.pid === rootPid)) {
			return [];
		}

		const byParent = new Map<number, number[]>();
		for (const row of rows) {
			const children = byParent.get(row.ppid);
			if (children) {
				children.push(row.pid);
			} else {
				byParent.set(row.ppid, [row.pid]);
			}
		}

		const discovered: number[] = [];
		const queue = [rootPid];
		while (queue.length > 0) {
			const pid = queue.shift();
			if (pid === undefined || discovered.includes(pid)) {
				continue;
			}
			discovered.push(pid);
			for (const childPid of byParent.get(pid) ?? []) {
				queue.push(childPid);
			}
		}
		return discovered;
	}

	private _tryForceCloseSessionProcess(sessionId: string): void {
		const session = this._sessions.get(sessionId);
		if (!session?.pid) {
			return;
		}
		const sharedPidUsers = [...this._sessions.values()].filter(
			(candidate) =>
				candidate.sessionId !== sessionId && candidate.pid === session.pid,
		);
		if (sharedPidUsers.length > 0) {
			return;
		}
		if (this.#kernel instanceof NativeSidecarKernelProxy && session.processId) {
			void this._sidecarClient
				.killProcess(
					this._sidecarSession,
					this._sidecarVm,
					session.processId,
					"SIGKILL",
				)
				.catch(() => {});
		}
		// Native-sidecar control requests share the same framed transport as the
		// in-flight ACP prompt request. If that prompt is wedged in adapter I/O,
		// the sidecar may not service queued `kill_process` / `close_agent_session`
		// frames until the prompt itself unwinds. Kill the host process tree
		// directly as an out-of-band escape hatch so the blocked prompt tears down
		// immediately and the sidecar close path can finish deterministically.
		try {
			process.kill(-session.pid, "SIGKILL");
		} catch {
			// Ignore ESRCH/EPERM when the session pid is not a process-group leader.
		}
		for (const pid of this._collectHostProcessTree(session.pid).reverse()) {
			try {
				process.kill(pid, "SIGKILL");
			} catch {
				// Ignore ESRCH and permission errors; close_agent_session remains the source of truth.
			}
		}
		try {
			process.kill(session.pid, "SIGKILL");
		} catch {
			// Ignore ESRCH and permission errors; close_agent_session remains the source of truth.
		}
	}

	private async _closeSessionInternal(sessionId: string): Promise<void> {
		const closing = this._sessionClosePromises.get(sessionId);
		if (closing) {
			return closing;
		}
		if (this._closedSessionIds.has(sessionId)) {
			return;
		}

		const hasPendingRequests =
			(this._pendingSessionRequestResolvers.get(sessionId)?.size ?? 0) > 0;
		this._abortPendingSessionRequests(sessionId);
		this._rejectPendingPermissionReplies(sessionId);
		if (hasPendingRequests) {
			this._tryForceCloseSessionProcess(sessionId);
		}

		this._requireSession(sessionId);
		this._removeSession(sessionId);
		this._closedSessionIds.add(sessionId);

		const closePromise = this._sidecarClient
			.closeAgentSession(this._sidecarSession, this._sidecarVm, sessionId)
			.finally(() => {
				this._sessionClosePromises.delete(sessionId);
			});
		this._sessionClosePromises.set(sessionId, closePromise);
		await closePromise;
	}

	private async _hydrateSessionState(
		session: AgentSessionEntry,
	): Promise<void> {
		const state = await this._sidecarClient.getSessionState(
			this._sidecarSession,
			this._sidecarVm,
			session.sessionId,
			{
				...(session.highestSequenceNumber !== null
					? {
							acknowledgedSequenceNumber: session.highestSequenceNumber,
						}
					: {}),
			},
		);
		this._syncSessionState(session, state);
	}

	async createSession(
		agentType: AgentType | string,
		options?: CreateSessionOptions,
	): Promise<{ sessionId: string }> {
		const config = this._resolveAgentConfig(agentType);
		if (!config) {
			throw new Error(`Unknown agent type: ${agentType}`);
		}

		const toolReference = this._toolReference || undefined;
		let extraArgs: string[] = [];
		let extraEnv: Record<string, string> = {};
		if (config.prepareInstructions) {
			const cwd = options?.cwd ?? "/home/user";
			const skipBase = options?.skipOsInstructions ?? false;
			const hasToolRef = !!toolReference;
			const hasAdditionalInstructions = !!options?.additionalInstructions;

			if (!skipBase || hasToolRef || hasAdditionalInstructions) {
				const prepared = await config.prepareInstructions(
					this.#kernel,
					cwd,
					options?.additionalInstructions,
					{ toolReference, skipBase },
				);
				if (prepared.args) extraArgs = prepared.args;
				if (prepared.env) extraEnv = prepared.env;
			}
		}

		const launchArgs = [...(config.launchArgs ?? []), ...extraArgs];
		let launchEnv = { ...config.defaultEnv, ...extraEnv, ...options?.env };
		const sessionCwd = options?.cwd ?? "/home/user";
		const adapterEntrypoint = this._resolveAdapterBin(config.acpAdapter);
		if (
			(agentType === "pi" || agentType === "pi-cli") &&
			!launchEnv.PI_ACP_PI_COMMAND
		) {
			launchEnv = {
				...launchEnv,
				PI_ACP_PI_COMMAND: this._resolvePackageBin(config.agentPackage, "pi"),
			};
		}

		const created = await this._sidecarClient.createSession(
			this._sidecarSession,
			this._sidecarVm,
			{
				agentType: String(agentType),
				runtime: "java_script",
				adapterEntrypoint,
				args: launchArgs,
				env: launchEnv,
				cwd: sessionCwd,
				mcpServers: options?.mcpServers ?? [],
				protocolVersion: ACP_PROTOCOL_VERSION,
				clientCapabilities: defaultAcpClientCapabilities(),
			},
		);

		const initData: SessionInitData = {
			modes: toSessionModes(created.modes) ?? undefined,
			configOptions: toSessionConfigOptions(created.configOptions),
			capabilities: toAgentCapabilities(created.agentCapabilities),
			agentInfo: toAgentInfo(created.agentInfo) ?? undefined,
		};
		const session = sessionEntryFromInit(
			created.sessionId,
			String(agentType),
			initData,
		);
		this._closedSessionIds.delete(created.sessionId);
		this._sessions.set(created.sessionId, session);

		try {
			await this._hydrateSessionState(session);
		} catch (error) {
			this._removeSession(created.sessionId);
			throw error;
		}

		return { sessionId: created.sessionId };
	}

	/**
	 * Resolve the VM bin entry point of an ACP adapter package.
	 * Reads from the host filesystem since kernel.readFile() does NOT see
	 * the ModuleAccessFileSystem overlay.
	 */
	private _resolveAdapterBin(adapterPackage: string): string {
		return this._resolvePackageBin(adapterPackage);
	}

	private _resolvePackageBin(packageName: string, binName?: string): string {
		const vmPrefix = `/root/node_modules/${packageName}`;
		let hostPkgJsonPath: string | null = null;
		for (const root of this._softwareRoots) {
			if (root.vmPath === vmPrefix) {
				hostPkgJsonPath = join(root.hostPath, "package.json");
				break;
			}
		}
		// Fall back to CWD-based node_modules.
		if (!hostPkgJsonPath) {
			hostPkgJsonPath = join(
				resolvePackageDir(this._moduleAccessCwd, packageName),
				"package.json",
			);
		}
		const pkg = JSON.parse(readFileSync(hostPkgJsonPath, "utf-8"));

		let binEntry: string | undefined;
		if (typeof pkg.bin === "string") {
			binEntry = pkg.bin;
		} else if (typeof pkg.bin === "object" && pkg.bin !== null) {
			binEntry =
				(binName ? (pkg.bin as Record<string, string>)[binName] : undefined) ??
				(pkg.bin as Record<string, string>)[packageName] ??
				Object.values(pkg.bin)[0];
		}

		if (!binEntry) {
			throw new Error(`No bin entry found in ${packageName}/package.json`);
		}

		return `${vmPrefix}/${binEntry}`;
	}

	private _installSidecarRequestHandler(): void {
		const toolMap = buildToolMap(this._toolKits);
		this._sidecarClient.setSidecarRequestHandler((request) => {
			switch (request.payload.type) {
				case "tool_invocation":
					return handleToolInvocation(request, toolMap);
				case "permission_request":
					return this._handlePermissionSidecarRequest(request);
				case "acp_request":
					return this._handleAcpSidecarRequest(request.payload.request);
				case "js_bridge_call":
					return Promise.resolve({
						type: "js_bridge_result",
						call_id: request.payload.call_id,
						error: `unsupported sidecar request type: ${request.payload.type}`,
					});
			}
		});
	}

	private async _handleAcpSidecarRequest(
		request: JsonRpcRequest,
	): Promise<SidecarResponsePayload> {
		return {
			type: "acp_request_result",
			response: await this._dispatchAcpSidecarRequest(request),
		};
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
			case ACP_PERMISSION_METHOD:
				return this._handleAcpPermissionRequest(request, params);
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

	private _normalizeAcpPermissionOptionId(
		options: Array<Record<string, unknown>> | undefined,
		reply: PermissionReply,
	): string | null {
		const optionTargets =
			reply === "always"
				? {
						optionIds: new Set(["always", "allow_always"]),
						kinds: new Set(["allow_always"]),
					}
				: reply === "once"
					? {
							optionIds: new Set(["once", "allow_once"]),
							kinds: new Set(["allow_once"]),
						}
					: {
							optionIds: new Set(["reject", "reject_once"]),
							kinds: new Set(["reject_once"]),
						};

		const matched = options?.find((option) => {
			const optionId =
				typeof option.optionId === "string" ? option.optionId : undefined;
			const kind = typeof option.kind === "string" ? option.kind : undefined;
			return (
				(optionId !== undefined && optionTargets.optionIds.has(optionId)) ||
				(kind !== undefined && optionTargets.kinds.has(kind))
			);
		});
		if (matched && typeof matched.optionId === "string") {
			return matched.optionId;
		}
		if (reply === "always") {
			return "allow_always";
		}
		if (reply === "once") {
			return "allow_once";
		}
		return "reject_once";
	}

	private _buildAcpPermissionResult(
		reply: PermissionReply,
		params: Record<string, unknown>,
	): Record<string, unknown> {
		const options = Array.isArray(params.options)
			? params.options.filter(
					(option): option is Record<string, unknown> =>
						typeof option === "object" && option !== null,
				)
			: undefined;
		const optionId = this._normalizeAcpPermissionOptionId(options, reply);
		return {
			outcome: optionId
				? {
						outcome: "selected",
						optionId,
					}
				: {
						outcome: "cancelled",
					},
		};
	}

	private async _handleAcpPermissionRequest(
		request: JsonRpcRequest,
		params: Record<string, unknown>,
	): Promise<unknown> {
		const sessionId =
			typeof params.sessionId === "string" ? params.sessionId : undefined;
		if (!sessionId) {
			throw new AcpDispatchError(
				-32602,
				`${ACP_PERMISSION_METHOD} requires a sessionId`,
			);
		}

		const session = this._sessions.get(sessionId);
		if (!session) {
			throw new AcpDispatchError(-32602, `Session not found: ${sessionId}`);
		}

		const permissionId = String(request.id);
		const permissionParams: Record<string, unknown> = {
			...params,
			permissionId,
			_acpMethod: request.method,
		};
		if (session.permissionHandlers.size === 0) {
			return this._buildAcpPermissionResult("reject", permissionParams);
		}

		const reply = await new Promise<PermissionReply>((resolve, reject) => {
			const timer = setTimeout(() => {
				session.pendingPermissionReplies.delete(permissionId);
				reject(
					new Error(`Timed out waiting for permission reply: ${permissionId}`),
				);
			}, 120_000);
			session.pendingPermissionReplies.set(permissionId, {
				resolve,
				reject,
				timer,
			});

			const permissionRequest: PermissionRequest = {
				permissionId,
				description:
					typeof permissionParams["description"] === "string"
						? permissionParams["description"]
						: undefined,
				params: permissionParams,
			};
			for (const handler of session.permissionHandlers) {
				handler(permissionRequest);
			}
		});

		return this._buildAcpPermissionResult(reply, permissionParams);
	}

	private _handleUnsupportedAcpSidecarRequest(
		request: JsonRpcRequest,
	): SidecarResponsePayload {
		return {
			type: "acp_request_result",
			response: {
				jsonrpc: "2.0",
				id: request.id,
				error: {
					code: -32601,
					message: `Method not found: ${request.method}`,
					data: {
						method: request.method,
					},
				},
			},
		};
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
				onStderr: (data) => {
					this._appendAcpTerminalOutput(terminal, data);
				},
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

	private _handleAcpWriteTerminal(params: Record<string, unknown>): null {
		const method = "terminal/write";
		const terminal = this._requireAcpTerminal(params, method);
		const data = this._requireAcpStringParam(params, "data", method);
		const encoding = this._optionalAcpStringParam(params, "encoding", method);
		terminal.handle.write(
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

	private async _handlePermissionSidecarRequest(
		request: SidecarRequestFrame,
	): Promise<SidecarResponsePayload> {
		const payload = request.payload;
		if (payload.type !== "permission_request") {
			return {
				type: "permission_request_result",
				permission_id: "unknown",
				error: `unsupported sidecar request type: ${payload.type}`,
			};
		}

		const session = this._sessions.get(payload.session_id);
		if (!session) {
			return {
				type: "permission_request_result",
				permission_id: payload.permission_id,
				error: `Session not found: ${payload.session_id}`,
			};
		}

		if (session.permissionHandlers.size === 0) {
			return {
				type: "permission_request_result",
				permission_id: payload.permission_id,
				reply: "reject",
			};
		}

		const params = toRecord(payload.params);
		try {
			const reply = await new Promise<PermissionReply>((resolve, reject) => {
				const timer = setTimeout(() => {
					session.pendingPermissionReplies.delete(payload.permission_id);
					reject(
						new Error(
							`Timed out waiting for permission reply: ${payload.permission_id}`,
						),
					);
				}, 120_000);
				session.pendingPermissionReplies.set(payload.permission_id, {
					resolve,
					reject,
					timer,
				});

				const permissionRequest: PermissionRequest = {
					permissionId: payload.permission_id,
					description:
						typeof params.description === "string"
							? params.description
							: undefined,
					params,
				};
				for (const handler of session.permissionHandlers) {
					handler(permissionRequest);
				}
			});

			return {
				type: "permission_request_result",
				permission_id: payload.permission_id,
				reply,
			};
		} catch (error) {
			return {
				type: "permission_request_result",
				permission_id: payload.permission_id,
				error: validationMessage(error),
			};
		}
	}

	/**
	 * Resolve an agent config by ID. Package-provided configs take
	 * precedence over the hardcoded AGENT_CONFIGS.
	 */
	private _resolveAgentConfig(agentType: string): AgentConfig | undefined {
		return (
			this._softwareAgentConfigs.get(agentType) ??
			(AGENT_CONFIGS as Record<string, AgentConfig>)[agentType]
		);
	}

	/**
	 * Verify a session exists and is active.
	 * Throws if the session is not found.
	 */
	resumeSession(sessionId: string): { sessionId: string } {
		this._requireSession(sessionId);
		return { sessionId };
	}

	/**
	 * Gracefully destroy a session: cancel any pending work, close the client,
	 * and remove from tracking. Unlike close() which is abrupt, this attempts
	 * a graceful shutdown sequence.
	 */
	async destroySession(sessionId: string): Promise<void> {
		this._requireSession(sessionId);
		try {
			await this.cancelSession(sessionId);
		} catch {
			// Ignore cancellation failures during teardown.
		}
		await this._closeSessionInternal(sessionId);
	}

	// ── Flat session API (ID-based) ───────────────────────────────

	async prompt(sessionId: string, text: string): Promise<PromptResult> {
		const session = this._requireSession(sessionId);
		let agentText = "";
		const handler: SessionEventHandler = (event) => {
			const params = toRecord(event.params);
			const update = toRecord(params.update);
			if (update?.sessionUpdate === "agent_message_chunk") {
				const content = toRecord(update.content);
				if (typeof content.text === "string") {
					agentText += content.text;
				}
			}
		};
		const unsubscribe = this._subscribeSessionEvents(session, handler, {
			replayBuffered: false,
		});

		try {
			const response = await this._sendSessionRequest(
				sessionId,
				"session/prompt",
				{
					prompt: [{ type: "text", text }],
				},
			);
			return { response, text: agentText };
		} finally {
			unsubscribe();
		}
	}

	/** Cancel ongoing agent work for a session. */
	async cancelSession(sessionId: string): Promise<JsonRpcResponse> {
		this._requireSession(sessionId);
		const cancelledPendingPrompt =
			this._cancelPendingPromptRequests(sessionId);
		if (cancelledPendingPrompt) {
			// Session control requests share the same framed sidecar transport as an
			// in-flight prompt request. If the adapter is blocked in prompt I/O, a
			// synchronous cancel RPC can wedge behind that prompt until the transport
			// timeout fires. Resolve the local prompt immediately, then let the sidecar
			// cancellation continue in the background as best effort.
			void this._sendSessionRequest(sessionId, "session/cancel").catch(() => {});
			return {
				jsonrpc: "2.0",
				id: null,
				result: {
					cancelled: true,
					requested: true,
					via: "prompt-fallback",
				},
			};
		}
		return this._sendSessionRequest(sessionId, "session/cancel");
	}

	closeSession(sessionId: string): void {
		if (
			!this._sessions.has(sessionId) &&
			!this._closedSessionIds.has(sessionId) &&
			!this._sessionClosePromises.has(sessionId)
		) {
			throw new Error(`Session not found: ${sessionId}`);
		}
		const closePromise = this._closeSessionInternal(sessionId);
		// `closeSession()` is intentionally fire-and-forget; suppress unhandled
		// rejections here and let tracked close promises surface errors to any
		// internal/test callers awaiting `_sessionClosePromises`.
		void closePromise.catch(() => {});
	}

	getSessionEvents(
		sessionId: string,
		options?: GetEventsOptions,
	): SequencedEvent[] {
		let events = cloneSequencedEvents(this._requireSession(sessionId).events);
		if (options?.since !== undefined) {
			events = events.filter((event) => event.sequenceNumber > options.since!);
		}
		if (options?.method !== undefined) {
			events = events.filter(
				(event) => event.notification.method === options.method,
			);
		}
		return events;
	}

	async respondPermission(
		sessionId: string,
		permissionId: string,
		reply: PermissionReply,
	): Promise<JsonRpcResponse> {
		const session = this._requireSession(sessionId);
		const pendingReply = session.pendingPermissionReplies.get(permissionId);
		if (pendingReply) {
			session.pendingPermissionReplies.delete(permissionId);
			clearTimeout(pendingReply.timer);
			pendingReply.resolve(reply);
			return {
				jsonrpc: "2.0",
				id: null,
				result: {
					permissionId,
					reply,
					via: "sidecar-request",
				},
			};
		}

		return this._sendSessionRequest(sessionId, LEGACY_PERMISSION_METHOD, {
			permissionId,
			reply,
		});
	}

	async setSessionMode(
		sessionId: string,
		modeId: string,
	): Promise<JsonRpcResponse> {
		return this._sendSessionRequest(sessionId, "session/set_mode", {
			modeId,
		});
	}

	getSessionModes(sessionId: string): SessionModeState | null {
		return this._requireSession(sessionId).modes;
	}

	async setSessionModel(
		sessionId: string,
		model: string,
	): Promise<JsonRpcResponse> {
		return this._setSessionConfigByCategory(sessionId, "model", model);
	}

	async setSessionThoughtLevel(
		sessionId: string,
		level: string,
	): Promise<JsonRpcResponse> {
		return this._setSessionConfigByCategory(sessionId, "thought_level", level);
	}

	getSessionConfigOptions(sessionId: string): SessionConfigOption[] {
		return [...this._requireSession(sessionId).configOptions];
	}

	getSessionCapabilities(sessionId: string): AgentCapabilities | null {
		const caps = this._requireSession(sessionId).capabilities;
		return Object.keys(caps).length > 0 ? caps : null;
	}

	getSessionAgentInfo(sessionId: string): AgentInfo | null {
		return this._requireSession(sessionId).agentInfo;
	}

	async rawSessionSend(
		sessionId: string,
		method: string,
		params?: Record<string, unknown>,
	): Promise<JsonRpcResponse> {
		return this._sendSessionRequest(sessionId, method, params);
	}

	async rawSend(
		sessionId: string,
		method: string,
		params?: Record<string, unknown>,
	): Promise<JsonRpcResponse> {
		return this.rawSessionSend(sessionId, method, params);
	}

	onSessionEvent(sessionId: string, handler: SessionEventHandler): () => void {
		const session = this._requireSession(sessionId);
		return this._subscribeSessionEvents(session, handler);
	}

	onPermissionRequest(
		sessionId: string,
		handler: PermissionRequestHandler,
	): () => void {
		const session = this._requireSession(sessionId);
		session.permissionHandlers.add(handler);
		return () => {
			session.permissionHandlers.delete(handler);
		};
	}

	// ── Cron ────────────────────────────────────────────────────

	/** Schedule a cron job. Returns a handle with the job ID and a cancel method. */
	scheduleCron(options: CronJobOptions): CronJob {
		return this._cronManager.schedule(options);
	}

	/** List all registered cron jobs. */
	listCronJobs(): CronJobInfo[] {
		return this._cronManager.list();
	}

	/** Cancel a cron job by ID. */
	cancelCronJob(id: string): void {
		this._cronManager.cancel(id);
	}

	/** Subscribe to cron lifecycle events (fire, complete, error). */
	onCronEvent(handler: CronEventHandler): void {
		this._cronManager.onEvent(handler);
	}

	async dispose(): Promise<void> {
		this._cronManager.dispose();

		for (const sessionId of [...this._sessions.keys()]) {
			await this._closeSessionInternal(sessionId).catch(() => {});
		}

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
		await waitForTrackedExitPromises(
			[...shellExitPromises, ...terminalExitPromises],
			SHELL_DISPOSE_TIMEOUT_MS,
		);

		this._disposeSidecarEventListener();

		const sidecarLease = this._sidecarLease;
		this._sidecarLease = null;
		if (sidecarLease) {
			return sidecarLease.dispose();
		}
		return this.#kernel.dispose();
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
			config?.kind === "shared" ? { pool: config.pool } : undefined,
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

interface AgentOsSidecarState {
	description: AgentOsSidecarDescription;
	activeLeases: Set<AgentOsSidecarLeaseRecord>;
	sharedPool?: string;
}

const sidecarStates = new WeakMap<AgentOsSidecar, AgentOsSidecarState>();
const sharedSidecars = new Map<string, AgentOsSidecar>();

export class AgentOsSidecar {
	constructor(
		sidecarId: string,
		placement: AgentOsSidecarPlacement,
		sharedPool?: string,
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
	const sidecarId = options.sidecarId ?? `agent-os-sidecar-${randomUUID()}`;
	return new AgentOsSidecar(sidecarId, {
		kind: "explicit",
		sidecarId,
	});
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
		return existing;
	}

	const sidecar = new AgentOsSidecar(
		`agent-os-shared-sidecar:${pool}`,
		{ kind: "shared", ...(pool ? { pool } : {}) },
		pool,
	);
	sharedSidecars.set(pool, sidecar);
	return sidecar;
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
		async createSessionTransport(sessionBootstrap) {
			transport = await createInProcessSidecarTransport(
				sessionBootstrap,
				options,
			);
			return transport;
		},
	});

	let disposed = false;
	let leaseRecord: AgentOsSidecarLeaseRecord | undefined;

	try {
		const session = await client.createSession({
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
