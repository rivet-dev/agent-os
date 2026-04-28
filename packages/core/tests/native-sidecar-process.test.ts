import { execFileSync } from "node:child_process";
import {
	chmodSync,
	existsSync,
	mkdirSync,
	mkdtempSync,
	readFileSync,
	rmSync,
	symlinkSync,
	writeFileSync,
} from "node:fs";
import { constants as osConstants, tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, test, vi } from "vitest";
import { createHostDirBackend } from "../src/host-dir-mount.js";
import {
	createInMemoryFileSystem,
	createKernel,
	createNodeRuntime,
	NodeFileSystem,
	createWasmVmRuntime,
} from "../src/runtime-compat.js";
import {
	NativeSidecarKernelProxy,
	NativeSidecarProcessClient,
	SidecarEventBufferOverflow,
	SidecarProcessError,
	SidecarProcessExited,
	serializeMountConfigForSidecar,
	serializeRootFilesystemForSidecar,
	toSidecarSignalName,
} from "../src/sidecar/rpc-client.js";
import { findCargoBinary, resolveCargoBinary } from "../src/sidecar/cargo.js";
import { serializePermissionsForSidecar } from "../src/sidecar/permissions.js";

const REPO_ROOT = fileURLToPath(new URL("../../..", import.meta.url));
const SIDECAR_BINARY = join(REPO_ROOT, "target/debug/agent-os-sidecar");
const REGISTRY_COMMANDS_DIR = join(
	REPO_ROOT,
	"registry/native/target/wasm32-wasip1/release/commands",
);
const SIGNAL_STATE_CONTROL_PREFIX = "__AGENT_OS_SIGNAL_STATE__:";
const ALLOW_ALL_VM_PERMISSIONS = {
	fs: "allow",
	network: "allow",
	childProcess: "allow",
	process: "allow",
	env: "allow",
	tool: "allow",
} as const;
const ALLOW_ALL_SIDECAR_PERMISSIONS = serializePermissionsForSidecar(
	ALLOW_ALL_VM_PERMISSIONS,
);

function ensureSidecarBinaryReady(): void {
	const cargoBinary = findCargoBinary();
	if (cargoBinary) {
		execFileSync(cargoBinary, ["build", "-q", "-p", "agent-os-sidecar"], {
			cwd: REPO_ROOT,
			stdio: "pipe",
		});
		return;
	}

	if (!existsSync(SIDECAR_BINARY)) {
		execFileSync(resolveCargoBinary(), ["build", "-q", "-p", "agent-os-sidecar"], {
			cwd: REPO_ROOT,
			stdio: "pipe",
		});
	}
}
const BARE_FIXTURE_PROTOCOL_HELPERS = `
const writeVarUint = (value) => {
	let remaining = BigInt(value);
	const bytes = [];
	while (remaining >= 0x80n) {
		bytes.push(Number((remaining & 0x7fn) | 0x80n));
		remaining >>= 7n;
	}
	bytes.push(Number(remaining));
	return Buffer.from(bytes);
};
const encodeString = (value) => {
	const bytes = Buffer.from(String(value), "utf8");
	return Buffer.concat([writeVarUint(bytes.length), bytes]);
};
const encodeU16 = (value) => {
	const bytes = Buffer.allocUnsafe(2);
	bytes.writeUInt16LE(value, 0);
	return bytes;
};
const encodeI64 = (value) => {
	const bytes = Buffer.allocUnsafe(8);
	bytes.writeBigInt64LE(BigInt(value), 0);
	return bytes;
};
const encodeBool = (value) => Buffer.from([value ? 1 : 0]);
const encodeOptional = (value, encode) =>
	value === undefined || value === null
		? encodeBool(false)
		: Buffer.concat([encodeBool(true), encode(value)]);
const encodeJsonUtf8 = (value) => encodeString(JSON.stringify(value));
const encodeSchema = (schema) =>
	Buffer.concat([encodeString(schema.name), encodeU16(schema.version)]);
const encodeOwnership = (ownership) => {
	switch (ownership.scope) {
		case "connection":
			return Buffer.concat([writeVarUint(1), encodeString(ownership.connection_id)]);
		case "session":
			return Buffer.concat([
				writeVarUint(2),
				encodeString(ownership.connection_id),
				encodeString(ownership.session_id),
			]);
		case "vm":
			return Buffer.concat([
				writeVarUint(3),
				encodeString(ownership.connection_id),
				encodeString(ownership.session_id),
				encodeString(ownership.vm_id),
			]);
		default:
			throw new Error("unsupported ownership scope");
	}
};
const encodeSidecarRequestPayload = (payload) => {
	if (payload.type !== "js_bridge_call") {
		throw new Error("unsupported sidecar request payload");
	}
	return Buffer.concat([
		writeVarUint(4),
		encodeString(payload.call_id),
		encodeString(payload.mount_id),
		encodeString(payload.operation),
		encodeJsonUtf8(payload.args),
	]);
};
const encodeProtocolFrame = (frame) => {
	if (frame.frame_type !== "sidecar_request") {
		throw new Error("unsupported frame type");
	}
	return Buffer.concat([
		writeVarUint(4),
		encodeSchema(frame.schema),
		encodeI64(frame.request_id),
		encodeOwnership(frame.ownership),
		encodeSidecarRequestPayload(frame.payload),
	]);
};
const writeFrame = (frame) => {
	const payload = encodeProtocolFrame(frame);
	const prefix = Buffer.allocUnsafe(4);
	prefix.writeUInt32BE(payload.length, 0);
	process.stdout.write(Buffer.concat([prefix, payload]));
};
const readVarUint = (state) => {
	let result = 0n;
	let shift = 0n;
	for (;;) {
		const byte = state.buffer[state.offset++];
		result |= BigInt(byte & 0x7f) << shift;
		if ((byte & 0x80) === 0) {
			return Number(result);
		}
		shift += 7n;
	}
};
const readString = (state) => {
	const length = readVarUint(state);
	const value = state.buffer.subarray(state.offset, state.offset + length).toString("utf8");
	state.offset += length;
	return value;
};
const readU16 = (state) => {
	const value = state.buffer.readUInt16LE(state.offset);
	state.offset += 2;
	return value;
};
const readI64 = (state) => {
	const value = Number(state.buffer.readBigInt64LE(state.offset));
	state.offset += 8;
	return value;
};
const readBool = (state) => state.buffer[state.offset++] !== 0;
const readOptional = (state, read) => (readBool(state) ? read(state) : undefined);
const decodeSchema = (state) => ({
	name: readString(state),
	version: readU16(state),
});
const decodeOwnership = (state) => {
	switch (readVarUint(state)) {
		case 1:
			return { scope: "connection", connection_id: readString(state) };
		case 2:
			return {
				scope: "session",
				connection_id: readString(state),
				session_id: readString(state),
			};
		case 3:
			return {
				scope: "vm",
				connection_id: readString(state),
				session_id: readString(state),
				vm_id: readString(state),
			};
		default:
			throw new Error("unsupported ownership scope tag");
	}
};
const decodeSidecarResponsePayload = (state) => {
	switch (readVarUint(state)) {
		case 1:
			return {
				type: "tool_invocation_result",
				invocation_id: readString(state),
				result: readOptional(state, (inner) => JSON.parse(readString(inner))),
				error: readOptional(state, readString),
			};
		case 2:
			return {
				type: "permission_request_result",
				permission_id: readString(state),
				reply: readOptional(state, readString),
				error: readOptional(state, readString),
			};
		case 3:
			return {
				type: "acp_request_result",
				response: readOptional(state, (inner) => JSON.parse(readString(inner))),
				error: readOptional(state, readString),
			};
		case 4:
			return {
				type: "js_bridge_result",
				call_id: readString(state),
				result: readOptional(state, (inner) => JSON.parse(readString(inner))),
				error: readOptional(state, readString),
			};
		default:
			throw new Error("unsupported sidecar response payload");
	}
};
const decodeProtocolFrame = (payload) => {
	const state = { buffer: payload, offset: 0 };
	const tag = readVarUint(state);
	if (tag !== 5) {
		throw new Error("expected sidecar_response frame");
	}
	const frame = {
		frame_type: "sidecar_response",
		schema: decodeSchema(state),
		request_id: readI64(state),
		ownership: decodeOwnership(state),
		payload: decodeSidecarResponsePayload(state),
	};
	if (state.offset !== state.buffer.length) {
		throw new Error("unexpected trailing bytes in sidecar_response");
	}
	return frame;
};
`.trim();

async function waitFor<T>(
	read: () => Promise<T> | T,
	options?: {
		timeoutMs?: number;
		intervalMs?: number;
		isReady?: (value: T) => boolean;
	},
): Promise<T> {
	const timeoutMs = options?.timeoutMs ?? 10_000;
	const intervalMs = options?.intervalMs ?? 25;
	const isReady = options?.isReady ?? ((value: T) => Boolean(value));
	const deadline = Date.now() + timeoutMs;
	let lastValue = await read();
	while (!isReady(lastValue)) {
		if (Date.now() >= deadline) {
			throw new Error("timed out waiting for expected state");
		}
		await new Promise((resolve) => setTimeout(resolve, intervalMs));
		lastValue = await read();
	}
	return lastValue;
}

describe("native sidecar process client", () => {
	const cleanupPaths: string[] = [];

	afterEach(() => {
		vi.useRealTimers();
		vi.restoreAllMocks();
		for (const path of cleanupPaths.splice(0)) {
			rmSync(path, { recursive: true, force: true });
		}
	});

	test("maps numeric signals to canonical sidecar signal names", () => {
		expect(toSidecarSignalName(osConstants.signals.SIGKILL)).toBe("SIGKILL");
		expect(toSidecarSignalName(osConstants.signals.SIGUSR1)).toBe("SIGUSR1");
		expect(toSidecarSignalName(osConstants.signals.SIGSTOP)).toBe("SIGSTOP");
		expect(toSidecarSignalName(osConstants.signals.SIGCONT)).toBe("SIGCONT");
		expect(toSidecarSignalName(0)).toBe("0");
	});

	test("idle event pumps do not error after 25 hours", async () => {
		vi.useFakeTimers();

		const waitForEvent = vi.fn(
			(
				_matcher: (event: unknown) => boolean,
				_timeoutMs?: number,
				options?: { signal?: AbortSignal },
			) =>
				new Promise<never>((_resolve, reject) => {
					options?.signal?.addEventListener(
						"abort",
						() => {
							reject(
								options.signal?.reason instanceof Error
									? options.signal.reason
									: new Error("aborted"),
							);
						},
						{ once: true },
					);
				}),
		);
		const client = {
			waitForEvent,
			disposeVm: vi.fn(async () => {}),
			dispose: vi.fn(async () => {}),
		} as unknown as NativeSidecarProcessClient;

		const proxy = new NativeSidecarKernelProxy({
			client,
			session: {
				connectionId: "connection-1",
				sessionId: "session-1",
			},
			vm: {
				vmId: "vm-1",
			},
			env: {},
			cwd: "/workspace",
			localMounts: [],
			commandGuestPaths: new Map(),
		});

		try {
			await vi.advanceTimersByTimeAsync(25 * 60 * 60 * 1_000);
			expect(waitForEvent).toHaveBeenCalledTimes(1);
			expect(waitForEvent.mock.calls[0]?.[1]).toBeUndefined();
			expect(waitForEvent.mock.calls[0]?.[2]?.signal).toBeInstanceOf(
				AbortSignal,
			);
			expect(
				(proxy as unknown as { pumpError: Error | null }).pumpError,
			).toBeNull();
		} finally {
			await proxy.dispose();
		}
	});

	test("dispatches BARE sidecar_request frames to the registered handler", async () => {
		const fixtureRoot = mkdtempSync(
			join(tmpdir(), "agent-os-sidecar-request-"),
		);
		cleanupPaths.push(fixtureRoot);
		const capturePath = join(fixtureRoot, "captured-response.json");
		const driverPath = join(fixtureRoot, "fake-sidecar.mjs");
		writeFileSync(
			driverPath,
			[
				"import { writeFileSync } from 'node:fs';",
				"const capturePath = process.argv[2];",
				"const schema = { name: 'agent-os-sidecar', version: 1 };",
				"let stdinBuffer = Buffer.alloc(0);",
				BARE_FIXTURE_PROTOCOL_HELPERS,
				"const drain = () => {",
				"  while (stdinBuffer.length >= 4) {",
				"    const length = stdinBuffer.readUInt32BE(0);",
				"    if (stdinBuffer.length < 4 + length) return;",
				"    const frame = decodeProtocolFrame(stdinBuffer.subarray(4, 4 + length));",
				"    stdinBuffer = stdinBuffer.subarray(4 + length);",
				"    if (frame.frame_type === 'sidecar_response') {",
				"      writeFileSync(capturePath, JSON.stringify(frame));",
				"      process.exit(0);",
				"    }",
				"  }",
				"};",
				"process.stdin.on('data', (chunk) => {",
				"  stdinBuffer = Buffer.concat([stdinBuffer, Buffer.from(chunk)]);",
				"  drain();",
				"});",
				"process.stdin.resume();",
				"setTimeout(() => {",
				"  writeFrame({",
				"    frame_type: 'sidecar_request',",
				"    schema,",
				"    request_id: -1,",
				"    ownership: {",
				"      scope: 'vm',",
				"      connection_id: 'conn-1',",
				"      session_id: 'session-1',",
				"      vm_id: 'vm-1',",
				"    },",
				"    payload: {",
				"      type: 'js_bridge_call',",
				"      call_id: 'call-1',",
				"      mount_id: 'mount-1',",
				"      operation: 'read_file',",
				"      args: { path: '/workspace/input.txt' },",
				"    },",
				"  });",
				"}, 25);",
			].join("\n"),
		);

		const client = NativeSidecarProcessClient.spawn({
			cwd: REPO_ROOT,
			command: "node",
			args: [driverPath, capturePath],
			frameTimeoutMs: 5_000,
		});
		client.setSidecarRequestHandler(async (request) => {
			expect(request.request_id).toBe(-1);
			expect(request.payload.type).toBe("js_bridge_call");
			if (request.payload.type !== "js_bridge_call") {
				throw new Error("expected js_bridge_call payload");
			}
			return {
				type: "js_bridge_result",
				call_id: request.payload.call_id,
				result: {
					content: "from-handler",
				},
			};
		});

		try {
			const captured = await waitFor(
				() => {
					if (!existsSync(capturePath)) {
						return null;
					}
					return JSON.parse(readFileSync(capturePath, "utf8")) as {
						frame_type: string;
						request_id: number;
						payload: {
							type: string;
							call_id: string;
							result?: { content: string };
						};
					};
				},
				{
					isReady: (value) => value !== null,
				},
			);
			expect(captured?.frame_type).toBe("sidecar_response");
			expect(captured?.request_id).toBe(-1);
			expect(captured?.payload).toMatchObject({
				type: "js_bridge_result",
				call_id: "call-1",
				result: {
					content: "from-handler",
				},
			});
		} finally {
			await client.dispose();
		}
	});

	test("dispose forcibly terminates a sidecar that ignores stdin closure", async () => {
		const fixtureRoot = mkdtempSync(
			join(tmpdir(), "agent-os-sidecar-dispose-"),
		);
		cleanupPaths.push(fixtureRoot);
		const driverPath = join(fixtureRoot, "stuck-sidecar.mjs");
		writeFileSync(
			driverPath,
			[
				"// Drain stdin and ignore EOF so dispose() must time out and SIGKILL.",
				"process.stdin.on('data', () => {});",
				"process.stdin.resume();",
				"process.stdin.on('end', () => {});",
				"setInterval(() => {}, 60_000);",
			].join("\n"),
		);

		const client = NativeSidecarProcessClient.spawn({
			cwd: REPO_ROOT,
			command: "node",
			args: [driverPath],
			frameTimeoutMs: 5_000,
		});
		const childPid = (
			client as unknown as { child: { pid?: number } }
		).child?.pid;
		expect(typeof childPid).toBe("number");

		const startedAt = Date.now();
		await client.dispose();
		const elapsedMs = Date.now() - startedAt;
		expect(elapsedMs).toBeLessThan(15_000);
		expect(
			(client as unknown as { child: { exitCode: number | null; signalCode: string | null } })
				.child.exitCode === null
				? (client as unknown as { child: { signalCode: string | null } }).child.signalCode
				: "exited",
		).toBeTruthy();

		if (typeof childPid === "number") {
			let alive = true;
			try {
				process.kill(childPid, 0);
			} catch {
				alive = false;
			}
			expect(alive).toBe(false);
		}
	}, 30_000);

	test("caps buffered events and fails fast when 10k unmatched events arrive before draining", async () => {
		const fixtureRoot = mkdtempSync(
			join(tmpdir(), "agent-os-sidecar-event-buffer-"),
		);
		cleanupPaths.push(fixtureRoot);
		const driverPath = join(fixtureRoot, "overflow-sidecar.mjs");
		writeFileSync(
			driverPath,
			[
				"const schema = { name: 'agent-os-sidecar', version: 1 };",
				"const writeFrame = (frame) => {",
				"  const payload = Buffer.from(JSON.stringify(frame), 'utf8');",
				"  const prefix = Buffer.allocUnsafe(4);",
				"  prefix.writeUInt32BE(payload.length, 0);",
				"  process.stdout.write(Buffer.concat([prefix, payload]));",
				"};",
				"const ownership = {",
				"  scope: 'vm',",
				"  connection_id: 'conn-1',",
				"  session_id: 'session-1',",
				"  vm_id: 'vm-1',",
				"};",
				"for (let index = 0; index < 10_000; index += 1) {",
				"  writeFrame({",
				"    frame_type: 'event',",
				"    schema,",
				"    ownership,",
				"    payload: {",
				"      type: 'structured',",
				"      name: 'queued-event',",
				"      detail: { index: String(index) },",
				"    },",
				"  });",
				"}",
				"setInterval(() => {}, 60_000);",
			].join("\n"),
		);

		const client = NativeSidecarProcessClient.spawn({
			cwd: REPO_ROOT,
			command: "node",
			args: [driverPath],
			frameTimeoutMs: 5_000,
			payloadCodec: "json",
			eventBufferCapacity: 128,
		});

		try {
			const overflow = await waitFor(
				() =>
					(client as unknown as { stdoutClosedError: Error | null })
						.stdoutClosedError,
				{
					timeoutMs: 10_000,
					isReady: (value): value is SidecarEventBufferOverflow =>
						value instanceof SidecarEventBufferOverflow,
				},
			);
			expect(overflow.capacity).toBe(128);
			expect(overflow.eventType).toBe("structured");

			const bufferedEvents = (
				client as unknown as { bufferedEvents: Map<number, unknown> }
			).bufferedEvents;
			expect(bufferedEvents.size).toBeLessThanOrEqual(128);

			await expect(
				client.waitForEvent(
					{
						type: "structured",
						name: "queued-event",
					},
					50,
				),
			).rejects.toBeInstanceOf(SidecarEventBufferOverflow);
		} finally {
			await client.dispose().catch(() => {});
		}
	}, 30_000);

	test("rejects in-flight requests immediately when the sidecar child exits", async () => {
		const fixtureRoot = mkdtempSync(
			join(tmpdir(), "agent-os-sidecar-child-exit-"),
		);
		cleanupPaths.push(fixtureRoot);
		const driverPath = join(fixtureRoot, "fake-sidecar.mjs");
		writeFileSync(
			driverPath,
			[
				"const schema = { name: 'agent-os-sidecar', version: 1 };",
				"let stdinBuffer = Buffer.alloc(0);",
				"const writeFrame = (frame) => {",
				"  const payload = Buffer.from(JSON.stringify(frame), 'utf8');",
				"  const prefix = Buffer.allocUnsafe(4);",
				"  prefix.writeUInt32BE(payload.length, 0);",
				"  process.stdout.write(Buffer.concat([prefix, payload]));",
				"};",
				"const respond = (request, payload) => {",
				"  writeFrame({",
				"    frame_type: 'response',",
				"    schema,",
				"    request_id: request.request_id,",
				"    ownership: request.ownership,",
				"    payload,",
				"  });",
				"};",
				"const handleFrame = (frame) => {",
				"  if (frame.frame_type !== 'request') return;",
				"  switch (frame.payload.type) {",
				"    case 'authenticate':",
				"      respond(frame, {",
				"        type: 'authenticated',",
				"        connection_id: 'conn-1',",
				"      });",
				"      break;",
				"    case 'open_session':",
				"      respond(frame, {",
				"        type: 'session_opened',",
				"        session_id: 'session-1',",
				"      });",
				"      break;",
				"    case 'create_vm':",
				"      setTimeout(() => process.exit(17), 10);",
				"      break;",
				"    default:",
				"      throw new Error(`unexpected payload ${frame.payload.type}`);",
				"  }",
				"};",
				"const drain = () => {",
				"  while (stdinBuffer.length >= 4) {",
				"    const length = stdinBuffer.readUInt32BE(0);",
				"    if (stdinBuffer.length < 4 + length) return;",
				"    const payload = stdinBuffer.subarray(4, 4 + length);",
				"    stdinBuffer = stdinBuffer.subarray(4 + length);",
				"    handleFrame(JSON.parse(payload.toString('utf8')));",
				"  }",
				"};",
				"process.stdin.on('data', (chunk) => {",
				"  stdinBuffer = Buffer.concat([stdinBuffer, Buffer.from(chunk)]);",
				"  drain();",
				"});",
				"process.stdin.resume();",
			].join("\n"),
		);

		const client = NativeSidecarProcessClient.spawn({
			cwd: REPO_ROOT,
			command: "node",
			args: [driverPath],
			frameTimeoutMs: 30_000,
			payloadCodec: "json",
		});

		try {
			const session = await client.authenticateAndOpenSession();
			const startedAt = Date.now();
			const inFlightRequest = client.createVm(session, {
				runtime: "java_script",
			});
			const result = await Promise.race([
				inFlightRequest
					.then((value) => ({ type: "resolved" as const, value }))
					.catch((error: unknown) => ({
						type: "rejected" as const,
						error,
						elapsedMs: Date.now() - startedAt,
					})),
				new Promise<{ type: "timeout" }>((resolve) =>
					setTimeout(() => resolve({ type: "timeout" }), 100),
				),
			]);

			expect(result.type).toBe("rejected");
			if (result.type !== "rejected") {
				throw new Error(`expected rejection, got ${result.type}`);
			}
			expect(result.elapsedMs).toBeLessThan(100);
			expect(result.error).toBeInstanceOf(SidecarProcessExited);

			await waitFor(
				() =>
					(
						client as unknown as {
							child: { exitCode: number | null };
						}
					).child.exitCode,
				{
					timeoutMs: 1_000,
					isReady: (value) => value === 17,
				},
			);

			const secondStartedAt = Date.now();
			const secondRequest = client.createVm(session, {
				runtime: "java_script",
			});
			await expect(secondRequest).rejects.toMatchObject({
				exitCode: 17,
			});
			await expect(
				secondRequest,
			).rejects.toBeInstanceOf(SidecarProcessExited);
			expect(Date.now() - secondStartedAt).toBeLessThan(20);
		} finally {
			await client.dispose().catch(() => {});
		}
	});

	test("surfaces spawn failures as typed sidecar process errors", async () => {
		const client = NativeSidecarProcessClient.spawn({
			cwd: REPO_ROOT,
			command: join(
				tmpdir(),
				`agent-os-sidecar-missing-${process.pid}-${Date.now()}`,
			),
			args: [],
			frameTimeoutMs: 30_000,
		});

		await expect(client.authenticateAndOpenSession()).rejects.toBeInstanceOf(
			SidecarProcessError,
		);
		await expect(client.authenticateAndOpenSession()).rejects.toBeInstanceOf(
			SidecarProcessError,
		);
	});

	test("NativeKernel refreshes zombieTimerCount from the sidecar proxy", async () => {
		const zombieTimerCount = vi
			.spyOn(NativeSidecarProcessClient.prototype, "getZombieTimerCount")
			.mockResolvedValueOnce({ count: 3 })
			.mockResolvedValueOnce({ count: 0 });

		const kernel = createKernel({
			filesystem: createInMemoryFileSystem(),
			permissions: ALLOW_ALL_VM_PERMISSIONS,
		});

		try {
			await kernel.mount(createNodeRuntime());

			expect(kernel.zombieTimerCount).toBe(0);
			await waitFor(() => kernel.zombieTimerCount, {
				isReady: (value) => value === 3,
			});
			await waitFor(() => kernel.zombieTimerCount, {
				isReady: (value) => value === 0,
			});

			expect(zombieTimerCount).toHaveBeenCalled();
		} finally {
			await kernel.dispose();
		}
	}, 60_000);

	test("NativeKernel exposes symlinked node_modules passthrough directories", async () => {
		const projectRoot = mkdtempSync(join(tmpdir(), "agent-os-node-modules-root-"));
		const dependencyRoot = mkdtempSync(join(tmpdir(), "agent-os-node-modules-store-"));
		cleanupPaths.push(projectRoot, dependencyRoot);
		mkdirSync(join(dependencyRoot, ".bin"), { recursive: true });
		writeFileSync(join(dependencyRoot, ".bin", "astro"), "#!/bin/sh\nexit 0\n");
		chmodSync(join(dependencyRoot, ".bin", "astro"), 0o755);
		symlinkSync(dependencyRoot, join(projectRoot, "node_modules"), "dir");

		const kernel = createKernel({
			filesystem: new NodeFileSystem({ root: projectRoot }),
			permissions: ALLOW_ALL_VM_PERMISSIONS,
		});

		try {
			await kernel.mount(
				createWasmVmRuntime({ commandDirs: [REGISTRY_COMMANDS_DIR] }),
			);
			await kernel.mount(createNodeRuntime());
			let stdout = "";
			let stderr = "";
			const child = kernel.spawn(
				"node",
				[
					"-e",
					[
						"const fs = require('node:fs');",
						"console.log('node_modules', fs.existsSync('/node_modules'));",
						"console.log('bin', fs.existsSync('/node_modules/.bin'));",
						"console.log('astro', fs.existsSync('/node_modules/.bin/astro'));",
					].join(" "),
				],
				{
					onStdout: (chunk) => {
						stdout += Buffer.from(chunk).toString("utf8");
					},
					onStderr: (chunk) => {
						stderr += Buffer.from(chunk).toString("utf8");
					},
				},
			);
			const exitCode = await child.wait();

			expect(exitCode).toBe(0);
			expect(stderr).toBe("");
			expect(stdout).toContain("node_modules true");
			expect(stdout).toContain("bin true");
			expect(stdout).toContain("astro true");
		} finally {
			await kernel.dispose();
		}
	}, 60_000);

	test("NativeKernel wait() drains trailing stdout from short-lived processes", async () => {
		const projectRoot = mkdtempSync(join(tmpdir(), "agent-os-fast-exit-"));
		cleanupPaths.push(projectRoot);

		const kernel = createKernel({
			filesystem: new NodeFileSystem({ root: projectRoot }),
			permissions: ALLOW_ALL_VM_PERMISSIONS,
		});

		try {
			await kernel.mount(createNodeRuntime());
			let stdout = "";
			let stderr = "";
			const child = kernel.spawn(
				"node",
				[
					"-e",
					[
						"console.log('first');",
						"console.log('second');",
						"console.log('third');",
					].join(" "),
				],
				{
					onStdout: (chunk) => {
						stdout += Buffer.from(chunk).toString("utf8");
					},
					onStderr: (chunk) => {
						stderr += Buffer.from(chunk).toString("utf8");
					},
				},
			);
			const exitCode = await child.wait();

			expect(exitCode).toBe(0);
			expect(stderr).toBe("");
			expect(stdout).toBe("first\nsecond\nthird\n");
		} finally {
			await kernel.dispose();
		}
	}, 60_000);

	test("speaks to the real Rust sidecar binary over the framed stdio protocol", async () => {
		const fixtureRoot = mkdtempSync(join(tmpdir(), "agent-os-native-sidecar-"));
		cleanupPaths.push(fixtureRoot);
		writeFileSync(
			join(fixtureRoot, "entry.mjs"),
			"console.log('packages-core-native-sidecar-ok');\n",
		);
		ensureSidecarBinaryReady();

		const client = NativeSidecarProcessClient.spawn({
			cwd: REPO_ROOT,
			command: SIDECAR_BINARY,
			args: [],
			frameTimeoutMs: 20_000,
		});

		try {
			const session = await client.authenticateAndOpenSession();
			const vm = await client.createVm(session, {
				runtime: "java_script",
				metadata: {
					cwd: fixtureRoot,
				},
				rootFilesystem: serializeRootFilesystemForSidecar(),
				permissions: ALLOW_ALL_SIDECAR_PERMISSIONS,
			});

			const creating = await client.waitForEvent(
				(event) =>
					event.payload.type === "vm_lifecycle" &&
					event.payload.state === "creating",
				10_000,
			);
			const ready = await client.waitForEvent(
				(event) =>
					event.payload.type === "vm_lifecycle" &&
					event.payload.state === "ready",
				10_000,
			);
			expect(creating.payload.type).toBe("vm_lifecycle");
			expect(ready.payload.type).toBe("vm_lifecycle");

			await client.bootstrapRootFilesystem(session, vm, [
				{
					path: "/workspace",
					kind: "directory",
				},
				{
					path: "/workspace/seed.txt",
					kind: "file",
					content: "seeded",
				},
			]);

			expect(
				new TextDecoder().decode(
					await client.readFile(session, vm, "/workspace/seed.txt"),
				),
			).toBe("seeded");

			await client.mkdir(session, vm, "/workspace/nested", {
				recursive: true,
			});
			await client.writeFile(
				session,
				vm,
				"/workspace/nested/generated.txt",
				"generated-through-rust-vfs",
			);
			expect(
				new TextDecoder().decode(
					await client.readFile(session, vm, "/workspace/nested/generated.txt"),
				),
			).toBe("generated-through-rust-vfs");
			expect(await client.readdir(session, vm, "/workspace")).toContain(
				"nested",
			);
			expect(
				await client.exists(session, vm, "/workspace/nested/generated.txt"),
			).toBe(true);
			await client.rename(
				session,
				vm,
				"/workspace/nested/generated.txt",
				"/workspace/nested/renamed.txt",
			);
			expect(
				await client.exists(session, vm, "/workspace/nested/generated.txt"),
			).toBe(false);
			expect(
				await client.exists(session, vm, "/workspace/nested/renamed.txt"),
			).toBe(true);
			const snapshot = await client.snapshotRootFilesystem(session, vm);
			expect(
				snapshot.some(
					(entry) => entry.path === "/workspace/nested/renamed.txt",
				),
			).toBe(true);

			await client.execute(session, vm, {
				processId: "proc-1",
				runtime: "java_script",
				entrypoint: "./entry.mjs",
			});

			const stdout = await client.waitForEvent(
				(event) =>
					event.payload.type === "process_output" &&
					event.payload.process_id === "proc-1" &&
					event.payload.channel === "stdout",
				20_000,
			);
			if (stdout.payload.type !== "process_output") {
				throw new Error("expected process_output event");
			}
			expect(stdout.payload.chunk).toContain("packages-core-native-sidecar-ok");

			const exited = await client.waitForEvent(
				(event) =>
					event.payload.type === "process_exited" &&
					event.payload.process_id === "proc-1",
				20_000,
			);
			if (exited.payload.type !== "process_exited") {
				throw new Error("expected process_exited event");
			}
			expect(exited.payload.exit_code).toBe(0);
		} finally {
			await client.dispose();
		}
	}, 60_000);

	test("exercises moduleAccessCwd and layer RPCs against the real sidecar binary", async () => {
		const fixtureRoot = mkdtempSync(join(tmpdir(), "agent-os-native-sidecar-"));
		cleanupPaths.push(fixtureRoot);
		ensureSidecarBinaryReady();

		const client = NativeSidecarProcessClient.spawn({
			cwd: REPO_ROOT,
			command: SIDECAR_BINARY,
			args: [],
			frameTimeoutMs: 20_000,
		});

		try {
			const session = await client.authenticateAndOpenSession();
			const vm = await client.createVm(session, {
				runtime: "java_script",
				metadata: {
					cwd: fixtureRoot,
				},
				rootFilesystem: serializeRootFilesystemForSidecar(),
				permissions: ALLOW_ALL_SIDECAR_PERMISSIONS,
			});

			await client.waitForEvent(
				(event) =>
					event.payload.type === "vm_lifecycle" &&
					event.payload.state === "ready",
				10_000,
			);

			await client.configureVm(session, vm, {
				moduleAccessCwd: join(REPO_ROOT, "packages/core"),
			});

			const modulePackage = JSON.parse(
				new TextDecoder().decode(
					await client.readFile(
						session,
						vm,
						"/root/node_modules/vitest/package.json",
					),
				),
			) as { name: string };
			expect(modulePackage.name).toBe("vitest");

			const writableLayer = await client.createLayer(session, vm);
			const sealedLayer = await client.sealLayer(session, vm, writableLayer);
			const sealedEntries = await client.exportSnapshot(
				session,
				vm,
				sealedLayer,
			);
			expect(sealedEntries.some((entry) => entry.path === "/")).toBe(true);

			const lowerLayer = await client.importSnapshot(session, vm, [
				{
					path: "/workspace",
					kind: "directory",
				},
				{
					path: "/workspace/lower.txt",
					kind: "file",
					content: "lower",
				},
			]);
			const upperLayer = await client.importSnapshot(session, vm, [
				{
					path: "/workspace",
					kind: "directory",
				},
				{
					path: "/workspace/upper.txt",
					kind: "file",
					content: "upper",
				},
			]);
			const overlayLayer = await client.createOverlay(session, vm, {
				lowerLayerIds: [lowerLayer],
				upperLayerId: upperLayer,
			});
			const overlayEntries = await client.exportSnapshot(
				session,
				vm,
				overlayLayer,
			);
			expect(
				overlayEntries.some((entry) => entry.path === "/workspace/lower.txt"),
			).toBe(true);
			expect(
				overlayEntries.some((entry) => entry.path === "/workspace/upper.txt"),
			).toBe(true);
		} finally {
			await client.dispose();
		}
	}, 60_000);

	test("configures native mounts and streams stdin through the real Rust sidecar binary", async () => {
		const fixtureRoot = mkdtempSync(join(tmpdir(), "agent-os-native-sidecar-"));
		const hostMountRoot = mkdtempSync(
			join(tmpdir(), "agent-os-sidecar-host-dir-"),
		);
		cleanupPaths.push(fixtureRoot, hostMountRoot);
		writeFileSync(
			join(fixtureRoot, "stdin-echo.mjs"),
			[
				"process.stdin.setEncoding('utf8');",
				"let buffer = '';",
				"process.stdin.on('data', (chunk) => { buffer += chunk; });",
				"process.stdin.on('end', () => {",
				"  process.stdout.write(`STDIN:${buffer}`);",
				"});",
			].join("\n"),
		);
		writeFileSync(join(hostMountRoot, "existing.txt"), "host-mounted");
		ensureSidecarBinaryReady();

		const client = NativeSidecarProcessClient.spawn({
			cwd: REPO_ROOT,
			command: SIDECAR_BINARY,
			args: [],
			frameTimeoutMs: 20_000,
		});

		try {
			const session = await client.authenticateAndOpenSession();
			const vm = await client.createVm(session, {
				runtime: "java_script",
				metadata: {
					cwd: fixtureRoot,
				},
				rootFilesystem: serializeRootFilesystemForSidecar(),
				permissions: ALLOW_ALL_SIDECAR_PERMISSIONS,
			});

			await client.waitForEvent(
				(event) =>
					event.payload.type === "vm_lifecycle" &&
					event.payload.state === "ready",
				10_000,
			);

			await client.configureVm(session, vm, {
				mounts: [
					serializeMountConfigForSidecar({
						path: "/hostmnt",
						plugin: createHostDirBackend({
							hostPath: hostMountRoot,
							readOnly: false,
						}),
					}),
				],
			});

			expect(
				new TextDecoder().decode(
					await client.readFile(session, vm, "/hostmnt/existing.txt"),
				),
			).toBe("host-mounted");

			await client.writeFile(
				session,
				vm,
				"/hostmnt/generated.txt",
				"from-sidecar",
			);
			expect(readFileSync(join(hostMountRoot, "generated.txt"), "utf8")).toBe(
				"from-sidecar",
			);

			await client.execute(session, vm, {
				processId: "stdin-proc",
				runtime: "java_script",
				entrypoint: "./stdin-echo.mjs",
			});
			await client.writeStdin(
				session,
				vm,
				"stdin-proc",
				"hello through stdin\n",
			);
			await client.closeStdin(session, vm, "stdin-proc");

			const stdout = await client.waitForEvent(
				(event) =>
					event.payload.type === "process_output" &&
					event.payload.process_id === "stdin-proc" &&
					event.payload.channel === "stdout",
				20_000,
			);
			if (stdout.payload.type !== "process_output") {
				throw new Error("expected process_output event");
			}
			expect(stdout.payload.chunk).toContain("STDIN:hello through stdin");

			const exited = await client.waitForEvent(
				(event) =>
					event.payload.type === "process_exited" &&
					event.payload.process_id === "stdin-proc",
				20_000,
			);
			if (exited.payload.type !== "process_exited") {
				throw new Error("expected process_exited event");
			}
			expect(exited.payload.exit_code).toBe(0);
		} finally {
			await client.dispose();
		}
	}, 60_000);

	test("queries listener and UDP through the real sidecar protocol and ignores forged signal-state stderr", async () => {
		const fixtureRoot = mkdtempSync(join(tmpdir(), "agent-os-native-sidecar-"));
		cleanupPaths.push(fixtureRoot);
		writeFileSync(
			join(fixtureRoot, "tcp-listener.mjs"),
			[
				"import net from 'node:net';",
				`const port = Number(process.env.PORT ?? '43111');`,
				"const server = net.createServer(() => {});",
				"server.listen(port, '0.0.0.0', () => {",
				"  console.log(`tcp-listening:${port}`);",
				"});",
			].join("\n"),
		);
		writeFileSync(
			join(fixtureRoot, "udp-listener.mjs"),
			[
				"import dgram from 'node:dgram';",
				`const port = Number(process.env.PORT ?? '43112');`,
				"const socket = dgram.createSocket('udp4');",
				"socket.bind(port, '0.0.0.0', () => {",
				"  console.log(`udp-bound:${port}`);",
				"});",
			].join("\n"),
		);
		writeFileSync(
			join(fixtureRoot, "signal-state.mjs"),
			[
				`const prefix = ${JSON.stringify(SIGNAL_STATE_CONTROL_PREFIX)};`,
				"process.stderr.write(",
				"  `${prefix}${JSON.stringify({",
				"    signal: 2,",
				"    registration: { action: 'user', mask: [15], flags: 0x1234 },",
				"  })}\\n`,",
				");",
				"console.log('signal-registered');",
				"setInterval(() => {}, 1000);",
			].join("\n"),
		);
		ensureSidecarBinaryReady();

		const client = NativeSidecarProcessClient.spawn({
			cwd: REPO_ROOT,
			command: SIDECAR_BINARY,
			args: [],
			frameTimeoutMs: 20_000,
		});

		try {
			const session = await client.authenticateAndOpenSession();
			const vm = await client.createVm(session, {
				runtime: "java_script",
				metadata: {
					cwd: fixtureRoot,
					"env.AGENT_OS_ALLOWED_NODE_BUILTINS": JSON.stringify([
						"net",
						"dgram",
					]),
				},
				rootFilesystem: serializeRootFilesystemForSidecar(),
				permissions: ALLOW_ALL_SIDECAR_PERMISSIONS,
			});

			await client.waitForEvent(
				(event) =>
					event.payload.type === "vm_lifecycle" &&
					event.payload.state === "ready",
				10_000,
			);

			await client.execute(session, vm, {
				processId: "tcp-listener",
				runtime: "java_script",
				entrypoint: "./tcp-listener.mjs",
				env: { PORT: "43111" },
			});

			const listener = await waitFor(
				() =>
					client.findListener(session, vm, {
						host: "0.0.0.0",
						port: 43111,
					}),
				{ isReady: (value) => value !== null },
			);
			expect(listener?.processId).toBe("tcp-listener");

			await client.execute(session, vm, {
				processId: "udp-listener",
				runtime: "java_script",
				entrypoint: "./udp-listener.mjs",
				env: { PORT: "43112" },
			});

			const udpSocket = await waitFor(
				() =>
					client.findBoundUdp(session, vm, {
						host: "0.0.0.0",
						port: 43112,
					}),
				{ isReady: (value) => value !== null },
			);
			expect(udpSocket?.processId).toBe("udp-listener");

			await client.execute(session, vm, {
				processId: "signal-state",
				runtime: "java_script",
				entrypoint: "./signal-state.mjs",
			});
			const signalState = await client.getSignalState(
				session,
				vm,
				"signal-state",
			);
			expect(signalState.handlers.size).toBe(0);

			await client.killProcess(session, vm, "tcp-listener");
			await client.waitForEvent(
				(event) =>
					event.payload.type === "process_exited" &&
					event.payload.process_id === "tcp-listener",
				20_000,
			);
			await client.killProcess(session, vm, "udp-listener");
			await client.waitForEvent(
				(event) =>
					event.payload.type === "process_exited" &&
					event.payload.process_id === "udp-listener",
				20_000,
			);
			await client.killProcess(session, vm, "signal-state");
			await client.waitForEvent(
				(event) =>
					event.payload.type === "process_exited" &&
					event.payload.process_id === "signal-state",
				20_000,
			);
		} finally {
			await client.dispose();
		}
	}, 60_000);

	test("NativeKernel exposes cached socketTable and processTable state from the sidecar", async () => {
		const kernel = createKernel({
			filesystem: createInMemoryFileSystem(),
			permissions: ALLOW_ALL_VM_PERMISSIONS,
		});

		try {
			await kernel.mount(createNodeRuntime());

			let signalStdout = "";
			const tcpServer = kernel.spawn(
				"node",
				[
					"-e",
					[
						"const net = require('net');",
						"const port = 43121;",
						"const server = net.createServer(() => {});",
						"server.listen(port, '0.0.0.0', () => console.log(`tcp:${port}`));",
					].join("\n"),
				],
				{},
			);

			await waitFor(
				() => kernel.socketTable.findListener({ host: "0.0.0.0", port: 43121 }),
				{ isReady: (value) => value !== null },
			);

			const udpServer = kernel.spawn(
				"node",
				[
					"-e",
					[
						"const dgram = require('dgram');",
						"const port = 43122;",
						"const socket = dgram.createSocket('udp4');",
						"socket.bind(port, '0.0.0.0', () => console.log(`udp:${port}`));",
					].join("\n"),
				],
				{},
			);

			await waitFor(
				() => kernel.socketTable.findBoundUdp({ host: "0.0.0.0", port: 43122 }),
				{ isReady: (value) => value !== null },
			);

			const signalProc = kernel.spawn(
				"node",
				[
					"-e",
					[
						`const prefix = ${JSON.stringify(SIGNAL_STATE_CONTROL_PREFIX)};`,
						"process.stderr.write(",
						"  `${prefix}${JSON.stringify({",
						"    signal: 2,",
						"    registration: { action: 'user', mask: [15], flags: 0x4321 },",
						"  })}\\n`,",
						");",
						"console.log('registered');",
						"setTimeout(() => process.exit(0), 25);",
					].join("\n"),
				],
				{
					onStdout: (chunk) => {
						signalStdout += new TextDecoder().decode(chunk);
					},
				},
			);

			await waitFor(() => signalStdout, {
				isReady: (value) => value.includes("registered"),
			});
			expect(
				kernel.processTable.getSignalState(signalProc.pid).handlers.get(2),
			).toBe(undefined);

			tcpServer.kill(15);
			udpServer.kill(15);
			await tcpServer.wait();
			await udpServer.wait();
			await signalProc.wait();
		} finally {
			await kernel.dispose();
		}
	}, 60_000);

	test("delivers SIGSTOP and SIGCONT through killProcess", async () => {
		const fixtureRoot = mkdtempSync(join(tmpdir(), "agent-os-native-sidecar-"));
		cleanupPaths.push(fixtureRoot);
		writeFileSync(
			join(fixtureRoot, "signal-routing.mjs"),
			["console.log('ready');", "setInterval(() => {}, 25);"].join("\n"),
		);
		ensureSidecarBinaryReady();

		const client = NativeSidecarProcessClient.spawn({
			cwd: REPO_ROOT,
			command: SIDECAR_BINARY,
			args: [],
			frameTimeoutMs: 20_000,
		});

		try {
			const session = await client.authenticateAndOpenSession();
			const vm = await client.createVm(session, {
				runtime: "java_script",
				metadata: {
					cwd: fixtureRoot,
				},
				rootFilesystem: serializeRootFilesystemForSidecar(),
				permissions: ALLOW_ALL_SIDECAR_PERMISSIONS,
			});

			await client.waitForEvent(
				(event) =>
					event.payload.type === "vm_lifecycle" &&
					event.payload.state === "ready",
				10_000,
			);

			const started = await client.execute(session, vm, {
				processId: "signal-routing",
				runtime: "java_script",
				entrypoint: "./signal-routing.mjs",
			});
			if (started.pid === null) {
				throw new Error("expected sidecar process to expose a host pid");
			}

			await client.waitForEvent(
				(event) =>
					event.payload.type === "process_output" &&
					event.payload.process_id === "signal-routing" &&
					event.payload.channel === "stdout" &&
					event.payload.chunk.includes("ready"),
				20_000,
			);

			await client.killProcess(session, vm, "signal-routing", "SIGSTOP");
			await waitFor(
				async () =>
					(await client.getProcessSnapshot(session, vm)).find(
						(entry) => entry.processId === "signal-routing",
					)?.status,
				{ isReady: (value) => value === "stopped" },
			);

			await client.killProcess(session, vm, "signal-routing", "SIGCONT");
			await waitFor(
				async () =>
					(await client.getProcessSnapshot(session, vm)).find(
						(entry) => entry.processId === "signal-routing",
					)?.status,
				{ isReady: (value) => value === "running" },
			);

			await client.killProcess(session, vm, "signal-routing", "SIGTERM");
			await client.waitForEvent(
				(event) =>
					event.payload.type === "process_exited" &&
					event.payload.process_id === "signal-routing",
				20_000,
			);
		} finally {
			await client.dispose();
		}
	}, 60_000);

	test("process snapshots retain fast node failure exit codes until the client observes them", async () => {
		ensureSidecarBinaryReady();

		const client = NativeSidecarProcessClient.spawn({
			cwd: REPO_ROOT,
			command: SIDECAR_BINARY,
			args: [],
			frameTimeoutMs: 20_000,
		});

		try {
			const session = await client.authenticateAndOpenSession();
			const vm = await client.createVm(session, {
				runtime: "java_script",
				rootFilesystem: serializeRootFilesystemForSidecar(),
				permissions: ALLOW_ALL_SIDECAR_PERMISSIONS,
			});

			await client.waitForEvent(
				(event) =>
					event.payload.type === "vm_lifecycle" &&
					event.payload.state === "ready",
				10_000,
			);

			await client.mkdir(session, vm, "/app", { recursive: true });

			await client.execute(session, vm, {
				processId: "missing-module",
				command: "node",
				args: ["-e", "require('./nonexistent')"],
				cwd: "/app",
			});

			await client.waitForEvent(
				(event) =>
					event.payload.type === "process_output" &&
					event.payload.process_id === "missing-module" &&
					event.payload.channel === "stderr" &&
					event.payload.chunk.includes("Cannot find module"),
				20_000,
			);

			const snapshot = await waitFor(
				async () =>
					(await client.getProcessSnapshot(session, vm)).find(
						(entry) => entry.processId === "missing-module",
					),
				{
					timeoutMs: 20_000,
					isReady: (entry) =>
						entry?.status === "exited" && entry.exitCode === 1,
				},
			);
			expect(snapshot?.exitCode).toBe(1);

			const exited = await client.waitForEvent(
				(event) =>
					event.payload.type === "process_exited" &&
					event.payload.process_id === "missing-module",
				20_000,
			);
			if (exited.payload.type !== "process_exited") {
				throw new Error("expected process_exited event");
			}
			expect(exited.payload.exit_code).toBe(1);
		} finally {
			await client.dispose();
		}
	}, 60_000);

	test("connectTerminal forwards host stdin and output on the native sidecar path", async () => {
		const kernel = createKernel({
			filesystem: createInMemoryFileSystem(),
			permissions: ALLOW_ALL_VM_PERMISSIONS,
		});

		try {
			await kernel.mount(createNodeRuntime());

			let stdout = "";
			let stdinListener: ((data: Uint8Array | string) => void) | null = null;
			const decoder = new TextDecoder();
			const stdinOn = vi.spyOn(process.stdin, "on").mockImplementation(((
				event,
				listener,
			) => {
				if (event === "data") {
					stdinListener = listener as (data: Uint8Array | string) => void;
				}
				return process.stdin;
			}) as typeof process.stdin.on);
			const stdinRemoveListener = vi
				.spyOn(process.stdin, "removeListener")
				.mockImplementation(((event) => {
					if (event === "data") {
						stdinListener = null;
					}
					return process.stdin;
				}) as typeof process.stdin.removeListener);
			const stdinResume = vi
				.spyOn(process.stdin, "resume")
				.mockImplementation(() => process.stdin);
			const stdinPause = vi
				.spyOn(process.stdin, "pause")
				.mockImplementation(() => process.stdin);
			const stdoutOn = vi
				.spyOn(process.stdout, "on")
				.mockImplementation(
					((event) => process.stdout) as typeof process.stdout.on,
				);
			const stdoutRemoveListener = vi
				.spyOn(process.stdout, "removeListener")
				.mockImplementation(
					((event) => process.stdout) as typeof process.stdout.removeListener,
				);
			const setRawMode =
				typeof process.stdin.setRawMode === "function"
					? vi
							.spyOn(process.stdin, "setRawMode")
							.mockImplementation(() => process.stdin)
					: null;

			const pid = await kernel.connectTerminal({
				command: "node",
				args: [
					"-e",
					[
						"process.stdin.setEncoding('utf8');",
						"process.stdin.once('data', (chunk) => {",
						"  process.stdout.write(`CONNECT:${chunk}`);",
						"  process.exit(0);",
						"});",
					].join("\n"),
				],
				onData: (chunk) => {
					stdout += decoder.decode(chunk);
				},
			});

			expect(pid).toBeGreaterThan(0);
			expect(stdinOn).toHaveBeenCalledWith("data", expect.any(Function));
			expect(stdinResume).toHaveBeenCalled();
			expect(stdoutOn.mock.calls.every(([event]) => event === "resize")).toBe(
				true,
			);

			if (!stdinListener) {
				throw new Error(
					"connectTerminal did not register a stdin data handler",
				);
			}
			stdinListener(Buffer.from("hello-connect-terminal\n"));

			await waitFor(() => stdout, {
				isReady: (value) => value.includes("CONNECT:hello-connect-terminal"),
			});
			await waitFor(() => stdinRemoveListener.mock.calls.length, {
				isReady: (count) => count > 0,
			});

			expect(stdout).toContain("CONNECT:hello-connect-terminal");
			expect(stdinPause).toHaveBeenCalled();
			expect(stdinRemoveListener).toHaveBeenCalledWith(
				"data",
				expect.any(Function),
			);
			expect(
				stdoutRemoveListener.mock.calls.every(([event]) => event === "resize"),
			).toBe(true);
			if (setRawMode) {
				expect(setRawMode).toHaveBeenCalled();
			}
		} finally {
			await kernel.dispose();
		}
	}, 60_000);

	test("openShell keeps stdout and stderr separate on the native sidecar path", async () => {
		const kernel = createKernel({
			filesystem: createInMemoryFileSystem(),
			permissions: ALLOW_ALL_VM_PERMISSIONS,
		});

		try {
			await kernel.mount(createNodeRuntime());

			let stdout = "";
			let stderr = "";
			const decoder = new TextDecoder();
			const shell = kernel.openShell({
				command: "node",
				args: [
					"-e",
					[
						"process.stdin.setEncoding('utf8');",
						"process.stdin.once('data', (chunk) => {",
						"  process.stdout.write(`OUT:${chunk}`);",
						"  process.stderr.write(`ERR:${chunk}`);",
						"  process.exit(0);",
						"});",
					].join("\n"),
				],
				onStderr: (chunk) => {
					stderr += decoder.decode(chunk);
				},
			});

			shell.onData = (chunk) => {
				stdout += decoder.decode(chunk);
			};

			shell.write("hello-shell\n");

			await waitFor(() => stdout, {
				isReady: (value) => value.includes("OUT:hello-shell"),
			});
			await waitFor(() => stderr, {
				isReady: (value) => value.includes("ERR:hello-shell"),
			});

			expect(stdout).toContain("OUT:hello-shell");
			expect(stdout).not.toContain("ERR:hello-shell");
			expect(stderr).toContain("ERR:hello-shell");
			expect(stderr).not.toContain("OUT:hello-shell");
			expect(await shell.wait()).toBe(0);
		} finally {
			await kernel.dispose();
		}
	}, 60_000);
});
