// V8 runtime process manager: spawns the Rust binary, connects over UDS,
// and exposes session lifecycle.

import { spawn, type ChildProcess } from "node:child_process";
import { randomBytes } from "node:crypto";
import { existsSync } from "node:fs";
import { createRequire } from "node:module";
import { createInterface } from "node:readline";
import { resolve, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import v8 from "node:v8";
import { IpcClient } from "./ipc-client.js";
import type { BinaryFrame } from "./ipc-binary.js";
import type { V8Session, V8SessionOptions } from "./session.js";

// Bun's node:v8 module doesn't produce real V8 serialization format.
// Detect Bun and use CBOR codec instead (faster than JSON, binary-native).
const isBun = typeof (globalThis as Record<string, unknown>).Bun !== "undefined";

// Lazy-load cbor-x only when needed (Bun path)
let _cbor: typeof import("cbor-x") | null = null;
function getCbor(): typeof import("cbor-x") {
	if (!_cbor) {
		// eslint-disable-next-line @typescript-eslint/no-require-imports
		_cbor = require("cbor-x") as typeof import("cbor-x");
	}
	return _cbor;
}

/** Serialize a value for IPC — CBOR when running under Bun, V8 otherwise. */
function ipcSerialize(value: unknown): Buffer {
	if (isBun) {
		return Buffer.from(getCbor().encode(value));
	}
	return Buffer.from(v8.serialize(value));
}

/** Deserialize an IPC payload — CBOR when running under Bun, V8 otherwise. */
function ipcDeserialize(buf: Buffer | Uint8Array): unknown {
	if (isBun) {
		return getCbor().decode(Buffer.from(buf));
	}
	return v8.deserialize(buf);
}

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);

/** Platform-specific package name mapping. */
const PLATFORM_PACKAGES: Record<string, string> = {
	"linux-x64": "@secure-exec/v8-linux-x64-gnu",
	"linux-arm64": "@secure-exec/v8-linux-arm64-gnu",
	"darwin-x64": "@secure-exec/v8-darwin-x64",
	"darwin-arm64": "@secure-exec/v8-darwin-arm64",
	"win32-x64": "@secure-exec/v8-win32-x64",
};

/** Options for creating a V8 runtime. */
export interface V8RuntimeOptions {
	/** Path to the Rust binary. Auto-detected if omitted. */
	binaryPath?: string;
	/** Maximum concurrent sessions. Passed via SECURE_EXEC_V8_MAX_SESSIONS. */
	maxSessions?: number;
	/** Bridge code to pre-warm the snapshot cache with (fire-and-forget). Skipped if SECURE_EXEC_NO_SNAPSHOT_WARMUP=1. */
	warmupBridgeCode?: string;
}

/** Manages the Rust V8 child process and session lifecycle. */
export interface V8Runtime {
	/** Whether the Rust child process is still alive. */
	readonly isAlive: boolean;
	/** Create a new session (V8 isolate) in the runtime process. */
	createSession(options?: V8SessionOptions): Promise<V8Session>;
	/** Kill the child process and clean up. */
	dispose(): Promise<void>;
}

/** Resolve the platform-specific binary path. */
function resolveBinaryPath(): string {
	const binaryName =
		process.platform === "win32" ? "secure-exec-v8.exe" : "secure-exec-v8";

	// 1. Try cargo-built binary at crate target path (development workspace)
	const crateRelative = resolve(
		__dirname,
		"../../../native/v8-runtime/target/release/secure-exec-v8",
	);
	if (existsSync(crateRelative)) return crateRelative;

	const crateDebug = resolve(
		__dirname,
		"../../../native/v8-runtime/target/debug/secure-exec-v8",
	);
	if (existsSync(crateDebug)) return crateDebug;

	// 2. Try platform-specific npm package
	const platformKey = `${process.platform}-${process.arch}`;
	const platformPkg = PLATFORM_PACKAGES[platformKey];
	if (platformPkg) {
		try {
			const require = createRequire(import.meta.url);
			const pkgDir = dirname(require.resolve(`${platformPkg}/package.json`));
			const platformBinary = join(pkgDir, binaryName);
			if (existsSync(platformBinary)) return platformBinary;
		} catch {
			// Platform package not installed — fall through
		}
	}

	// 3. Try postinstall download location
	const downloadedBinary = resolve(__dirname, "../bin", binaryName);
	if (existsSync(downloadedBinary)) return downloadedBinary;

	// 4. Fallback: assume on PATH
	return "secure-exec-v8";
}

/**
 * Spawn the Rust V8 runtime process and return a handle.
 *
 * Generates a 128-bit auth token, passes it via SECURE_EXEC_V8_TOKEN,
 * reads the socket path from stdout, connects over UDS, and authenticates.
 */
export async function createV8Runtime(
	options?: V8RuntimeOptions,
): Promise<V8Runtime> {
	const binaryPath = options?.binaryPath ?? resolveBinaryPath();

	// Generate 128-bit random auth token
	const authToken = randomBytes(16).toString("hex");

	// Build child environment
	const childEnv: Record<string, string> = {
		...process.env as Record<string, string>,
		SECURE_EXEC_V8_TOKEN: authToken,
	};
	if (isBun) {
		childEnv.SECURE_EXEC_V8_CODEC = "cbor";
	}
	if (options?.maxSessions != null) {
		childEnv.SECURE_EXEC_V8_MAX_SESSIONS = String(options.maxSessions);
	}

	// Spawn the Rust binary
	const child = spawn(binaryPath, [], {
		stdio: ["ignore", "pipe", "pipe"],
		env: childEnv,
	});
	// Forward V8 runtime stderr to host stderr for debugging
	child.stderr?.pipe(process.stderr);

	// Message routing: session-level handlers registered per session_id.
	// Declared before the exit handler so Bun's eager event delivery
	// doesn't hit the temporal dead zone.
	const sessionHandlers = new Map<
		string,
		(frame: BinaryFrame) => void
	>();
	// Per-session reject functions for rejecting in-flight execute() promises
	const sessionRejects = new Map<string, (err: Error) => void>();

	// Track whether the process is alive
	let processAlive = true;
	let exitError: Error | null = null;
	const handleParentExit = () => {
		if (!processAlive) {
			return;
		}
		try {
			child.kill("SIGTERM");
		} catch {
			// Best effort during process shutdown.
		}
	};
	process.once("exit", handleParentExit);

	function formatRuntimeCloseError(baseMessage: string): Error {
		const details: string[] = [baseMessage];
		if (exitError) {
			details.push(`runtime: ${exitError.message}`);
		}
		if (stderrBuf) {
			details.push(`stderr: ${stderrBuf}`);
		}
		return new Error(details.join("\n"));
	}

	child.on("exit", (code, signal) => {
		processAlive = false;
		process.removeListener("exit", handleParentExit);
		if (code !== 0 && code !== null) {
			exitError = new Error(
				`V8 runtime process exited with code ${code}`,
			);
		} else if (signal) {
			exitError = new Error(
				`V8 runtime process killed by signal ${signal}`,
			);
		}

		// Resolve all pending executions with a crash error
		rejectPendingSessions(
			exitError ?? new Error("V8 runtime process exited unexpectedly"),
		);
	});

	// Collect stderr for error reporting
	let stderrBuf = "";
	child.stderr!.on("data", (chunk: Buffer) => {
		stderrBuf += chunk.toString();
		// Cap buffer to avoid unbounded growth
		if (stderrBuf.length > 8192) {
			stderrBuf = stderrBuf.slice(-4096);
		}
	});

	// Read socket path from first line of stdout
	const socketPath = await readSocketPath(child);

	// Unref child process and its stdio so they don't keep the event loop
	// alive on their own. The IPC socket (ref-counted by active sessions)
	// is the sole handle that gates event loop lifetime.
	child.unref();
	child.stdout?.destroy(); // Done reading after readline
	// Unref stderr (still collects errors, but doesn't block exit)
	const stderrStream = child.stderr as NodeJS.ReadableStream & { unref?: () => void };
	stderrStream?.unref?.();

	// Connect IPC client
	let ipcClient: IpcClient | null = null;
	let disposed = false;

	ipcClient = new IpcClient({
		socketPath,
		onMessage: (frame) => {
			// Route frame to the appropriate session handler by sessionId
			if ("sessionId" in frame && frame.sessionId) {
				const handler = sessionHandlers.get(frame.sessionId);
				handler?.(frame);
			}
		},
		onClose: () => {
			ipcClient = null;
			// Give the child 'exit' event one tick to populate exitError/stderr
			// before collapsing everything into a generic IPC-close failure.
			setTimeout(() => {
				rejectPendingSessions(
					formatRuntimeCloseError("IPC connection closed"),
				);
			}, 0);
		},
		onError: (err) => {
			// Surface IPC errors as exit errors if process is still alive
			if (!exitError) {
				exitError = err;
			}
		},
	});

	try {
		await ipcClient.connect();
		ipcClient.authenticate(authToken);

		// Send warm-up snapshot request (fire-and-forget)
		if (options?.warmupBridgeCode && process.env.SECURE_EXEC_NO_SNAPSHOT_WARMUP !== "1") {
			ipcClient.send({
				type: "WarmSnapshot",
				bridgeCode: options.warmupBridgeCode,
			});
		}

		// No active sessions yet — unref socket so it doesn't block exit
		ipcClient.unref();
	} catch (err) {
		// Connection failed — kill child and surface error
		child.kill("SIGTERM");
		const msg =
			err instanceof Error ? err.message : String(err);
		throw new Error(
			`Failed to connect to V8 runtime: ${msg}${stderrBuf ? `\nstderr: ${stderrBuf}` : ""}`,
		);
	}

	/** Ref/unref the IPC socket based on active session count. */
	function updateSocketRef(): void {
		if (!ipcClient || disposed) return;
		if (sessionHandlers.size > 0) {
			ipcClient.ref();
		} else {
			ipcClient.unref();
		}
	}

	/** Resolve all pending execute() promises with a crash/close error result. */
	function rejectPendingSessions(error: Error): void {
		const handlers = [...sessionHandlers.entries()];
		for (const [sid, handler] of handlers) {
			handler({
				type: "ExecutionResult",
				sessionId: sid,
				exitCode: 1,
				exports: null,
				error: {
					errorType: "Error",
					message: error.message,
					stack: "",
					code: "ERR_V8_PROCESS_CRASH",
				},
			});
		}
		// Fallback: reject any sessions that weren't resolved by
		// the synthetic ExecutionResult (e.g. handler not yet registered)
		const rejects = [...sessionRejects.entries()];
		for (const [sid, reject] of rejects) {
			sessionHandlers.delete(sid);
			sessionRejects.delete(sid);
			reject(error);
		}
		updateSocketRef();
	}

	/** Ensure the process is alive, throw if crashed. */
	function ensureAlive(): void {
		if (!processAlive || disposed) {
			throw exitError ?? new Error("V8 runtime process is not running");
		}
	}

	const runtime: V8Runtime = {
		get isAlive() {
			return processAlive && !disposed && ipcClient !== null;
		},
		async createSession(sessionOptions?: V8SessionOptions): Promise<V8Session> {
			ensureAlive();
			if (!ipcClient) {
				throw new Error("IPC client is not connected");
			}

			// Generate 128-bit session ID
			const sessionId = randomBytes(16).toString("hex");

			// Send CreateSession
			ipcClient.send({
				type: "CreateSession",
				sessionId,
				heapLimitMb: sessionOptions?.heapLimitMb ?? 0,
				cpuTimeLimitMs: sessionOptions?.cpuTimeLimitMs ?? 0,
			});

			// Create session proxy
			const client = ipcClient;
			// Track bridge code hash to skip resending unchanged bridge code
			let lastBridgeCodeHash: number | null = null;
			const session: V8Session = {
				sendStreamEvent(eventType: string, payload: Uint8Array): void {
					ensureAlive();
					if (!client.isConnected) {
						throw new Error("IPC client is not connected");
					}
					client.send({
						type: "StreamEvent",
						sessionId,
						eventType,
						payload: Buffer.from(payload),
					});
				},

				async execute(execOptions) {
					ensureAlive();
					if (!client.isConnected) {
						throw new Error("IPC client is not connected");
					}

					// Inject globals — serialize { processConfig, osConfig }
					const globalsPayload = ipcSerialize({
						processConfig: execOptions.processConfig,
						osConfig: execOptions.osConfig,
					});
					client.send({
						type: "InjectGlobals",
						sessionId,
						payload: globalsPayload,
					});

					// Set up result promise
					return new Promise((resolve, reject) => {
						// Store reject so rejectPendingSessions can
						// reject this promise on IPC close or process crash
						sessionRejects.set(sessionId, reject);

						// Register session message handler
						sessionHandlers.set(sessionId, (frame) => {
							switch (frame.type) {
								case "BridgeCall": {
									// Route to bridge handler
									const handler =
										execOptions.bridgeHandlers[frame.method];
									if (!handler) {
										client.send({
											type: "BridgeResponse",
											sessionId,
											callId: frame.callId,
											status: 1,
											payload: Buffer.from(`No handler for bridge method: ${frame.method}`, "utf8"),
										});
										return;
									}
									// Deserialize args and call handler
									void (async () => {
										try {
											const args = ipcDeserialize(
												frame.payload,
											) as unknown[];
											const result = await handler(
												...(Array.isArray(args)
													? args
													: [args]),
											);
											if (!client.isConnected) return;
											// Use status=2 for raw binary (Uint8Array/Buffer) to avoid
											// V8 typed array format incompatibility across V8 versions.
											if (result instanceof Uint8Array) {
												client.send({
													type: "BridgeResponse",
													sessionId,
													callId: frame.callId,
													status: 2,
													payload: Buffer.from(result),
												});
											} else {
												client.send({
													type: "BridgeResponse",
													sessionId,
													callId: frame.callId,
													status: 0,
													payload:
														result !== undefined
															? ipcSerialize(result)
															: Buffer.alloc(0),
												});
											}
										} catch (err) {
											if (!client.isConnected) return;
											const errMsg = err instanceof Error
												? err.message
												: String(err);
											client.send({
												type: "BridgeResponse",
												sessionId,
												callId: frame.callId,
												status: 1,
												payload: Buffer.from(errMsg, "utf8"),
											});
										}
									})();
									break;
								}
								case "ExecutionResult": {
									// Clean up handler and reject entry, then resolve
									sessionHandlers.delete(sessionId);
									sessionRejects.delete(sessionId);
									updateSocketRef();
									resolve({
										code: frame.exitCode,
										exports: frame.exports,
										error: frame.error ? {
											type: frame.error.errorType,
											message: frame.error.message,
											stack: frame.error.stack,
											code: frame.error.code || undefined,
										} : null,
									});
									break;
								}
								case "Log":
									// Emit to stdout/stderr
									if (frame.channel === 1) {
										process.stderr.write(frame.message);
									} else {
										process.stdout.write(frame.message);
									}
									break;
								case "StreamCallback":
									// Route to execution-level stream callback handler
									execOptions.onStreamCallback?.(
										frame.callbackType,
										frame.payload,
									);
									break;
							}
						});

						// Send Execute — skip bridge code if unchanged since last send
					const bridgeHash = fnv1aHash(execOptions.bridgeCode);
					const sendBridgeCode = bridgeHash !== lastBridgeCodeHash;
					if (sendBridgeCode) {
						lastBridgeCodeHash = bridgeHash;
					}
					client.send({
						type: "Execute",
						sessionId,
						bridgeCode: sendBridgeCode ? execOptions.bridgeCode : "",
						postRestoreScript: execOptions.postRestoreScript ?? "",
						userCode: execOptions.userCode,
						mode: execOptions.mode === "exec" ? 0 : 1,
						filePath: execOptions.filePath ?? "",
					});
					// Ref the socket while execution is in-flight
					updateSocketRef();
					});
				},

				async destroy(): Promise<void> {
					sessionHandlers.delete(sessionId);
					sessionRejects.delete(sessionId);
					updateSocketRef();
					if (client.isConnected) {
						client.send({
							type: "DestroySession",
							sessionId,
						});
					}
				},
			};

			return session;
		},

		async dispose(): Promise<void> {
			if (disposed) return;
			disposed = true;
			process.removeListener("exit", handleParentExit);

			// Close IPC connection
			ipcClient?.close();
			ipcClient = null;

			// Terminate child process
			if (processAlive) {
				child.kill("SIGTERM");

				// Wait for exit with timeout
				await new Promise<void>((resolve) => {
					const timeout = setTimeout(() => {
						if (processAlive) {
							child.kill("SIGKILL");
						}
						resolve();
					}, 5000);

					child.on("exit", () => {
						clearTimeout(timeout);
						resolve();
					});
				});
			}

			// Clean up child process handles to fully release the event loop
			child.stdout?.removeAllListeners();
			child.stdout?.destroy();
			child.stderr?.removeAllListeners();
			child.stderr?.destroy();
			child.removeAllListeners();
		},
	};

	return runtime;
}

/** Read the socket path from the child's first stdout line. */
function readSocketPath(child: ChildProcess): Promise<string> {
	return new Promise<string>((resolve, reject) => {
		let resolved = false;

		// Timeout if socket path is not received within 10s
		const timeout = setTimeout(() => {
			if (!resolved) {
				resolved = true;
				child.kill("SIGTERM");
				reject(
					new Error(
						"Timed out waiting for V8 runtime socket path",
					),
				);
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
				reject(
					new Error(
						"V8 runtime process closed stdout before sending socket path",
					),
				);
			}
		});

		child.on("exit", (code, signal) => {
			if (!resolved) {
				resolved = true;
				clearTimeout(timeout);
				reject(
					new Error(
						`V8 runtime process exited (code=${code}, signal=${signal}) before sending socket path`,
					),
				);
			}
		});
	});
}

/** FNV-1a hash of a string, returning a 32-bit integer.
 * Hashes over UTF-8 bytes to match the Rust side. */
export function fnv1aHash(str: string): number {
	const bytes = Buffer.from(str, "utf8");
	let hash = 0x811c9dc5;
	for (let i = 0; i < bytes.length; i++) {
		hash ^= bytes[i];
		hash = Math.imul(hash, 0x01000193);
	}
	return hash >>> 0;
}
