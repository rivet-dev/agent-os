import { toExactArrayBuffer } from "./bytes.js";
import {
	type LiveMountDescriptor,
	type LivePackageDescriptor,
	type LiveProjectedModuleDescriptor,
	type LiveSidecarPlacement,
	type LiveSoftwareDescriptor,
	toGeneratedMountDescriptor,
	toGeneratedPackageDescriptor,
	toGeneratedProjectedModuleDescriptor,
	toGeneratedSidecarPlacement,
	toGeneratedSoftwareDescriptor,
} from "./descriptors.js";
import { type LiveExtEnvelope, toGeneratedExtEnvelope } from "./ext.js";
import {
	type LiveRootFilesystemEntry,
	type LiveRootFilesystemEntryEncoding,
	toGeneratedRootFilesystemEntry,
} from "./filesystem.js";
import type { CreateVmConfig } from "./generated/CreateVmConfig.js";
import type * as protocol from "./generated-protocol.js";
import { stringifyJsonUtf8 } from "./json.js";
import {
	type LivePermissionsPolicy,
	toGeneratedPermissionsPolicy,
} from "./permissions.js";
import {
	type LiveDisposeReason,
	type LiveFilesystemOperation,
	type LiveGuestFilesystemOperation,
	type LiveGuestRuntimeKind,
	type LiveRootFilesystemMode,
	type LiveWasmPermissionTier,
	type LiveStandaloneWasmBackend,
	toGeneratedDisposeReason,
	toGeneratedFilesystemOperation,
	toGeneratedGuestFilesystemOperation,
	toGeneratedGuestRuntimeKind,
	toGeneratedRootFilesystemEntryEncoding,
	toGeneratedRootFilesystemMode,
	toGeneratedWasmPermissionTier,
	toGeneratedStandaloneWasmBackend,
} from "./protocol-maps.js";

export interface LiveRegisteredHostCallbackExample {
	description: string;
	input: unknown;
}

export interface LiveRegisteredHostCallbackDefinition {
	description: string;
	input_schema: unknown;
	timeout_ms?: number;
	examples?: LiveRegisteredHostCallbackExample[];
}

export type LiveRequestPayload =
	| {
			type: "authenticate";
			client_name: string;
			auth_token: string;
			protocol_version: number;
			bridge_version: number;
	  }
	| {
			type: "open_session";
			placement: LiveSidecarPlacement;
			metadata: Record<string, string>;
	  }
	| {
			type: "create_vm";
			runtime: LiveGuestRuntimeKind;
			config: CreateVmConfig;
	  }
	| {
			type: "configure_vm";
			mounts: LiveMountDescriptor[];
			software: LiveSoftwareDescriptor[];
			permissions?: LivePermissionsPolicy;
			module_access_cwd?: string;
			instructions: string[];
			projected_modules: LiveProjectedModuleDescriptor[];
			command_permissions: Record<string, LiveWasmPermissionTier>;
			loopback_exempt_ports?: number[];
			packages?: LivePackageDescriptor[];
			packages_mount_at?: string;
			bootstrap_commands?: string[];
			binding_shim_commands?: string[];
	  }
	| {
			type: "link_package";
			package: LivePackageDescriptor;
	  }
	| {
			type: "provided_commands";
	  }
	| {
			type: "register_host_callbacks";
			name: string;
			description: string;
			command_aliases?: string[];
			registry_command_aliases?: string[];
			callbacks: Record<string, LiveRegisteredHostCallbackDefinition>;
	  }
	| {
			type: "dispose_vm";
			reason: LiveDisposeReason;
	  }
	| {
			type: "bootstrap_root_filesystem";
			entries: LiveRootFilesystemEntry[];
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
			entries: LiveRootFilesystemEntry[];
	  }
	| {
			type: "export_snapshot";
			layer_id: string;
	  }
	| {
			type: "create_overlay";
			mode?: LiveRootFilesystemMode;
			upper_layer_id?: string;
			lower_layer_ids: string[];
	  }
	| {
			type: "snapshot_root_filesystem";
			max_bytes: number;
	  }
	| {
			type: "list_mounts";
	  }
	| {
			type: "guest_kernel_call";
			execution_id: string;
			operation: string;
			payload: ArrayBuffer;
	  }
	| {
			type: "guest_filesystem_call";
			operation: LiveGuestFilesystemOperation;
			path: string;
			destination_path?: string;
			target?: string;
			content?: string;
			encoding?: LiveRootFilesystemEntryEncoding;
			recursive?: boolean;
			max_depth?: number;
			mode?: number;
			uid?: number;
			gid?: number;
			atime_ms?: number;
			mtime_ms?: number;
			len?: number;
			offset?: number;
	  }
	| {
			type: "execute";
			process_id: string;
			command?: string;
			runtime?: LiveGuestRuntimeKind;
			entrypoint?: string;
			args: string[];
			env?: Record<string, string>;
			cwd?: string;
			wasm_permission_tier?: LiveWasmPermissionTier;
			wasm_backend?: LiveStandaloneWasmBackend;
	  }
	| {
			type: "write_stdin";
			process_id: string;
			chunk: Uint8Array;
	  }
	| {
			type: "resize_pty";
			process_id: string;
			cols: number;
			rows: number;
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
			type: "get_resource_snapshot";
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
			type: "vm_fetch";
			port: number;
			method: string;
			path: string;
			headers_json: string;
			body?: string;
			body_base64?: string;
			stream_operation?: "start" | "read" | "cancel";
			stream_id?: string;
			max_bytes?: number;
	  }
	| {
			type: "get_signal_state";
			process_id: string;
	  }
	| {
			type: "get_zombie_timer_count";
	  }
	| {
			type: "host_filesystem_call";
			operation: LiveFilesystemOperation;
			path: string;
			payload_size_bytes: number;
	  }
	| {
			type: "persistence_load";
			key: string;
	  }
	| {
			type: "persistence_flush";
			key: string;
			payload_size_bytes: number;
	  }
	| { type: "shell_execution"; request: protocol.ShellExecutionRequest }
	| { type: "argv_execution"; request: protocol.ArgvExecutionRequest }
	| {
			type: "javascript_execution";
			request: protocol.JavaScriptExecutionRequest;
	  }
	| {
			type: "javascript_evaluation";
			request: protocol.JavaScriptEvaluationRequest;
	  }
	| {
			type: "javascript_file_execution";
			request: protocol.JavaScriptFileExecutionRequest;
	  }
	| {
			type: "typescript_execution";
			request: protocol.TypeScriptExecutionRequest;
	  }
	| {
			type: "typescript_evaluation";
			request: protocol.TypeScriptEvaluationRequest;
	  }
	| {
			type: "typescript_file_execution";
			request: protocol.TypeScriptFileExecutionRequest;
	  }
	| { type: "typescript_check"; request: protocol.TypeScriptCheckRequest }
	| {
			type: "typescript_project_check";
			request: protocol.TypeScriptProjectCheckRequest;
	  }
	| { type: "npm_project_install"; request: protocol.NpmProjectInstallRequest }
	| { type: "npm_package_install"; request: protocol.NpmPackageInstallRequest }
	| {
			type: "npm_script_execution";
			request: protocol.NpmScriptExecutionRequest;
	  }
	| {
			type: "npm_package_execution";
			request: protocol.NpmPackageExecutionRequest;
	  }
	| { type: "python_execution"; request: protocol.PythonExecutionRequest }
	| { type: "python_evaluation"; request: protocol.PythonEvaluationRequest }
	| {
			type: "python_file_execution";
			request: protocol.PythonFileExecutionRequest;
	  }
	| {
			type: "python_module_execution";
			request: protocol.PythonModuleExecutionRequest;
	  }
	| { type: "python_install"; request: protocol.PythonInstallRequest }
	| { type: "create_context"; request: protocol.CreateContextRequest }
	| { type: "get_execution"; request: protocol.GetExecutionRequest }
	| { type: "list_executions" }
	| { type: "wait_execution"; request: protocol.WaitExecutionRequest }
	| { type: "cancel_execution"; request: protocol.CancelExecutionRequest }
	| { type: "signal_execution"; request: protocol.SignalExecutionRequest }
	| { type: "reset_execution"; request: protocol.ResetExecutionRequest }
	| { type: "delete_execution"; request: protocol.DeleteExecutionRequest }
	| {
			type: "write_execution_stdin";
			request: protocol.WriteExecutionStdinRequest;
	  }
	| {
			type: "close_execution_stdin";
			request: protocol.CloseExecutionStdinRequest;
	  }
	| {
			type: "resize_execution_pty";
			request: protocol.ResizeExecutionPtyRequest;
	  }
	| {
			type: "read_execution_output";
			request: protocol.ReadExecutionOutputRequest;
	  }
	| {
			type: "ext";
			envelope: LiveExtEnvelope;
	  };

export function toGeneratedRequestPayload(
	payload: LiveRequestPayload,
): protocol.RequestPayload {
	switch (payload.type) {
		case "authenticate":
			return {
				tag: "AuthenticateRequest",
				val: {
					clientName: payload.client_name,
					authToken: payload.auth_token,
					protocolVersion: payload.protocol_version,
					bridgeVersion: payload.bridge_version,
				},
			};
		case "open_session":
			return {
				tag: "OpenSessionRequest",
				val: {
					placement: toGeneratedSidecarPlacement(payload.placement),
					metadata: new Map(Object.entries(payload.metadata ?? {})),
				},
			};
		case "create_vm":
			return {
				tag: "CreateVmRequest",
				val: {
					runtime: toGeneratedGuestRuntimeKind(payload.runtime),
					config: stringifyJsonUtf8(payload.config, "create VM config"),
				},
			};
		case "dispose_vm":
			return {
				tag: "DisposeVmRequest",
				val: { reason: toGeneratedDisposeReason(payload.reason) },
			};
		case "bootstrap_root_filesystem":
			return {
				tag: "BootstrapRootFilesystemRequest",
				val: { entries: payload.entries.map(toGeneratedRootFilesystemEntry) },
			};
		case "configure_vm":
			return {
				tag: "ConfigureVmRequest",
				val: {
					mounts: (payload.mounts ?? []).map(toGeneratedMountDescriptor),
					software: (payload.software ?? []).map(toGeneratedSoftwareDescriptor),
					permissions: toGeneratedPermissionsPolicy(payload.permissions),
					moduleAccessCwd: payload.module_access_cwd ?? null,
					instructions: payload.instructions ?? [],
					projectedModules: (payload.projected_modules ?? []).map(
						toGeneratedProjectedModuleDescriptor,
					),
					commandPermissions: new Map(
						Object.entries(payload.command_permissions ?? {}).map(
							([name, tier]) => [name, toGeneratedWasmPermissionTier(tier)],
						),
					),
					loopbackExemptPorts: new Uint16Array(
						payload.loopback_exempt_ports ?? [],
					),
					packages: (payload.packages ?? []).map(toGeneratedPackageDescriptor),
					packagesMountAt: payload.packages_mount_at ?? "",
					bootstrapCommands: payload.bootstrap_commands ?? [],
					bindingShimCommands: payload.binding_shim_commands ?? [],
				},
			};
		case "link_package":
			return {
				tag: "LinkPackageRequest",
				val: {
					package: toGeneratedPackageDescriptor(payload.package),
				},
			};
		case "provided_commands":
			return {
				tag: "ProvidedCommandsRequest",
				val: null,
			};
		case "register_host_callbacks":
			return {
				tag: "RegisterHostCallbacksRequest",
				val: {
					name: payload.name,
					description: payload.description,
					commandAliases: payload.command_aliases ?? [],
					registryCommandAliases: payload.registry_command_aliases ?? [],
					callbacks: new Map(
						Object.entries(payload.callbacks).map(([name, callback]) => [
							name,
							{
								description: callback.description,
								inputSchema: stringifyJsonUtf8(
									callback.input_schema,
									"register_host_callbacks.callback.input_schema",
								),
								timeoutMs:
									callback.timeout_ms === undefined
										? null
										: BigInt(callback.timeout_ms),
								examples: (callback.examples ?? []).map((example) => ({
									description: example.description,
									input: stringifyJsonUtf8(
										example.input,
										"register_host_callbacks.callback.example.input",
									),
								})),
							},
						]),
					),
				},
			};
		case "create_layer":
			return { tag: "CreateLayerRequest", val: null };
		case "seal_layer":
			return { tag: "SealLayerRequest", val: { layerId: payload.layer_id } };
		case "import_snapshot":
			return {
				tag: "ImportSnapshotRequest",
				val: { entries: payload.entries.map(toGeneratedRootFilesystemEntry) },
			};
		case "export_snapshot":
			return {
				tag: "ExportSnapshotRequest",
				val: { layerId: payload.layer_id },
			};
		case "create_overlay":
			return {
				tag: "CreateOverlayRequest",
				val: {
					mode: toGeneratedRootFilesystemMode(payload.mode ?? "ephemeral"),
					upperLayerId: payload.upper_layer_id ?? null,
					lowerLayerIds: payload.lower_layer_ids ?? [],
				},
			};
		case "guest_filesystem_call":
			return {
				tag: "GuestFilesystemCallRequest",
				val: {
					operation: toGeneratedGuestFilesystemOperation(payload.operation),
					path: payload.path,
					destinationPath: payload.destination_path ?? null,
					target: payload.target ?? null,
					content: payload.content ?? null,
					encoding:
						payload.encoding === undefined
							? null
							: toGeneratedRootFilesystemEntryEncoding(payload.encoding),
					recursive: payload.recursive ?? false,
					maxDepth: payload.max_depth ?? null,
					mode: payload.mode ?? null,
					uid: payload.uid ?? null,
					gid: payload.gid ?? null,
					atimeMs: toGeneratedOptionalU64(payload.atime_ms),
					mtimeMs: toGeneratedOptionalU64(payload.mtime_ms),
					len: toGeneratedOptionalU64(payload.len),
					offset: toGeneratedOptionalU64(payload.offset),
				},
			};
		case "guest_kernel_call":
			return {
				tag: "GuestKernelCallRequest",
				val: {
					executionId: payload.execution_id,
					operation: payload.operation,
					payload: payload.payload,
				},
			};
		case "snapshot_root_filesystem":
			return {
				tag: "SnapshotRootFilesystemRequest",
				val: { maxBytes: BigInt(payload.max_bytes) },
			};
		case "list_mounts":
			return { tag: "ListMountsRequest", val: null };
		case "execute":
			return {
				tag: "ExecuteRequest",
				val: {
					processId: payload.process_id,
					command: payload.command ?? null,
					runtime:
						payload.runtime === undefined
							? null
							: toGeneratedGuestRuntimeKind(payload.runtime),
					entrypoint: payload.entrypoint ?? null,
					args: payload.args ?? [],
					env: new Map(Object.entries(payload.env ?? {})),
					cwd: payload.cwd ?? null,
					wasmPermissionTier:
						payload.wasm_permission_tier === undefined
							? null
							: toGeneratedWasmPermissionTier(payload.wasm_permission_tier),
					wasmBackend:
						payload.wasm_backend === undefined
							? null
							: toGeneratedStandaloneWasmBackend(payload.wasm_backend),
				},
			};
		case "write_stdin":
			return {
				tag: "WriteStdinRequest",
				val: {
					processId: payload.process_id,
					chunk: toExactArrayBuffer(payload.chunk),
				},
			};
		case "resize_pty":
			return {
				tag: "ResizePtyRequest",
				val: {
					processId: payload.process_id,
					cols: payload.cols,
					rows: payload.rows,
				},
			};
		case "close_stdin":
			return {
				tag: "CloseStdinRequest",
				val: { processId: payload.process_id },
			};
		case "kill_process":
			return {
				tag: "KillProcessRequest",
				val: { processId: payload.process_id, signal: payload.signal },
			};
		case "get_process_snapshot":
			return { tag: "GetProcessSnapshotRequest", val: null };
		case "get_resource_snapshot":
			return { tag: "GetResourceSnapshotRequest", val: null };
		case "find_listener":
			return {
				tag: "FindListenerRequest",
				val: {
					host: payload.host ?? null,
					port: payload.port ?? null,
					path: payload.path ?? null,
				},
			};
		case "find_bound_udp":
			return {
				tag: "FindBoundUdpRequest",
				val: { host: payload.host ?? null, port: payload.port ?? null },
			};
		case "vm_fetch":
			return {
				tag: "VmFetchRequest",
				val: {
					port: payload.port,
					method: payload.method,
					path: payload.path,
					headersJson: payload.headers_json,
					body: payload.body ?? null,
					bodyBase64: payload.body_base64 ?? null,
					streamOperation: payload.stream_operation ?? null,
					streamId: payload.stream_id ?? null,
					maxBytes: payload.max_bytes ?? null,
				},
			};
		case "get_signal_state":
			return {
				tag: "GetSignalStateRequest",
				val: { processId: payload.process_id },
			};
		case "get_zombie_timer_count":
			return { tag: "GetZombieTimerCountRequest", val: null };
		case "host_filesystem_call":
			return {
				tag: "HostFilesystemCallRequest",
				val: {
					operation: toGeneratedFilesystemOperation(payload.operation),
					path: payload.path,
					payloadSizeBytes: BigInt(payload.payload_size_bytes),
				},
			};
		case "persistence_load":
			return {
				tag: "PersistenceLoadRequest",
				val: { key: payload.key },
			};
		case "persistence_flush":
			return {
				tag: "PersistenceFlushRequest",
				val: {
					key: payload.key,
					payloadSizeBytes: BigInt(payload.payload_size_bytes),
				},
			};
		case "shell_execution":
			return { tag: "ShellExecutionRequest", val: payload.request };
		case "argv_execution":
			return { tag: "ArgvExecutionRequest", val: payload.request };
		case "javascript_execution":
			return { tag: "JavaScriptExecutionRequest", val: payload.request };
		case "javascript_evaluation":
			return { tag: "JavaScriptEvaluationRequest", val: payload.request };
		case "javascript_file_execution":
			return { tag: "JavaScriptFileExecutionRequest", val: payload.request };
		case "typescript_execution":
			return { tag: "TypeScriptExecutionRequest", val: payload.request };
		case "typescript_evaluation":
			return { tag: "TypeScriptEvaluationRequest", val: payload.request };
		case "typescript_file_execution":
			return { tag: "TypeScriptFileExecutionRequest", val: payload.request };
		case "typescript_check":
			return { tag: "TypeScriptCheckRequest", val: payload.request };
		case "typescript_project_check":
			return { tag: "TypeScriptProjectCheckRequest", val: payload.request };
		case "npm_project_install":
			return { tag: "NpmProjectInstallRequest", val: payload.request };
		case "npm_package_install":
			return { tag: "NpmPackageInstallRequest", val: payload.request };
		case "npm_script_execution":
			return { tag: "NpmScriptExecutionRequest", val: payload.request };
		case "npm_package_execution":
			return { tag: "NpmPackageExecutionRequest", val: payload.request };
		case "python_execution":
			return { tag: "PythonExecutionRequest", val: payload.request };
		case "python_evaluation":
			return { tag: "PythonEvaluationRequest", val: payload.request };
		case "python_file_execution":
			return { tag: "PythonFileExecutionRequest", val: payload.request };
		case "python_module_execution":
			return { tag: "PythonModuleExecutionRequest", val: payload.request };
		case "python_install":
			return { tag: "PythonInstallRequest", val: payload.request };
		case "create_context":
			return { tag: "CreateContextRequest", val: payload.request };
		case "get_execution":
			return { tag: "GetExecutionRequest", val: payload.request };
		case "list_executions":
			return { tag: "ListExecutionsRequest", val: null };
		case "wait_execution":
			return { tag: "WaitExecutionRequest", val: payload.request };
		case "cancel_execution":
			return { tag: "CancelExecutionRequest", val: payload.request };
		case "signal_execution":
			return { tag: "SignalExecutionRequest", val: payload.request };
		case "reset_execution":
			return { tag: "ResetExecutionRequest", val: payload.request };
		case "delete_execution":
			return { tag: "DeleteExecutionRequest", val: payload.request };
		case "write_execution_stdin":
			return { tag: "WriteExecutionStdinRequest", val: payload.request };
		case "close_execution_stdin":
			return { tag: "CloseExecutionStdinRequest", val: payload.request };
		case "resize_execution_pty":
			return { tag: "ResizeExecutionPtyRequest", val: payload.request };
		case "read_execution_output":
			return { tag: "ReadExecutionOutputRequest", val: payload.request };
		case "ext":
			return {
				tag: "ExtEnvelope",
				val: toGeneratedExtEnvelope(payload.envelope),
			};
	}
}

function toGeneratedOptionalU64(value: number | undefined): bigint | null {
	return value === undefined ? null : BigInt(value);
}
