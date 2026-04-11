import { type ChildProcessWithoutNullStreams, spawn } from "node:child_process";
import type { JsonRpcNotification, JsonRpcResponse } from "../json-rpc.js";

const PROTOCOL_SCHEMA = {
	name: "agent-os-sidecar",
	version: 1,
} as const;

type OwnershipScope =
	| { scope: "connection"; connection_id: string }
	| { scope: "session"; connection_id: string; session_id: string }
	| {
			scope: "vm";
			connection_id: string;
			session_id: string;
			vm_id: string;
	  };

type SidecarPlacement =
	| { kind: "shared"; pool?: string | null }
	| { kind: "explicit"; sidecar_id: string };

type GuestRuntimeKind = "java_script" | "web_assembly";
type WasmPermissionTier = "full" | "read-write" | "read-only" | "isolated";
type RootFilesystemEntryEncoding = "utf8" | "base64";

type RootFilesystemDescriptor = {
	mode?: "ephemeral" | "read_only";
	disableDefaultBaseLayer?: boolean;
	lowers?: RootFilesystemLowerDescriptor[];
	bootstrapEntries?: RootFilesystemEntry[];
};

type WireRootFilesystemDescriptor = {
	mode?: "ephemeral" | "read_only";
	disable_default_base_layer?: boolean;
	lowers?: WireRootFilesystemLowerDescriptor[];
	bootstrap_entries?: WireRootFilesystemEntry[];
};

export interface RootFilesystemEntry {
	path: string;
	kind: "file" | "directory" | "symlink";
	mode?: number;
	uid?: number;
	gid?: number;
	content?: string;
	encoding?: RootFilesystemEntryEncoding;
	target?: string;
	executable?: boolean;
}

export interface RootFilesystemLowerDescriptor {
	kind: "snapshot" | "bundled_base_filesystem";
	entries?: RootFilesystemEntry[];
}

type WireRootFilesystemLowerDescriptor =
	| {
			kind: "snapshot";
			entries: WireRootFilesystemEntry[];
	  }
	| {
			kind: "bundled_base_filesystem";
	  };

type WireRootFilesystemEntry = {
	path: string;
	kind: "file" | "directory" | "symlink";
	mode?: number;
	uid?: number;
	gid?: number;
	content?: string;
	encoding?: RootFilesystemEntryEncoding;
	target?: string;
	executable?: boolean;
};

export interface GuestFilesystemStat {
	mode: number;
	size: number;
	blocks: number;
	dev: number;
	rdev: number;
	is_directory: boolean;
	is_symbolic_link: boolean;
	atime_ms: number;
	mtime_ms: number;
	ctime_ms: number;
	birthtime_ms: number;
	ino: number;
	nlink: number;
	uid: number;
	gid: number;
}

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
	status: "running" | "exited";
	exitCode: number | null;
}

export interface SidecarZombieTimerCount {
	count: number;
}

type GuestFilesystemOperation =
	| "read_file"
	| "write_file"
	| "create_dir"
	| "mkdir"
	| "exists"
	| "stat"
	| "lstat"
	| "read_dir"
	| "remove_file"
	| "remove_dir"
	| "rename"
	| "realpath"
	| "symlink"
	| "read_link"
	| "link"
	| "chmod"
	| "chown"
	| "utimes"
	| "truncate";

export interface SidecarRegisteredToolExample {
	description: string;
	input: unknown;
}

export interface SidecarRegisteredToolDefinition {
	description: string;
	inputSchema: unknown;
	timeoutMs?: number;
	examples?: SidecarRegisteredToolExample[];
}

type RequestPayload =
	| {
			type: "authenticate";
			client_name: string;
			auth_token: string;
	  }
	| {
			type: "open_session";
			placement: SidecarPlacement;
			metadata: Record<string, string>;
	  }
	| {
			type: "create_vm";
			runtime: GuestRuntimeKind;
			metadata: Record<string, string>;
			root_filesystem: WireRootFilesystemDescriptor;
			permissions?: WirePermissionsPolicy;
	  }
	| {
			type: "create_session";
			agent_type: string;
			runtime?: GuestRuntimeKind;
			adapter_entrypoint: string;
			args: string[];
			env: Record<string, string>;
			cwd: string;
			mcp_servers: unknown[];
	  }
	| {
			type: "session_request";
			session_id: string;
			method: string;
			params?: unknown;
	  }
	| {
			type: "get_session_state";
			session_id: string;
	  }
	| {
			type: "close_agent_session";
			session_id: string;
	  }
	| {
			type: "configure_vm";
			mounts: WireMountDescriptor[];
			software: WireSoftwareDescriptor[];
			permissions?: WirePermissionsPolicy;
			module_access_cwd?: string;
			instructions: string[];
			projected_modules: WireProjectedModuleDescriptor[];
			command_permissions: Record<string, WasmPermissionTier>;
			allowed_node_builtins?: string[];
			loopback_exempt_ports?: number[];
	  }
	| {
			type: "register_toolkit";
			name: string;
			description: string;
			tools: Record<
				string,
				{
					description: string;
					input_schema: unknown;
					timeout_ms?: number;
					examples?: Array<{ description: string; input: unknown }>;
				}
			>;
	  }
	| {
			type: "dispose_vm";
			reason: "requested" | "connection_closed" | "host_shutdown";
	  }
	| {
			type: "bootstrap_root_filesystem";
			entries: RootFilesystemEntry[];
	  }
	| {
			type: "create_layer";
	  }
	| {
			type: "seal_layer";
			layer_id: string;
	  }
	| {
			type: "import_snapshot";
			entries: RootFilesystemEntry[];
	  }
	| {
			type: "export_snapshot";
			layer_id: string;
	  }
	| {
			type: "create_overlay";
			mode?: "ephemeral" | "read_only";
			upper_layer_id?: string;
			lower_layer_ids: string[];
	  }
	| {
			type: "snapshot_root_filesystem";
	  }
	| {
			type: "guest_filesystem_call";
			operation: GuestFilesystemOperation;
			path: string;
			destination_path?: string;
			target?: string;
			content?: string;
			encoding?: RootFilesystemEntryEncoding;
			recursive?: boolean;
			mode?: number;
			uid?: number;
			gid?: number;
			atime_ms?: number;
			mtime_ms?: number;
			len?: number;
	  }
	| {
			type: "execute";
			process_id: string;
			command?: string;
			runtime?: GuestRuntimeKind;
			entrypoint?: string;
			args: string[];
			env?: Record<string, string>;
			cwd?: string;
			wasm_permission_tier?: WasmPermissionTier;
	  }
	| {
			type: "write_stdin";
			process_id: string;
			chunk: string;
	  }
	| {
			type: "close_stdin";
			process_id: string;
	  }
	| {
			type: "kill_process";
			process_id: string;
			signal: string;
	  }
	| {
			type: "get_process_snapshot";
	  }
	| {
			type: "find_listener";
			host?: string;
			port?: number;
			path?: string;
	  }
	| {
			type: "find_bound_udp";
			host?: string;
			port?: number;
	  }
	| {
			type: "get_signal_state";
			process_id: string;
	  }
	| {
			type: "get_zombie_timer_count";
	  };

export type SidecarRequestPayload =
	| {
			type: "tool_invocation";
			invocation_id: string;
			tool_key: string;
			input: unknown;
			timeout_ms: number;
	  }
	| {
			type: "permission_request";
			session_id: string;
			permission_id: string;
			params: unknown;
	  }
	| {
			type: "js_bridge_call";
			call_id: string;
			mount_id: string;
			operation: string;
			args: unknown;
	  };

export type SidecarResponsePayload =
	| {
			type: "tool_invocation_result";
			invocation_id: string;
			result?: unknown;
			error?: string;
	  }
	| {
			type: "permission_request_result";
			permission_id: string;
			reply?: "once" | "always" | "reject";
			error?: string;
	  }
	| {
			type: "js_bridge_result";
			call_id: string;
			result?: unknown;
			error?: string;
	  };

interface RequestFrame {
	frame_type: "request";
	schema: typeof PROTOCOL_SCHEMA;
	request_id: number;
	ownership: OwnershipScope;
	payload: RequestPayload;
}

interface EventFrame {
	frame_type: "event";
	schema: typeof PROTOCOL_SCHEMA;
	ownership: OwnershipScope;
	payload:
		| {
				type: "vm_lifecycle";
				state: "creating" | "ready" | "disposing" | "disposed" | "failed";
		  }
		| {
				type: "process_output";
				process_id: string;
				channel: "stdout" | "stderr";
				chunk: string;
		  }
		| {
				type: "process_exited";
				process_id: string;
				exit_code: number;
		  }
		| {
				type: "structured";
				name: string;
				detail: Record<string, string>;
		  };
}

export interface SidecarRequestFrame {
	frame_type: "sidecar_request";
	schema: typeof PROTOCOL_SCHEMA;
	request_id: number;
	ownership: OwnershipScope;
	payload: SidecarRequestPayload;
}

interface ResponseFrame {
	frame_type: "response";
	schema: typeof PROTOCOL_SCHEMA;
	request_id: number;
	ownership: OwnershipScope;
	payload:
		| {
				type: "authenticated";
				sidecar_id: string;
				connection_id: string;
				max_frame_bytes: number;
		  }
		| {
				type: "session_opened";
				session_id: string;
				owner_connection_id: string;
		  }
		| {
				type: "vm_created";
				vm_id: string;
		  }
		| {
				type: "session_created";
				session_id: string;
				pid?: number;
				modes?: unknown;
				config_options: unknown[];
				agent_capabilities?: unknown;
				agent_info?: unknown;
		  }
		| {
				type: "session_rpc";
				session_id: string;
				response: unknown;
		  }
		| {
				type: "session_state";
				session_id: string;
				agent_type: string;
				process_id: string;
				pid?: number;
				closed: boolean;
				modes?: unknown;
				config_options: unknown[];
				agent_capabilities?: unknown;
				agent_info?: unknown;
				events: Array<{
					sequence_number: number;
					notification: unknown;
				}>;
		  }
		| {
				type: "agent_session_closed";
				session_id: string;
		  }
		| {
				type: "vm_configured";
				applied_mounts: number;
				applied_software: number;
		  }
		| {
				type: "toolkit_registered";
				toolkit: string;
				command_count: number;
				prompt_markdown: string;
		  }
		| {
				type: "layer_created";
				layer_id: string;
		  }
		| {
				type: "layer_sealed";
				layer_id: string;
		  }
		| {
				type: "snapshot_imported";
				layer_id: string;
		  }
		| {
				type: "snapshot_exported";
				layer_id: string;
				entries: RootFilesystemEntry[];
		  }
		| {
				type: "overlay_created";
				layer_id: string;
		  }
		| {
				type: "root_filesystem_bootstrapped";
				entry_count: number;
		  }
		| {
				type: "guest_filesystem_result";
				operation: GuestFilesystemOperation;
				path: string;
				content?: string;
				encoding?: RootFilesystemEntryEncoding;
				entries?: string[];
				stat?: GuestFilesystemStat;
				exists?: boolean;
				target?: string;
		  }
		| {
				type: "root_filesystem_snapshot";
				entries: RootFilesystemEntry[];
		  }
		| {
				type: "vm_disposed";
				vm_id: string;
		  }
		| {
				type: "process_started";
				process_id: string;
				pid?: number;
		  }
		| {
				type: "stdin_written";
				process_id: string;
				accepted_bytes: number;
		  }
		| {
				type: "stdin_closed";
				process_id: string;
		  }
		| {
				type: "process_killed";
				process_id: string;
		  }
		| {
				type: "process_snapshot";
				processes: Array<{
					process_id: string;
					pid: number;
					ppid: number;
					pgid: number;
					sid: number;
					driver: string;
					command: string;
					args?: string[];
					cwd: string;
					status: "running" | "exited";
					exit_code?: number;
				}>;
		  }
		| {
				type: "listener_snapshot";
				listener?: {
					process_id: string;
					host?: string;
					port?: number;
					path?: string;
				};
		  }
		| {
				type: "bound_udp_snapshot";
				socket?: {
					process_id: string;
					host?: string;
					port?: number;
					path?: string;
				};
		  }
		| {
				type: "signal_state";
				process_id: string;
				handlers: Record<
					string,
					{
						action: "default" | "ignore" | "user";
						mask: number[];
						flags: number;
					}
				>;
		  }
		| {
				type: "zombie_timer_count";
				count: number;
		  }
		| {
				type: "rejected";
				code: string;
				message: string;
		  };
}

interface SidecarResponseFrame {
	frame_type: "sidecar_response";
	schema: typeof PROTOCOL_SCHEMA;
	request_id: number;
	ownership: OwnershipScope;
	payload: SidecarResponsePayload;
}

type ProtocolFrame =
	| RequestFrame
	| ResponseFrame
	| EventFrame
	| SidecarRequestFrame
	| SidecarResponseFrame;

type NativeTransportPayloadCodec = "bare" | "json";

export type SidecarRequestHandler = (
	request: SidecarRequestFrame,
) => Promise<SidecarResponsePayload> | SidecarResponsePayload;

export interface NativeSidecarSpawnOptions {
	cwd: string;
	command?: string;
	args?: string[];
	frameTimeoutMs?: number;
	// Migration-only compatibility path for pre-BARE test fixtures.
	payloadCodec?: NativeTransportPayloadCodec;
}

export interface AuthenticatedSession {
	connectionId: string;
	sessionId: string;
}

export interface CreatedVm {
	vmId: string;
}

export interface SidecarSequencedNotification {
	sequenceNumber: number;
	notification: JsonRpcNotification;
}

export interface SidecarSessionCreated {
	sessionId: string;
	pid?: number;
	modes?: unknown;
	configOptions: unknown[];
	agentCapabilities?: unknown;
	agentInfo?: unknown;
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
	events: SidecarSequencedNotification[];
}

export interface SidecarMountPluginDescriptor {
	id: string;
	config?: Record<string, unknown>;
}

export interface SidecarMountDescriptor {
	guestPath: string;
	readOnly: boolean;
	plugin: SidecarMountPluginDescriptor;
}

type WireMountDescriptor = {
	guest_path: string;
	read_only: boolean;
	plugin: {
		id: string;
		config: Record<string, unknown>;
	};
};

export interface SidecarSoftwareDescriptor {
	packageName: string;
	root: string;
}

type WireSoftwareDescriptor = {
	package_name: string;
	root: string;
};

export type SidecarPermissionMode = "allow" | "ask" | "deny";

export interface SidecarFsPermissionRule {
	mode: SidecarPermissionMode;
	operations?: string[];
	paths?: string[];
}

export interface SidecarPatternPermissionRule {
	mode: SidecarPermissionMode;
	operations?: string[];
	patterns?: string[];
}

export interface SidecarRulePermissions<TRule> {
	default?: SidecarPermissionMode;
	rules: TRule[];
}

export type SidecarPermissionScope<TRule> =
	| SidecarPermissionMode
	| SidecarRulePermissions<TRule>;

export interface SidecarPermissionsPolicy {
	fs?: SidecarPermissionScope<SidecarFsPermissionRule>;
	network?: SidecarPermissionScope<SidecarPatternPermissionRule>;
	childProcess?: SidecarPermissionScope<SidecarPatternPermissionRule>;
	env?: SidecarPermissionScope<SidecarPatternPermissionRule>;
}

type WirePermissionsPolicy = {
	fs?: SidecarPermissionScope<SidecarFsPermissionRule>;
	network?: SidecarPermissionScope<SidecarPatternPermissionRule>;
	child_process?: SidecarPermissionScope<SidecarPatternPermissionRule>;
	env?: SidecarPermissionScope<SidecarPatternPermissionRule>;
};

export interface SidecarProjectedModuleDescriptor {
	packageName: string;
	entrypoint: string;
}

type WireProjectedModuleDescriptor = {
	package_name: string;
	entrypoint: string;
};

export class NativeSidecarProcessClient {
	private readonly child: ChildProcessWithoutNullStreams;
	private readonly bufferedEvents: EventFrame[] = [];
	private readonly eventListeners = new Set<(event: EventFrame) => void>();
	private readonly stderrChunks: Buffer[] = [];
	private readonly frameTimeoutMs: number;
	private readonly payloadCodec: NativeTransportPayloadCodec;
	private stdoutBuffer = Buffer.alloc(0);
	private stdoutClosedError: Error | null = null;
	private readonly pendingResponses = new Map<
		number,
		{
			resolve: (frame: ResponseFrame) => void;
			reject: (error: Error) => void;
			timer: ReturnType<typeof setTimeout>;
		}
	>();
	private readonly eventWaiters = new Set<{
		matcher: (event: EventFrame) => boolean;
		resolve: (event: EventFrame) => void;
		reject: (error: Error) => void;
		timer: ReturnType<typeof setTimeout>;
	}>();
	private nextRequestId = 1;
	private sidecarRequestHandler: SidecarRequestHandler | null = null;

	private constructor(
		child: ChildProcessWithoutNullStreams,
		frameTimeoutMs: number,
		payloadCodec: NativeTransportPayloadCodec,
	) {
		this.child = child;
		this.frameTimeoutMs = frameTimeoutMs;
		this.payloadCodec = payloadCodec;
		this.child.stderr.on("data", (chunk: Buffer | string) => {
			this.stderrChunks.push(
				typeof chunk === "string" ? Buffer.from(chunk) : Buffer.from(chunk),
			);
		});
		this.child.stdout.on("data", (chunk: Buffer | string) => {
			const bytes =
				typeof chunk === "string" ? Buffer.from(chunk) : Buffer.from(chunk);
			this.stdoutBuffer = Buffer.concat([this.stdoutBuffer, bytes]);
			this.drainFrames();
		});
		this.child.stdout.on("end", () => {
			this.stdoutClosedError = new Error(
				`sidecar stdout closed while reading frame\nstderr:\n${this.stderrText()}`,
			);
			this.rejectPending(this.stdoutClosedError);
		});
		this.child.stdout.on("error", (error) => {
			const normalized =
				error instanceof Error ? error : new Error(String(error));
			this.stdoutClosedError = normalized;
			this.rejectPending(normalized);
		});
	}

	static spawn(options: NativeSidecarSpawnOptions): NativeSidecarProcessClient {
		const child = spawn(
			options.command ?? "cargo",
			options.args ?? ["run", "-q", "-p", "agent-os-sidecar"],
			{
				cwd: options.cwd,
				stdio: ["pipe", "pipe", "pipe"],
			},
		);
		return new NativeSidecarProcessClient(
			child,
			options.frameTimeoutMs ?? 60_000,
			options.payloadCodec ?? "bare",
		);
	}

	setSidecarRequestHandler(handler: SidecarRequestHandler | null): void {
		this.sidecarRequestHandler = handler;
	}

	onEvent(handler: (event: EventFrame) => void): () => void {
		this.eventListeners.add(handler);
		return () => {
			this.eventListeners.delete(handler);
		};
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
				client_name: "packages-core-vitest",
				auth_token: "packages-core-vitest-token",
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
			metadata?: Record<string, string>;
			rootFilesystem?: RootFilesystemDescriptor;
			permissions?: SidecarPermissionsPolicy;
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
				metadata: options.metadata ?? {},
				root_filesystem: toWireRootFilesystemDescriptor(options.rootFilesystem),
				permissions: toWirePermissionsPolicy(options.permissions),
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

	async createSession(
		session: AuthenticatedSession,
		vm: CreatedVm,
		options: {
			agentType: string;
			runtime?: GuestRuntimeKind;
			adapterEntrypoint: string;
			args?: string[];
			env?: Record<string, string>;
			cwd: string;
			mcpServers?: unknown[];
		},
	): Promise<SidecarSessionCreated> {
		const response = await this.sendRequest({
			ownership: {
				scope: "vm",
				connection_id: session.connectionId,
				session_id: session.sessionId,
				vm_id: vm.vmId,
			},
			payload: {
				type: "create_session",
				agent_type: options.agentType,
				...(options.runtime ? { runtime: options.runtime } : {}),
				adapter_entrypoint: options.adapterEntrypoint,
				args: options.args ?? [],
				env: options.env ?? {},
				cwd: options.cwd,
				mcp_servers: options.mcpServers ?? [],
			},
		});
		if (response.payload.type !== "session_created") {
			throw new Error(
				`unexpected create_session response: ${response.payload.type}`,
			);
		}
		return {
			sessionId: response.payload.session_id,
			...(response.payload.pid !== undefined
				? { pid: response.payload.pid }
				: {}),
			modes: response.payload.modes,
			configOptions: response.payload.config_options ?? [],
			agentCapabilities: response.payload.agent_capabilities,
			agentInfo: response.payload.agent_info,
		};
	}

	async sessionRequest(
		session: AuthenticatedSession,
		vm: CreatedVm,
		options: {
			sessionId: string;
			method: string;
			params?: unknown;
		},
	): Promise<JsonRpcResponse> {
		const response = await this.sendRequest({
			ownership: {
				scope: "vm",
				connection_id: session.connectionId,
				session_id: session.sessionId,
				vm_id: vm.vmId,
			},
			payload: {
				type: "session_request",
				session_id: options.sessionId,
				method: options.method,
				...(options.params !== undefined ? { params: options.params } : {}),
			},
		});
		if (response.payload.type !== "session_rpc") {
			throw new Error(
				`unexpected session_request response: ${response.payload.type}`,
			);
		}
		return toJsonRpcResponse(response.payload.response);
	}

	async getSessionState(
		session: AuthenticatedSession,
		vm: CreatedVm,
		sessionId: string,
	): Promise<SidecarSessionState> {
		const response = await this.sendRequest({
			ownership: {
				scope: "vm",
				connection_id: session.connectionId,
				session_id: session.sessionId,
				vm_id: vm.vmId,
			},
			payload: {
				type: "get_session_state",
				session_id: sessionId,
			},
		});
		if (response.payload.type !== "session_state") {
			throw new Error(
				`unexpected get_session_state response: ${response.payload.type}`,
			);
		}
		return {
			sessionId: response.payload.session_id,
			agentType: response.payload.agent_type,
			processId: response.payload.process_id,
			...(response.payload.pid !== undefined
				? { pid: response.payload.pid }
				: {}),
			closed: response.payload.closed,
			modes: response.payload.modes,
			configOptions: response.payload.config_options ?? [],
			agentCapabilities: response.payload.agent_capabilities,
			agentInfo: response.payload.agent_info,
			events: (response.payload.events ?? []).map((event) => ({
				sequenceNumber: event.sequence_number,
				notification: toJsonRpcNotification(event.notification),
			})),
		};
	}

	async closeAgentSession(
		session: AuthenticatedSession,
		vm: CreatedVm,
		sessionId: string,
	): Promise<void> {
		const response = await this.sendRequest({
			ownership: {
				scope: "vm",
				connection_id: session.connectionId,
				session_id: session.sessionId,
				vm_id: vm.vmId,
			},
			payload: {
				type: "close_agent_session",
				session_id: sessionId,
			},
		});
		if (response.payload.type !== "agent_session_closed") {
			throw new Error(
				`unexpected close_agent_session response: ${response.payload.type}`,
			);
		}
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
			allowedNodeBuiltins?: string[];
			loopbackExemptPorts?: number[];
		},
	): Promise<void> {
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
				...(options.allowedNodeBuiltins
					? { allowed_node_builtins: options.allowedNodeBuiltins }
					: {}),
				...(options.loopbackExemptPorts
					? { loopback_exempt_ports: options.loopbackExemptPorts }
					: {}),
			},
		});
		if (response.payload.type !== "vm_configured") {
			throw new Error(
				`unexpected configure_vm response: ${response.payload.type}`,
			);
		}
	}

	async registerToolkit(
		session: AuthenticatedSession,
		vm: CreatedVm,
		toolkit: {
			name: string;
			description: string;
			tools: Record<string, SidecarRegisteredToolDefinition>;
		},
	): Promise<{
		toolkit: string;
		commandCount: number;
		promptMarkdown: string;
	}> {
		const response = await this.sendRequest({
			ownership: {
				scope: "vm",
				connection_id: session.connectionId,
				session_id: session.sessionId,
				vm_id: vm.vmId,
			},
			payload: {
				type: "register_toolkit",
				name: toolkit.name,
				description: toolkit.description,
				tools: Object.fromEntries(
					Object.entries(toolkit.tools).map(([toolName, tool]) => [
						toolName,
						{
							description: tool.description,
							input_schema: tool.inputSchema,
							...(tool.timeoutMs !== undefined
								? { timeout_ms: tool.timeoutMs }
								: {}),
							...(tool.examples && tool.examples.length > 0
								? {
										examples: tool.examples.map((example) => ({
											description: example.description,
											input: example.input,
										})),
									}
								: {}),
						},
					]),
				),
			},
		});
		if (response.payload.type !== "toolkit_registered") {
			throw new Error(
				`unexpected register_toolkit response: ${response.payload.type}`,
			);
		}
		return {
			toolkit: response.payload.toolkit,
			commandCount: response.payload.command_count,
			promptMarkdown: response.payload.prompt_markdown,
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
			},
		});
		if (response.payload.type !== "root_filesystem_snapshot") {
			throw new Error(
				`unexpected snapshot_root_filesystem response: ${response.payload.type}`,
			);
		}
		return response.payload.entries;
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
					typeof chunk === "string"
						? chunk
						: Buffer.from(chunk).toString("utf8"),
			},
		});
		if (response.payload.type !== "stdin_written") {
			throw new Error(
				`unexpected write_stdin response: ${response.payload.type}`,
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

	async waitForEvent(
		matcher: (event: EventFrame) => boolean,
		timeoutMs = 30_000,
	): Promise<EventFrame> {
		const bufferedIndex = this.bufferedEvents.findIndex(matcher);
		if (bufferedIndex >= 0) {
			return this.bufferedEvents.splice(bufferedIndex, 1)[0];
		}
		if (this.stdoutClosedError) {
			throw this.stdoutClosedError;
		}

		return await new Promise<EventFrame>((resolve, reject) => {
			const waiter = {
				matcher,
				resolve: (event: EventFrame) => {
					clearTimeout(waiter.timer);
					this.eventWaiters.delete(waiter);
					resolve(event);
				},
				reject: (error: Error) => {
					clearTimeout(waiter.timer);
					this.eventWaiters.delete(waiter);
					reject(error);
				},
				timer: setTimeout(() => {
					this.eventWaiters.delete(waiter);
					reject(
						new Error(
							`timed out waiting for sidecar event\nstderr:\n${this.stderrText()}`,
						),
					);
				}, timeoutMs),
			};
			this.eventWaiters.add(waiter);
		});
	}

	async dispose(): Promise<void> {
		if (!this.child.stdin.destroyed) {
			this.child.stdin.end();
		}
		const exitCode = await new Promise<number | null>((resolve, reject) => {
			const cleanup = () => {
				this.child.off("error", onError);
				this.child.off("exit", onExit);
				this.child.off("close", onClose);
			};
			const resolveIfExited = (): boolean => {
				if (this.child.exitCode !== null || this.child.signalCode !== null) {
					cleanup();
					resolve(this.child.exitCode);
					return true;
				}
				return false;
			};
			const onError = (error: Error) => {
				cleanup();
				reject(error);
			};
			const onExit = (code: number | null) => {
				cleanup();
				resolve(code);
			};
			const onClose = (code: number | null) => {
				cleanup();
				resolve(code);
			};

			if (resolveIfExited()) {
				return;
			}

			this.child.on("error", onError);
			this.child.on("exit", onExit);
			this.child.on("close", onClose);

			resolveIfExited();
		});
		if (exitCode !== 0 && exitCode !== null) {
			throw new Error(
				`native sidecar exited with code ${exitCode}\nstderr:\n${this.stderrText()}`,
			);
		}
	}

	private async sendRequest(input: {
		ownership: OwnershipScope;
		payload: RequestPayload;
	}): Promise<ResponseFrame> {
		if (this.stdoutClosedError) {
			throw this.stdoutClosedError;
		}

		const requestId = this.nextRequestId++;
		const request: RequestFrame = {
			frame_type: "request",
			schema: PROTOCOL_SCHEMA,
			request_id: requestId,
			ownership: input.ownership,
			payload: input.payload,
		};
		const response = await new Promise<ResponseFrame>((resolve, reject) => {
			const entry = {
				resolve: (frame: ResponseFrame) => {
					clearTimeout(entry.timer);
					this.pendingResponses.delete(requestId);
					resolve(frame);
				},
				reject: (error: Error) => {
					clearTimeout(entry.timer);
					this.pendingResponses.delete(requestId);
					reject(error);
				},
				timer: setTimeout(() => {
					this.pendingResponses.delete(requestId);
					reject(
						new Error(
							`timed out waiting for sidecar protocol frame for ${input.payload.type}\nstderr:\n${this.stderrText()}`,
						),
					);
				}, this.frameTimeoutMs),
			};
			this.pendingResponses.set(requestId, entry);

			void this.writeFrame(request).catch((error) => {
				entry.reject(error instanceof Error ? error : new Error(String(error)));
			});
		});

		if (response.payload.type === "rejected") {
			throw new Error(
				`sidecar rejected request ${request.request_id}: ${response.payload.code}: ${response.payload.message}`,
			);
		}
		return response;
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

	private async writeFrame(frame: ProtocolFrame): Promise<void> {
		const payload = encodeProtocolFramePayload(frame, this.payloadCodec);
		const encoded = Buffer.allocUnsafe(4 + payload.length);
		encoded.writeUInt32BE(payload.length, 0);
		payload.copy(encoded, 4);
		await new Promise<void>((resolve, reject) => {
			this.child.stdin.write(encoded, (error) => {
				if (error) {
					reject(error);
					return;
				}
				resolve();
			});
		});
	}

	private tryTakeFrame():
		| ResponseFrame
		| EventFrame
		| SidecarRequestFrame
		| null {
		if (this.stdoutBuffer.length < 4) {
			return null;
		}

		const declaredLength = this.stdoutBuffer.readUInt32BE(0);
		if (this.stdoutBuffer.length < 4 + declaredLength) {
			return null;
		}

		const payload = this.stdoutBuffer.subarray(4, 4 + declaredLength);
		this.stdoutBuffer = this.stdoutBuffer.subarray(4 + declaredLength);
		return decodeProtocolFramePayload(payload, this.payloadCodec) as
			| ResponseFrame
			| EventFrame
			| SidecarRequestFrame;
	}

	private drainFrames(): void {
		for (;;) {
			const frame = this.tryTakeFrame();
			if (!frame) {
				return;
			}
			if (frame.frame_type === "response") {
				const pending = this.pendingResponses.get(frame.request_id);
				if (pending) {
					pending.resolve(frame);
				}
				continue;
			}
			if (frame.frame_type === "sidecar_request") {
				void this.dispatchSidecarRequest(frame);
				continue;
			}
			this.dispatchEvent(frame);
		}
	}

	private async dispatchSidecarRequest(
		request: SidecarRequestFrame,
	): Promise<void> {
		let payload: SidecarResponsePayload;
		try {
			if (!this.sidecarRequestHandler) {
				throw new Error(
					`no sidecar request handler registered for ${request.payload.type}`,
				);
			}
			payload = await this.sidecarRequestHandler(request);
			if (!isMatchingSidecarResponsePayload(request.payload, payload)) {
				throw new Error(
					`sidecar handler returned ${payload.type} for ${request.payload.type}`,
				);
			}
		} catch (error) {
			payload = errorSidecarResponsePayload(request.payload, error);
		}

		try {
			await this.writeFrame({
				frame_type: "sidecar_response",
				schema: PROTOCOL_SCHEMA,
				request_id: request.request_id,
				ownership: request.ownership,
				payload,
			});
		} catch (error) {
			const normalized =
				error instanceof Error ? error : new Error(String(error));
			this.stdoutClosedError = normalized;
			this.rejectPending(normalized);
		}
	}

	private dispatchEvent(event: EventFrame): void {
		for (const listener of this.eventListeners) {
			try {
				listener(event);
			} catch {
				// Event listeners are best-effort observers and must not break framing.
			}
		}
		for (const waiter of this.eventWaiters) {
			if (!waiter.matcher(event)) {
				continue;
			}
			waiter.resolve(event);
			return;
		}
		this.bufferedEvents.push(event);
	}

	private rejectPending(error: Error): void {
		for (const pending of this.pendingResponses.values()) {
			pending.reject(error);
		}
		this.pendingResponses.clear();
		for (const waiter of this.eventWaiters) {
			waiter.reject(error);
		}
		this.eventWaiters.clear();
	}

	private stderrText(): string {
		return Buffer.concat(this.stderrChunks).toString("utf8").trim();
	}
}

function encodeProtocolFramePayload(
	frame: ProtocolFrame,
	codec: NativeTransportPayloadCodec,
): Buffer {
	if (codec === "json") {
		return Buffer.from(JSON.stringify(frame), "utf8");
	}
	return encodeBareProtocolFrame(frame);
}

function decodeProtocolFramePayload(
	payload: Uint8Array,
	codec: NativeTransportPayloadCodec,
): ResponseFrame | EventFrame | SidecarRequestFrame {
	if (codec === "json") {
		return JSON.parse(Buffer.from(payload).toString("utf8")) as
			| ResponseFrame
			| EventFrame
			| SidecarRequestFrame;
	}
	return decodeBareProtocolFrame(payload);
}

type BareEnumCodec<TValue extends string> = {
	encode(value: TValue, context: string): number;
	decode(tag: number, context: string): TValue;
};

function createBareEnumCodec<TValue extends string>(
	entries: ReadonlyArray<readonly [TValue, number]>,
): BareEnumCodec<TValue> {
	const tagByValue = new Map(entries);
	const valueByTag = new Map(entries.map(([value, tag]) => [tag, value]));
	return {
		encode(value, context) {
			const tag = tagByValue.get(value);
			if (tag === undefined) {
				throw new Error(`unsupported ${context}: ${value}`);
			}
			return tag;
		},
		decode(tag, context) {
			const value = valueByTag.get(tag);
			if (value === undefined) {
				throw new Error(`unsupported ${context} tag: ${tag}`);
			}
			return value;
		},
	};
}

const BARE_GUEST_RUNTIME_KIND = createBareEnumCodec<
	GuestRuntimeKind | "python"
>([
	["java_script", 1],
	["python", 2],
	["web_assembly", 3],
]);
const BARE_DISPOSE_REASON = createBareEnumCodec<
	Extract<RequestPayload, { type: "dispose_vm" }>["reason"]
>([
	["requested", 1],
	["connection_closed", 2],
	["host_shutdown", 3],
]);
const BARE_GUEST_FILESYSTEM_OPERATION = createBareEnumCodec<
	GuestFilesystemOperation
>([
	["read_file", 1],
	["write_file", 2],
	["create_dir", 3],
	["mkdir", 4],
	["exists", 5],
	["stat", 6],
	["lstat", 7],
	["read_dir", 8],
	["remove_file", 9],
	["remove_dir", 10],
	["rename", 11],
	["realpath", 12],
	["symlink", 13],
	["read_link", 14],
	["link", 15],
	["chmod", 16],
	["chown", 17],
	["utimes", 18],
	["truncate", 19],
]);
const BARE_PERMISSION_MODE = createBareEnumCodec<SidecarPermissionMode>([
	["allow", 1],
	["ask", 2],
	["deny", 3],
]);
const BARE_ROOT_FILESYSTEM_ENTRY_KIND = createBareEnumCodec<
	RootFilesystemEntry["kind"]
>([
	["file", 1],
	["directory", 2],
	["symlink", 3],
]);
const BARE_ROOT_FILESYSTEM_MODE = createBareEnumCodec<
	NonNullable<WireRootFilesystemDescriptor["mode"]>
>([
	["ephemeral", 1],
	["read_only", 2],
]);
const BARE_ROOT_FILESYSTEM_ENTRY_ENCODING = createBareEnumCodec<
	RootFilesystemEntryEncoding
>([
	["utf8", 1],
	["base64", 2],
]);
const BARE_WASM_PERMISSION_TIER = createBareEnumCodec<WasmPermissionTier>([
	["full", 1],
	["read-write", 2],
	["read-only", 3],
	["isolated", 4],
]);
const BARE_STREAM_CHANNEL = createBareEnumCodec<
	Extract<EventFrame["payload"], { type: "process_output" }>["channel"]
>([
	["stdout", 1],
	["stderr", 2],
]);
const BARE_VM_LIFECYCLE_STATE = createBareEnumCodec<
	Extract<EventFrame["payload"], { type: "vm_lifecycle" }>["state"]
>([
	["creating", 1],
	["ready", 2],
	["disposing", 3],
	["disposed", 4],
	["failed", 5],
]);
const BARE_SIGNAL_DISPOSITION_ACTION = createBareEnumCodec<
	SidecarSignalHandlerRegistration["action"]
>([
	["default", 1],
	["ignore", 2],
	["user", 3],
]);
const BARE_PROCESS_SNAPSHOT_STATUS = createBareEnumCodec<
	SidecarProcessSnapshotEntry["status"]
>([
	["running", 1],
	["exited", 2],
]);

class BareWriter {
	private readonly chunks: Buffer[] = [];
	private length = 0;

	writeByte(value: number): void {
		const chunk = Buffer.from([value & 0xff]);
		this.chunks.push(chunk);
		this.length += chunk.length;
	}

	writeBool(value: boolean): void {
		this.writeByte(value ? 1 : 0);
	}

	writeI32(value: number): void {
		const chunk = Buffer.allocUnsafe(4);
		chunk.writeInt32LE(value, 0);
		this.push(chunk);
	}

	writeI64(value: number): void {
		const chunk = Buffer.allocUnsafe(8);
		chunk.writeBigInt64LE(BigInt(value), 0);
		this.push(chunk);
	}

	writeU16(value: number): void {
		const chunk = Buffer.allocUnsafe(2);
		chunk.writeUInt16LE(value, 0);
		this.push(chunk);
	}

	writeU32(value: number): void {
		const chunk = Buffer.allocUnsafe(4);
		chunk.writeUInt32LE(value, 0);
		this.push(chunk);
	}

	writeU64(value: number): void {
		const chunk = Buffer.allocUnsafe(8);
		chunk.writeBigUInt64LE(BigInt(assertInteger(value, "u64 value")), 0);
		this.push(chunk);
	}

	writeVarUint(value: number): void {
		let remaining = BigInt(assertInteger(value, "varuint value"));
		while (remaining >= 0x80n) {
			this.writeByte(Number((remaining & 0x7fn) | 0x80n));
			remaining >>= 7n;
		}
		this.writeByte(Number(remaining));
	}

	writeString(value: string): void {
		const encoded = Buffer.from(value, "utf8");
		this.writeVarUint(encoded.length);
		this.push(encoded);
	}

	writeOptional<T>(value: T | undefined, encoder: (value: T) => void): void {
		if (value === undefined) {
			this.writeBool(false);
			return;
		}
		this.writeBool(true);
		encoder(value);
	}

	writeList<T>(values: readonly T[], encoder: (value: T) => void): void {
		this.writeVarUint(values.length);
		for (const value of values) {
			encoder(value);
		}
	}

	writeMap<TKey, TValue>(
		entries: readonly (readonly [TKey, TValue])[],
		writeKey: (key: TKey) => void,
		writeValue: (value: TValue) => void,
	): void {
		this.writeVarUint(entries.length);
		for (const [key, value] of entries) {
			writeKey(key);
			writeValue(value);
		}
	}

	toBuffer(): Buffer {
		return Buffer.concat(this.chunks, this.length);
	}

	private push(chunk: Buffer): void {
		this.chunks.push(chunk);
		this.length += chunk.length;
	}
}

class BareReader {
	private offset = 0;

	constructor(private readonly bytes: Uint8Array) {}

	readByte(): number {
		this.ensureAvailable(1, "byte");
		return this.bytes[this.offset++]!;
	}

	readBool(): boolean {
		return this.readByte() !== 0;
	}

	readI32(): number {
		this.ensureAvailable(4, "i32");
		const value = Buffer.from(
			this.bytes.buffer,
			this.bytes.byteOffset + this.offset,
			4,
		).readInt32LE(0);
		this.offset += 4;
		return value;
	}

	readI64(context: string): number {
		this.ensureAvailable(8, "i64");
		const value = Buffer.from(
			this.bytes.buffer,
			this.bytes.byteOffset + this.offset,
			8,
		).readBigInt64LE(0);
		this.offset += 8;
		return bigIntToSafeNumber(value, context);
	}

	readU16(): number {
		this.ensureAvailable(2, "u16");
		const value = Buffer.from(
			this.bytes.buffer,
			this.bytes.byteOffset + this.offset,
			2,
		).readUInt16LE(0);
		this.offset += 2;
		return value;
	}

	readU32(): number {
		this.ensureAvailable(4, "u32");
		const value = Buffer.from(
			this.bytes.buffer,
			this.bytes.byteOffset + this.offset,
			4,
		).readUInt32LE(0);
		this.offset += 4;
		return value;
	}

	readU64(context: string): number {
		this.ensureAvailable(8, "u64");
		const value = Buffer.from(
			this.bytes.buffer,
			this.bytes.byteOffset + this.offset,
			8,
		).readBigUInt64LE(0);
		this.offset += 8;
		return bigIntToSafeNumber(value, context);
	}

	readVarUint(context: string): number {
		let result = 0n;
		let shift = 0n;
		for (let index = 0; index < 10; index += 1) {
			const byte = this.readByte();
			result |= BigInt(byte & 0x7f) << shift;
			if ((byte & 0x80) === 0) {
				return bigIntToSafeNumber(result, context);
			}
			shift += 7n;
		}
		throw new Error(`invalid ${context}: variable-length integer too long`);
	}

	readString(context: string): string {
		const length = this.readVarUint(`${context} length`);
		this.ensureAvailable(length, context);
		const value = Buffer.from(
			this.bytes.buffer,
			this.bytes.byteOffset + this.offset,
			length,
		).toString("utf8");
		this.offset += length;
		return value;
	}

	readOptional<T>(reader: () => T): T | undefined {
		return this.readBool() ? reader() : undefined;
	}

	readList<T>(reader: () => T, context: string): T[] {
		const length = this.readVarUint(`${context} length`);
		const values: T[] = [];
		for (let index = 0; index < length; index += 1) {
			values.push(reader());
		}
		return values;
	}

	readMap<TKey, TValue>(
		readKey: () => TKey,
		readValue: () => TValue,
		context: string,
	): Array<[TKey, TValue]> {
		const length = this.readVarUint(`${context} length`);
		const entries: Array<[TKey, TValue]> = [];
		for (let index = 0; index < length; index += 1) {
			entries.push([readKey(), readValue()]);
		}
		return entries;
	}

	ensureConsumed(context: string): void {
		if (this.offset !== this.bytes.length) {
			throw new Error(
				`invalid ${context}: trailing ${this.bytes.length - this.offset} byte(s)`,
			);
		}
	}

	private ensureAvailable(length: number, context: string): void {
		if (this.offset + length > this.bytes.length) {
			throw new Error(`invalid ${context}: unexpected end of frame`);
		}
	}
}

function assertInteger(value: number, context: string): number {
	if (!Number.isInteger(value)) {
		throw new Error(`expected integer ${context}, received ${value}`);
	}
	return value;
}

function bigIntToSafeNumber(value: bigint, context: string): number {
	const max = BigInt(Number.MAX_SAFE_INTEGER);
	const min = BigInt(Number.MIN_SAFE_INTEGER);
	if (value > max || value < min) {
		throw new Error(`${context} exceeds JavaScript safe integer range`);
	}
	return Number(value);
}

function stringifyJsonUtf8(value: unknown, context: string): string {
	try {
		const encoded = JSON.stringify(value);
		if (encoded === undefined) {
			throw new Error(`${context} must be JSON-serializable`);
		}
		return encoded;
	} catch (error) {
		throw new Error(
			`${context} must be JSON-serializable: ${
				error instanceof Error ? error.message : String(error)
			}`,
		);
	}
}

function parseJsonUtf8(value: string, context: string): unknown {
	try {
		return JSON.parse(value);
	} catch (error) {
		throw new Error(
			`invalid ${context} JSON payload: ${
				error instanceof Error ? error.message : String(error)
			}`,
		);
	}
}

function encodeBareProtocolFrame(frame: ProtocolFrame): Buffer {
	const writer = new BareWriter();
	switch (frame.frame_type) {
		case "request":
			writer.writeVarUint(1);
			encodeProtocolSchema(writer, frame.schema);
			writer.writeI64(frame.request_id);
			encodeOwnershipScope(writer, frame.ownership);
			encodeRequestPayload(writer, frame.payload);
			break;
		case "sidecar_response":
			writer.writeVarUint(5);
			encodeProtocolSchema(writer, frame.schema);
			writer.writeI64(frame.request_id);
			encodeOwnershipScope(writer, frame.ownership);
			encodeSidecarResponsePayload(writer, frame.payload);
			break;
		default:
			throw new Error(
				`BARE encoding is only implemented for host-written frames, received ${frame.frame_type}`,
			);
	}
	return writer.toBuffer();
}

function decodeBareProtocolFrame(
	payload: Uint8Array,
): ResponseFrame | EventFrame | SidecarRequestFrame {
	const reader = new BareReader(payload);
	const tag = reader.readVarUint("protocol frame tag");
	let frame: ResponseFrame | EventFrame | SidecarRequestFrame;
	switch (tag) {
		case 2:
			frame = {
				frame_type: "response",
				schema: decodeProtocolSchema(reader),
				request_id: reader.readI64("response request id"),
				ownership: decodeOwnershipScope(reader),
				payload: decodeResponsePayload(reader),
			};
			break;
		case 3:
			frame = {
				frame_type: "event",
				schema: decodeProtocolSchema(reader),
				ownership: decodeOwnershipScope(reader),
				payload: decodeEventPayload(reader),
			};
			break;
		case 4:
			frame = {
				frame_type: "sidecar_request",
				schema: decodeProtocolSchema(reader),
				request_id: reader.readI64("sidecar request id"),
				ownership: decodeOwnershipScope(reader),
				payload: decodeSidecarRequestPayload(reader),
			};
			break;
		default:
			throw new Error(`unsupported BARE protocol frame tag: ${tag}`);
	}
	reader.ensureConsumed("protocol frame");
	return frame;
}

function encodeProtocolSchema(
	writer: BareWriter,
	schema: typeof PROTOCOL_SCHEMA,
): void {
	writer.writeString(schema.name);
	writer.writeU16(schema.version);
}

function decodeProtocolSchema(reader: BareReader): typeof PROTOCOL_SCHEMA {
	return {
		name: reader.readString("protocol schema name"),
		version: reader.readU16(),
	} as typeof PROTOCOL_SCHEMA;
}

function encodeOwnershipScope(writer: BareWriter, ownership: OwnershipScope): void {
	switch (ownership.scope) {
		case "connection":
			writer.writeVarUint(1);
			writer.writeString(ownership.connection_id);
			return;
		case "session":
			writer.writeVarUint(2);
			writer.writeString(ownership.connection_id);
			writer.writeString(ownership.session_id);
			return;
		case "vm":
			writer.writeVarUint(3);
			writer.writeString(ownership.connection_id);
			writer.writeString(ownership.session_id);
			writer.writeString(ownership.vm_id);
			return;
	}
}

function decodeOwnershipScope(reader: BareReader): OwnershipScope {
	switch (reader.readVarUint("ownership scope tag")) {
		case 1:
			return {
				scope: "connection",
				connection_id: reader.readString("connection ownership id"),
			};
		case 2:
			return {
				scope: "session",
				connection_id: reader.readString("session ownership connection id"),
				session_id: reader.readString("session ownership session id"),
			};
		case 3:
			return {
				scope: "vm",
				connection_id: reader.readString("vm ownership connection id"),
				session_id: reader.readString("vm ownership session id"),
				vm_id: reader.readString("vm ownership vm id"),
			};
		default:
			throw new Error("unsupported ownership scope tag");
	}
}

function encodeRequestPayload(writer: BareWriter, payload: RequestPayload): void {
	switch (payload.type) {
		case "authenticate":
			writer.writeVarUint(1);
			writer.writeString(payload.client_name);
			writer.writeString(payload.auth_token);
			return;
		case "open_session":
			writer.writeVarUint(2);
			encodeSidecarPlacement(writer, payload.placement);
			writer.writeMap(
				Object.entries(payload.metadata ?? {}),
				(key) => writer.writeString(key),
				(value) => writer.writeString(value),
			);
			return;
		case "create_vm":
			writer.writeVarUint(3);
			writer.writeVarUint(
				BARE_GUEST_RUNTIME_KIND.encode(payload.runtime, "guest runtime"),
			);
			writer.writeMap(
				Object.entries(payload.metadata ?? {}),
				(key) => writer.writeString(key),
				(value) => writer.writeString(value),
			);
			encodeWireRootFilesystemDescriptor(writer, payload.root_filesystem);
			writer.writeOptional(payload.permissions, (permissions) =>
				encodeWirePermissionsPolicy(writer, permissions),
			);
			return;
		case "create_session":
			writer.writeVarUint(4);
			writer.writeString(payload.agent_type);
			writer.writeVarUint(
				BARE_GUEST_RUNTIME_KIND.encode(
					(payload.runtime ?? "java_script") as GuestRuntimeKind | "python",
					"create session runtime",
				),
			);
			writer.writeString(payload.adapter_entrypoint);
			writer.writeList(payload.args ?? [], (value) => writer.writeString(value));
			writer.writeMap(
				Object.entries(payload.env ?? {}),
				(key) => writer.writeString(key),
				(value) => writer.writeString(value),
			);
			writer.writeString(payload.cwd);
			writer.writeList(payload.mcp_servers ?? [], (value) =>
				writer.writeString(stringifyJsonUtf8(value, "create_session.mcp_servers")),
			);
			return;
		case "session_request":
			writer.writeVarUint(5);
			writer.writeString(payload.session_id);
			writer.writeString(payload.method);
			writer.writeOptional(payload.params, (value) =>
				writer.writeString(stringifyJsonUtf8(value, "session_request.params")),
			);
			return;
		case "get_session_state":
			writer.writeVarUint(6);
			writer.writeString(payload.session_id);
			return;
		case "close_agent_session":
			writer.writeVarUint(7);
			writer.writeString(payload.session_id);
			return;
		case "dispose_vm":
			writer.writeVarUint(8);
			writer.writeVarUint(
				BARE_DISPOSE_REASON.encode(payload.reason, "dispose reason"),
			);
			return;
		case "bootstrap_root_filesystem":
			writer.writeVarUint(9);
			writer.writeList(payload.entries, (entry) =>
				encodeRootFilesystemEntry(writer, entry),
			);
			return;
		case "configure_vm":
			writer.writeVarUint(10);
			writer.writeList(payload.mounts ?? [], (mount) =>
				encodeWireMountDescriptor(writer, mount),
			);
			writer.writeList(payload.software ?? [], (software) =>
				encodeWireSoftwareDescriptor(writer, software),
			);
			writer.writeOptional(payload.permissions, (permissions) =>
				encodeWirePermissionsPolicy(writer, permissions),
			);
			writer.writeOptional(payload.module_access_cwd, (value) =>
				writer.writeString(value),
			);
			writer.writeList(payload.instructions ?? [], (value) =>
				writer.writeString(value),
			);
			writer.writeList(payload.projected_modules ?? [], (descriptor) =>
				encodeWireProjectedModuleDescriptor(writer, descriptor),
			);
			writer.writeMap(
				Object.entries(payload.command_permissions ?? {}),
				(key) => writer.writeString(key),
				(value) =>
					writer.writeVarUint(
						BARE_WASM_PERMISSION_TIER.encode(value, "command permission"),
					),
			);
			writer.writeList(payload.allowed_node_builtins ?? [], (value) =>
				writer.writeString(value),
			);
			writer.writeList(payload.loopback_exempt_ports ?? [], (value) =>
				writer.writeU16(value),
			);
			return;
		case "register_toolkit":
			writer.writeVarUint(11);
			writer.writeString(payload.name);
			writer.writeString(payload.description);
			writer.writeMap(
				Object.entries(payload.tools),
				(key) => writer.writeString(key),
				(tool) => encodeRegisteredToolDefinition(writer, tool),
			);
			return;
		case "create_layer":
			writer.writeVarUint(12);
			return;
		case "seal_layer":
			writer.writeVarUint(13);
			writer.writeString(payload.layer_id);
			return;
		case "import_snapshot":
			writer.writeVarUint(14);
			writer.writeList(payload.entries, (entry) =>
				encodeRootFilesystemEntry(writer, entry),
			);
			return;
		case "export_snapshot":
			writer.writeVarUint(15);
			writer.writeString(payload.layer_id);
			return;
		case "create_overlay":
			writer.writeVarUint(16);
			writer.writeVarUint(
				BARE_ROOT_FILESYSTEM_MODE.encode(
					payload.mode ?? "ephemeral",
					"overlay mode",
				),
			);
			writer.writeOptional(payload.upper_layer_id, (value) =>
				writer.writeString(value),
			);
			writer.writeList(payload.lower_layer_ids ?? [], (value) =>
				writer.writeString(value),
			);
			return;
		case "guest_filesystem_call":
			writer.writeVarUint(17);
			writer.writeVarUint(
				BARE_GUEST_FILESYSTEM_OPERATION.encode(
					payload.operation,
					"guest filesystem operation",
				),
			);
			writer.writeString(payload.path);
			writer.writeOptional(payload.destination_path, (value) =>
				writer.writeString(value),
			);
			writer.writeOptional(payload.target, (value) => writer.writeString(value));
			writer.writeOptional(payload.content, (value) => writer.writeString(value));
			writer.writeOptional(payload.encoding, (value) =>
				writer.writeVarUint(
					BARE_ROOT_FILESYSTEM_ENTRY_ENCODING.encode(
						value,
						"root filesystem entry encoding",
					),
				),
			);
			writer.writeBool(payload.recursive ?? false);
			writer.writeOptional(payload.mode, (value) => writer.writeU32(value));
			writer.writeOptional(payload.uid, (value) => writer.writeU32(value));
			writer.writeOptional(payload.gid, (value) => writer.writeU32(value));
			writer.writeOptional(payload.atime_ms, (value) => writer.writeU64(value));
			writer.writeOptional(payload.mtime_ms, (value) => writer.writeU64(value));
			writer.writeOptional(payload.len, (value) => writer.writeU64(value));
			return;
		case "snapshot_root_filesystem":
			writer.writeVarUint(18);
			return;
		case "execute":
			writer.writeVarUint(19);
			writer.writeString(payload.process_id);
			writer.writeOptional(payload.command, (value) => writer.writeString(value));
			writer.writeOptional(payload.runtime, (value) =>
				writer.writeVarUint(
					BARE_GUEST_RUNTIME_KIND.encode(
						value as GuestRuntimeKind | "python",
						"execute runtime",
					),
				),
			);
			writer.writeOptional(payload.entrypoint, (value) =>
				writer.writeString(value),
			);
			writer.writeList(payload.args ?? [], (value) => writer.writeString(value));
			writer.writeMap(
				Object.entries(payload.env ?? {}),
				(key) => writer.writeString(key),
				(value) => writer.writeString(value),
			);
			writer.writeOptional(payload.cwd, (value) => writer.writeString(value));
			writer.writeOptional(payload.wasm_permission_tier, (value) =>
				writer.writeVarUint(
					BARE_WASM_PERMISSION_TIER.encode(value, "wasm permission tier"),
				),
			);
			return;
		case "write_stdin":
			writer.writeVarUint(20);
			writer.writeString(payload.process_id);
			writer.writeString(payload.chunk);
			return;
		case "close_stdin":
			writer.writeVarUint(21);
			writer.writeString(payload.process_id);
			return;
		case "kill_process":
			writer.writeVarUint(22);
			writer.writeString(payload.process_id);
			writer.writeString(payload.signal);
			return;
		case "get_process_snapshot":
			writer.writeVarUint(23);
			return;
		case "find_listener":
			writer.writeVarUint(24);
			writer.writeOptional(payload.host, (value) => writer.writeString(value));
			writer.writeOptional(payload.port, (value) => writer.writeU16(value));
			writer.writeOptional(payload.path, (value) => writer.writeString(value));
			return;
		case "find_bound_udp":
			writer.writeVarUint(25);
			writer.writeOptional(payload.host, (value) => writer.writeString(value));
			writer.writeOptional(payload.port, (value) => writer.writeU16(value));
			return;
		case "get_signal_state":
			writer.writeVarUint(26);
			writer.writeString(payload.process_id);
			return;
		case "get_zombie_timer_count":
			writer.writeVarUint(27);
			return;
	}
}

function encodeSidecarResponsePayload(
	writer: BareWriter,
	payload: SidecarResponsePayload,
): void {
	switch (payload.type) {
		case "tool_invocation_result":
			writer.writeVarUint(1);
			writer.writeString(payload.invocation_id);
			writer.writeOptional(payload.result, (value) =>
				writer.writeString(
					stringifyJsonUtf8(value, "tool_invocation_result.result"),
				),
			);
			writer.writeOptional(payload.error, (value) => writer.writeString(value));
			return;
		case "permission_request_result":
			writer.writeVarUint(2);
			writer.writeString(payload.permission_id);
			writer.writeOptional(payload.reply, (value) => writer.writeString(value));
			writer.writeOptional(payload.error, (value) => writer.writeString(value));
			return;
		case "js_bridge_result":
			writer.writeVarUint(3);
			writer.writeString(payload.call_id);
			writer.writeOptional(payload.result, (value) =>
				writer.writeString(stringifyJsonUtf8(value, "js_bridge_result.result")),
			);
			writer.writeOptional(payload.error, (value) => writer.writeString(value));
			return;
	}
}

function decodeResponsePayload(reader: BareReader): ResponseFrame["payload"] {
	switch (reader.readVarUint("response payload tag")) {
		case 1:
			return {
				type: "authenticated",
				sidecar_id: reader.readString("authenticated.sidecar_id"),
				connection_id: reader.readString("authenticated.connection_id"),
				max_frame_bytes: reader.readU32(),
			};
		case 2:
			return {
				type: "session_opened",
				session_id: reader.readString("session_opened.session_id"),
				owner_connection_id: reader.readString(
					"session_opened.owner_connection_id",
				),
			};
		case 3:
			return {
				type: "vm_created",
				vm_id: reader.readString("vm_created.vm_id"),
			};
		case 4: {
			const sessionId = reader.readString("session_created.session_id");
			const pid = reader.readOptional(() => reader.readU32());
			const modes = reader.readOptional(() =>
				parseJsonUtf8(reader.readString("session_created.modes"), "modes"),
			);
			const configOptions = reader.readList(
				() =>
					parseJsonUtf8(
						reader.readString("session_created.config_options"),
						"config options",
					),
				"session_created.config_options",
			);
			const agentCapabilities = reader.readOptional(() =>
				parseJsonUtf8(
					reader.readString("session_created.agent_capabilities"),
					"agent capabilities",
				),
			);
			const agentInfo = reader.readOptional(() =>
				parseJsonUtf8(
					reader.readString("session_created.agent_info"),
					"agent info",
				),
			);
			return {
				type: "session_created",
				session_id: sessionId,
				...(pid !== undefined ? { pid } : {}),
				...(modes !== undefined ? { modes } : {}),
				config_options: configOptions,
				...(agentCapabilities !== undefined
					? { agent_capabilities: agentCapabilities }
					: {}),
				...(agentInfo !== undefined ? { agent_info: agentInfo } : {}),
			};
		}
		case 5:
			return {
				type: "session_rpc",
				session_id: reader.readString("session_rpc.session_id"),
				response: parseJsonUtf8(
					reader.readString("session_rpc.response"),
					"session RPC response",
				),
			};
		case 6: {
			const sessionId = reader.readString("session_state.session_id");
			const agentType = reader.readString("session_state.agent_type");
			const processId = reader.readString("session_state.process_id");
			const pid = reader.readOptional(() => reader.readU32());
			const closed = reader.readBool();
			const modes = reader.readOptional(() =>
				parseJsonUtf8(reader.readString("session_state.modes"), "modes"),
			);
			const configOptions = reader.readList(
				() =>
					parseJsonUtf8(
						reader.readString("session_state.config_options"),
						"config options",
					),
				"session_state.config_options",
			);
			const agentCapabilities = reader.readOptional(() =>
				parseJsonUtf8(
					reader.readString("session_state.agent_capabilities"),
					"agent capabilities",
				),
			);
			const agentInfo = reader.readOptional(() =>
				parseJsonUtf8(
					reader.readString("session_state.agent_info"),
					"agent info",
				),
			);
			const events = reader.readList(
				() => ({
					sequence_number: reader.readU64("session_state.events.sequence_number"),
					notification: parseJsonUtf8(
						reader.readString("session_state.events.notification"),
						"session state notification",
					),
				}),
				"session_state.events",
			);
			return {
				type: "session_state",
				session_id: sessionId,
				agent_type: agentType,
				process_id: processId,
				...(pid !== undefined ? { pid } : {}),
				closed,
				...(modes !== undefined ? { modes } : {}),
				config_options: configOptions,
				...(agentCapabilities !== undefined
					? { agent_capabilities: agentCapabilities }
					: {}),
				...(agentInfo !== undefined ? { agent_info: agentInfo } : {}),
				events,
			};
		}
		case 7:
			return {
				type: "agent_session_closed",
				session_id: reader.readString("agent_session_closed.session_id"),
			};
		case 8:
			return {
				type: "vm_disposed",
				vm_id: reader.readString("vm_disposed.vm_id"),
			};
		case 9:
			return {
				type: "root_filesystem_bootstrapped",
				entry_count: reader.readU32(),
			};
		case 10:
			return {
				type: "vm_configured",
				applied_mounts: reader.readU32(),
				applied_software: reader.readU32(),
			};
		case 11:
			return {
				type: "toolkit_registered",
				toolkit: reader.readString("toolkit_registered.toolkit"),
				command_count: reader.readU32(),
				prompt_markdown: reader.readString("toolkit_registered.prompt_markdown"),
			};
		case 12:
			return {
				type: "layer_created",
				layer_id: reader.readString("layer_created.layer_id"),
			};
		case 13:
			return {
				type: "layer_sealed",
				layer_id: reader.readString("layer_sealed.layer_id"),
			};
		case 14:
			return {
				type: "snapshot_imported",
				layer_id: reader.readString("snapshot_imported.layer_id"),
			};
		case 15:
			return {
				type: "snapshot_exported",
				layer_id: reader.readString("snapshot_exported.layer_id"),
				entries: reader.readList(
					() => decodeRootFilesystemEntry(reader),
					"snapshot_exported.entries",
				),
			};
		case 16:
			return {
				type: "overlay_created",
				layer_id: reader.readString("overlay_created.layer_id"),
			};
		case 17: {
			const operation = BARE_GUEST_FILESYSTEM_OPERATION.decode(
				reader.readVarUint("guest_filesystem_result.operation"),
				"guest filesystem operation",
			);
			const path = reader.readString("guest_filesystem_result.path");
			const content = reader.readOptional(() =>
				reader.readString("guest_filesystem_result.content"),
			);
			const encoding = reader.readOptional(() =>
				BARE_ROOT_FILESYSTEM_ENTRY_ENCODING.decode(
					reader.readVarUint("guest_filesystem_result.encoding"),
					"root filesystem entry encoding",
				),
			);
			const entries = reader.readOptional(() =>
				reader.readList(
					() => reader.readString("guest_filesystem_result.entries"),
					"guest_filesystem_result.entries",
				),
			);
			const stat = reader.readOptional(() => decodeGuestFilesystemStat(reader));
			const exists = reader.readOptional(() => reader.readBool());
			const target = reader.readOptional(() =>
				reader.readString("guest_filesystem_result.target"),
			);
			return {
				type: "guest_filesystem_result",
				operation,
				path,
				...(content !== undefined ? { content } : {}),
				...(encoding !== undefined ? { encoding } : {}),
				...(entries !== undefined ? { entries } : {}),
				...(stat !== undefined ? { stat } : {}),
				...(exists !== undefined ? { exists } : {}),
				...(target !== undefined ? { target } : {}),
			};
		}
		case 18:
			return {
				type: "root_filesystem_snapshot",
				entries: reader.readList(
					() => decodeRootFilesystemEntry(reader),
					"root_filesystem_snapshot.entries",
				),
			};
		case 19: {
			const process_id = reader.readString("process_started.process_id");
			const pid = reader.readOptional(() => reader.readU32());
			return {
				type: "process_started",
				process_id,
				...(pid !== undefined ? { pid } : {}),
			};
		}
		case 20:
			return {
				type: "stdin_written",
				process_id: reader.readString("stdin_written.process_id"),
				accepted_bytes: reader.readU64("stdin_written.accepted_bytes"),
			};
		case 21:
			return {
				type: "stdin_closed",
				process_id: reader.readString("stdin_closed.process_id"),
			};
		case 22:
			return {
				type: "process_killed",
				process_id: reader.readString("process_killed.process_id"),
			};
		case 23:
			return {
				type: "process_snapshot",
				processes: reader.readList(
					() => decodeProcessSnapshotEntry(reader),
					"process_snapshot.processes",
				),
			};
		case 24: {
			const listener = reader.readOptional(() => decodeSocketStateEntry(reader));
			return {
				type: "listener_snapshot",
				...(listener !== undefined ? { listener } : {}),
			};
		}
		case 25: {
			const socket = reader.readOptional(() => decodeSocketStateEntry(reader));
			return {
				type: "bound_udp_snapshot",
				...(socket !== undefined ? { socket } : {}),
			};
		}
		case 26:
			return {
				type: "signal_state",
				process_id: reader.readString("signal_state.process_id"),
				handlers: Object.fromEntries(
					reader.readMap(
						() => String(reader.readU32()),
						() => decodeSignalHandlerRegistration(reader),
						"signal_state.handlers",
					),
				),
			};
		case 27:
			return {
				type: "zombie_timer_count",
				count: reader.readU64("zombie_timer_count.count"),
			};
		case 28:
			throw new Error(
				"unsupported bare response payload tag: filesystem_result",
			);
		case 29:
			throw new Error(
				"unsupported bare response payload tag: permission_decision",
			);
		case 30:
			throw new Error(
				"unsupported bare response payload tag: persistence_state",
			);
		case 31:
			throw new Error(
				"unsupported bare response payload tag: persistence_flushed",
			);
		case 32:
			return {
				type: "rejected",
				code: reader.readString("rejected.code"),
				message: reader.readString("rejected.message"),
			};
		default:
			throw new Error("unsupported response payload tag");
	}
}

function decodeEventPayload(reader: BareReader): EventFrame["payload"] {
	switch (reader.readVarUint("event payload tag")) {
		case 1:
			return {
				type: "vm_lifecycle",
				state: BARE_VM_LIFECYCLE_STATE.decode(
					reader.readVarUint("vm_lifecycle.state"),
					"vm lifecycle state",
				),
			};
		case 2:
			return {
				type: "process_output",
				process_id: reader.readString("process_output.process_id"),
				channel: BARE_STREAM_CHANNEL.decode(
					reader.readVarUint("process_output.channel"),
					"stream channel",
				),
				chunk: reader.readString("process_output.chunk"),
			};
		case 3:
			return {
				type: "process_exited",
				process_id: reader.readString("process_exited.process_id"),
				exit_code: reader.readI32(),
			};
		case 4:
			return {
				type: "structured",
				name: reader.readString("structured.name"),
				detail: Object.fromEntries(
					reader.readMap(
						() => reader.readString("structured.detail.key"),
						() => reader.readString("structured.detail.value"),
						"structured.detail",
					),
				),
			};
		default:
			throw new Error("unsupported event payload tag");
	}
}

function decodeSidecarRequestPayload(
	reader: BareReader,
): SidecarRequestFrame["payload"] {
	switch (reader.readVarUint("sidecar request payload tag")) {
		case 1:
			return {
				type: "tool_invocation",
				invocation_id: reader.readString("tool_invocation.invocation_id"),
				tool_key: reader.readString("tool_invocation.tool_key"),
				input: parseJsonUtf8(
					reader.readString("tool_invocation.input"),
					"tool invocation input",
				),
				timeout_ms: reader.readU64("tool_invocation.timeout_ms"),
			};
		case 2:
			return {
				type: "permission_request",
				session_id: reader.readString("permission_request.session_id"),
				permission_id: reader.readString("permission_request.permission_id"),
				params: parseJsonUtf8(
					reader.readString("permission_request.params"),
					"permission request params",
				),
			};
		case 3:
			return {
				type: "js_bridge_call",
				call_id: reader.readString("js_bridge_call.call_id"),
				mount_id: reader.readString("js_bridge_call.mount_id"),
				operation: reader.readString("js_bridge_call.operation"),
				args: parseJsonUtf8(
					reader.readString("js_bridge_call.args"),
					"js bridge call args",
				),
			};
		default:
			throw new Error("unsupported sidecar request payload tag");
	}
}

function encodeSidecarPlacement(
	writer: BareWriter,
	placement: SidecarPlacement,
): void {
	switch (placement.kind) {
		case "shared":
			writer.writeVarUint(1);
			writer.writeOptional(placement.pool ?? undefined, (value) =>
				writer.writeString(value),
			);
			return;
		case "explicit":
			writer.writeVarUint(2);
			writer.writeString(placement.sidecar_id);
			return;
	}
}

function encodeWireRootFilesystemDescriptor(
	writer: BareWriter,
	descriptor: WireRootFilesystemDescriptor | undefined,
): void {
	writer.writeVarUint(
		BARE_ROOT_FILESYSTEM_MODE.encode(
			descriptor?.mode ?? "ephemeral",
			"root filesystem mode",
		),
	);
	writer.writeBool(descriptor?.disable_default_base_layer ?? false);
	writer.writeList(descriptor?.lowers ?? [], (lower) =>
		encodeWireRootFilesystemLowerDescriptor(writer, lower),
	);
	writer.writeList(descriptor?.bootstrap_entries ?? [], (entry) =>
		encodeRootFilesystemEntry(writer, entry),
	);
}

function encodeWireRootFilesystemLowerDescriptor(
	writer: BareWriter,
	lower: WireRootFilesystemLowerDescriptor,
): void {
	if (lower.kind === "snapshot") {
		writer.writeVarUint(1);
		writer.writeList(lower.entries ?? [], (entry) =>
			encodeRootFilesystemEntry(writer, entry),
		);
		return;
	}
	writer.writeVarUint(2);
	writer.writeBool(false);
}

function encodeRootFilesystemEntry(
	writer: BareWriter,
	entry: RootFilesystemEntry,
): void {
	writer.writeString(entry.path);
	writer.writeVarUint(
		BARE_ROOT_FILESYSTEM_ENTRY_KIND.encode(
			entry.kind,
			"root filesystem entry kind",
		),
	);
	writer.writeOptional(entry.mode, (value) => writer.writeU32(value));
	writer.writeOptional(entry.uid, (value) => writer.writeU32(value));
	writer.writeOptional(entry.gid, (value) => writer.writeU32(value));
	writer.writeOptional(entry.content, (value) => writer.writeString(value));
	writer.writeOptional(entry.encoding, (value) =>
		writer.writeVarUint(
			BARE_ROOT_FILESYSTEM_ENTRY_ENCODING.encode(
				value,
				"root filesystem entry encoding",
			),
		),
	);
	writer.writeOptional(entry.target, (value) => writer.writeString(value));
	writer.writeBool(entry.executable ?? false);
}

function decodeRootFilesystemEntry(reader: BareReader): RootFilesystemEntry {
	const path = reader.readString("root filesystem entry path");
	const kind = BARE_ROOT_FILESYSTEM_ENTRY_KIND.decode(
		reader.readVarUint("root filesystem entry kind"),
		"root filesystem entry kind",
	);
	const mode = reader.readOptional(() => reader.readU32());
	const uid = reader.readOptional(() => reader.readU32());
	const gid = reader.readOptional(() => reader.readU32());
	const content = reader.readOptional(() =>
		reader.readString("root filesystem entry content"),
	);
	const encoding = reader.readOptional(() =>
		BARE_ROOT_FILESYSTEM_ENTRY_ENCODING.decode(
			reader.readVarUint("root filesystem entry encoding"),
			"root filesystem entry encoding",
		),
	);
	const target = reader.readOptional(() =>
		reader.readString("root filesystem entry target"),
	);
	const executable = reader.readBool();
	return {
		path,
		kind,
		...(mode !== undefined ? { mode } : {}),
		...(uid !== undefined ? { uid } : {}),
		...(gid !== undefined ? { gid } : {}),
		...(content !== undefined ? { content } : {}),
		...(encoding !== undefined ? { encoding } : {}),
		...(target !== undefined ? { target } : {}),
		executable,
	};
}

function encodeWireMountDescriptor(
	writer: BareWriter,
	descriptor: WireMountDescriptor,
): void {
	writer.writeString(descriptor.guest_path);
	writer.writeBool(descriptor.read_only);
	writer.writeString(descriptor.plugin.id);
	writer.writeString(
		stringifyJsonUtf8(descriptor.plugin.config ?? {}, "mount plugin config"),
	);
}

function encodeWireSoftwareDescriptor(
	writer: BareWriter,
	descriptor: WireSoftwareDescriptor,
): void {
	writer.writeString(descriptor.package_name);
	writer.writeString(descriptor.root);
}

function encodeWireProjectedModuleDescriptor(
	writer: BareWriter,
	descriptor: WireProjectedModuleDescriptor,
): void {
	writer.writeString(descriptor.package_name);
	writer.writeString(descriptor.entrypoint);
}

function encodeWirePermissionsPolicy(
	writer: BareWriter,
	policy: WirePermissionsPolicy,
): void {
	writer.writeOptional(policy.fs, (value) =>
		encodeFilesystemPermissionScope(writer, value),
	);
	writer.writeOptional(policy.network, (value) =>
		encodePatternPermissionScope(writer, value),
	);
	writer.writeOptional(policy.child_process, (value) =>
		encodePatternPermissionScope(writer, value),
	);
	writer.writeOptional(policy.env, (value) =>
		encodePatternPermissionScope(writer, value),
	);
}

function encodeFilesystemPermissionScope(
	writer: BareWriter,
	scope: SidecarPermissionScope<SidecarFsPermissionRule>,
): void {
	if (typeof scope === "string") {
		writer.writeVarUint(1);
		writer.writeVarUint(BARE_PERMISSION_MODE.encode(scope, "permission mode"));
		return;
	}
	writer.writeVarUint(2);
	writer.writeOptional(scope.default, (value) =>
		writer.writeVarUint(BARE_PERMISSION_MODE.encode(value, "permission mode")),
	);
	writer.writeList(scope.rules, (rule) => {
		writer.writeVarUint(BARE_PERMISSION_MODE.encode(rule.mode, "permission mode"));
		writer.writeList(rule.operations ?? [], (value) => writer.writeString(value));
		writer.writeList(rule.paths ?? [], (value) => writer.writeString(value));
	});
}

function encodePatternPermissionScope(
	writer: BareWriter,
	scope: SidecarPermissionScope<SidecarPatternPermissionRule>,
): void {
	if (typeof scope === "string") {
		writer.writeVarUint(1);
		writer.writeVarUint(BARE_PERMISSION_MODE.encode(scope, "permission mode"));
		return;
	}
	writer.writeVarUint(2);
	writer.writeOptional(scope.default, (value) =>
		writer.writeVarUint(BARE_PERMISSION_MODE.encode(value, "permission mode")),
	);
	writer.writeList(scope.rules, (rule) => {
		writer.writeVarUint(BARE_PERMISSION_MODE.encode(rule.mode, "permission mode"));
		writer.writeList(rule.operations ?? [], (value) => writer.writeString(value));
		writer.writeList(rule.patterns ?? [], (value) => writer.writeString(value));
	});
}

function encodeRegisteredToolDefinition(
	writer: BareWriter,
	tool: {
		description: string;
		input_schema: unknown;
		timeout_ms?: number;
		examples?: Array<{ description: string; input: unknown }>;
	},
): void {
	writer.writeString(tool.description);
	writer.writeString(
		stringifyJsonUtf8(tool.input_schema, "registered tool input schema"),
	);
	writer.writeOptional(tool.timeout_ms, (value) => writer.writeU64(value));
	writer.writeList(tool.examples ?? [], (example) => {
		writer.writeString(example.description);
		writer.writeString(
			stringifyJsonUtf8(example.input, "registered tool example input"),
		);
	});
}

function decodeGuestFilesystemStat(reader: BareReader): GuestFilesystemStat {
	return {
		mode: reader.readU32(),
		size: reader.readU64("guest filesystem stat.size"),
		blocks: reader.readU64("guest filesystem stat.blocks"),
		dev: reader.readU64("guest filesystem stat.dev"),
		rdev: reader.readU64("guest filesystem stat.rdev"),
		is_directory: reader.readBool(),
		is_symbolic_link: reader.readBool(),
		atime_ms: reader.readU64("guest filesystem stat.atime_ms"),
		mtime_ms: reader.readU64("guest filesystem stat.mtime_ms"),
		ctime_ms: reader.readU64("guest filesystem stat.ctime_ms"),
		birthtime_ms: reader.readU64("guest filesystem stat.birthtime_ms"),
		ino: reader.readU64("guest filesystem stat.ino"),
		nlink: reader.readU64("guest filesystem stat.nlink"),
		uid: reader.readU32(),
		gid: reader.readU32(),
	};
}

function decodeProcessSnapshotEntry(
	reader: BareReader,
): Extract<ResponseFrame["payload"], { type: "process_snapshot" }>["processes"][number] {
	const process_id = reader.readString("process_snapshot.process_id");
	const pid = reader.readU32();
	const ppid = reader.readU32();
	const pgid = reader.readU32();
	const sid = reader.readU32();
	const driver = reader.readString("process_snapshot.driver");
	const command = reader.readString("process_snapshot.command");
	const args = reader.readList(
		() => reader.readString("process_snapshot.args"),
		"process_snapshot.args",
	);
	const cwd = reader.readString("process_snapshot.cwd");
	const status = BARE_PROCESS_SNAPSHOT_STATUS.decode(
		reader.readVarUint("process_snapshot.status"),
		"process snapshot status",
	);
	const exit_code = reader.readOptional(() => reader.readI32());
	return {
		process_id,
		pid,
		ppid,
		pgid,
		sid,
		driver,
		command,
		...(args.length > 0 ? { args } : {}),
		cwd,
		status,
		...(exit_code !== undefined ? { exit_code } : {}),
	};
}

function decodeSocketStateEntry(
	reader: BareReader,
): { process_id: string; host?: string; port?: number; path?: string } {
	const process_id = reader.readString("socket_state.process_id");
	const host = reader.readOptional(() => reader.readString("socket_state.host"));
	const port = reader.readOptional(() => reader.readU16());
	const path = reader.readOptional(() => reader.readString("socket_state.path"));
	return {
		process_id,
		...(host !== undefined ? { host } : {}),
		...(port !== undefined ? { port } : {}),
		...(path !== undefined ? { path } : {}),
	};
}

function decodeSignalHandlerRegistration(
	reader: BareReader,
): {
	action: SidecarSignalHandlerRegistration["action"];
	mask: number[];
	flags: number;
} {
	return {
		action: BARE_SIGNAL_DISPOSITION_ACTION.decode(
			reader.readVarUint("signal handler action"),
			"signal disposition action",
		),
		mask: reader.readList(() => reader.readU32(), "signal handler mask"),
		flags: reader.readU32(),
	};
}

function encodeGuestFilesystemContent(content: string | Uint8Array): {
	content: string;
	encoding?: RootFilesystemEntryEncoding;
} {
	if (typeof content === "string") {
		return { content };
	}

	return {
		content: Buffer.from(content).toString("base64"),
		encoding: "base64",
	};
}

function decodeGuestFilesystemContent(
	response: Extract<
		ResponseFrame["payload"],
		{ type: "guest_filesystem_result" }
	>,
): Uint8Array {
	if (response.content === undefined) {
		throw new Error(`sidecar returned no file content for ${response.path}`);
	}

	if (response.encoding === "base64") {
		return Buffer.from(response.content, "base64");
	}

	return Buffer.from(response.content, "utf8");
}

function isMatchingSidecarResponsePayload(
	request: SidecarRequestPayload,
	response: SidecarResponsePayload,
): boolean {
	switch (request.type) {
		case "tool_invocation":
			return response.type === "tool_invocation_result";
		case "permission_request":
			return response.type === "permission_request_result";
		case "js_bridge_call":
			return response.type === "js_bridge_result";
	}
}

function errorSidecarResponsePayload(
	request: SidecarRequestPayload,
	error: unknown,
): SidecarResponsePayload {
	const message = error instanceof Error ? error.message : String(error);
	switch (request.type) {
		case "tool_invocation":
			return {
				type: "tool_invocation_result",
				invocation_id: request.invocation_id,
				error: message,
			};
		case "permission_request":
			return {
				type: "permission_request_result",
				permission_id: request.permission_id,
				error: message,
			};
		case "js_bridge_call":
			return {
				type: "js_bridge_result",
				call_id: request.call_id,
				error: message,
			};
	}
}

function toSidecarSocketStateEntry(entry: {
	process_id: string;
	host?: string;
	port?: number;
	path?: string;
}): SidecarSocketStateEntry {
	return {
		processId: entry.process_id,
		...(entry.host !== undefined ? { host: entry.host } : {}),
		...(entry.port !== undefined ? { port: entry.port } : {}),
		...(entry.path !== undefined ? { path: entry.path } : {}),
	};
}

function toSidecarProcessSnapshotEntry(entry: {
	process_id: string;
	pid: number;
	ppid: number;
	pgid: number;
	sid: number;
	driver: string;
	command: string;
	args?: string[];
	cwd: string;
	status: "running" | "exited";
	exit_code?: number;
}): SidecarProcessSnapshotEntry {
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
	read_only: boolean;
	plugin: {
		id: string;
		config: Record<string, unknown>;
	};
} {
	return {
		guest_path: descriptor.guestPath,
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
		env: policy.env,
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

function toJsonRpcRecord(
	value: unknown,
): JsonRpcResponse | Record<string, unknown> {
	if (value && typeof value === "object" && !Array.isArray(value)) {
		return value as JsonRpcResponse | Record<string, unknown>;
	}
	throw new Error("sidecar returned invalid JSON-RPC payload");
}

function toJsonRpcNotification(value: unknown): JsonRpcNotification {
	const notification = toJsonRpcRecord(value);
	if (
		notification.jsonrpc !== "2.0" ||
		!("method" in notification) ||
		typeof notification.method !== "string"
	) {
		throw new Error("sidecar returned invalid JSON-RPC notification");
	}
	return notification as unknown as JsonRpcNotification;
}

function toJsonRpcResponse(value: unknown): JsonRpcResponse {
	const response = toJsonRpcRecord(value);
	if (
		response.jsonrpc !== "2.0" ||
		!("id" in response) ||
		(typeof response.id !== "number" &&
			typeof response.id !== "string" &&
			response.id !== null)
	) {
		throw new Error("sidecar returned invalid JSON-RPC response");
	}
	return response as JsonRpcResponse;
}
