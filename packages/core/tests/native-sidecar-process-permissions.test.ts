import {
	existsSync,
	mkdtempSync,
	readFileSync,
	rmSync,
	writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, test } from "vitest";
import { NativeSidecarProcessClient } from "../src/sidecar/rpc-client.js";

const REPO_ROOT = fileURLToPath(new URL("../../..", import.meta.url));

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

	test("writes declarative permissions policies with child_process wire keys", async () => {
		const fixtureRoot = mkdtempSync(
			join(tmpdir(), "agent-os-sidecar-permissions-"),
		);
		cleanupPaths.push(fixtureRoot);
		const capturePath = join(fixtureRoot, "captured-requests.json");
		const driverPath = join(fixtureRoot, "fake-sidecar.mjs");
		writeFileSync(
			driverPath,
			[
				"import { writeFileSync } from 'node:fs';",
				"const capturePath = process.argv[2];",
				"const schema = { name: 'agent-os-sidecar', version: 1 };",
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
				"      captures.push({ type: frame.payload.type, permissions: frame.payload.permissions });",
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
			frameTimeoutMs: 5_000,
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
				env: {
					rules: [
						{
							mode: "allow" as const,
							patterns: ["PATH", "OPENAI_*"],
						},
					],
				},
			};
			const vm = await client.createVm(session, {
				runtime: "java_script",
				permissions,
			});
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
						child_process: "deny",
						env: permissions.env,
					},
				},
				{
					type: "configure_vm",
					permissions: {
						fs: permissions.fs,
						network: permissions.network,
						child_process: "deny",
						env: permissions.env,
					},
				},
			]);
			expect(
				captured.every((entry) => !("childProcess" in entry.permissions)),
			).toBe(true);
		} finally {
			await client.dispose();
		}
	});
});
