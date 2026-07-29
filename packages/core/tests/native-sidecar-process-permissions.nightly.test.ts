import {
	existsSync,
	mkdtempSync,
	readFileSync,
	rmSync,
	writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import type { CreateVmConfig } from "@rivet-dev/agentos-runtime-core/vm-config";
import { afterEach, describe, expect, test } from "vitest";
import {
	NativeSidecarProcessClient,
	serializeRootFilesystemForSidecar,
} from "../src/sidecar/rpc-client.js";

const REPO_ROOT = fileURLToPath(new URL("../../..", import.meta.url));
const SIDECAR_BINARY = process.env.AGENTOS_SIDECAR_BIN
	? resolve(process.env.AGENTOS_SIDECAR_BIN)
	: join(REPO_ROOT, "target/debug/agentos-sidecar");

type JavaScriptVmConfigOptions = Partial<
	Pick<
		CreateVmConfig,
		"cwd" | "env" | "jsRuntime" | "loopbackExemptPorts" | "permissions"
	>
> & {
	rootFilesystem?: CreateVmConfig["rootFilesystem"];
};

function createJavaScriptVmOptions(options: JavaScriptVmConfigOptions = {}) {
	return {
		runtime: "java_script" as const,
		config: {
			env: options.env ?? {},
			rootFilesystem:
				options.rootFilesystem ?? serializeRootFilesystemForSidecar(),
			permissions: options.permissions,
			cwd: options.cwd,
			loopbackExemptPorts: options.loopbackExemptPorts ?? [],
			...(options.jsRuntime ? { jsRuntime: options.jsRuntime } : {}),
		} satisfies CreateVmConfig,
	};
}

function nodeBuiltinsConfig(...allowedBuiltins: string[]) {
	return {
		platform: "node" as const,
		moduleResolution: "node" as const,
		allowedBuiltins,
	};
}

function ensureSidecarBinaryReady(): void {
	if (!existsSync(SIDECAR_BINARY)) {
		throw new Error(
			`agentos-sidecar is missing at ${SIDECAR_BINARY}; build the shared CI/test binary before running this suite`,
		);
	}
}

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

describe("native sidecar process client permissions", () => {
	const cleanupPaths: string[] = [];

	afterEach(() => {
		for (const path of cleanupPaths.splice(0)) {
			rmSync(path, { recursive: true, force: true });
		}
	});

	test("writes create-VM config and configure permissions policies", async () => {
		const fixtureRoot = mkdtempSync(
			join(tmpdir(), "agentos-sidecar-permissions-"),
		);
		cleanupPaths.push(fixtureRoot);
		const capturePath = join(fixtureRoot, "captured-requests.json");
		const driverPath = join(fixtureRoot, "fake-sidecar.mjs");
		writeFileSync(
			driverPath,
			[
				"import { writeFileSync } from 'node:fs';",
				"const capturePath = process.argv[2];",
				"const schema = { name: 'agentos-native-sidecar', version: 8 };",
				"let stdinBuffer = Buffer.alloc(0);",
				"const captures = [];",
				"const writeFrame = (frame) => {",
				"  const payload = Buffer.from(JSON.stringify(frame), 'utf8');",
				"  const prefix = Buffer.allocUnsafe(4);",
				"  prefix.writeUInt32BE(payload.length, 0);",
				"  process.stdout.write(Buffer.concat([prefix, payload]));",
				"};",
				"const respond = (requestId, ownership, payload) => {",
				"  writeFrame({ frame_type: 'response', schema, request_id: requestId, ownership, payload });",
				"};",
				"const flushCapture = () => {",
				"  writeFileSync(capturePath, JSON.stringify(captures, null, 2));",
				"};",
				"const handleFrame = (frame) => {",
				"  switch (frame.payload.type) {",
				"    case 'authenticate':",
				"      respond(frame.request_id, { scope: 'connection', connection_id: 'conn-1' }, {",
				"        type: 'authenticated',",
				"        sidecar_id: 'sidecar-1',",
				"        connection_id: 'conn-1',",
				"        max_frame_bytes: 1048576,",
				"      });",
				"      break;",
				"    case 'open_session':",
				"      respond(frame.request_id, { scope: 'connection', connection_id: 'conn-1' }, {",
				"        type: 'session_opened',",
				"        session_id: 'session-1',",
				"        owner_connection_id: 'conn-1',",
				"      });",
				"      break;",
				"    case 'create_vm':",
				"      {",
				"        const config = typeof frame.payload.config === 'string' ? JSON.parse(frame.payload.config) : frame.payload.config;",
				"        captures.push({ type: frame.payload.type, permissions: config.permissions });",
				"      }",
				"      respond(frame.request_id, frame.ownership, { type: 'vm_created', vm_id: 'vm-1' });",
				"      flushCapture();",
				"      break;",
				"    case 'configure_vm':",
				"      captures.push({ type: frame.payload.type, permissions: frame.payload.permissions });",
				"      respond(frame.request_id, frame.ownership, {",
				"        type: 'vm_configured',",
				"        applied_mounts: 0,",
				"        applied_software: 0,",
				"      });",
				"      flushCapture();",
				"      setTimeout(() => process.exit(0), 25);",
				"      break;",
				"    default:",
				"      throw new Error(`unexpected payload type: ${frame.payload.type}`);",
				"  }",
				"};",
				"const drain = () => {",
				"  while (stdinBuffer.length >= 4) {",
				"    const length = stdinBuffer.readUInt32BE(0);",
				"    if (stdinBuffer.length < 4 + length) return;",
				"    const frame = JSON.parse(stdinBuffer.subarray(4, 4 + length).toString('utf8'));",
				"    stdinBuffer = stdinBuffer.subarray(4 + length);",
				"    handleFrame(frame);",
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
			args: [driverPath, capturePath],
			payloadCodec: "json",
		});

		try {
			const session = await client.authenticateAndOpenSession();
			const permissions = {
				fs: {
					default: "deny" as const,
					rules: [
						{
							mode: "allow" as const,
							operations: ["read"],
							paths: ["/workspace/**"],
						},
					],
				},
				network: {
					default: "deny" as const,
					rules: [
						{
							mode: "allow" as const,
							operations: ["dns"],
							patterns: ["dns://*.example.test"],
						},
					],
				},
				childProcess: "deny" as const,
				process: {
					default: "deny" as const,
					rules: [
						{
							mode: "allow" as const,
							operations: ["inspect"],
							patterns: ["**"],
						},
					],
				},
				env: {
					rules: [
						{
							mode: "allow" as const,
							patterns: ["PATH", "OPENAI_*"],
						},
					],
				},
			};
			const vm = await client.createVm(
				session,
				createJavaScriptVmOptions({
					permissions,
				}),
			);
			await client.configureVm(session, vm, {
				permissions,
			});

			const captured = await waitFor(
				() => {
					if (!existsSync(capturePath)) {
						return null;
					}
					return JSON.parse(readFileSync(capturePath, "utf8")) as Array<{
						type: string;
						permissions: {
							fs?: unknown;
							network?: unknown;
							child_process?: unknown;
							childProcess?: unknown;
							process?: unknown;
							env?: unknown;
						};
					}>;
				},
				{ isReady: (value) => value !== null && value.length === 2 },
			);

			expect(captured).toEqual([
				{
					type: "create_vm",
					permissions: {
						fs: permissions.fs,
						network: permissions.network,
						childProcess: "deny",
						process: permissions.process,
						env: permissions.env,
					},
				},
				{
					type: "configure_vm",
					permissions: {
						fs: permissions.fs,
						network: permissions.network,
						child_process: "deny",
						process: permissions.process,
						env: permissions.env,
					},
				},
			]);
			const createCapture = captured[0];
			const configureCapture = captured[1];
			if (!createCapture || !configureCapture) {
				throw new Error("expected create_vm and configure_vm permission captures");
			}
			expect("child_process" in createCapture.permissions).toBe(false);
			expect("childProcess" in configureCapture.permissions).toBe(false);
		} finally {
			await client.dispose();
		}
	});

	test("rejects empty permission rule operations and paths in the native sidecar", async () => {
		ensureSidecarBinaryReady();

		const client = NativeSidecarProcessClient.spawn({
			cwd: REPO_ROOT,
			command: SIDECAR_BINARY,
			args: [],
		});

		try {
			const session = await client.authenticateAndOpenSession();

			await expect(
				client.createVm(
					session,
					createJavaScriptVmOptions({
						permissions: {
							fs: {
								default: "deny",
								rules: [
									{
										mode: "allow",
										operations: [],
										paths: ["*"],
									},
								],
							},
						},
					}),
				),
			).rejects.toThrow(
				/invalid_state: .*fs\.rules\[0\]\.operations must not be empty/,
			);

			await expect(
				client.createVm(
					session,
					createJavaScriptVmOptions({
						permissions: {
							fs: {
								default: "deny",
								rules: [
									{
										mode: "allow",
										operations: ["read"],
										paths: [],
									},
								],
							},
						},
					}),
				),
			).rejects.toThrow(
				/invalid_state: .*fs\.rules\[0\]\.paths must not be empty/,
			);
		} finally {
			await client.dispose();
		}
	});

	test("inspection RPCs are denied by default and allowed with explicit inspect permissions", async () => {
		const fixtureRoot = mkdtempSync(
			join(tmpdir(), "agentos-sidecar-inspection-permissions-"),
		);
		cleanupPaths.push(fixtureRoot);
		ensureSidecarBinaryReady();

		writeFileSync(
			join(fixtureRoot, "tcp-listener.mjs"),
			[
				"import net from 'node:net';",
				"const port = Number(process.env.PORT ?? '43111');",
				"const server = net.createServer(() => {});",
				"server.listen(port, '0.0.0.0', () => console.log(`tcp-listening:${port}`));",
			].join("\n"),
		);
		writeFileSync(
			join(fixtureRoot, "udp-listener.mjs"),
			[
				"import dgram from 'node:dgram';",
				"const port = Number(process.env.PORT ?? '43112');",
				"const socket = dgram.createSocket('udp4');",
				"socket.bind(port, '0.0.0.0', () => console.log(`udp-bound:${port}`));",
			].join("\n"),
		);
		writeFileSync(
			join(fixtureRoot, "idle.mjs"),
			["console.log('idle-ready');", "setInterval(() => {}, 1000);"].join("\n"),
		);

		const client = NativeSidecarProcessClient.spawn({
			cwd: REPO_ROOT,
			command: SIDECAR_BINARY,
			args: [],
		});

		try {
			const session = await client.authenticateAndOpenSession();

			const deniedVm = await client.createVm(
				session,
				createJavaScriptVmOptions({
					cwd: fixtureRoot,
					jsRuntime: nodeBuiltinsConfig("net", "dgram"),
					permissions: {
						fs: "allow",
						network: {
							default: "deny",
							rules: [
								{
									mode: "allow",
									operations: ["listen"],
									patterns: ["**"],
								},
							],
						},
						childProcess: "allow",
						process: "deny",
						env: "allow",
					},
				}),
			);

			await client.waitForEvent(
				(event) =>
					event.payload.type === "vm_lifecycle" &&
					event.payload.state === "ready" &&
					event.ownership.scope === "vm" &&
					event.ownership.vm_id === deniedVm.vmId,
				10_000,
			);

			await client.execute(session, deniedVm, {
				processId: "tcp-listener-denied",
				runtime: "java_script",
				entrypoint: "./tcp-listener.mjs",
				env: { PORT: "43111" },
			});
			await client.waitForEvent(
				(event) =>
					event.ownership.scope === "vm" &&
					event.ownership.vm_id === deniedVm.vmId &&
					event.payload.type === "process_output" &&
					event.payload.process_id === "tcp-listener-denied" &&
					event.payload.chunk.includes("tcp-listening:43111"),
				10_000,
			);

			await client.execute(session, deniedVm, {
				processId: "udp-listener-denied",
				runtime: "java_script",
				entrypoint: "./udp-listener.mjs",
				env: { PORT: "43112" },
			});
			await client.waitForEvent(
				(event) =>
					event.ownership.scope === "vm" &&
					event.ownership.vm_id === deniedVm.vmId &&
					event.payload.type === "process_output" &&
					event.payload.process_id === "udp-listener-denied" &&
					event.payload.chunk.includes("udp-bound:43112"),
				10_000,
			);

			await client.execute(session, deniedVm, {
				processId: "idle-denied",
				runtime: "java_script",
				entrypoint: "./idle.mjs",
			});
			await client.waitForEvent(
				(event) =>
					event.ownership.scope === "vm" &&
					event.ownership.vm_id === deniedVm.vmId &&
					event.payload.type === "process_output" &&
					event.payload.process_id === "idle-denied" &&
					event.payload.chunk.includes("idle-ready"),
				10_000,
			);

			await expect(
				client.findListener(session, deniedVm, {
					host: "0.0.0.0",
					port: 43111,
				}),
			).rejects.toThrow(/network\.inspect/);
			await expect(
				client.findBoundUdp(session, deniedVm, {
					host: "0.0.0.0",
					port: 43112,
				}),
			).rejects.toThrow(/network\.inspect/);
			await expect(
				client.getProcessSnapshot(session, deniedVm),
			).rejects.toThrow(/process\.inspect/);

			const allowedVm = await client.createVm(
				session,
				createJavaScriptVmOptions({
					cwd: fixtureRoot,
					jsRuntime: nodeBuiltinsConfig("net", "dgram"),
					permissions: {
						fs: "allow",
						network: {
							default: "deny",
							rules: [
								{
									mode: "allow",
									operations: ["listen"],
									patterns: ["**"],
								},
								{
									mode: "allow",
									operations: ["inspect"],
									patterns: ["**"],
								},
							],
						},
						childProcess: "allow",
						process: {
							default: "deny",
							rules: [
								{
									mode: "allow",
									operations: ["inspect"],
									patterns: ["**"],
								},
							],
						},
						env: "allow",
					},
				}),
			);

			await client.waitForEvent(
				(event) =>
					event.payload.type === "vm_lifecycle" &&
					event.payload.state === "ready" &&
					event.ownership.scope === "vm" &&
					event.ownership.vm_id === allowedVm.vmId,
				10_000,
			);

			await client.execute(session, allowedVm, {
				processId: "tcp-listener-allowed",
				runtime: "java_script",
				entrypoint: "./tcp-listener.mjs",
				env: { PORT: "43121" },
			});
			await client.waitForEvent(
				(event) =>
					event.ownership.scope === "vm" &&
					event.ownership.vm_id === allowedVm.vmId &&
					event.payload.type === "process_output" &&
					event.payload.process_id === "tcp-listener-allowed" &&
					event.payload.chunk.includes("tcp-listening:43121"),
				10_000,
			);

			await client.execute(session, allowedVm, {
				processId: "udp-listener-allowed",
				runtime: "java_script",
				entrypoint: "./udp-listener.mjs",
				env: { PORT: "43122" },
			});
			await client.waitForEvent(
				(event) =>
					event.ownership.scope === "vm" &&
					event.ownership.vm_id === allowedVm.vmId &&
					event.payload.type === "process_output" &&
					event.payload.process_id === "udp-listener-allowed" &&
					event.payload.chunk.includes("udp-bound:43122"),
				10_000,
			);

			await client.execute(session, allowedVm, {
				processId: "idle-allowed",
				runtime: "java_script",
				entrypoint: "./idle.mjs",
			});
			await client.waitForEvent(
				(event) =>
					event.ownership.scope === "vm" &&
					event.ownership.vm_id === allowedVm.vmId &&
					event.payload.type === "process_output" &&
					event.payload.process_id === "idle-allowed" &&
					event.payload.chunk.includes("idle-ready"),
				10_000,
			);

			expect(
				await waitFor(
					() =>
						client.findListener(session, allowedVm, {
							host: "0.0.0.0",
							port: 43121,
						}),
					{ isReady: (value) => value !== null },
				),
			).toMatchObject({ processId: "tcp-listener-allowed", port: 43121 });
			expect(
				await waitFor(
					() =>
						client.findBoundUdp(session, allowedVm, {
							host: "0.0.0.0",
							port: 43122,
						}),
					{ isReady: (value) => value !== null },
				),
			).toMatchObject({ processId: "udp-listener-allowed", port: 43122 });
			expect(
				(await client.getProcessSnapshot(session, allowedVm)).map(
					(entry) => entry.processId,
				),
			).toContain("idle-allowed");
		} finally {
			await client.dispose();
		}
	});

	test("keeps single-star fs permission globs within one path segment", async () => {
		const fixtureRoot = mkdtempSync(
			join(tmpdir(), "agentos-sidecar-permission-glob-"),
		);
		cleanupPaths.push(fixtureRoot);
		ensureSidecarBinaryReady();

		const client = NativeSidecarProcessClient.spawn({
			cwd: REPO_ROOT,
			command: SIDECAR_BINARY,
			args: [],
		});

		try {
			const session = await client.authenticateAndOpenSession();
			const vm = await client.createVm(
				session,
				createJavaScriptVmOptions({
					cwd: fixtureRoot,
					permissions: {
						fs: "allow",
					},
				}),
			);

			await client.waitForEvent(
				(event) =>
					event.payload.type === "vm_lifecycle" &&
					event.payload.state === "ready",
				10_000,
			);

			await client.bootstrapRootFilesystem(session, vm, [
				{
					path: "/workspace",
					kind: "directory",
				},
				{
					path: "/workspace/top.txt",
					kind: "file",
					content: "top-level",
				},
				{
					path: "/workspace/nested",
					kind: "directory",
				},
				{
					path: "/workspace/nested/blocked.txt",
					kind: "file",
					content: "nested",
				},
			]);

			await client.configureVm(session, vm, {
				permissions: {
					fs: {
						default: "deny",
						rules: [
							{
								mode: "allow",
								operations: ["read"],
								paths: ["/workspace/*"],
							},
						],
					},
				},
			});

			expect(
				new TextDecoder().decode(
					await client.readFile(session, vm, "/workspace/top.txt"),
				),
			).toBe("top-level");

			await expect(
				client.readFile(session, vm, "/workspace/nested/blocked.txt"),
			).rejects.toThrow("EACCES");
		} finally {
			await client.dispose();
		}
	});
});
