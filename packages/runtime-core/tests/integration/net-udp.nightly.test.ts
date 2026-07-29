/**
 * Integration test for WasmVM UDP sockets.
 *
 * Spawns the udp_echo C program as WASM and sends datagrams from a guest Node
 * process over VM loopback.
 */

import { afterEach, describe, expect, it } from "vitest";
import { existsSync } from "node:fs";
import { resolve } from "node:path";
import {
	C_BUILD_DIR,
	COMMANDS_DIR,
	createIntegrationKernel,
	describeIf,
	skipUnlessWasmBuilt,
} from "@rivet-dev/agentos-vm-test-harness";
import type {
	IntegrationKernelResult,
	Kernel,
} from "@rivet-dev/agentos-vm-test-harness";

const WASM_UDP_ECHO = resolve(C_BUILD_DIR, "udp_echo");

function skipReason(): string | false {
	const wasmSkipReason = skipUnlessWasmBuilt();
	if (wasmSkipReason) return wasmSkipReason;
	if (!existsSync(WASM_UDP_ECHO)) {
		return `udp_echo WASM binary not found at ${WASM_UDP_ECHO}`;
	}
	return false;
}

interface RunningGuestProgram {
	process: ReturnType<Kernel["spawn"]>;
	stdoutChunks: Uint8Array[];
	stderrChunks: Uint8Array[];
}

function decodeChunks(chunks: Uint8Array[]): string {
	return chunks.map((chunk) => new TextDecoder().decode(chunk)).join("");
}

function spawnGuestProgram(
	kernel: Kernel,
	command: string,
	args: string[],
): RunningGuestProgram {
	const stdoutChunks: Uint8Array[] = [];
	const stderrChunks: Uint8Array[] = [];
	const process = kernel.spawn(command, args, {
		onStdout: (chunk) => stdoutChunks.push(chunk),
		onStderr: (chunk) => stderrChunks.push(chunk),
	});
	return { process, stdoutChunks, stderrChunks };
}

async function runGuestNodeProgram(
	kernel: Kernel,
	code: string,
): Promise<{ exitCode: number; stdout: string; stderr: string }> {
	const program = spawnGuestProgram(kernel, "node", ["-e", code]);
	const exitCode = await program.process.wait();
	return {
		exitCode,
		stdout: decodeChunks(program.stdoutChunks),
		stderr: decodeChunks(program.stderrChunks),
	};
}

async function waitForUdpBinding(kernel: Kernel, port: number): Promise<void> {
	const deadline = Date.now() + 20_000;
	while (Date.now() < deadline) {
		if (kernel.socketTable.findBoundUdp({ host: "0.0.0.0", port })) {
			return;
		}
		await new Promise((resolveWait) => setTimeout(resolveWait, 20));
	}
	throw new Error(`Timed out waiting for UDP binding on port ${port}`);
}

async function runUdpEchoCase(
	kernel: Kernel,
	port: number,
	message: string,
): Promise<{ client: { exitCode: number; stdout: string; stderr: string }; server: RunningGuestProgram }> {
	const server = spawnGuestProgram(kernel, "udp_echo", [String(port)]);
	await waitForUdpBinding(kernel, port);

	const client = await runGuestNodeProgram(
		kernel,
		[
			"const dgram = require('dgram');",
			"const client = dgram.createSocket('udp4');",
			`const payload = Buffer.from(${JSON.stringify(message)});`,
			"const timer = setTimeout(() => { console.error('timeout'); client.close(); process.exit(1); }, 5000);",
			"client.on('message', (msg) => { clearTimeout(timer); console.log(msg.toString()); client.close(); });",
			"client.on('error', (error) => { clearTimeout(timer); console.error(error); client.close(); process.exit(1); });",
			`client.bind(0, '127.0.0.1', () => client.send(payload, ${port}, '127.0.0.1'));`,
		].join("\n"),
	);

	return { client, server };
}

const TEST_PORT = 9877;

describeIf(!skipReason(), "WasmVM UDP integration", { timeout: 30_000 }, () => {
	let ctx: IntegrationKernelResult | undefined;

	afterEach(async () => {
		await ctx?.dispose();
		ctx = undefined;
	});

	it("udp_echo: recv datagram and echo it back", async () => {
		ctx = await createIntegrationKernel({
			runtimes: ["wasmvm", "node"],
			commandDirs: [C_BUILD_DIR, COMMANDS_DIR],
		});

		const { client, server } = await runUdpEchoCase(
			ctx.kernel,
			TEST_PORT,
			"hello",
		);
		const serverExit = await server.process.wait();

		expect(client.exitCode).toBe(0);
		expect(client.stderr).toBe("");
		expect(client.stdout.trim()).toBe("hello");
		expect(serverExit).toBe(0);
		expect(decodeChunks(server.stdoutChunks)).toContain(
			"listening on port 9877",
		);
		expect(decodeChunks(server.stdoutChunks)).toContain("received: hello");
		expect(decodeChunks(server.stdoutChunks)).toContain("echoed: 5");
	});

	it("udp_echo: message boundaries are preserved", async () => {
		ctx = await createIntegrationKernel({
			runtimes: ["wasmvm", "node"],
			commandDirs: [C_BUILD_DIR, COMMANDS_DIR],
		});

		const { client, server } = await runUdpEchoCase(
			ctx.kernel,
			TEST_PORT + 1,
			"boundary-test-message",
		);
		const serverExit = await server.process.wait();

		expect(client.exitCode).toBe(0);
		expect(client.stderr).toBe("");
		expect(client.stdout.trim()).toBe("boundary-test-message");
		expect(serverExit).toBe(0);
		expect(decodeChunks(server.stdoutChunks)).toContain(
			"received: boundary-test-message",
		);
	});
});
