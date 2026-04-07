import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";

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

export type SidecarRequestHandler = (
	request: SidecarRequestFrame,
) => Promise<SidecarResponsePayload> | SidecarResponsePayload;

export interface NativeSidecarSpawnOptions {
	cwd: string;
	command?: string;
	args?: string[];
	frameTimeoutMs?: number;
}

export interface AuthenticatedSession {
	connectionId: string;
	sessionId: string;
}

export interface CreatedVm {
	vmId: string;
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
	private readonly stderrChunks: Buffer[] = [];
	private readonly frameTimeoutMs: number;
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
	) {
		this.child = child;
		this.frameTimeoutMs = frameTimeoutMs;
		this.child.stderr.on("data", (chunk: Buffer | string) => {
			this.stderrChunks.push(
				typeof chunk === "string" ? Buffer.from(chunk) : Buffer.from(chunk),
			);
		});
		this.child.stdout.on("data", (chunk: Buffer | string) => {
			this.stdoutBuffer = Buffer.concat([
				this.stdoutBuffer,
				typeof chunk === "string" ? Buffer.from(chunk) : Buffer.from(chunk),
			]);
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
		);
	}

	setSidecarRequestHandler(handler: SidecarRequestHandler | null): void {
		this.sidecarRequestHandler = handler;
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
	): Promise<{ toolkit: string; commandCount: number; promptMarkdown: string }> {
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
		const response = await new Promise<ResponseFrame>(
			async (resolve, reject) => {
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

				try {
					await this.writeFrame(request);
				} catch (error) {
					entry.reject(
						error instanceof Error ? error : new Error(String(error)),
					);
				}
			},
		);

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
		const payload = Buffer.from(JSON.stringify(frame), "utf8");
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
		return JSON.parse(payload.toString("utf8")) as
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
