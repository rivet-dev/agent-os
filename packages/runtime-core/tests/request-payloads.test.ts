import { describe, expect, it } from "vitest";
import * as protocol from "../src/generated-protocol.js";
import { toGeneratedRequestPayload } from "../src/request-payloads.js";

describe("request payload conversion", () => {
	it("maps authentication and session requests", () => {
		expect(
			toGeneratedRequestPayload({
				type: "authenticate",
				client_name: "agentos",
				auth_token: "token",
				protocol_version: 8,
				bridge_version: 1,
			}),
		).toEqual({
			tag: "AuthenticateRequest",
			val: {
				clientName: "agentos",
				authToken: "token",
				protocolVersion: 8,
				bridgeVersion: 1,
			},
		});

		expect(
			toGeneratedRequestPayload({
				type: "open_session",
				placement: { kind: "shared", pool: "default" },
				metadata: { owner: "test" },
			}),
		).toEqual({
			tag: "OpenSessionRequest",
			val: {
				placement: {
					tag: "SidecarPlacementShared",
					val: { pool: "default" },
				},
				metadata: new Map([["owner", "test"]]),
			},
		});
	});

	it("maps VM creation and configuration requests", () => {
		const createVmPayload = toGeneratedRequestPayload({
			type: "create_vm",
			runtime: "java_script",
			config: {
				env: {},
				wasmBackend: "wasmtime",
				rootFilesystem: {
					mode: "read-only",
					disableDefaultBaseLayer: true,
					lowers: [],
					bootstrapEntries: [{ path: "/tmp", kind: "directory" }],
				},
				permissions: { fs: "allow" },
				loopbackExemptPorts: [],
			},
		});
		expect(createVmPayload).toMatchObject({
			tag: "CreateVmRequest",
			val: {
				runtime: protocol.GuestRuntimeKind.JavaScript,
			},
		});
		expect(JSON.parse(createVmPayload.val.config)).toMatchObject({
			wasmBackend: "wasmtime",
			rootFilesystem: {
				mode: "read-only",
				disableDefaultBaseLayer: true,
				bootstrapEntries: [{ path: "/tmp", kind: "directory" }],
			},
			permissions: { fs: "allow" },
		});

		expect(
			toGeneratedRequestPayload({
				type: "configure_vm",
				mounts: [
					{
						guest_path: "/workspace",
						read_only: true,
						plugin: { id: "host", config: { source: "/src" } },
					},
				],
				software: [{ package_name: "node", root: "/software/node" }],
				instructions: ["use node"],
				projected_modules: [
					{ package_name: "@acme/tool", entrypoint: "dist/index.js" },
				],
				command_permissions: { node: "read-only" },
				loopback_exempt_ports: [8080],
			}),
		).toMatchObject({
			tag: "ConfigureVmRequest",
			val: {
				moduleAccessCwd: null,
				instructions: ["use node"],
				commandPermissions: new Map([
					["node", protocol.WasmPermissionTier.ReadOnly],
				]),
			},
		});
	});

	it("maps resource snapshot requests", () => {
		expect(
			toGeneratedRequestPayload({
				type: "get_resource_snapshot",
			}),
		).toEqual({ tag: "GetResourceSnapshotRequest", val: null });
	});

	it("maps host callback registration JSON fields", () => {
		expect(
			toGeneratedRequestPayload({
				type: "register_host_callbacks",
				name: "tools",
				description: "demo tools",
				command_aliases: ["agentos-tools"],
				registry_command_aliases: ["agentos"],
				callbacks: {
					read: {
						description: "read a file",
						input_schema: { type: "object" },
						timeout_ms: 2500,
						examples: [{ description: "basic", input: { path: "/tmp" } }],
					},
				},
			}),
		).toEqual({
			tag: "RegisterHostCallbacksRequest",
			val: {
				name: "tools",
				description: "demo tools",
				commandAliases: ["agentos-tools"],
				registryCommandAliases: ["agentos"],
				callbacks: new Map([
					[
						"read",
						{
							description: "read a file",
							inputSchema: '{"type":"object"}',
							timeoutMs: 2500n,
							examples: [
								{
									description: "basic",
									input: '{"path":"/tmp"}',
								},
							],
						},
					],
				]),
			},
		});
	});

	it("maps filesystem and process requests", () => {
		expect(
			toGeneratedRequestPayload({
				type: "guest_filesystem_call",
				operation: "pread",
				path: "/tmp/file",
				len: 10,
				offset: 2,
				encoding: "base64",
			}),
		).toEqual({
			tag: "GuestFilesystemCallRequest",
			val: {
				operation: protocol.GuestFilesystemOperation.Pread,
				path: "/tmp/file",
				destinationPath: null,
				target: null,
				content: null,
				encoding: protocol.RootFilesystemEntryEncoding.BasE64,
				recursive: false,
				mode: null,
				uid: null,
				gid: null,
				atimeMs: null,
				mtimeMs: null,
				len: 10n,
				offset: 2n,
				maxDepth: null,
			},
		});

		expect(
			toGeneratedRequestPayload({
				type: "execute",
				process_id: "proc",
				command: "node",
				args: ["-e", "0"],
				env: { A: "1" },
				runtime: "java_script",
				wasm_permission_tier: "isolated",
			}),
		).toEqual({
			tag: "ExecuteRequest",
			val: {
				processId: "proc",
				command: "node",
				runtime: protocol.GuestRuntimeKind.JavaScript,
				entrypoint: null,
				args: ["-e", "0"],
				env: new Map([["A", "1"]]),
				cwd: null,
				wasmPermissionTier: protocol.WasmPermissionTier.Isolated,
				wasmBackend: null,
			},
		});
	});

	it("maps every standalone WASM backend selector", () => {
		for (const [wasm_backend, wasmBackend] of [
			["v8", protocol.StandaloneWasmBackend.V8],
			["wasmtime", protocol.StandaloneWasmBackend.Wasmtime],
			["wasmtime-threads", protocol.StandaloneWasmBackend.WasmtimeThreads],
		] as const) {
			expect(
				toGeneratedRequestPayload({
					type: "execute",
					process_id: `proc-${wasm_backend}`,
					args: [],
					env: {},
					wasm_backend,
				}),
			).toMatchObject({
				tag: "ExecuteRequest",
				val: { wasmBackend },
			});
		}
	});

	it("maps guest kernel call requests", () => {
		const payload = new TextEncoder().encode(
			JSON.stringify({ host: "127.0.0.1", port: 39221 }),
		).buffer;
		expect(
			toGeneratedRequestPayload({
				type: "guest_kernel_call",
				execution_id: "exec-1",
				operation: "net.connect",
				payload,
			}),
		).toEqual({
			tag: "GuestKernelCallRequest",
			val: {
				executionId: "exec-1",
				operation: "net.connect",
				payload,
			},
		});
	});

	it("maps stdin and ext requests", () => {
		expect(
			toGeneratedRequestPayload({
				type: "write_stdin",
				process_id: "proc",
				chunk: new Uint8Array([1, 2, 3]),
			}),
		).toMatchObject({
			tag: "WriteStdinRequest",
			val: {
				processId: "proc",
			},
		});

		expect(
			toGeneratedRequestPayload({
				type: "ext",
				envelope: {
					namespace: "dev.test",
					payload: new Uint8Array([4, 5]),
				},
			}),
		).toMatchObject({
			tag: "ExtEnvelope",
			val: {
				namespace: "dev.test",
			},
		});
	});
});
