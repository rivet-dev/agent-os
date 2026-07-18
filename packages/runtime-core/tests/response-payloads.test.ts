import { describe, expect, it } from "vitest";
import * as protocol from "../src/generated-protocol.js";
import { fromGeneratedResponsePayload } from "../src/response-payloads.js";

describe("response payload conversion", () => {
	it("maps simple generated responses to live payloads", () => {
		expect(
			fromGeneratedResponsePayload({
				tag: "AuthenticatedResponse",
				val: {
					sidecarId: "sidecar",
					connectionId: "conn",
					maxFrameBytes: 1024,
				},
			}),
		).toEqual({
			type: "authenticated",
			sidecar_id: "sidecar",
			connection_id: "conn",
			max_frame_bytes: 1024,
		});

		expect(
			fromGeneratedResponsePayload({
				tag: "RejectedResponse",
				val: {
					code: "ERR_AGENTOS_RESOURCE_LIMIT",
					message: "bridge response bytes exceeded",
					limitName: "bridgeResponseBytes",
					configuredLimit: 1024n,
					currentUsage: 768n,
					requested: 512n,
					unit: "bytes",
					scope: "vm",
					vmId: "vm-1",
					sessionGeneration: 7n,
					capabilityId: 9n,
					operation: "bridge.settle",
					configurationPath: "limits.reactor.maxBridgeResponseBytes",
					retryable: true,
					errno: "EAGAIN",
				},
			}),
		).toEqual({
			type: "rejected",
			code: "ERR_AGENTOS_RESOURCE_LIMIT",
			message: "bridge response bytes exceeded",
			limit_name: "bridgeResponseBytes",
			configured_limit: 1024,
			current_usage: 768,
			requested: 512,
			unit: "bytes",
			scope: "vm",
			vm_id: "vm-1",
			session_generation: 7,
			capability_id: 9,
			operation: "bridge.settle",
			configuration_path: "limits.reactor.maxBridgeResponseBytes",
			retryable: true,
			errno: "EAGAIN",
		});
	});

	it("maps guest filesystem result details", () => {
		expect(
			fromGeneratedResponsePayload({
				tag: "GuestFilesystemResultResponse",
				val: {
					operation: protocol.GuestFilesystemOperation.ReadFile,
					path: "/tmp/file",
					content: "hello",
					encoding: protocol.RootFilesystemEntryEncoding.UtF8,
					entries: null,
					stat: null,
					exists: true,
					target: null,
				},
			}),
		).toEqual({
			type: "guest_filesystem_result",
			operation: "read_file",
			path: "/tmp/file",
			content: "hello",
			encoding: "utf8",
			exists: true,
		});
	});

	it("preserves oversized guest directory entry sizes", () => {
		const unsafeSize = BigInt(Number.MAX_SAFE_INTEGER) + 2n;

		expect(
			fromGeneratedResponsePayload({
				tag: "GuestFilesystemResultResponse",
				val: {
					operation: protocol.GuestFilesystemOperation.ReadDir,
					path: "/tmp",
					content: null,
					encoding: null,
					entries: [
						{
							name: "huge.bin",
							path: "/tmp/huge.bin",
							isDirectory: false,
							isSymbolicLink: false,
							size: unsafeSize,
						},
					],
					stat: null,
					exists: null,
					target: null,
				},
			}),
		).toEqual({
			type: "guest_filesystem_result",
			operation: "read_dir",
			path: "/tmp",
			entries: [
				{
					name: "huge.bin",
					path: "/tmp/huge.bin",
					isDirectory: false,
					isSymbolicLink: false,
					size: unsafeSize,
				},
			],
		});
	});

	it("maps guest kernel call results", () => {
		const payload = new TextEncoder().encode(
			JSON.stringify({ socketId: 3, written: 5 }),
		).buffer;
		expect(
			fromGeneratedResponsePayload({
				tag: "GuestKernelResultResponse",
				val: { payload },
			}),
		).toEqual({
			type: "guest_kernel_result",
			payload,
		});
	});

	it("maps process and socket snapshots", () => {
		expect(
			fromGeneratedResponsePayload({
				tag: "ProcessSnapshotResponse",
				val: {
					processes: [
						{
							processId: "proc",
							pid: 10,
							ppid: 1,
							pgid: 10,
							sid: 10,
							driver: "native",
							command: "node",
							args: ["-e", "0"],
							cwd: "/work",
							status: protocol.ProcessSnapshotStatus.Running,
							exitCode: null,
						},
					],
				},
			}),
		).toEqual({
			type: "process_snapshot",
			processes: [
				{
					process_id: "proc",
					pid: 10,
					ppid: 1,
					pgid: 10,
					sid: 10,
					driver: "native",
					command: "node",
					args: ["-e", "0"],
					cwd: "/work",
					status: "running",
				},
			],
		});

		expect(
			fromGeneratedResponsePayload({
				tag: "ListenerSnapshotResponse",
				val: {
					listener: {
						processId: "proc",
						host: "127.0.0.1",
						port: 8080,
						path: null,
					},
				},
			}),
		).toEqual({
			type: "listener_snapshot",
			listener: {
				process_id: "proc",
				host: "127.0.0.1",
				port: 8080,
			},
		});
	});

	it("maps resource snapshots", () => {
		expect(
			fromGeneratedResponsePayload({
				tag: "ResourceSnapshotResponse",
				val: {
					runningProcesses: 2n,
					stoppedProcesses: 3n,
					exitedProcesses: 1n,
					fdTables: 2n,
					openFds: 6n,
					pipes: 1n,
					pipeBufferedBytes: 12n,
					ptys: 0n,
					ptyBufferedInputBytes: 0n,
					ptyBufferedOutputBytes: 0n,
					sockets: 3n,
					socketListeners: 1n,
					socketConnections: 2n,
					socketBufferedBytes: 256n,
					socketDatagramQueueLen: 4n,
					queueSnapshots: [
						{
							name: "pending_process_events",
							category: "queue",
							depth: 1n,
							highWater: 3n,
							capacity: 128n,
							fillPercent: 2n,
						},
					],
				},
			}),
		).toEqual({
			type: "resource_snapshot",
			running_processes: 2,
			stopped_processes: 3,
			exited_processes: 1,
			fd_tables: 2,
			open_fds: 6,
			pipes: 1,
			pipe_buffered_bytes: 12,
			ptys: 0,
			pty_buffered_input_bytes: 0,
			pty_buffered_output_bytes: 0,
			sockets: 3,
			socket_listeners: 1,
			socket_connections: 2,
			socket_buffered_bytes: 256,
			socket_datagram_queue_len: 4,
			queue_snapshots: [
				{
					name: "pending_process_events",
					category: "queue",
					depth: 1,
					high_water: 3,
					capacity: 128,
					fill_percent: 2,
				},
			],
		});
	});

	it("maps generated binding collection registration to host callback registration", () => {
		expect(
			fromGeneratedResponsePayload({
				tag: "HostCallbacksRegisteredResponse",
				val: { registration: "tools", commandCount: 2 },
			}),
		).toEqual({
			type: "host_callbacks_registered",
			registration: "tools",
			command_count: 2,
		});
	});

	it("maps signal handlers and bigint counts", () => {
		expect(
			fromGeneratedResponsePayload({
				tag: "SignalStateResponse",
				val: {
					processId: "proc",
					handlers: new Map([
						[
							15,
							{
								action: protocol.SignalDispositionAction.User,
								mask: new Uint32Array([2, 15]),
								flags: 4,
							},
						],
					]),
				},
			}),
		).toEqual({
			type: "signal_state",
			process_id: "proc",
			handlers: {
				"15": { action: "user", mask: [2, 15], flags: 4 },
			},
		});

		expect(
			fromGeneratedResponsePayload({
				tag: "ZombieTimerCountResponse",
				val: { count: 3n },
			}),
		).toEqual({
			type: "zombie_timer_count",
			count: 3,
		});
	});

	it("keeps unsupported legacy response tags fail-closed", () => {
		expect(() =>
			fromGeneratedResponsePayload({
				tag: "PermissionDecisionResponse",
				val: { allowed: true },
			}),
		).toThrow("unsupported bare response payload tag: permission_decision");
	});
});
