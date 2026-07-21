import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, test } from "vitest";
import { StdioSidecarProtocolClient } from "../src/native-client.js";
import { SidecarProcess } from "../src/sidecar-process.js";

const ownership = {
	scope: "connection" as const,
	connection_id: "conn",
};

describe("stdio sidecar protocol client", () => {
	test("drives a stdio protocol process", async () => {
		const fixtureRoot = mkdtempSync(join(tmpdir(), "agentos-client-"));
		const driverPath = join(fixtureRoot, "fake-sidecar.mjs");
		writeFileSync(
			driverPath,
			[
				"import { Socket } from 'node:net';",
				"const control = new Socket({ fd: 3, readable: true, writable: true });",
				"let stdinBuffer = Buffer.alloc(0);",
				"const writeFrame = (stream, frame) => {",
				"  const payload = Buffer.from(JSON.stringify(frame));",
				"  const header = Buffer.alloc(4);",
				"  header.writeUInt32BE(payload.length, 0);",
				"  stream.write(Buffer.concat([header, payload]));",
				"};",
				"const handleFrame = (frame) => {",
				"  if (frame.payload.type !== 'create_layer') {",
				"    throw new Error('unexpected payload ' + frame.payload.type);",
				"  }",
				"  writeFrame(control, {",
				"    frame_type: 'response',",
				"    schema: frame.schema,",
				"    request_id: frame.request_id,",
				"    ownership: frame.ownership,",
				"    payload: { type: 'layer_created', layer_id: 'layer-1' },",
				"  });",
				"  writeFrame(process.stdout, {",
				"    frame_type: 'event',",
				"    schema: frame.schema,",
				"    ownership: frame.ownership,",
				"    payload: { type: 'structured', name: 'ready', detail: {} },",
				"  });",
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

		const client = StdioSidecarProtocolClient.spawn({
			command: process.execPath,
			args: [driverPath],
			eventBufferCapacity: 8,
			payloadCodec: "json",
		});

		try {
			const ready = client.waitForEvent({
				type: "structured",
				name: "ready",
			});
			await expect(
				client.sendRequest({
					ownership,
					payload: { type: "create_layer" },
				}),
			).resolves.toMatchObject({
				payload: { type: "layer_created", layer_id: "layer-1" },
			});
			await expect(ready).resolves.toMatchObject({
				payload: { type: "structured", name: "ready" },
			});
		} finally {
			await client.dispose();
		}
	});

	test("registers native spawn for the shared SidecarProcess wrapper", async () => {
		const fixtureRoot = mkdtempSync(join(tmpdir(), "agentos-process-"));
		const driverPath = join(fixtureRoot, "fake-sidecar.mjs");
		writeFileSync(
			driverPath,
			[
				"import { Socket } from 'node:net';",
				"const control = new Socket({ fd: 3, readable: true, writable: true });",
				"let stdinBuffer = Buffer.alloc(0);",
				"const writeFrame = (stream, frame) => {",
				"  const payload = Buffer.from(JSON.stringify(frame));",
				"  const header = Buffer.alloc(4);",
				"  header.writeUInt32BE(payload.length, 0);",
				"  stream.write(Buffer.concat([header, payload]));",
				"};",
				"const handleFrame = (frame) => {",
				"  writeFrame(control, {",
				"    frame_type: 'response',",
				"    schema: frame.schema,",
				"    request_id: frame.request_id,",
				"    ownership: frame.ownership,",
				"    payload: { type: 'layer_created', layer_id: 'layer-1' },",
				"  });",
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

		const sidecarProcess = SidecarProcess.spawn({
			command: process.execPath,
			args: [driverPath],
			eventBufferCapacity: 8,
			payloadCodec: "json",
		});

		try {
			await expect(
				sidecarProcess.createLayer(
					{ connectionId: "conn", sessionId: "session" },
					{ vmId: "vm" },
				),
			).resolves.toBe("layer-1");
		} finally {
			await sidecarProcess.dispose();
		}
	});
});
