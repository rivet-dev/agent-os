/**
 * Integration test for WasmVM TCP server sockets.
 *
 * Spawns the tcp_server C program as WASM and connects to it from a guest Node
 * process over VM loopback, proving bytes cross the kernel socket table.
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

const WASM_TCP_SERVER = resolve(C_BUILD_DIR, "tcp_server");

function skipReason(): string | false {
	const wasmSkipReason = skipUnlessWasmBuilt();
	if (wasmSkipReason) return wasmSkipReason;
	if (!existsSync(WASM_TCP_SERVER)) {
		return `tcp_server WASM binary not found at ${WASM_TCP_SERVER}`;
	}
	return false;
}

interface RunningGuestProgram {
	process: ReturnType<Kernel["spawn"]>;
	stdoutChunks: Uint8Array[];
	stderrChunks: Uint8Array[];
	getExitCode: () => number | null;
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
	let exitCode: number | null = null;
	const process = kernel.spawn(command, args, {
		onStdout: (chunk) => stdoutChunks.push(chunk),
		onStderr: (chunk) => stderrChunks.push(chunk),
	});
	void process.wait().then((code) => {
		exitCode = code;
	});
	return {
		process,
		stdoutChunks,
		stderrChunks,
		getExitCode: () => exitCode,
	};
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

async function waitForListener(
	kernel: Kernel,
	port: number,
	label: string,
): Promise<void> {
	const deadline = Date.now() + 20_000;
	while (Date.now() < deadline) {
		if (kernel.socketTable.findListener({ host: "0.0.0.0", port })) {
			return;
		}
		await new Promise((resolveWait) => setTimeout(resolveWait, 20));
	}
	throw new Error(`Timed out waiting for ${label} listener on port ${port}`);
}

const TEST_PORT = 9876;

describeIf(!skipReason(), "WasmVM TCP server integration", { timeout: 30_000 }, () => {
	let ctx: IntegrationKernelResult | undefined;

	afterEach(async () => {
		await ctx?.dispose();
		ctx = undefined;
	});

	it("tcp_server: accept connection, recv data, send pong", async () => {
		ctx = await createIntegrationKernel({
			runtimes: ["wasmvm", "node"],
			commandDirs: [C_BUILD_DIR, COMMANDS_DIR],
		});
		const server = spawnGuestProgram(ctx.kernel, "tcp_server", [
			String(TEST_PORT),
		]);
		await waitForListener(ctx.kernel, TEST_PORT, "WASM TCP server");

		const client = await runGuestNodeProgram(
			ctx.kernel,
			[
				"const net = require('net');",
				`const client = net.connect({ host: '127.0.0.1', port: ${TEST_PORT} }, () => client.write('ping'));`,
				"client.on('data', (chunk) => { console.log(chunk.toString()); client.end(); });",
				"client.on('error', (error) => { console.error(error); process.exit(1); });",
			].join("\n"),
		);
		const serverExit = await server.process.wait();

		expect(client.exitCode).toBe(0);
		expect(client.stderr).toBe("");
		expect(client.stdout.trim()).toBe("pong");
		expect(serverExit).toBe(0);
		expect(decodeChunks(server.stdoutChunks)).toContain(
			"listening on port 9876",
		);
		expect(decodeChunks(server.stdoutChunks)).toContain("received: ping");
		expect(decodeChunks(server.stdoutChunks)).toContain("sent: 4");
	});
});
