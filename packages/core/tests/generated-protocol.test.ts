import * as bare from "@rivetkit/bare-ts";
import { describe, expect, test } from "vitest";
import {
	decodeProtocolFrame,
	encodeProtocolFrame,
	GuestFilesystemOperation,
	PermissionMode,
	type ProtocolFrame,
	readGuestFilesystemCallRequest,
	StreamChannel,
	WasmPermissionTier,
	writeGuestFilesystemCallRequest,
} from "@rivet-dev/agentos-core/protocol";
import {
	decodeBareProtocolFrame,
	encodeBareProtocolFrame,
} from "@rivet-dev/agentos-core/protocol-frames";

const GENERATED_AUTH_FRAME_HEX =
	"000f6167656e746f732d73696465636172080007000000000000000006636f6e6e2d31000e67656e6572617465642d7465737405746f6b656e080001000000";
const PROTOCOL_VERSION = 8;

describe("generated sidecar protocol", () => {
	test("round-trips request frames", () => {
		const frame: ProtocolFrame = {
			tag: "RequestFrame",
			val: {
				schema: { name: "agentos-sidecar", version: PROTOCOL_VERSION },
				requestId: 7n,
				ownership: {
					tag: "ConnectionOwnership",
					val: { connectionId: "conn-1" },
				},
				payload: {
					tag: "AuthenticateRequest",
					val: {
						clientName: "generated-test",
						authToken: "token",
						protocolVersion: PROTOCOL_VERSION,
						bridgeVersion: 1,
					},
				},
			},
		};

		const encoded = encodeProtocolFrame(frame);
		const decoded = decodeProtocolFrame(encoded);

		expect(decoded).toEqual(frame);
	});

	test("matches cross-language auth frame bytes", () => {
		const frame = authFrame();
		const encoded = encodeProtocolFrame(frame);

		expect(Buffer.from(encoded).toString("hex")).toBe(GENERATED_AUTH_FRAME_HEX);
		expect(
			decodeProtocolFrame(Buffer.from(GENERATED_AUTH_FRAME_HEX, "hex")),
		).toEqual(frame);
	});

	test("live TypeScript BARE encoder matches generated request bytes", () => {
		const generatedConfigureFrame: ProtocolFrame = {
			tag: "RequestFrame",
			val: {
				schema: { name: "agentos-sidecar", version: PROTOCOL_VERSION },
				requestId: 9n,
				ownership: {
					tag: "VmOwnership",
					val: {
						connectionId: "conn-1",
						sessionId: "session-1",
						vmId: "vm-1",
					},
				},
				payload: {
					tag: "ConfigureVmRequest",
					val: {
						mounts: [
							{
								guestPath: "/node_modules",
								guestSource: "host_dir",
								guestFstype: "host_dir",
								readOnly: true,
								plugin: {
									id: "host_dir",
									config: JSON.stringify({
										hostPath: "/tmp/deps",
										readOnly: true,
									}),
								},
							},
						],
						software: [],
						permissions: {
							fs: { tag: "PermissionMode", val: PermissionMode.Allow },
							network: null,
							childProcess: null,
							process: null,
							env: null,
							tool: null,
						},
						moduleAccessCwd: "/workspace",
						instructions: ["keep it generic"],
						projectedModules: [
							{ packageName: "workspace", entrypoint: "/workspace/index.js" },
						],
						commandPermissions: new Map([["cat", WasmPermissionTier.ReadOnly]]),
						loopbackExemptPorts: new Uint16Array([3000]),
						packages: [],
						packagesMountAt: "",
						bootstrapCommands: [],
						bindingShimCommands: [],
					},
				},
			},
		};
		const nativeConfigureFrame = {
			frame_type: "request",
			schema: { name: "agentos-sidecar", version: PROTOCOL_VERSION },
			request_id: 9,
			ownership: {
				scope: "vm",
				connection_id: "conn-1",
				session_id: "session-1",
				vm_id: "vm-1",
			},
			payload: {
				type: "configure_vm",
				mounts: [
					{
						guest_path: "/node_modules",
						read_only: true,
						plugin: {
							id: "host_dir",
							config: { hostPath: "/tmp/deps", readOnly: true },
						},
					},
				],
				software: [],
				permissions: { fs: "allow" },
				module_access_cwd: "/workspace",
				instructions: ["keep it generic"],
				projected_modules: [
					{ package_name: "workspace", entrypoint: "/workspace/index.js" },
				],
				command_permissions: { cat: "read-only" },
				loopback_exempt_ports: [3000],
				packages: [],
				packages_mount_at: "",
				bootstrap_commands: [],
				binding_shim_commands: [],
			},
		};

		expect(Buffer.from(encodeBareProtocolFrame(authFrameForNative()))).toEqual(
			Buffer.from(encodeProtocolFrame(authFrame())),
		);
		expect(Buffer.from(encodeBareProtocolFrame(nativeConfigureFrame))).toEqual(
			Buffer.from(encodeProtocolFrame(generatedConfigureFrame)),
		);

		const extPayload = new Uint8Array([1, 2, 3, 4]);
		const generatedExtFrame: ProtocolFrame = {
			tag: "RequestFrame",
			val: {
				schema: { name: "agentos-sidecar", version: PROTOCOL_VERSION },
				requestId: 11n,
				ownership: {
					tag: "ConnectionOwnership",
					val: { connectionId: "conn-1" },
				},
				payload: {
					tag: "ExtEnvelope",
					val: {
						namespace: "dev.rivet.agent-os.test",
						payload: extPayload.buffer,
					},
				},
			},
		};
		const nativeExtFrame = {
			frame_type: "request",
			schema: { name: "agentos-sidecar", version: PROTOCOL_VERSION },
			request_id: 11,
			ownership: { scope: "connection", connection_id: "conn-1" },
			payload: {
				type: "ext",
				envelope: {
					namespace: "dev.rivet.agent-os.test",
					payload: extPayload,
				},
			},
		};

		expect(Buffer.from(encodeBareProtocolFrame(nativeExtFrame))).toEqual(
			Buffer.from(encodeProtocolFrame(generatedExtFrame)),
		);
	});

	test("live TypeScript BARE decoder accepts generated response bytes", () => {
		const generatedFrame: ProtocolFrame = {
			tag: "ResponseFrame",
			val: {
				schema: { name: "agentos-sidecar", version: PROTOCOL_VERSION },
				requestId: 9n,
				ownership: {
					tag: "VmOwnership",
					val: {
						connectionId: "conn-1",
						sessionId: "session-1",
						vmId: "vm-1",
					},
				},
				payload: {
					tag: "VmConfiguredResponse",
					val: {
						appliedMounts: 2,
						appliedSoftware: 0,
						projectedCommands: [],
						agents: [],
					},
				},
			},
		};

		expect(
			decodeBareProtocolFrame(encodeProtocolFrame(generatedFrame)),
		).toEqual({
			frame_type: "response",
			schema: { name: "agentos-sidecar", version: PROTOCOL_VERSION },
			request_id: 9,
			ownership: {
				scope: "vm",
				connection_id: "conn-1",
				session_id: "session-1",
				vm_id: "vm-1",
			},
			payload: {
				type: "vm_configured",
				applied_mounts: 2,
				applied_software: 0,
				projected_commands: [],
				agents: [],
			},
		});
	});

	test("live TypeScript BARE decoder preserves process output chunks", () => {
		const generatedFrame: ProtocolFrame = {
			tag: "EventFrame",
			val: {
				schema: { name: "agentos-sidecar", version: PROTOCOL_VERSION },
				ownership: {
					tag: "VmOwnership",
					val: {
						connectionId: "conn-1",
						sessionId: "session-1",
						vmId: "vm-1",
					},
				},
				payload: {
					tag: "ProcessOutputEvent",
					val: {
						processId: "process-1",
						channel: StreamChannel.Stdout,
						chunk: Buffer.from("first\nsecond\nthird\n"),
					},
				},
			},
		};
		const encoded = encodeProtocolFrame(generatedFrame);
		const framed = Buffer.allocUnsafe(4 + encoded.length);
		framed.writeUInt32BE(encoded.length, 0);
		Buffer.from(encoded).copy(framed, 4);

		expect(decodeBareProtocolFrame(framed.subarray(4))).toEqual({
			frame_type: "event",
			schema: { name: "agentos-sidecar", version: PROTOCOL_VERSION },
			ownership: {
				scope: "vm",
				connection_id: "conn-1",
				session_id: "session-1",
				vm_id: "vm-1",
			},
			payload: {
				type: "process_output",
				process_id: "process-1",
				channel: "stdout",
				chunk: Buffer.from("first\nsecond\nthird\n"),
			},
		});
	});

	test("preserves nested JsonUtf8 string fields", () => {
		const config = JSON.stringify({ bucket: "demo", prefix: "workspace" });
		const frame: ProtocolFrame = {
			tag: "RequestFrame",
			val: {
				schema: { name: "agentos-sidecar", version: PROTOCOL_VERSION },
				requestId: 8n,
				ownership: {
					tag: "ConnectionOwnership",
					val: { connectionId: "conn-1" },
				},
				payload: {
					tag: "ConfigureVmRequest",
					val: {
						mounts: [
							{
								guestPath: "/workspace",
								guestSource: "s3",
								guestFstype: "s3",
								readOnly: true,
								plugin: { id: "s3", config },
							},
						],
						software: [],
						permissions: null,
						moduleAccessCwd: null,
						instructions: [],
						projectedModules: [],
						commandPermissions: new Map(),
						loopbackExemptPorts: new Uint16Array(),
						packages: [],
						packagesMountAt: "",
						bootstrapCommands: [],
						bindingShimCommands: [],
					},
				},
			},
		};

		const encoded = encodeProtocolFrame(frame);
		const decoded = decodeProtocolFrame(encoded);

		expect(decoded).toEqual(frame);
	});

	test("preserves guest filesystem call offsets", () => {
		const config = bare.DEFAULT_CONFIG;
		const bc = new bare.ByteCursor(
			new Uint8Array(config.initialBufferLength),
			config,
		);
		const request = {
			operation: GuestFilesystemOperation.Pread,
			path: "/workspace/data.bin",
			destinationPath: null,
			target: null,
			content: null,
			encoding: null,
			recursive: false,
			maxDepth: null,
			mode: null,
			uid: null,
			gid: null,
			atimeMs: null,
			mtimeMs: null,
			len: 12n,
			offset: 34n,
		};

		writeGuestFilesystemCallRequest(bc, request);
		const encoded = new Uint8Array(
			bc.view.buffer,
			bc.view.byteOffset,
			bc.offset,
		);

		expect(
			readGuestFilesystemCallRequest(new bare.ByteCursor(encoded, config)),
		).toEqual(request);
	});
});

function authFrame(): ProtocolFrame {
	return {
		tag: "RequestFrame",
		val: {
			schema: { name: "agentos-sidecar", version: PROTOCOL_VERSION },
			requestId: 7n,
			ownership: {
				tag: "ConnectionOwnership",
				val: { connectionId: "conn-1" },
			},
			payload: {
				tag: "AuthenticateRequest",
				val: {
					clientName: "generated-test",
					authToken: "token",
					protocolVersion: PROTOCOL_VERSION,
					bridgeVersion: 1,
				},
			},
		},
	};
}

function authFrameForNative(): unknown {
	return {
		frame_type: "request",
		schema: { name: "agentos-sidecar", version: PROTOCOL_VERSION },
		request_id: 7,
		ownership: { scope: "connection", connection_id: "conn-1" },
		payload: {
			type: "authenticate",
			client_name: "generated-test",
			auth_token: "token",
			protocol_version: PROTOCOL_VERSION,
			bridge_version: 1,
		},
	};
}
