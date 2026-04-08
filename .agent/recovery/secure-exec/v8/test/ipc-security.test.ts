/**
 * IPC security tests for the V8 runtime.
 *
 * Covers: auth token validation, cross-session access prevention,
 * oversized message rejection, and duplicate BridgeResponse callId
 * integrity.
 */

import { describe, it, expect, afterEach } from "vitest";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { existsSync } from "node:fs";
import { spawn, type ChildProcess } from "node:child_process";
import { randomBytes } from "node:crypto";
import { createInterface } from "node:readline";
import net from "node:net";
import v8 from "node:v8";
import { IpcClient } from "../src/ipc-client.js";
import { encodeFrame, type BinaryFrame } from "../src/ipc-binary.js";
import { createV8Runtime } from "../src/runtime.js";
import type { V8Runtime } from "../src/runtime.js";
import type { V8ExecutionOptions } from "../src/session.js";

const __dirname = dirname(fileURLToPath(import.meta.url));

const BINARY_PATH = (() => {
	const release = resolve(
		__dirname,
		"../../../native/v8-runtime/target/release/secure-exec-v8",
	);
	if (existsSync(release)) return release;
	const debug = resolve(
		__dirname,
		"../../../native/v8-runtime/target/debug/secure-exec-v8",
	);
	if (existsSync(debug)) return debug;
	return undefined;
})();

const skipUnlessBinary = !BINARY_PATH;

/** Default execution options with minimal config. */
function defaultExecOptions(
	overrides: Partial<V8ExecutionOptions> = {},
): V8ExecutionOptions {
	return {
		bridgeCode: "",
		userCode: "",
		mode: "exec",
		processConfig: {
			cwd: "/tmp",
			env: {},
			timing_mitigation: "none",
			frozen_time_ms: null,
		},
		osConfig: {
			homedir: "/root",
			tmpdir: "/tmp",
			platform: "linux",
			arch: "x64",
		},
		bridgeHandlers: {},
		...overrides,
	};
}

/** Spawn the Rust binary and return the child, socket path, and auth token. */
async function spawnRustBinary(): Promise<{
	child: ChildProcess;
	socketPath: string;
	authToken: string;
}> {
	const authToken = randomBytes(16).toString("hex");
	const child = spawn(BINARY_PATH!, [], {
		stdio: ["ignore", "pipe", "pipe"],
		env: {
			...process.env,
			SECURE_EXEC_V8_TOKEN: authToken,
		},
	});

	// Read socket path from first stdout line
	const socketPath = await new Promise<string>((resolve, reject) => {
		let resolved = false;
		const timeout = setTimeout(() => {
			if (!resolved) {
				resolved = true;
				child.kill("SIGTERM");
				reject(new Error("Timed out waiting for socket path"));
			}
		}, 10_000);

		const rl = createInterface({ input: child.stdout! });
		rl.on("line", (line) => {
			if (!resolved) {
				resolved = true;
				clearTimeout(timeout);
				rl.close();
				resolve(line.trim());
			}
		});
		rl.on("close", () => {
			if (!resolved) {
				resolved = true;
				clearTimeout(timeout);
				reject(new Error("stdout closed before socket path"));
			}
		});
	});

	return { child, socketPath, authToken };
}

/** Kill child process and wait for exit. */
async function killChild(child: ChildProcess): Promise<void> {
	if (child.exitCode !== null) return;
	child.kill("SIGTERM");
	await new Promise<void>((resolve) => {
		const timeout = setTimeout(() => {
			child.kill("SIGKILL");
			resolve();
		}, 3000);
		child.on("exit", () => {
			clearTimeout(timeout);
			resolve();
		});
	});
}

/** Create a connected IpcClient to the given socket path. */
async function connectClient(
	socketPath: string,
	onMessage: (msg: BinaryFrame) => void,
): Promise<IpcClient> {
	const client = new IpcClient({
		socketPath,
		onMessage,
		onError: () => {},
	});
	await client.connect();
	return client;
}

/** Write raw bytes (length-prefixed frame) to a UDS. */
function writeRawFrame(
	socketPath: string,
	lengthPrefix: number,
	payload?: Buffer,
): Promise<{ closed: boolean; data: Buffer[] }> {
	return new Promise((resolve) => {
		const socket = net.createConnection(socketPath);
		const received: Buffer[] = [];
		let closed = false;

		socket.on("connect", () => {
			const header = Buffer.alloc(4);
			header.writeUInt32BE(lengthPrefix, 0);
			socket.write(header);
			if (payload) {
				socket.write(payload);
			}
		});

		socket.on("data", (chunk) => {
			received.push(chunk);
		});

		socket.on("close", () => {
			closed = true;
			resolve({ closed: true, data: received });
		});

		// Resolve after a short wait if not closed
		setTimeout(() => {
			if (!closed) {
				socket.destroy();
				resolve({ closed: false, data: received });
			}
		}, 2000);
	});
}

describe.skipIf(skipUnlessBinary)("V8 IPC security", () => {
	const children: ChildProcess[] = [];
	const clients: IpcClient[] = [];
	let runtime: V8Runtime | null = null;

	afterEach(async () => {
		for (const c of clients) {
			c.close();
		}
		clients.length = 0;

		if (runtime) {
			await runtime.dispose();
			runtime = null;
		}

		for (const child of children) {
			await killChild(child);
		}
		children.length = 0;
	});

	// --- Auth token rejection ---

	it("rejects connection with wrong auth token", async () => {
		const { child, socketPath } = await spawnRustBinary();
		children.push(child);

		const messages: BinaryFrame[] = [];
		let connectionClosed = false;

		const client = new IpcClient({
			socketPath,
			onMessage: (msg) => messages.push(msg),
			onClose: () => {
				connectionClosed = true;
			},
		});
		await client.connect();
		clients.push(client);

		// Send wrong token
		client.authenticate("wrong-token-0000000000000000");

		// Wait for Rust to close the connection
		await new Promise((r) => setTimeout(r, 500));

		expect(connectionClosed).toBe(true);
		// No valid messages should have been received
		expect(messages.length).toBe(0);
	});

	it("rejects connection without auth token (non-Authenticate message first)", async () => {
		const { child, socketPath } = await spawnRustBinary();
		children.push(child);

		const messages: BinaryFrame[] = [];
		let connectionClosed = false;

		const client = new IpcClient({
			socketPath,
			onMessage: (msg) => messages.push(msg),
			onClose: () => {
				connectionClosed = true;
			},
		});
		await client.connect();
		clients.push(client);

		// Send a CreateSession instead of Authenticate
		client.send({
			type: "CreateSession",
			sessionId: randomBytes(16).toString("hex"),
			heapLimitMb: 0,
			cpuTimeLimitMs: 0,
		});

		// Wait for Rust to close the connection
		await new Promise((r) => setTimeout(r, 500));

		expect(connectionClosed).toBe(true);
		expect(messages.length).toBe(0);
	});

	// --- Cross-session access prevention ---

	it("connection B cannot send messages to connection A's sessions", async () => {
		const { child, socketPath, authToken } = await spawnRustBinary();
		children.push(child);

		// Connect client A
		const messagesA: BinaryFrame[] = [];
		const clientA = await connectClient(socketPath, (msg) =>
			messagesA.push(msg),
		);
		clients.push(clientA);
		clientA.authenticate(authToken);

		// Connect client B
		const messagesB: BinaryFrame[] = [];
		const clientB = await connectClient(socketPath, (msg) =>
			messagesB.push(msg),
		);
		clients.push(clientB);
		clientB.authenticate(authToken);

		// Client A creates a session
		const sessionId = randomBytes(16).toString("hex");
		clientA.send({
			type: "CreateSession",
			sessionId,
			heapLimitMb: 0,
			cpuTimeLimitMs: 0,
		});

		// Give session time to initialize
		await new Promise((r) => setTimeout(r, 200));

		// Client B tries to execute code on client A's session
		clientB.send({
			type: "InjectGlobals",
			sessionId,
			payload: v8.serialize({
				processConfig: {
					cwd: "/tmp",
					env: {},
					timing_mitigation: "none",
					frozen_time_ms: null,
				},
				osConfig: {
					homedir: "/root",
					tmpdir: "/tmp",
					platform: "linux",
					arch: "x64",
				},
			}),
		});

		clientB.send({
			type: "Execute",
			sessionId,
			bridgeCode: "",
			postRestoreScript: "",
			userCode: "1 + 1;",
			mode: 0,
			filePath: "",
		});

		// Wait for any responses
		await new Promise((r) => setTimeout(r, 1000));

		// Client B should NOT receive an ExecutionResult for A's session
		const bResults = messagesB.filter(
			(m) => m.type === "ExecutionResult",
		);
		expect(bResults.length).toBe(0);

		// Client A's session should still work — execute on it
		clientA.send({
			type: "InjectGlobals",
			sessionId,
			payload: v8.serialize({
				processConfig: {
					cwd: "/tmp",
					env: {},
					timing_mitigation: "none",
					frozen_time_ms: null,
				},
				osConfig: {
					homedir: "/root",
					tmpdir: "/tmp",
					platform: "linux",
					arch: "x64",
				},
			}),
		});
		clientA.send({
			type: "Execute",
			sessionId,
			bridgeCode: "",
			postRestoreScript: "",
			userCode: "1 + 1;",
			mode: 0,
			filePath: "",
		});

		// Wait for result
		await new Promise((r) => setTimeout(r, 1000));

		const aResults = messagesA.filter(
			(m) => m.type === "ExecutionResult",
		);
		expect(aResults.length).toBe(1);
		expect((aResults[0] as { exitCode: number }).exitCode).toBe(0);

		// Clean up session
		clientA.send({ type: "DestroySession", sessionId });
	});

	// --- Oversized message rejection ---

	it("encodeFrame rejects frames exceeding 64MB", () => {
		// Create a frame with payload > 64MB by constructing an Execute
		// with a large userCode field
		const largeCode = "x".repeat(64 * 1024 * 1024 + 1);

		// encodeFrame should throw when the body exceeds 64 MB
		expect(() =>
			encodeFrame({
				type: "Execute",
				sessionId: "test",
				bridgeCode: "",
				postRestoreScript: "",
				userCode: largeCode,
				mode: 0,
				filePath: "",
			}),
		).toThrow(/exceeds maximum/);
	});

	it("Rust process rejects oversized message length prefix", async () => {
		const { child, socketPath, authToken } = await spawnRustBinary();
		children.push(child);

		// First authenticate properly
		const socket = net.createConnection(socketPath);
		await new Promise<void>((r) => socket.on("connect", r));

		// Send valid auth message using binary frame format
		const authFrame = encodeFrame({ type: "Authenticate", token: authToken });
		socket.write(authFrame);

		// Wait for auth to be processed
		await new Promise((r) => setTimeout(r, 200));

		// Send a frame with length prefix > 64MB (but no actual payload)
		const oversizedHeader = Buffer.alloc(4);
		oversizedHeader.writeUInt32BE(64 * 1024 * 1024 + 1, 0);
		socket.write(oversizedHeader);

		// The Rust process should close this connection due to the invalid length
		const closed = await new Promise<boolean>((resolve) => {
			const timeout = setTimeout(() => resolve(false), 3000);
			socket.on("close", () => {
				clearTimeout(timeout);
				resolve(true);
			});
			socket.on("error", () => {
				clearTimeout(timeout);
				resolve(true);
			});
		});

		expect(closed).toBe(true);
		socket.destroy();

		// Verify the Rust process is still alive (didn't crash)
		expect(child.exitCode).toBeNull();
	});

	// --- Duplicate BridgeResponse callId integrity ---

	it("duplicate BridgeResponse callId does not crash or corrupt state", async () => {
		const { child, socketPath, authToken } = await spawnRustBinary();
		children.push(child);

		const messages: BinaryFrame[] = [];
		const client = await connectClient(socketPath, (msg) =>
			messages.push(msg),
		);
		clients.push(client);
		client.authenticate(authToken);

		// Create a session
		const sessionId = randomBytes(16).toString("hex");
		client.send({
			type: "CreateSession",
			sessionId,
			heapLimitMb: 0,
			cpuTimeLimitMs: 0,
		});
		await new Promise((r) => setTimeout(r, 200));

		// Inject globals
		client.send({
			type: "InjectGlobals",
			sessionId,
			payload: v8.serialize({
				processConfig: {
					cwd: "/tmp",
					env: {},
					timing_mitigation: "none",
					frozen_time_ms: null,
				},
				osConfig: {
					homedir: "/root",
					tmpdir: "/tmp",
					platform: "linux",
					arch: "x64",
				},
			}),
		});

		// Execute code that makes a sync bridge call — we'll manually
		// respond with the correct callId TWICE
		client.send({
			type: "Execute",
			sessionId,
			bridgeCode: "",
			postRestoreScript: "",
			userCode: '_log("test");',
			mode: 0,
			filePath: "",
		});

		// Wait for the BridgeCall
		const bridgeCall = await new Promise<BinaryFrame>((resolve) => {
			const check = setInterval(() => {
				const bc = messages.find((m) => m.type === "BridgeCall");
				if (bc) {
					clearInterval(check);
					resolve(bc);
				}
			}, 50);
			setTimeout(() => clearInterval(check), 5000);
		});

		expect(bridgeCall.type).toBe("BridgeCall");
		const callId = (bridgeCall as { callId: number }).callId;

		// Send the first BridgeResponse (legitimate)
		client.send({
			type: "BridgeResponse",
			sessionId,
			callId,
			status: 0,
			payload: Buffer.alloc(0),
		});

		// Send a duplicate BridgeResponse with the same callId
		client.send({
			type: "BridgeResponse",
			sessionId,
			callId,
			status: 0,
			payload: Buffer.alloc(0),
		});

		// Wait for the ExecutionResult
		await new Promise<void>((resolve) => {
			const check = setInterval(() => {
				const er = messages.find(
					(m) => m.type === "ExecutionResult",
				);
				if (er) {
					clearInterval(check);
					resolve();
				}
			}, 50);
			setTimeout(() => {
				clearInterval(check);
				resolve();
			}, 3000);
		});

		// Execution should have completed successfully
		const execResult = messages.find(
			(m) => m.type === "ExecutionResult",
		) as { exitCode: number; error: unknown } | undefined;
		expect(execResult).toBeTruthy();
		expect(execResult!.exitCode).toBe(0);

		// Rust process should still be alive (not crashed by duplicate)
		expect(child.exitCode).toBeNull();

		// Session should still be usable — run another execution
		messages.length = 0;

		client.send({
			type: "InjectGlobals",
			sessionId,
			payload: v8.serialize({
				processConfig: {
					cwd: "/tmp",
					env: {},
					timing_mitigation: "none",
					frozen_time_ms: null,
				},
				osConfig: {
					homedir: "/root",
					tmpdir: "/tmp",
					platform: "linux",
					arch: "x64",
				},
			}),
		});

		client.send({
			type: "Execute",
			sessionId,
			bridgeCode: "",
			postRestoreScript: "",
			userCode: "42;",
			mode: 0,
			filePath: "",
		});

		// Wait for result
		await new Promise<void>((resolve) => {
			const check = setInterval(() => {
				const er = messages.find(
					(m) => m.type === "ExecutionResult",
				);
				if (er) {
					clearInterval(check);
					resolve();
				}
			}, 50);
			setTimeout(() => {
				clearInterval(check);
				resolve();
			}, 3000);
		});

		const secondResult = messages.find(
			(m) => m.type === "ExecutionResult",
		) as { exitCode: number } | undefined;
		expect(secondResult).toBeTruthy();
		expect(secondResult!.exitCode).toBe(0);

		client.send({ type: "DestroySession", sessionId });
	});

	// --- Socket close rejects pending executions ---

	it("closing IPC socket while execute() is in-flight rejects the promise", async () => {
		runtime = await createV8Runtime({ binaryPath: BINARY_PATH! });
		const session = await runtime.createSession();

		// Start execution with a sync bridge call that will block
		const execPromise = session.execute(
			defaultExecOptions({
				userCode: '_log("waiting");',
				bridgeHandlers: {
					_log: () => {
						// Don't respond — leave the execution blocked
						return new Promise(() => {});
					},
				},
			}),
		);

		// Give the execution time to start and send the BridgeCall
		await new Promise((r) => setTimeout(r, 300));

		// Kill the Rust process to trigger IPC socket close
		await runtime.dispose();

		// The execute() promise should complete (not hang forever).
		// It resolves with an error result when the process crashes/exits.
		const result = await execPromise;
		expect(result.code).toBe(1);
		expect(result.error).toBeTruthy();
		expect(result.error!.message).toMatch(/process|connection/i);

		// Prevent afterEach from double-disposing
		runtime = null;
	});
});
