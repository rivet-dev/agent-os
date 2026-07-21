/**
 * Cross-runtime network integration matrix.
 *
 * These tests intentionally avoid host loopback exemptions for VM-local rows.
 * A passing row means bytes crossed the kernel socket table between the named
 * client and listener runtimes.
 */

import { describe, it, expect, afterEach } from "vitest";
import { existsSync } from "node:fs";
import { createServer as createHttpServer } from "node:http";
import { resolve } from "node:path";
import {
	COMMANDS_DIR,
	C_BUILD_DIR,
	createIntegrationKernel,
	itIf,
	skipUnlessWasmBuilt,
} from "@rivet-dev/agentos-vm-test-harness";
import type {
	IntegrationKernelResult,
	Kernel,
} from "@rivet-dev/agentos-vm-test-harness";

const WASM_CURL = resolve(C_BUILD_DIR, "curl");
const WASM_HTTP_SERVER = resolve(C_BUILD_DIR, "http_server");
const WASM_TCP_ECHO = resolve(C_BUILD_DIR, "tcp_echo");
const WASM_TCP_SERVER = resolve(C_BUILD_DIR, "tcp_server");

function skipReasonWasmNetwork(): string | false {
	const wasmSkipReason = skipUnlessWasmBuilt();
	if (wasmSkipReason) return wasmSkipReason;
	for (const [name, path] of [
		["curl", WASM_CURL],
		["http_server", WASM_HTTP_SERVER],
		["tcp_echo", WASM_TCP_ECHO],
		["tcp_server", WASM_TCP_SERVER],
	] as const) {
		if (!existsSync(path)) {
			return `${name} WASM binary not found at ${path} - rebuild registry C command artifacts`;
		}
	}
	return false;
}

const wasmNetworkSkipReason = skipReasonWasmNetwork();

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

function spawnGuestNodeProgram(
	kernel: Kernel,
	code: string,
): RunningGuestProgram {
	return spawnGuestProgram(kernel, "node", ["-e", code]);
}

async function runGuestNodeProgram(
	kernel: Kernel,
	code: string,
): Promise<{ exitCode: number; stdout: string; stderr: string }> {
	const program = spawnGuestNodeProgram(kernel, code);
	const exitCode = await program.process.wait();
	return {
		exitCode,
		stdout: decodeChunks(program.stdoutChunks),
		stderr: decodeChunks(program.stderrChunks),
	};
}

async function waitForOutput(
	program: RunningGuestProgram,
	needle: string,
	label: string,
): Promise<void> {
	const deadline = Date.now() + 20_000;
	while (Date.now() < deadline) {
		const stdout = decodeChunks(program.stdoutChunks);
		if (stdout.includes(needle)) {
			return;
		}
		if (program.getExitCode() !== null) {
			throw new Error(
				`${label} exited before ${JSON.stringify(needle)}\nstdout:\n${stdout}\nstderr:\n${decodeChunks(program.stderrChunks)}`,
			);
		}
		await new Promise((resolveWait) => setTimeout(resolveWait, 20));
	}
	throw new Error(
		`Timed out waiting for ${label} to print ${JSON.stringify(needle)}\nstdout:\n${decodeChunks(program.stdoutChunks)}\nstderr:\n${decodeChunks(program.stderrChunks)}`,
	);
}

async function waitForListener(
	kernel: Kernel,
	port: number,
	label: string,
	program?: RunningGuestProgram,
): Promise<void> {
	const deadline = Date.now() + 20_000;
	while (Date.now() < deadline) {
		if (kernel.socketTable.findListener({ host: "0.0.0.0", port })) {
			return;
		}
		if (program && program.getExitCode() !== null) {
			throw new Error(
				`${label} exited before opening port ${port}\nstdout:\n${decodeChunks(program.stdoutChunks)}\nstderr:\n${decodeChunks(program.stderrChunks)}`,
			);
		}
		await new Promise((resolveWait) => setTimeout(resolveWait, 20));
	}
	throw new Error(`Timed out waiting for ${label} listener on port ${port}`);
}

function parseVmFetchResponse(responseJson: string): {
	status: number;
	body: string;
} {
	const parsed = JSON.parse(responseJson) as {
		status?: number;
		body?: string;
		bodyEncoding?: string;
	};
	let body = parsed.body ?? "";
	if (parsed.bodyEncoding === "base64" && body.length > 0) {
		body = Buffer.from(body, "base64").toString("utf8");
	}
	return { status: parsed.status ?? 0, body };
}

function guestJsHttpServer(port: number): string {
	return `
const http = require('http');
const server = http.createServer((req, res) => {
  res.writeHead(200, { 'content-type': 'text/plain' });
  res.end('js:' + req.method + ':' + req.url);
});
server.listen(${port}, '127.0.0.1', () => {
  console.log('js http listening ${port}');
});
`;
}

function guestJsTcpServer(port: number): string {
	return `
const net = require('net');
const server = net.createServer((socket) => {
  socket.on('data', (chunk) => {
    socket.end('js-pong:' + chunk.toString());
  });
});
server.listen(${port}, '127.0.0.1', () => {
  console.log('js tcp listening ${port}');
});
`;
}

describe("cross-runtime network integration", { timeout: 90_000 }, () => {
	let ctx: IntegrationKernelResult;

	afterEach(async () => {
		await ctx?.dispose();
	});

	it("J1 JS fetch -> JS node:http server over VM loopback", async () => {
		ctx = await createIntegrationKernel({
			runtimes: ["node"],
		});
		const server = spawnGuestNodeProgram(ctx.kernel, guestJsHttpServer(3101));
		await waitForOutput(server, "js http listening 3101", "JS HTTP server");

		const client = await runGuestNodeProgram(
			ctx.kernel,
			[
				"fetch('http://127.0.0.1:3101/from-js')",
				"  .then(async (res) => console.log(res.status + ':' + await res.text()))",
				"  .catch((error) => { console.error(error); process.exit(1); });",
			].join("\n"),
		);

		server.process.kill(15);
		await server.process.wait().catch(() => {});
		expect(client.exitCode).toBe(0);
		expect(client.stderr).toBe("");
		expect(client.stdout.trim()).toBe("200:js:GET:/from-js");
	});

	it("J2 JS net.connect -> JS net.Server over VM loopback", async () => {
		ctx = await createIntegrationKernel({
			runtimes: ["node"],
		});
		const server = spawnGuestNodeProgram(ctx.kernel, guestJsTcpServer(3105));
		await waitForListener(ctx.kernel, 3105, "JS TCP server", server);

		const client = await runGuestNodeProgram(
			ctx.kernel,
			[
				"const net = require('net');",
				"const client = net.connect({ host: '127.0.0.1', port: 3105 }, () => client.write('ping'));",
				"client.on('data', (chunk) => { console.log(chunk.toString()); client.end(); });",
				"client.on('error', (error) => { console.error(error); process.exit(1); });",
			].join("\n"),
		);

		server.process.kill(15);
		await server.process.wait().catch(() => {});
		expect(client.exitCode).toBe(0);
		expect(client.stderr).toBe("");
		expect(client.stdout.trim()).toBe("js-pong:ping");
	});

	itIf(
		!wasmNetworkSkipReason,
		"W1 WASM curl -> JS node:http server over VM loopback",
		async () => {
			ctx = await createIntegrationKernel({
				runtimes: ["wasmvm", "node"],
				commandDirs: [C_BUILD_DIR, COMMANDS_DIR],
			});
			const server = spawnGuestNodeProgram(ctx.kernel, guestJsHttpServer(3102));
			await waitForOutput(server, "js http listening 3102", "JS HTTP server");

			const wasm = await ctx.kernel.exec(
				"curl -fsS http://127.0.0.1:3102/from-wasm",
			);

			server.process.kill(15);
			await server.process.wait().catch(() => {});
			expect(wasm.exitCode).toBe(0);
			expect(wasm.stderr).toBe("");
			expect(wasm.stdout.trim()).toBe("js:GET:/from-wasm");
		},
	);

	itIf(
		!wasmNetworkSkipReason,
		"J3 JS fetch -> WASM HTTP server over VM loopback",
		async () => {
			ctx = await createIntegrationKernel({
				runtimes: ["wasmvm", "node"],
				commandDirs: [C_BUILD_DIR, COMMANDS_DIR],
			});
			const server = spawnGuestProgram(ctx.kernel, "http_server", ["3103"]);
			await waitForListener(ctx.kernel, 3103, "WASM HTTP server", server);

			const client = await runGuestNodeProgram(
				ctx.kernel,
				[
					"fetch('http://127.0.0.1:3103/from-js')",
					"  .then(async (res) => console.log(res.status + ':' + await res.text()))",
					"  .catch((error) => { console.error(error); process.exit(1); });",
				].join("\n"),
			);
			const serverExit = await server.process.wait();

			expect(client.exitCode).toBe(0);
			expect(client.stderr).toBe("");
			expect(client.stdout.trim()).toBe("200:wasm:GET:/from-js");
			expect(serverExit).toBe(0);
			expect(decodeChunks(server.stdoutChunks)).toContain(
				"received request: GET /from-js",
			);
		},
	);

	itIf(
		!wasmNetworkSkipReason,
		"J4 JS net.connect -> WASM TCP server over VM loopback",
		async () => {
			ctx = await createIntegrationKernel({
				runtimes: ["wasmvm", "node"],
				commandDirs: [C_BUILD_DIR, COMMANDS_DIR],
			});
			const server = spawnGuestProgram(ctx.kernel, "tcp_server", ["3106"]);
			await waitForListener(ctx.kernel, 3106, "WASM TCP server", server);

			const client = await runGuestNodeProgram(
				ctx.kernel,
				[
					"const net = require('net');",
					"const client = net.connect({ host: '127.0.0.1', port: 3106 }, () => client.write('ping'));",
					"client.on('data', (chunk) => { console.log(chunk.toString()); client.end(); });",
					"client.on('error', (error) => { console.error(error); process.exit(1); });",
				].join("\n"),
			);
			const serverExit = await server.process.wait();

			expect(client.exitCode).toBe(0);
			expect(client.stderr).toBe("");
			expect(client.stdout.trim()).toBe("pong");
			expect(serverExit).toBe(0);
			expect(decodeChunks(server.stdoutChunks)).toContain("received: ping");
		},
	);

	itIf(
		!wasmNetworkSkipReason,
		"H2 host vmFetch -> WASM HTTP server over VM loopback",
		async () => {
			ctx = await createIntegrationKernel({
				runtimes: ["wasmvm", "node"],
				commandDirs: [C_BUILD_DIR, COMMANDS_DIR],
			});
			const server = spawnGuestProgram(ctx.kernel, "http_server", ["3104"]);
			await waitForListener(ctx.kernel, 3104, "WASM HTTP server", server);

			const response = parseVmFetchResponse(
				await ctx.kernel.vmFetch({
					port: 3104,
					method: "GET",
					path: "/from-host",
					headersJson: JSON.stringify({}),
				}),
			);
			const serverExit = await server.process.wait();

			expect(response.status).toBe(200);
			expect(response.body).toBe("wasm:GET:/from-host");
			expect(serverExit).toBe(0);
			expect(decodeChunks(server.stdoutChunks)).toContain(
				"received request: GET /from-host",
			);
		},
	);

	itIf(
		!wasmNetworkSkipReason,
		"W2 WASM tcp_echo -> JS net.Server over VM loopback",
		async () => {
			ctx = await createIntegrationKernel({
				runtimes: ["wasmvm", "node"],
				commandDirs: [C_BUILD_DIR, COMMANDS_DIR],
			});
			const server = spawnGuestNodeProgram(ctx.kernel, guestJsTcpServer(3107));
			await waitForListener(ctx.kernel, 3107, "JS TCP server", server);

			const wasm = await ctx.kernel.exec("tcp_echo 3107");

			server.process.kill(15);
			await server.process.wait().catch(() => {});
			expect(wasm.exitCode).toBe(0);
			expect(wasm.stderr).not.toContain("socket error");
			expect(wasm.stdout).toContain("sent: 5");
			expect(wasm.stdout).toContain("received: js-pong:hello");
		},
	);

	itIf(
		!wasmNetworkSkipReason,
		"W3 WASM curl -> WASM HTTP server over VM loopback",
		async () => {
			ctx = await createIntegrationKernel({
				runtimes: ["wasmvm", "node"],
				commandDirs: [C_BUILD_DIR, COMMANDS_DIR],
			});
			const server = spawnGuestProgram(ctx.kernel, "http_server", ["3108"]);
			await waitForListener(ctx.kernel, 3108, "WASM HTTP server", server);

			const wasm = await ctx.kernel.exec(
				"curl -fsS http://127.0.0.1:3108/from-wasm",
			);
			const serverExit = await server.process.wait();

			expect(wasm.exitCode).toBe(0);
			expect(wasm.stderr).toBe("");
			expect(wasm.stdout.trim()).toBe("wasm:GET:/from-wasm");
			expect(serverExit).toBe(0);
		},
	);

	itIf(
		!wasmNetworkSkipReason,
		"W4 WASM tcp_echo -> WASM TCP server over VM loopback",
		async () => {
			ctx = await createIntegrationKernel({
				runtimes: ["wasmvm", "node"],
				commandDirs: [C_BUILD_DIR, COMMANDS_DIR],
			});
			const server = spawnGuestProgram(ctx.kernel, "tcp_server", ["3109"]);
			await waitForListener(ctx.kernel, 3109, "WASM TCP server", server);

			const wasm = await ctx.kernel.exec("tcp_echo 3109");
			const serverExit = await server.process.wait();

			expect(wasm.exitCode).toBe(0);
			expect(wasm.stderr).not.toContain("socket error");
			expect(wasm.stdout).toContain("sent: 5");
			expect(wasm.stdout).toContain("received: pong");
			expect(serverExit).toBe(0);
		},
	);

	it("O1 JS fetch -> host loopback requires loopback exemption", async () => {
		const seenRequests: string[] = [];
		const hostServer = createHttpServer((req, res) => {
			seenRequests.push(req.url ?? "");
			res.writeHead(200, { "content-type": "text/plain" });
			res.end("host:" + req.url);
		});
		await new Promise<void>((resolveListen) => {
			hostServer.listen(0, "127.0.0.1", () => resolveListen());
		});
		const port = (hostServer.address() as import("node:net").AddressInfo).port;

		try {
			ctx = await createIntegrationKernel({
				runtimes: ["node"],
			});
			const noExemption = await runGuestNodeProgram(
				ctx.kernel,
				[
					`fetch('http://127.0.0.1:${port}/blocked')`,
					"  .then(async (res) => console.log('unexpected:' + res.status + ':' + await res.text()))",
					"  .catch((error) => { console.log(error.cause?.code || error.code || error.name); });",
				].join("\n"),
			);
			expect(noExemption.exitCode).toBe(0);
			expect(noExemption.stdout.trim()).toBe("EACCES");
			expect(seenRequests).toEqual([]);
			await ctx.dispose();

			ctx = await createIntegrationKernel({
				runtimes: ["node"],
				loopbackExemptPorts: [port],
			});
			const allowed = await runGuestNodeProgram(
				ctx.kernel,
				[
					`fetch('http://127.0.0.1:${port}/allowed')`,
					"  .then(async (res) => console.log(res.status + ':' + await res.text()))",
					"  .catch((error) => { console.error(error); process.exit(1); });",
				].join("\n"),
			);
			expect(allowed.exitCode).toBe(0);
			expect(allowed.stderr).toBe("");
			expect(allowed.stdout.trim()).toBe("200:host:/allowed");
			expect(seenRequests).toEqual(["/allowed"]);
		} finally {
			await new Promise<void>((resolveClose) =>
				hostServer.close(() => resolveClose()),
			);
		}
	});

	itIf(
		!wasmNetworkSkipReason,
		"O2 WASM curl -> host loopback requires loopback exemption",
		async () => {
			const seenRequests: string[] = [];
			const hostServer = createHttpServer((req, res) => {
				seenRequests.push(req.url ?? "");
				res.writeHead(200, { "content-type": "text/plain" });
				res.end("host:" + req.url);
			});
			await new Promise<void>((resolveListen) => {
				hostServer.listen(0, "127.0.0.1", () => resolveListen());
			});
			const port = (hostServer.address() as import("node:net").AddressInfo)
				.port;

			try {
				ctx = await createIntegrationKernel({
					runtimes: ["wasmvm", "node"],
					commandDirs: [C_BUILD_DIR, COMMANDS_DIR],
				});
				const noExemption = await ctx.kernel.exec(
					`curl -fsS http://127.0.0.1:${port}/blocked`,
				);
				expect(noExemption.exitCode).not.toBe(0);
				expect(noExemption.stderr).toMatch(
					/EACCES|Bad address|Connection refused|connect|Failed to connect|Invalid argument/,
				);
				expect(seenRequests).toEqual([]);
				await ctx.dispose();

				ctx = await createIntegrationKernel({
					runtimes: ["wasmvm", "node"],
					commandDirs: [C_BUILD_DIR, COMMANDS_DIR],
					loopbackExemptPorts: [port],
				});
				const allowed = await ctx.kernel.exec(
					`curl -fsS http://127.0.0.1:${port}/allowed`,
				);
				expect(allowed.exitCode).toBe(0);
				expect(allowed.stderr).toBe("");
				expect(allowed.stdout.trim()).toBe("host:/allowed");
				expect(seenRequests).toEqual(["/allowed"]);
			} finally {
				await new Promise<void>((resolveClose) =>
					hostServer.close(() => resolveClose()),
				);
			}
		},
	);
});
