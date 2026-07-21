import type {
	LiveSidecarRequestPayload,
	LiveSidecarResponsePayload,
} from "./callbacks.js";
import type { MountConfigJsonObject } from "./descriptors.js";
import type { LiveSidecarEventSelector } from "./event-buffer.js";
import {
	decodeGuestFilesystemContent,
	encodeGuestFilesystemContent,
	type LiveRootFilesystemEntry,
	type LiveRootFilesystemEntryEncoding,
	type LiveRootFilesystemLowerDescriptor,
} from "./filesystem.js";
import type { CreateVmConfig } from "./generated/CreateVmConfig.js";
import type { LiveOwnershipScope } from "./ownership.js";
import type {
	LiveFsPermissionRule,
	LivePatternPermissionRule,
	LivePermissionMode,
	LivePermissionScope,
	LivePermissionsPolicy,
	LiveRulePermissions,
} from "./permissions.js";
import type {
	LiveEventFrame,
	LiveRequestFrame,
	LiveResponseFrame,
	LiveSidecarRequestFrame,
	LiveSidecarRequestHandler,
	LiveSidecarResponseFrame,
	ProtocolFramePayloadCodec,
} from "./protocol-frames.js";
import type {
	LiveFilesystemOperation,
	LiveGuestRuntimeKind,
	LiveWasmPermissionTier,
} from "./protocol-maps.js";
import { SIDECAR_PROTOCOL_SCHEMA } from "./protocol-schema.js";
import type { LiveRequestPayload } from "./request-payloads.js";
import type {
	LiveGuestDirEntry,
	LiveResponsePayload,
} from "./response-payloads.js";
import type { SidecarProcessTransport } from "./sidecar-client.js";
import type {
	LiveGuestFilesystemStat,
	LiveProcessSnapshotEntry,
	LiveSocketStateEntry,
} from "./state.js";

export { SidecarEventBufferOverflow } from "./event-buffer.js";
export {
	SidecarProcessError,
	SidecarProcessExited,
	SidecarSilenceTimeout,
} from "./sidecar-errors.js";
// `Sidecar` is the public name for the native sidecar process client. The class
// is `SidecarProcess` internally; consumers import it as `Sidecar` via the
// `@rivet-dev/agentos-runtime-core/sidecar-client` subpath and the package root.
export { SidecarProcess as Sidecar };

const BRIDGE_CONTRACT_VERSION = 1;

const DEFAULT_SIDECAR_EVENT_BUFFER_CAPACITY = 4_096;
const DEFAULT_SIDECAR_GRACEFUL_EXIT_MS = 5_000;
const DEFAULT_SIDECAR_FORCE_EXIT_MS = 2_000;

type OwnershipScope = LiveOwnershipScope;

type GuestRuntimeKind = Extract<
	LiveGuestRuntimeKind,
	"java_script" | "python" | "web_assembly"
>;
type WasmPermissionTier = LiveWasmPermissionTier;
type StandaloneWasmBackend = "v8" | "wasmtime";
type RootFilesystemEntryEncoding = LiveRootFilesystemEntryEncoding;

type RootFilesystemDescriptor = {
	mode?: "ephemeral" | "read_only";
	disableDefaultBaseLayer?: boolean;
	lowers?: RootFilesystemLowerDescriptor[];
	bootstrapEntries?: RootFilesystemEntry[];
};

export interface RootFilesystemEntry extends LiveRootFilesystemEntry {}

export interface RootFilesystemLowerDescriptor {
	kind: "snapshot" | "bundled_base_filesystem";
	entries?: RootFilesystemEntry[];
}

type WireRootFilesystemLowerDescriptor = LiveRootFilesystemLowerDescriptor;
type WireRootFilesystemEntry = LiveRootFilesystemEntry;

export interface GuestFilesystemStat extends LiveGuestFilesystemStat {}

export interface SidecarSocketStateEntry {
	processId: string;
	host?: string;
	port?: number;
	path?: string;
}

export interface SidecarSignalHandlerRegistration {
	action: "default" | "ignore" | "user";
	mask: number[];
	flags: number;
}

export interface SidecarSignalState {
	processId: string;
	handlers: Map<number, SidecarSignalHandlerRegistration>;
}

export interface SidecarProcessSnapshotEntry {
	processId: string;
	pid: number;
	ppid: number;
	pgid: number;
	sid: number;
	driver: string;
	command: string;
	args: string[];
	cwd: string;
	status: "running" | "exited" | "stopped";
	exitCode: number | null;
}

export interface SidecarQueueSnapshotEntry {
	name: string;
	category: string;
	depth: number;
	highWater: number;
	capacity: number;
	fillPercent: number;
}

export interface SidecarResourceSnapshot {
	runningProcesses: number;
	stoppedProcesses: number;
	exitedProcesses: number;
	fdTables: number;
	openFds: number;
	pipes: number;
	pipeBufferedBytes: number;
	ptys: number;
	ptyBufferedInputBytes: number;
	ptyBufferedOutputBytes: number;
	sockets: number;
	socketListeners: number;
	socketConnections: number;
	socketBufferedBytes: number;
	socketDatagramQueueLen: number;
	wasmReservedMemoryBytes: number;
	wasmtimeEngineProfiles: number;
	wasmtimeModuleEntries: number;
	wasmtimeModuleCacheHits: number;
	wasmtimeModuleCacheMisses: number;
	wasmtimeModuleCacheEvictions: number;
	wasmtimeCompiledSourceBytes: number;
	wasmtimeChargedModuleBytes: number;
	wasmtimeCompileTimeMicros: number;
	wasmtimeProcessRetainedRssBytes?: number;
	queueSnapshots: SidecarQueueSnapshotEntry[];
}

export interface SidecarZombieTimerCount {
	count: number;
}

export interface SidecarRegisteredHostCallbackExample {
	description: string;
	input: unknown;
}

export interface SidecarRegisteredHostCallbackDefinition {
	description: string;
	inputSchema: unknown;
	timeoutMs?: number;
	examples?: SidecarRegisteredHostCallbackExample[];
}

export interface ExtEnvelope {
	namespace: string;
	payload: Uint8Array;
}

type RequestPayload = LiveRequestPayload;

export type SidecarRequestPayload = LiveSidecarRequestPayload;

export type SidecarResponsePayload = LiveSidecarResponsePayload;

type RequestFrame = LiveRequestFrame;

type EventFrame = LiveEventFrame;

export type SidecarEventSelector = LiveSidecarEventSelector;

export type SidecarRequestFrame = LiveSidecarRequestFrame;

type ResponseFrame = LiveResponseFrame;

export type SidecarResponseFrame = LiveSidecarResponseFrame;

type NativeTransportPayloadCodec = ProtocolFramePayloadCodec;

export type SidecarRequestHandler = LiveSidecarRequestHandler;

export interface SidecarSpawnOptions {
	cwd?: string;
	command?: string;
	args?: string[];
	eventBufferCapacity?: number;
	gracefulExitMs?: number;
	forceExitMs?: number;
	// Migration-only compatibility path for pre-BARE test fixtures.
	payloadCodec?: NativeTransportPayloadCodec;
	/**
	 * Override the sidecar silence watchdog window (default 30s). Tests only —
	 * it is a fixed protocol constant paired with the sidecar's 10s heartbeat
	 * cadence, not an operator tunable.
	 */
	silenceTimeoutMs?: number;
}

export interface ResolvedSidecarSpawnOptions {
	cwd?: string;
	command?: string;
	args: string[];
	eventBufferCapacity: number;
	gracefulExitMs: number;
	forceExitMs: number;
	disposedErrorMessage: string;
	payloadCodec: NativeTransportPayloadCodec;
	silenceTimeoutMs?: number;
}

type SidecarProcessSpawnFactory = (
	options: ResolvedSidecarSpawnOptions,
) => SidecarProcessTransport;

let sidecarProcessSpawnFactory: SidecarProcessSpawnFactory | null = null;

export function registerSidecarProcessSpawnFactory(
	factory: SidecarProcessSpawnFactory,
): void {
	sidecarProcessSpawnFactory = factory;
}

export interface AuthenticatedSession {
	connectionId: string;
	sessionId: string;
}

export interface CreatedVm {
	vmId: string;
}

export interface SidecarSessionState {
	sessionId: string;
	agentType: string;
	processId: string;
	pid?: number;
	closed: boolean;
	modes?: unknown;
	configOptions: unknown[];
	agentCapabilities?: unknown;
	agentInfo?: unknown;
}

export interface SidecarMountPluginDescriptor {
	id: string;
	config?: MountConfigJsonObject;
}

export interface SidecarMountDescriptor {
	guestPath: string;
	guestSource?: string;
	guestFstype?: string;
	readOnly: boolean;
	plugin: SidecarMountPluginDescriptor;
}

export interface SidecarSoftwareDescriptor {
	packageName: string;
	root: string;
}

export type SidecarPermissionMode = LivePermissionMode;

export interface SidecarFsPermissionRule extends LiveFsPermissionRule {}

export interface SidecarPatternPermissionRule
	extends LivePatternPermissionRule {}

export interface SidecarRulePermissions<TRule>
	extends LiveRulePermissions<TRule> {}

export type SidecarPermissionScope<TRule> = LivePermissionScope<TRule>;

export interface SidecarPermissionsPolicy {
	fs?: SidecarPermissionScope<SidecarFsPermissionRule>;
	network?: SidecarPermissionScope<SidecarPatternPermissionRule>;
	childProcess?: SidecarPermissionScope<SidecarPatternPermissionRule>;
	process?: SidecarPermissionScope<SidecarPatternPermissionRule>;
	env?: SidecarPermissionScope<SidecarPatternPermissionRule>;
	binding?: SidecarPermissionScope<SidecarPatternPermissionRule>;
}

type WirePermissionsPolicy = LivePermissionsPolicy;

export interface SidecarProjectedModuleDescriptor {
	packageName: string;
	entrypoint: string;
}

export interface SidecarPackageDescriptor {
	path: string;
}

export interface SidecarProjectedAgent {
	id: string;
	acpEntrypoint: string;
	adapterEntrypoint: string;
}

export interface SidecarLinkPackageResult {
	projectedCommands: SidecarProjectedCommand[];
	agents: SidecarProjectedAgent[];
}

export interface SidecarProjectedCommand {
	name: string;
	guestPath: string;
}

export interface SidecarPackageCommands {
	packageName: string;
	commands: string[];
}

export interface SidecarVmConfiguredResponse {
	appliedMounts: number;
	appliedSoftware: number;
	projectedCommands: SidecarProjectedCommand[];
	agents: SidecarProjectedAgent[];
}

export interface SidecarFilesystemResult {
	operation: LiveFilesystemOperation;
	status: string;
	payloadSizeBytes: number;
}
export interface SidecarPersistenceState {
	key: string;
	found: boolean;
	payloadSizeBytes: number;
}
export interface SidecarPersistenceFlushed {
	key: string;
	committedBytes: number;
}

export class SidecarProcess {
	private readonly protocolClient: SidecarProcessTransport;

	private constructor(protocolClient: SidecarProcessTransport) {
		this.protocolClient = protocolClient;
	}

	static fromClient(protocolClient: SidecarProcessTransport): SidecarProcess {
		return new SidecarProcess(protocolClient);
	}

	static spawn(options: SidecarSpawnOptions = {}): SidecarProcess {
		if (!sidecarProcessSpawnFactory) {
			throw new Error(
				"native sidecar spawn is not registered; import @rivet-dev/agentos-runtime-core/native-client before calling SidecarProcess.spawn, or use SidecarProcess.fromClient",
			);
		}
		const protocolClient = sidecarProcessSpawnFactory({
			command: options.command,
			args: options.args ?? [],
			cwd: options.cwd,
			silenceTimeoutMs: options.silenceTimeoutMs,
			eventBufferCapacity:
				options.eventBufferCapacity ?? DEFAULT_SIDECAR_EVENT_BUFFER_CAPACITY,
			gracefulExitMs:
				options.gracefulExitMs ?? DEFAULT_SIDECAR_GRACEFUL_EXIT_MS,
			forceExitMs: options.forceExitMs ?? DEFAULT_SIDECAR_FORCE_EXIT_MS,
			disposedErrorMessage: "native sidecar disposed",
			payloadCodec: options.payloadCodec ?? "bare",
		});
		return SidecarProcess.fromClient(protocolClient);
	}

	setSidecarRequestHandler(handler: SidecarRequestHandler | null): void {
		this.protocolClient.setSidecarRequestHandler(handler);
	}

	onEvent(handler: (event: EventFrame) => void): () => void {
		return this.protocolClient.onEvent(handler);
	}

	async authenticateAndOpenSession(
		sessionMetadata: Record<string, string> = {},
	): Promise<AuthenticatedSession> {
		const authenticated = await this.sendRequest({
			ownership: {
				scope: "connection",
				connection_id: "client-hint",
			},
			payload: {
				type: "authenticate",
				client_name: "agentos-core-client",
				auth_token: "agentos-core-client-token",
				protocol_version: SIDECAR_PROTOCOL_SCHEMA.version,
				bridge_version: BRIDGE_CONTRACT_VERSION,
			},
		});
		if (authenticated.payload.type !== "authenticated") {
			throw new Error(
				`unexpected authenticate response: ${authenticated.payload.type}`,
			);
		}

		const opened = await this.sendRequest({
			ownership: {
				scope: "connection",
				connection_id: authenticated.payload.connection_id,
			},
			payload: {
				type: "open_session",
				placement: {
					kind: "shared",
					pool: null,
				},
				metadata: sessionMetadata,
			},
		});
		if (opened.payload.type !== "session_opened") {
			throw new Error(
				`unexpected open_session response: ${opened.payload.type}`,
			);
		}

		return {
			connectionId: authenticated.payload.connection_id,
			sessionId: opened.payload.session_id,
		};
	}

	async createVm(
		session: AuthenticatedSession,
		options: {
			runtime: GuestRuntimeKind;
			config: CreateVmConfig;
		},
	): Promise<CreatedVm> {
		const response = await this.sendRequest({
			ownership: {
				scope: "session",
				connection_id: session.connectionId,
				session_id: session.sessionId,
			},
			payload: {
				type: "create_vm",
				runtime: options.runtime,
				config: options.config,
			},
		});
		if (response.payload.type !== "vm_created") {
			throw new Error(
				`unexpected create_vm response: ${response.payload.type}`,
			);
		}

		return {
			vmId: response.payload.vm_id,
		};
	}

	async extensionRequest(
		session: AuthenticatedSession,
		vm: CreatedVm,
		envelope: ExtEnvelope,
	): Promise<ExtEnvelope> {
		const response = await this.sendRequest({
			ownership: {
				scope: "vm",
				connection_id: session.connectionId,
				session_id: session.sessionId,
				vm_id: vm.vmId,
			},
			payload: {
				type: "ext",
				envelope,
			},
		});
		if (response.payload.type !== "ext_result") {
			throw new Error(`unexpected ext response: ${response.payload.type}`);
		}
		return response.payload.envelope;
	}

	async configureVm(
		session: AuthenticatedSession,
		vm: CreatedVm,
		options: {
			mounts?: SidecarMountDescriptor[];
			software?: SidecarSoftwareDescriptor[];
			permissions?: SidecarPermissionsPolicy;
			moduleAccessCwd?: string;
			instructions?: string[];
			projectedModules?: SidecarProjectedModuleDescriptor[];
			commandPermissions?: Record<string, WasmPermissionTier>;
			loopbackExemptPorts?: number[];
			packages?: SidecarPackageDescriptor[];
			packagesMountAt?: string;
			bootstrapCommands?: string[];
			bindingShimCommands?: string[];
		},
	): Promise<SidecarVmConfiguredResponse> {
		const response = await this.sendRequest({
			ownership: {
				scope: "vm",
				connection_id: session.connectionId,
				session_id: session.sessionId,
				vm_id: vm.vmId,
			},
			payload: {
				type: "configure_vm",
				mounts: (options.mounts ?? []).map(toWireMountDescriptor),
				software: (options.software ?? []).map(toWireSoftwareDescriptor),
				permissions: toWirePermissionsPolicy(options.permissions),
				module_access_cwd: options.moduleAccessCwd,
				instructions: options.instructions ?? [],
				projected_modules: (options.projectedModules ?? []).map(
					toWireProjectedModuleDescriptor,
				),
				command_permissions: options.commandPermissions ?? {},
				...(options.loopbackExemptPorts
					? { loopback_exempt_ports: options.loopbackExemptPorts }
					: {}),
				packages: (options.packages ?? []).map(toWirePackageDescriptor),
				...(options.packagesMountAt
					? { packages_mount_at: options.packagesMountAt }
					: {}),
				bootstrap_commands: options.bootstrapCommands ?? [],
				binding_shim_commands: options.bindingShimCommands ?? [],
			},
		});
		if (response.payload.type !== "vm_configured") {
			throw new Error(
				`unexpected configure_vm response: ${response.payload.type}`,
			);
		}
		return {
			appliedMounts: response.payload.applied_mounts,
			appliedSoftware: response.payload.applied_software,
			projectedCommands: response.payload.projected_commands.map((command) => ({
				name: command.name,
				guestPath: command.guest_path,
			})),
			agents: response.payload.agents.map(fromWireProjectedAgent),
		};
	}

	/**
	 * Runtime dynamic `linkSoftware`: project one package into the live
	 * `/opt/agentos` tree. Returns projected command entrypoints and agents.
	 */
	async linkPackage(
		session: AuthenticatedSession,
		vm: CreatedVm,
		descriptor: SidecarPackageDescriptor,
	): Promise<SidecarLinkPackageResult> {
		const response = await this.sendRequest({
			ownership: {
				scope: "vm",
				connection_id: session.connectionId,
				session_id: session.sessionId,
				vm_id: vm.vmId,
			},
			payload: {
				type: "link_package",
				package: toWirePackageDescriptor(descriptor),
			},
		});
		if (response.payload.type !== "package_linked") {
			throw new Error(
				`unexpected link_package response: ${response.payload.type}`,
			);
		}
		return {
			projectedCommands: response.payload.projected_commands.map((command) => ({
				name: command.name,
				guestPath: command.guest_path,
			})),
			agents: response.payload.agents.map(fromWireProjectedAgent),
		};
	}

	async providedCommands(
		session: AuthenticatedSession,
		vm: CreatedVm,
	): Promise<SidecarPackageCommands[]> {
		const response = await this.sendRequest({
			ownership: {
				scope: "vm",
				connection_id: session.connectionId,
				session_id: session.sessionId,
				vm_id: vm.vmId,
			},
			payload: {
				type: "provided_commands",
			},
		});
		if (response.payload.type !== "provided_commands_response") {
			throw new Error(
				`unexpected provided_commands response: ${response.payload.type}`,
			);
		}
		return response.payload.packages.map((pkg) => ({
			packageName: pkg.package_name,
			commands: [...pkg.commands],
		}));
	}

	async registerHostCallbacks(
		session: AuthenticatedSession,
		vm: CreatedVm,
		registration: {
			name: string;
			description: string;
			commandAliases?: string[];
			registryCommandAliases?: string[];
			callbacks: Record<string, SidecarRegisteredHostCallbackDefinition>;
		},
	): Promise<{
		registration: string;
		commandCount: number;
	}> {
		const response = await this.sendRequest({
			ownership: {
				scope: "vm",
				connection_id: session.connectionId,
				session_id: session.sessionId,
				vm_id: vm.vmId,
			},
			payload: {
				type: "register_host_callbacks",
				name: registration.name,
				description: registration.description,
				command_aliases: registration.commandAliases ?? [],
				registry_command_aliases: registration.registryCommandAliases ?? [],
				callbacks: Object.fromEntries(
					Object.entries(registration.callbacks).map(
						([callbackName, callback]) => [
							callbackName,
							{
								description: callback.description,
								input_schema: callback.inputSchema,
								...(callback.timeoutMs !== undefined
									? { timeout_ms: callback.timeoutMs }
									: {}),
								...(callback.examples && callback.examples.length > 0
									? {
											examples: callback.examples.map((example) => ({
												description: example.description,
												input: example.input,
											})),
										}
									: {}),
							},
						],
					),
				),
			},
		});
		if (response.payload.type !== "host_callbacks_registered") {
			throw new Error(
				`unexpected register_host_callbacks response: ${response.payload.type}`,
			);
		}
		return {
			registration: response.payload.registration,
			commandCount: response.payload.command_count,
		};
	}

	async createLayer(
		session: AuthenticatedSession,
		vm: CreatedVm,
	): Promise<string> {
		const response = await this.sendRequest({
			ownership: {
				scope: "vm",
				connection_id: session.connectionId,
				session_id: session.sessionId,
				vm_id: vm.vmId,
			},
			payload: {
				type: "create_layer",
			},
		});
		if (response.payload.type !== "layer_created") {
			throw new Error(
				`unexpected create_layer response: ${response.payload.type}`,
			);
		}
		return response.payload.layer_id;
	}

	async sealLayer(
		session: AuthenticatedSession,
		vm: CreatedVm,
		layerId: string,
	): Promise<string> {
		const response = await this.sendRequest({
			ownership: {
				scope: "vm",
				connection_id: session.connectionId,
				session_id: session.sessionId,
				vm_id: vm.vmId,
			},
			payload: {
				type: "seal_layer",
				layer_id: layerId,
			},
		});
		if (response.payload.type !== "layer_sealed") {
			throw new Error(
				`unexpected seal_layer response: ${response.payload.type}`,
			);
		}
		return response.payload.layer_id;
	}

	async importSnapshot(
		session: AuthenticatedSession,
		vm: CreatedVm,
		entries: RootFilesystemEntry[],
	): Promise<string> {
		const response = await this.sendRequest({
			ownership: {
				scope: "vm",
				connection_id: session.connectionId,
				session_id: session.sessionId,
				vm_id: vm.vmId,
			},
			payload: {
				type: "import_snapshot",
				entries,
			},
		});
		if (response.payload.type !== "snapshot_imported") {
			throw new Error(
				`unexpected import_snapshot response: ${response.payload.type}`,
			);
		}
		return response.payload.layer_id;
	}

	async exportSnapshot(
		session: AuthenticatedSession,
		vm: CreatedVm,
		layerId: string,
	): Promise<RootFilesystemEntry[]> {
		const response = await this.sendRequest({
			ownership: {
				scope: "vm",
				connection_id: session.connectionId,
				session_id: session.sessionId,
				vm_id: vm.vmId,
			},
			payload: {
				type: "export_snapshot",
				layer_id: layerId,
			},
		});
		if (response.payload.type !== "snapshot_exported") {
			throw new Error(
				`unexpected export_snapshot response: ${response.payload.type}`,
			);
		}
		return response.payload.entries;
	}

	async createOverlay(
		session: AuthenticatedSession,
		vm: CreatedVm,
		options: {
			mode?: "ephemeral" | "read_only";
			upperLayerId?: string;
			lowerLayerIds: string[];
		},
	): Promise<string> {
		const response = await this.sendRequest({
			ownership: {
				scope: "vm",
				connection_id: session.connectionId,
				session_id: session.sessionId,
				vm_id: vm.vmId,
			},
			payload: {
				type: "create_overlay",
				mode: options.mode,
				upper_layer_id: options.upperLayerId,
				lower_layer_ids: options.lowerLayerIds,
			},
		});
		if (response.payload.type !== "overlay_created") {
			throw new Error(
				`unexpected create_overlay response: ${response.payload.type}`,
			);
		}
		return response.payload.layer_id;
	}

	async bootstrapRootFilesystem(
		session: AuthenticatedSession,
		vm: CreatedVm,
		entries: RootFilesystemEntry[],
	): Promise<void> {
		const response = await this.sendRequest({
			ownership: {
				scope: "vm",
				connection_id: session.connectionId,
				session_id: session.sessionId,
				vm_id: vm.vmId,
			},
			payload: {
				type: "bootstrap_root_filesystem",
				entries,
			},
		});
		if (response.payload.type !== "root_filesystem_bootstrapped") {
			throw new Error(
				`unexpected bootstrap_root_filesystem response: ${response.payload.type}`,
			);
		}
	}

	async snapshotRootFilesystem(
		session: AuthenticatedSession,
		vm: CreatedVm,
		maxBytes: number,
	): Promise<RootFilesystemEntry[]> {
		const response = await this.sendRequest({
			ownership: {
				scope: "vm",
				connection_id: session.connectionId,
				session_id: session.sessionId,
				vm_id: vm.vmId,
			},
			payload: {
				type: "snapshot_root_filesystem",
				max_bytes: maxBytes,
			},
		});
		if (response.payload.type !== "root_filesystem_snapshot") {
			throw new Error(
				`unexpected snapshot_root_filesystem response: ${response.payload.type}`,
			);
		}
		return response.payload.entries;
	}

	async listMounts(
		session: AuthenticatedSession,
		vm: CreatedVm,
	): Promise<Array<{ path: string; kind: string; readOnly: boolean }>> {
		const response = await this.sendRequest({
			ownership: {
				scope: "vm",
				connection_id: session.connectionId,
				session_id: session.sessionId,
				vm_id: vm.vmId,
			},
			payload: { type: "list_mounts" },
		});
		if (response.payload.type !== "mounts_listed") {
			throw new Error(
				`unexpected list_mounts response: ${response.payload.type}`,
			);
		}
		return response.payload.mounts.map((mount) => ({
			path: mount.path,
			kind: mount.kind,
			readOnly: mount.read_only,
		}));
	}

	async readFile(
		session: AuthenticatedSession,
		vm: CreatedVm,
		path: string,
	): Promise<Uint8Array> {
		const response = await this.guestFilesystemCall(session, vm, {
			operation: "read_file",
			path,
		});
		return decodeGuestFilesystemContent(response);
	}

	async pread(
		session: AuthenticatedSession,
		vm: CreatedVm,
		path: string,
		offset: number,
		length: number,
	): Promise<Uint8Array> {
		const response = await this.guestFilesystemCall(session, vm, {
			operation: "pread",
			path,
			offset,
			len: length,
		});
		return decodeGuestFilesystemContent(response);
	}

	async writeFile(
		session: AuthenticatedSession,
		vm: CreatedVm,
		path: string,
		content: string | Uint8Array,
	): Promise<void> {
		const encoded = encodeGuestFilesystemContent(content);
		await this.guestFilesystemCall(session, vm, {
			operation: "write_file",
			path,
			content: encoded.content,
			encoding: encoded.encoding,
		});
	}

	async pwrite(
		session: AuthenticatedSession,
		vm: CreatedVm,
		path: string,
		offset: number,
		content: Uint8Array,
	): Promise<void> {
		const encoded = encodeGuestFilesystemContent(content);
		await this.guestFilesystemCall(session, vm, {
			operation: "pwrite",
			path,
			offset,
			content: encoded.content,
			encoding: encoded.encoding,
		});
	}

	async mkdir(
		session: AuthenticatedSession,
		vm: CreatedVm,
		path: string,
		options?: { recursive?: boolean },
	): Promise<void> {
		await this.guestFilesystemCall(session, vm, {
			operation: options?.recursive ? "mkdir" : "create_dir",
			path,
			recursive: options?.recursive ?? false,
		});
	}

	async readdir(
		session: AuthenticatedSession,
		vm: CreatedVm,
		path: string,
	): Promise<string[]> {
		const response = await this.guestFilesystemCall(session, vm, {
			operation: "read_dir",
			path,
		});
		return (response.entries ?? []).map((entry) => entry.name);
	}

	async readdirRecursive(
		session: AuthenticatedSession,
		vm: CreatedVm,
		path: string,
		options?: { maxDepth?: number },
	): Promise<LiveGuestDirEntry[]> {
		const response = await this.guestFilesystemCall(session, vm, {
			operation: "read_dir_recursive",
			path,
			max_depth: options?.maxDepth,
		});
		return response.entries ?? [];
	}

	async exists(
		session: AuthenticatedSession,
		vm: CreatedVm,
		path: string,
	): Promise<boolean> {
		const response = await this.guestFilesystemCall(session, vm, {
			operation: "exists",
			path,
		});
		return response.exists ?? false;
	}

	async stat(
		session: AuthenticatedSession,
		vm: CreatedVm,
		path: string,
		options?: { dereference?: boolean },
	): Promise<GuestFilesystemStat> {
		const response = await this.guestFilesystemCall(session, vm, {
			operation: options?.dereference === false ? "lstat" : "stat",
			path,
		});
		if (!response.stat) {
			throw new Error(`sidecar returned no stat payload for ${path}`);
		}
		return response.stat;
	}

	async lstat(
		session: AuthenticatedSession,
		vm: CreatedVm,
		path: string,
	): Promise<GuestFilesystemStat> {
		return this.stat(session, vm, path, { dereference: false });
	}

	async rename(
		session: AuthenticatedSession,
		vm: CreatedVm,
		fromPath: string,
		toPath: string,
	): Promise<void> {
		await this.guestFilesystemCall(session, vm, {
			operation: "rename",
			path: fromPath,
			destination_path: toPath,
		});
	}

	async realpath(
		session: AuthenticatedSession,
		vm: CreatedVm,
		path: string,
	): Promise<string> {
		const response = await this.guestFilesystemCall(session, vm, {
			operation: "realpath",
			path,
		});
		if (response.target === undefined) {
			throw new Error(`sidecar returned no realpath payload for ${path}`);
		}
		return response.target;
	}

	async removeFile(
		session: AuthenticatedSession,
		vm: CreatedVm,
		path: string,
	): Promise<void> {
		await this.guestFilesystemCall(session, vm, {
			operation: "remove_file",
			path,
		});
	}

	async removeDir(
		session: AuthenticatedSession,
		vm: CreatedVm,
		path: string,
	): Promise<void> {
		await this.guestFilesystemCall(session, vm, {
			operation: "remove_dir",
			path,
		});
	}

	async removePath(
		session: AuthenticatedSession,
		vm: CreatedVm,
		path: string,
		options?: { recursive?: boolean },
	): Promise<void> {
		await this.guestFilesystemCall(session, vm, {
			operation: "remove",
			path,
			recursive: options?.recursive ?? false,
		});
	}

	async copyPath(
		session: AuthenticatedSession,
		vm: CreatedVm,
		fromPath: string,
		toPath: string,
		options?: { recursive?: boolean },
	): Promise<void> {
		await this.guestFilesystemCall(session, vm, {
			operation: "copy",
			path: fromPath,
			destination_path: toPath,
			recursive: options?.recursive ?? false,
		});
	}

	async movePath(
		session: AuthenticatedSession,
		vm: CreatedVm,
		fromPath: string,
		toPath: string,
	): Promise<void> {
		await this.guestFilesystemCall(session, vm, {
			operation: "move",
			path: fromPath,
			destination_path: toPath,
			recursive: true,
		});
	}

	async symlink(
		session: AuthenticatedSession,
		vm: CreatedVm,
		target: string,
		linkPath: string,
	): Promise<void> {
		await this.guestFilesystemCall(session, vm, {
			operation: "symlink",
			path: linkPath,
			target,
		});
	}

	async readLink(
		session: AuthenticatedSession,
		vm: CreatedVm,
		path: string,
	): Promise<string> {
		const response = await this.guestFilesystemCall(session, vm, {
			operation: "read_link",
			path,
		});
		if (response.target === undefined) {
			throw new Error(`sidecar returned no symlink target for ${path}`);
		}
		return response.target;
	}

	async link(
		session: AuthenticatedSession,
		vm: CreatedVm,
		fromPath: string,
		toPath: string,
	): Promise<void> {
		await this.guestFilesystemCall(session, vm, {
			operation: "link",
			path: fromPath,
			destination_path: toPath,
		});
	}

	async chmod(
		session: AuthenticatedSession,
		vm: CreatedVm,
		path: string,
		mode: number,
	): Promise<void> {
		await this.guestFilesystemCall(session, vm, {
			operation: "chmod",
			path,
			mode,
		});
	}

	async chown(
		session: AuthenticatedSession,
		vm: CreatedVm,
		path: string,
		uid: number,
		gid: number,
	): Promise<void> {
		await this.guestFilesystemCall(session, vm, {
			operation: "chown",
			path,
			uid,
			gid,
		});
	}

	async utimes(
		session: AuthenticatedSession,
		vm: CreatedVm,
		path: string,
		atimeMs: number,
		mtimeMs: number,
	): Promise<void> {
		await this.guestFilesystemCall(session, vm, {
			operation: "utimes",
			path,
			atime_ms: atimeMs,
			mtime_ms: mtimeMs,
		});
	}

	async truncate(
		session: AuthenticatedSession,
		vm: CreatedVm,
		path: string,
		length: number,
	): Promise<void> {
		await this.guestFilesystemCall(session, vm, {
			operation: "truncate",
			path,
			len: length,
		});
	}

	async disposeVm(session: AuthenticatedSession, vm: CreatedVm): Promise<void> {
		const response = await this.sendRequest({
			ownership: {
				scope: "vm",
				connection_id: session.connectionId,
				session_id: session.sessionId,
				vm_id: vm.vmId,
			},
			payload: {
				type: "dispose_vm",
				reason: "requested",
			},
		});
		if (response.payload.type !== "vm_disposed") {
			throw new Error(
				`unexpected dispose_vm response: ${response.payload.type}`,
			);
		}
	}

	async execute(
		session: AuthenticatedSession,
		vm: CreatedVm,
		options: {
			processId: string;
			command?: string;
			runtime?: GuestRuntimeKind;
			entrypoint?: string;
			args?: string[];
			env?: Record<string, string>;
			cwd?: string;
			wasmPermissionTier?: WasmPermissionTier;
			wasmBackend?: StandaloneWasmBackend;
		},
	): Promise<{ pid: number | null }> {
		const response = await this.sendRequest({
			ownership: {
				scope: "vm",
				connection_id: session.connectionId,
				session_id: session.sessionId,
				vm_id: vm.vmId,
			},
			payload: {
				type: "execute",
				process_id: options.processId,
				args: options.args ?? [],
				...(options.command ? { command: options.command } : {}),
				...(options.runtime ? { runtime: options.runtime } : {}),
				...(options.entrypoint ? { entrypoint: options.entrypoint } : {}),
				...(options.env ? { env: options.env } : {}),
				...(options.cwd ? { cwd: options.cwd } : {}),
				...(options.wasmPermissionTier
					? { wasm_permission_tier: options.wasmPermissionTier }
					: {}),
				...(options.wasmBackend ? { wasm_backend: options.wasmBackend } : {}),
			},
		});
		if (response.payload.type !== "process_started") {
			throw new Error(`unexpected execute response: ${response.payload.type}`);
		}
		return {
			pid: response.payload.pid ?? null,
		};
	}

	async writeStdin(
		session: AuthenticatedSession,
		vm: CreatedVm,
		processId: string,
		chunk: string | Uint8Array,
	): Promise<void> {
		const response = await this.sendRequest({
			ownership: {
				scope: "vm",
				connection_id: session.connectionId,
				session_id: session.sessionId,
				vm_id: vm.vmId,
			},
			payload: {
				type: "write_stdin",
				process_id: processId,
				chunk:
					typeof chunk === "string" ? new TextEncoder().encode(chunk) : chunk,
			},
		});
		if (response.payload.type !== "stdin_written") {
			throw new Error(
				`unexpected write_stdin response: ${response.payload.type}`,
			);
		}
	}

	async resizePty(
		session: AuthenticatedSession,
		vm: CreatedVm,
		processId: string,
		cols: number,
		rows: number,
	): Promise<void> {
		const response = await this.sendRequest({
			ownership: {
				scope: "vm",
				connection_id: session.connectionId,
				session_id: session.sessionId,
				vm_id: vm.vmId,
			},
			payload: {
				type: "resize_pty",
				process_id: processId,
				cols,
				rows,
			},
		});
		if (response.payload.type !== "pty_resized") {
			throw new Error(
				`unexpected resize_pty response: ${response.payload.type}`,
			);
		}
	}

	async closeStdin(
		session: AuthenticatedSession,
		vm: CreatedVm,
		processId: string,
	): Promise<void> {
		const response = await this.sendRequest({
			ownership: {
				scope: "vm",
				connection_id: session.connectionId,
				session_id: session.sessionId,
				vm_id: vm.vmId,
			},
			payload: {
				type: "close_stdin",
				process_id: processId,
			},
		});
		if (response.payload.type !== "stdin_closed") {
			throw new Error(
				`unexpected close_stdin response: ${response.payload.type}`,
			);
		}
	}

	async killProcess(
		session: AuthenticatedSession,
		vm: CreatedVm,
		processId: string,
		signal = "SIGTERM",
	): Promise<void> {
		const response = await this.sendRequest({
			ownership: {
				scope: "vm",
				connection_id: session.connectionId,
				session_id: session.sessionId,
				vm_id: vm.vmId,
			},
			payload: {
				type: "kill_process",
				process_id: processId,
				signal,
			},
		});
		if (response.payload.type !== "process_killed") {
			throw new Error(
				`unexpected kill_process response: ${response.payload.type}`,
			);
		}
	}

	async findListener(
		session: AuthenticatedSession,
		vm: CreatedVm,
		request: { host?: string; port?: number; path?: string },
	): Promise<SidecarSocketStateEntry | null> {
		const response = await this.sendRequest({
			ownership: {
				scope: "vm",
				connection_id: session.connectionId,
				session_id: session.sessionId,
				vm_id: vm.vmId,
			},
			payload: {
				type: "find_listener",
				...(request.host !== undefined ? { host: request.host } : {}),
				...(request.port !== undefined ? { port: request.port } : {}),
				...(request.path !== undefined ? { path: request.path } : {}),
			},
		});
		if (response.payload.type !== "listener_snapshot") {
			throw new Error(
				`unexpected find_listener response: ${response.payload.type}`,
			);
		}
		return response.payload.listener
			? toSidecarSocketStateEntry(response.payload.listener)
			: null;
	}

	async getProcessSnapshot(
		session: AuthenticatedSession,
		vm: CreatedVm,
	): Promise<SidecarProcessSnapshotEntry[]> {
		const response = await this.sendRequest({
			ownership: {
				scope: "vm",
				connection_id: session.connectionId,
				session_id: session.sessionId,
				vm_id: vm.vmId,
			},
			payload: {
				type: "get_process_snapshot",
			},
		});
		if (response.payload.type !== "process_snapshot") {
			throw new Error(
				`unexpected get_process_snapshot response: ${response.payload.type}`,
			);
		}
		return response.payload.processes.map(toSidecarProcessSnapshotEntry);
	}

	async getResourceSnapshot(
		session: AuthenticatedSession,
		vm: CreatedVm,
	): Promise<SidecarResourceSnapshot> {
		const response = await this.sendRequest({
			ownership: {
				scope: "vm",
				connection_id: session.connectionId,
				session_id: session.sessionId,
				vm_id: vm.vmId,
			},
			payload: {
				type: "get_resource_snapshot",
			},
		});
		if (response.payload.type !== "resource_snapshot") {
			throw new Error(
				`unexpected get_resource_snapshot response: ${response.payload.type}`,
			);
		}
		return {
			runningProcesses: response.payload.running_processes,
			stoppedProcesses: response.payload.stopped_processes,
			exitedProcesses: response.payload.exited_processes,
			fdTables: response.payload.fd_tables,
			openFds: response.payload.open_fds,
			pipes: response.payload.pipes,
			pipeBufferedBytes: response.payload.pipe_buffered_bytes,
			ptys: response.payload.ptys,
			ptyBufferedInputBytes: response.payload.pty_buffered_input_bytes,
			ptyBufferedOutputBytes: response.payload.pty_buffered_output_bytes,
			sockets: response.payload.sockets,
			socketListeners: response.payload.socket_listeners,
			socketConnections: response.payload.socket_connections,
			socketBufferedBytes: response.payload.socket_buffered_bytes,
			socketDatagramQueueLen: response.payload.socket_datagram_queue_len,
			wasmReservedMemoryBytes: response.payload.wasm_reserved_memory_bytes,
			wasmtimeEngineProfiles: response.payload.wasmtime_engine_profiles,
			wasmtimeModuleEntries: response.payload.wasmtime_module_entries,
			wasmtimeModuleCacheHits: response.payload.wasmtime_module_cache_hits,
			wasmtimeModuleCacheMisses: response.payload.wasmtime_module_cache_misses,
			wasmtimeModuleCacheEvictions:
				response.payload.wasmtime_module_cache_evictions,
			wasmtimeCompiledSourceBytes:
				response.payload.wasmtime_compiled_source_bytes,
			wasmtimeChargedModuleBytes:
				response.payload.wasmtime_charged_module_bytes,
			wasmtimeCompileTimeMicros: response.payload.wasmtime_compile_time_micros,
			...(response.payload.wasmtime_process_retained_rss_bytes !== undefined
				? {
						wasmtimeProcessRetainedRssBytes:
							response.payload.wasmtime_process_retained_rss_bytes,
					}
				: {}),
			queueSnapshots: response.payload.queue_snapshots.map((queue) => ({
				name: queue.name,
				category: queue.category,
				depth: queue.depth,
				highWater: queue.high_water,
				capacity: queue.capacity,
				fillPercent: queue.fill_percent,
			})),
		};
	}

	async findBoundUdp(
		session: AuthenticatedSession,
		vm: CreatedVm,
		request: { host?: string; port?: number },
	): Promise<SidecarSocketStateEntry | null> {
		const response = await this.sendRequest({
			ownership: {
				scope: "vm",
				connection_id: session.connectionId,
				session_id: session.sessionId,
				vm_id: vm.vmId,
			},
			payload: {
				type: "find_bound_udp",
				...(request.host !== undefined ? { host: request.host } : {}),
				...(request.port !== undefined ? { port: request.port } : {}),
			},
		});
		if (response.payload.type !== "bound_udp_snapshot") {
			throw new Error(
				`unexpected find_bound_udp response: ${response.payload.type}`,
			);
		}
		return response.payload.socket
			? toSidecarSocketStateEntry(response.payload.socket)
			: null;
	}

	async vmFetch(
		session: AuthenticatedSession,
		vm: CreatedVm,
		request: {
			port: number;
			method: string;
			path: string;
			headersJson: string;
			body?: string;
			bodyBase64?: string;
			streamOperation?: "start" | "read" | "cancel";
			streamId?: string;
			maxBytes?: number;
		},
	): Promise<string> {
		const response = await this.sendRequest({
			ownership: {
				scope: "vm",
				connection_id: session.connectionId,
				session_id: session.sessionId,
				vm_id: vm.vmId,
			},
			payload: {
				type: "vm_fetch",
				port: request.port,
				method: request.method,
				path: request.path,
				headers_json: request.headersJson,
				...(request.body !== undefined ? { body: request.body } : {}),
				...(request.bodyBase64 !== undefined
					? { body_base64: request.bodyBase64 }
					: {}),
				...(request.streamOperation !== undefined
					? { stream_operation: request.streamOperation }
					: {}),
				...(request.streamId !== undefined
					? { stream_id: request.streamId }
					: {}),
				...(request.maxBytes !== undefined
					? { max_bytes: request.maxBytes }
					: {}),
			},
		});
		if (response.payload.type !== "vm_fetch_result") {
			throw new Error(`unexpected vm_fetch response: ${response.payload.type}`);
		}
		return response.payload.response_json;
	}

	async getSignalState(
		session: AuthenticatedSession,
		vm: CreatedVm,
		processId: string,
	): Promise<SidecarSignalState> {
		const response = await this.sendRequest({
			ownership: {
				scope: "vm",
				connection_id: session.connectionId,
				session_id: session.sessionId,
				vm_id: vm.vmId,
			},
			payload: {
				type: "get_signal_state",
				process_id: processId,
			},
		});
		if (response.payload.type !== "signal_state") {
			throw new Error(
				`unexpected get_signal_state response: ${response.payload.type}`,
			);
		}
		return {
			processId: response.payload.process_id,
			handlers: new Map(
				Object.entries(response.payload.handlers).map(
					([signal, registration]) => [
						Number(signal),
						{
							action: registration.action,
							mask: [...registration.mask],
							flags: registration.flags,
						},
					],
				),
			),
		};
	}

	async getZombieTimerCount(
		session: AuthenticatedSession,
		vm: CreatedVm,
	): Promise<SidecarZombieTimerCount> {
		const response = await this.sendRequest({
			ownership: {
				scope: "vm",
				connection_id: session.connectionId,
				session_id: session.sessionId,
				vm_id: vm.vmId,
			},
			payload: {
				type: "get_zombie_timer_count",
			},
		});
		if (response.payload.type !== "zombie_timer_count") {
			throw new Error(
				`unexpected get_zombie_timer_count response: ${response.payload.type}`,
			);
		}
		return {
			count: response.payload.count,
		};
	}

	async hostFilesystemCall(
		session: AuthenticatedSession,
		vm: CreatedVm,
		request: {
			operation: LiveFilesystemOperation;
			path: string;
			payloadSizeBytes: number;
		},
	): Promise<SidecarFilesystemResult> {
		const response = await this.sendRequest({
			ownership: {
				scope: "vm",
				connection_id: session.connectionId,
				session_id: session.sessionId,
				vm_id: vm.vmId,
			},
			payload: {
				type: "host_filesystem_call",
				operation: request.operation,
				path: request.path,
				payload_size_bytes: request.payloadSizeBytes,
			},
		});
		if (response.payload.type !== "filesystem_result") {
			throw new Error(
				`unexpected host_filesystem_call response: ${response.payload.type}`,
			);
		}
		return {
			operation: response.payload.operation,
			status: response.payload.status,
			payloadSizeBytes: response.payload.payload_size_bytes,
		};
	}

	async persistenceLoad(
		session: AuthenticatedSession,
		key: string,
	): Promise<SidecarPersistenceState> {
		const response = await this.sendRequest({
			ownership: {
				scope: "session",
				connection_id: session.connectionId,
				session_id: session.sessionId,
			},
			payload: { type: "persistence_load", key },
		});
		if (response.payload.type !== "persistence_state") {
			throw new Error(
				`unexpected persistence_load response: ${response.payload.type}`,
			);
		}
		return {
			key: response.payload.key,
			found: response.payload.found,
			payloadSizeBytes: response.payload.payload_size_bytes,
		};
	}

	async persistenceFlush(
		session: AuthenticatedSession,
		request: { key: string; payloadSizeBytes: number },
	): Promise<SidecarPersistenceFlushed> {
		const response = await this.sendRequest({
			ownership: {
				scope: "session",
				connection_id: session.connectionId,
				session_id: session.sessionId,
			},
			payload: {
				type: "persistence_flush",
				key: request.key,
				payload_size_bytes: request.payloadSizeBytes,
			},
		});
		if (response.payload.type !== "persistence_flushed") {
			throw new Error(
				`unexpected persistence_flush response: ${response.payload.type}`,
			);
		}
		return {
			key: response.payload.key,
			committedBytes: response.payload.committed_bytes,
		};
	}

	async waitForEvent(
		matcher: SidecarEventSelector | ((event: EventFrame) => boolean),
		timeoutMs?: number,
		options?: {
			signal?: AbortSignal;
		},
	): Promise<EventFrame> {
		return await this.protocolClient.waitForEvent(matcher, timeoutMs, options);
	}

	/** Internal semantic request entrypoint used by the AgentOS client surface. */
	async sendVmRequest(
		session: AuthenticatedSession,
		vm: CreatedVm,
		payload: LiveRequestPayload,
	): Promise<LiveResponsePayload> {
		const response = await this.sendRequest({
			ownership: {
				scope: "vm",
				connection_id: session.connectionId,
				session_id: session.sessionId,
				vm_id: vm.vmId,
			},
			payload,
		});
		return response.payload;
	}

	async dispose(): Promise<void> {
		await this.protocolClient.dispose();
	}

	private async sendRequest(input: {
		ownership: OwnershipScope;
		payload: RequestPayload;
	}): Promise<ResponseFrame> {
		return await this.protocolClient.sendRequest(input);
	}

	private async guestFilesystemCall(
		session: AuthenticatedSession,
		vm: CreatedVm,
		payload: Omit<
			Extract<RequestPayload, { type: "guest_filesystem_call" }>,
			"type"
		>,
	): Promise<
		Extract<ResponseFrame["payload"], { type: "guest_filesystem_result" }>
	> {
		const response = await this.sendRequest({
			ownership: {
				scope: "vm",
				connection_id: session.connectionId,
				session_id: session.sessionId,
				vm_id: vm.vmId,
			},
			payload: {
				type: "guest_filesystem_call",
				...payload,
			},
		});
		if (response.payload.type !== "guest_filesystem_result") {
			throw new Error(
				`unexpected guest_filesystem_call response: ${response.payload.type}`,
			);
		}
		return response.payload;
	}
}

function toSidecarSocketStateEntry(
	entry: LiveSocketStateEntry,
): SidecarSocketStateEntry {
	return {
		processId: entry.process_id,
		...(entry.host !== undefined ? { host: entry.host } : {}),
		...(entry.port !== undefined ? { port: entry.port } : {}),
		...(entry.path !== undefined ? { path: entry.path } : {}),
	};
}

function toSidecarProcessSnapshotEntry(
	entry: LiveProcessSnapshotEntry,
): SidecarProcessSnapshotEntry {
	return {
		processId: entry.process_id,
		pid: entry.pid,
		ppid: entry.ppid,
		pgid: entry.pgid,
		sid: entry.sid,
		driver: entry.driver,
		command: entry.command,
		args: [...(entry.args ?? [])],
		cwd: entry.cwd,
		status: entry.status,
		exitCode: entry.exit_code ?? null,
	};
}

function toWireRootFilesystemDescriptor(
	descriptor: RootFilesystemDescriptor | undefined,
): {
	mode?: "ephemeral" | "read_only";
	disable_default_base_layer?: boolean;
	lowers?: WireRootFilesystemLowerDescriptor[];
	bootstrap_entries?: Array<{
		path: string;
		kind: "file" | "directory" | "symlink";
		mode?: number;
		uid?: number;
		gid?: number;
		content?: string;
		encoding?: RootFilesystemEntryEncoding;
		target?: string;
		executable?: boolean;
	}>;
} {
	if (!descriptor) {
		return {};
	}

	return {
		...(descriptor.mode ? { mode: descriptor.mode } : {}),
		...(descriptor.disableDefaultBaseLayer !== undefined
			? { disable_default_base_layer: descriptor.disableDefaultBaseLayer }
			: {}),
		...(descriptor.lowers
			? {
					lowers: descriptor.lowers.map((lower) =>
						lower.kind === "bundled_base_filesystem"
							? { kind: "bundled_base_filesystem" }
							: {
									kind: "snapshot",
									entries: (lower.entries ?? []).map(toWireRootFilesystemEntry),
								},
					),
				}
			: {}),
		...(descriptor.bootstrapEntries
			? {
					bootstrap_entries: descriptor.bootstrapEntries.map(
						toWireRootFilesystemEntry,
					),
				}
			: {}),
	};
}

function toWireRootFilesystemEntry(entry: RootFilesystemEntry): {
	path: string;
	kind: "file" | "directory" | "symlink";
	mode?: number;
	uid?: number;
	gid?: number;
	content?: string;
	encoding?: RootFilesystemEntryEncoding;
	target?: string;
	executable?: boolean;
} {
	return {
		path: entry.path,
		kind: entry.kind,
		...(entry.mode !== undefined ? { mode: entry.mode } : {}),
		...(entry.uid !== undefined ? { uid: entry.uid } : {}),
		...(entry.gid !== undefined ? { gid: entry.gid } : {}),
		...(entry.content !== undefined ? { content: entry.content } : {}),
		...(entry.encoding !== undefined ? { encoding: entry.encoding } : {}),
		...(entry.target !== undefined ? { target: entry.target } : {}),
		...(entry.executable !== undefined ? { executable: entry.executable } : {}),
	};
}

function toWireMountDescriptor(descriptor: SidecarMountDescriptor): {
	guest_path: string;
	guest_source: string;
	guest_fstype: string;
	read_only: boolean;
	plugin: {
		id: string;
		config: MountConfigJsonObject;
	};
} {
	return {
		guest_path: descriptor.guestPath,
		guest_source: descriptor.guestSource ?? descriptor.plugin.id,
		guest_fstype: descriptor.guestFstype ?? descriptor.plugin.id,
		read_only: descriptor.readOnly,
		plugin: {
			id: descriptor.plugin.id,
			config: descriptor.plugin.config ?? {},
		},
	};
}

function toWireSoftwareDescriptor(descriptor: SidecarSoftwareDescriptor): {
	package_name: string;
	root: string;
} {
	return {
		package_name: descriptor.packageName,
		root: descriptor.root,
	};
}

function toWirePermissionsPolicy(
	policy: SidecarPermissionsPolicy | undefined,
): WirePermissionsPolicy | undefined {
	if (!policy) {
		return undefined;
	}
	return {
		fs: policy.fs,
		network: policy.network,
		child_process: policy.childProcess,
		process: policy.process,
		env: policy.env,
		binding: policy.binding,
	};
}

function toWireProjectedModuleDescriptor(
	descriptor: SidecarProjectedModuleDescriptor,
): {
	package_name: string;
	entrypoint: string;
} {
	return {
		package_name: descriptor.packageName,
		entrypoint: descriptor.entrypoint,
	};
}

function toWirePackageDescriptor(descriptor: SidecarPackageDescriptor): {
	path: string;
} {
	return {
		path: descriptor.path,
	};
}

function fromWireProjectedAgent(agent: {
	id: string;
	acp_entrypoint: string;
	adapter_entrypoint: string;
}): SidecarProjectedAgent {
	return {
		id: agent.id,
		acpEntrypoint: agent.acp_entrypoint,
		adapterEntrypoint: agent.adapter_entrypoint,
	};
}
