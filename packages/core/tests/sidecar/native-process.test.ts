import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { constants as osConstants, tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, test, vi } from "vitest";
import { createHostDirBackend } from "../../src/host-dir-mount.js";
import {
	createInMemoryFileSystem,
	createKernel,
	createNodeRuntime,
} from "../../src/runtime.js";
import { serializeMountConfigForSidecar } from "../../src/sidecar/mount-descriptors.js";
import { toSidecarSignalName } from "../../src/sidecar/native-kernel-proxy.js";
import { NativeSidecarProcessClient } from "../../src/sidecar/native-process-client.js";
import { serializeRootFilesystemForSidecar } from "../../src/sidecar/root-filesystem-descriptors.js";

const REPO_ROOT = fileURLToPath(new URL("../../../..", import.meta.url));
const SIDECAR_BINARY = join(REPO_ROOT, "target/debug/agent-os-sidecar");
const SIGNAL_STATE_CONTROL_PREFIX = "__AGENT_OS_SIGNAL_STATE__:";

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

	test(
		"NativeKernel refreshes zombieTimerCount from the sidecar proxy",
		async () => {
			const zombieTimerCount = vi
				.spyOn(NativeSidecarProcessClient.prototype, "getZombieTimerCount")
				.mockResolvedValueOnce({ count: 3 })
				.mockResolvedValueOnce({ count: 0 });

			const kernel = createKernel({
				filesystem: createInMemoryFileSystem(),
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
		},
		60_000,
	);

	test(
		"speaks to the real Rust sidecar binary over the framed stdio protocol",
		async () => {
			const fixtureRoot = mkdtempSync(join(tmpdir(), "agent-os-native-sidecar-"));
			cleanupPaths.push(fixtureRoot);
			writeFileSync(
				join(fixtureRoot, "entry.mjs"),
				"console.log('packages-core-native-sidecar-ok');\n",
			);
			execFileSync("cargo", ["build", "-q", "-p", "agent-os-sidecar"], {
				cwd: REPO_ROOT,
				stdio: "pipe",
			});

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
				});

				const creating = await client.waitForEvent(
					(event) =>
						event.payload.type === "vm_lifecycle"
						&& event.payload.state === "creating",
					10_000,
				);
				const ready = await client.waitForEvent(
					(event) =>
						event.payload.type === "vm_lifecycle"
						&& event.payload.state === "ready",
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
				expect(await client.readdir(session, vm, "/workspace")).toContain("nested");
				expect(await client.exists(session, vm, "/workspace/nested/generated.txt")).toBe(
					true,
				);
				await client.rename(
					session,
					vm,
					"/workspace/nested/generated.txt",
					"/workspace/nested/renamed.txt",
				);
				expect(await client.exists(session, vm, "/workspace/nested/generated.txt")).toBe(
					false,
				);
				expect(await client.exists(session, vm, "/workspace/nested/renamed.txt")).toBe(
					true,
				);
				const snapshot = await client.snapshotRootFilesystem(session, vm);
				expect(snapshot.some((entry) => entry.path === "/workspace/nested/renamed.txt")).toBe(
					true,
				);

				await client.execute(session, vm, {
					processId: "proc-1",
					runtime: "java_script",
					entrypoint: "./entry.mjs",
				});

				const stdout = await client.waitForEvent(
					(event) =>
						event.payload.type === "process_output"
						&& event.payload.process_id === "proc-1"
						&& event.payload.channel === "stdout",
					20_000,
				);
				if (stdout.payload.type !== "process_output") {
					throw new Error("expected process_output event");
				}
				expect(stdout.payload.chunk).toContain(
					"packages-core-native-sidecar-ok",
				);

				const exited = await client.waitForEvent(
					(event) =>
						event.payload.type === "process_exited"
						&& event.payload.process_id === "proc-1",
					20_000,
				);
				if (exited.payload.type !== "process_exited") {
					throw new Error("expected process_exited event");
				}
				expect(exited.payload.exit_code).toBe(0);
			} finally {
				await client.dispose();
			}
		},
		60_000,
	);

	test(
		"configures native mounts and streams stdin through the real Rust sidecar binary",
		async () => {
			const fixtureRoot = mkdtempSync(join(tmpdir(), "agent-os-native-sidecar-"));
			const hostMountRoot = mkdtempSync(join(tmpdir(), "agent-os-sidecar-host-dir-"));
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
			execFileSync("cargo", ["build", "-q", "-p", "agent-os-sidecar"], {
				cwd: REPO_ROOT,
				stdio: "pipe",
			});

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
				});

				await client.waitForEvent(
					(event) =>
						event.payload.type === "vm_lifecycle"
						&& event.payload.state === "ready",
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

				await client.writeFile(session, vm, "/hostmnt/generated.txt", "from-sidecar");
				expect(
					readFileSync(join(hostMountRoot, "generated.txt"), "utf8"),
				).toBe("from-sidecar");

				await client.execute(session, vm, {
					processId: "stdin-proc",
					runtime: "java_script",
					entrypoint: "./stdin-echo.mjs",
				});
				await client.writeStdin(session, vm, "stdin-proc", "hello through stdin\n");
				await client.closeStdin(session, vm, "stdin-proc");

				const stdout = await client.waitForEvent(
					(event) =>
						event.payload.type === "process_output"
						&& event.payload.process_id === "stdin-proc"
						&& event.payload.channel === "stdout",
					20_000,
				);
				if (stdout.payload.type !== "process_output") {
					throw new Error("expected process_output event");
				}
				expect(stdout.payload.chunk).toContain("STDIN:hello through stdin");

				const exited = await client.waitForEvent(
					(event) =>
						event.payload.type === "process_exited"
						&& event.payload.process_id === "stdin-proc",
					20_000,
				);
				if (exited.payload.type !== "process_exited") {
					throw new Error("expected process_exited event");
				}
				expect(exited.payload.exit_code).toBe(0);
			} finally {
				await client.dispose();
			}
		},
		60_000,
	);

	test(
		"queries listener and UDP through the real sidecar protocol and ignores forged signal-state stderr",
		async () => {
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
			execFileSync("cargo", ["build", "-q", "-p", "agent-os-sidecar"], {
				cwd: REPO_ROOT,
				stdio: "pipe",
			});

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
				});

				await client.waitForEvent(
					(event) =>
						event.payload.type === "vm_lifecycle"
						&& event.payload.state === "ready",
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
						event.payload.type === "process_exited"
						&& event.payload.process_id === "tcp-listener",
					20_000,
				);
				await client.killProcess(session, vm, "udp-listener");
				await client.waitForEvent(
					(event) =>
						event.payload.type === "process_exited"
						&& event.payload.process_id === "udp-listener",
					20_000,
				);
				await client.killProcess(session, vm, "signal-state");
				await client.waitForEvent(
					(event) =>
						event.payload.type === "process_exited"
						&& event.payload.process_id === "signal-state",
					20_000,
				);
			} finally {
				await client.dispose();
			}
		},
		60_000,
	);

	test(
		"NativeKernel exposes cached socketTable and processTable state from the sidecar",
		async () => {
			const kernel = createKernel({
				filesystem: createInMemoryFileSystem(),
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

				await waitFor(
					() => signalStdout,
					{ isReady: (value) => value.includes("registered") },
				);
				expect(kernel.processTable.getSignalState(signalProc.pid).handlers.get(2)).toBe(
					undefined,
				);

				tcpServer.kill(15);
				udpServer.kill(15);
				await tcpServer.wait();
				await udpServer.wait();
				await signalProc.wait();
			} finally {
				await kernel.dispose();
			}
		},
		60_000,
	);

	test(
		"delivers SIGSTOP and SIGCONT through killProcess",
		async () => {
			const fixtureRoot = mkdtempSync(join(tmpdir(), "agent-os-native-sidecar-"));
			cleanupPaths.push(fixtureRoot);
			writeFileSync(
				join(fixtureRoot, "signal-routing.mjs"),
				[
					"console.log('ready');",
					"setInterval(() => {}, 25);",
				].join("\n"),
			);
			execFileSync("cargo", ["build", "-q", "-p", "agent-os-sidecar"], {
				cwd: REPO_ROOT,
				stdio: "pipe",
			});

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
				});

				await client.waitForEvent(
					(event) =>
						event.payload.type === "vm_lifecycle"
						&& event.payload.state === "ready",
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
						event.payload.type === "process_output"
						&& event.payload.process_id === "signal-routing"
						&& event.payload.channel === "stdout"
						&& event.payload.chunk.includes("ready"),
					20_000,
				);

				await client.killProcess(session, vm, "signal-routing", "SIGSTOP");
				await waitFor(
					() =>
						execFileSync("ps", ["-o", "state=", "-p", String(started.pid)], {
							encoding: "utf8",
						}).trim(),
					{ isReady: (value) => value.startsWith("T") },
				);

				await client.killProcess(session, vm, "signal-routing", "SIGCONT");
				await waitFor(
					() =>
						execFileSync("ps", ["-o", "state=", "-p", String(started.pid)], {
							encoding: "utf8",
						}).trim(),
					{
						isReady: (value) => value.length > 0 && !value.startsWith("T"),
					},
				);

				await client.killProcess(session, vm, "signal-routing", "SIGTERM");
				await client.waitForEvent(
					(event) =>
						event.payload.type === "process_exited"
						&& event.payload.process_id === "signal-routing",
					20_000,
				);
			} finally {
				await client.dispose();
			}
		},
		60_000,
	);

	test(
		"connectTerminal forwards host stdin and output on the native sidecar path",
		async () => {
			const kernel = createKernel({
				filesystem: createInMemoryFileSystem(),
			});

			try {
				await kernel.mount(createNodeRuntime());

				let stdout = "";
				let stdinListener:
					| ((data: Uint8Array | string) => void)
					| null = null;
				const decoder = new TextDecoder();
				const stdinOn = vi
					.spyOn(process.stdin, "on")
					.mockImplementation(((event, listener) => {
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
					.mockImplementation(((event) => process.stdout) as typeof process.stdout.on);
				const stdoutRemoveListener = vi
					.spyOn(process.stdout, "removeListener")
					.mockImplementation(
						((event) => process.stdout) as typeof process.stdout.removeListener,
					);
				const setRawMode = typeof process.stdin.setRawMode === "function"
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
				expect(stdoutOn.mock.calls.every(([event]) => event === "resize")).toBe(true);

				if (!stdinListener) {
					throw new Error("connectTerminal did not register a stdin data handler");
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
				expect(stdinRemoveListener).toHaveBeenCalledWith("data", expect.any(Function));
				expect(stdoutRemoveListener.mock.calls.every(([event]) => event === "resize")).toBe(
					true,
				);
				if (setRawMode) {
					expect(setRawMode).toHaveBeenCalled();
				}
			} finally {
				await kernel.dispose();
			}
		},
		60_000,
	);

	test(
		"openShell keeps stdout and stderr separate on the native sidecar path",
		async () => {
			const kernel = createKernel({
				filesystem: createInMemoryFileSystem(),
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
		},
		60_000,
	);
});
