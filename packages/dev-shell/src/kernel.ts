import { existsSync } from "node:fs";
import * as fsPromises from "node:fs/promises";
import { createRequire } from "node:module";
import { homedir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import {
	AgentOs,
	createHostDirBackend,
	type ConnectTerminalOptions,
	type OpenShellOptions,
} from "@rivet-dev/agent-os";
import type { DebugLogger } from "./debug-logger.js";
import { createDebugLogger, createNoopLogger } from "./debug-logger.js";
import type { WorkspacePaths } from "./shared.js";
import { collectShellEnv, resolveWorkspacePaths } from "./shared.js";

const moduleDir = path.dirname(fileURLToPath(import.meta.url));
const moduleRequire = createRequire(import.meta.url);

type VmSpawnOptions = Parameters<AgentOs["spawn"]>[2];

export interface DevShellOptions {
	workDir?: string;
	mountWasm?: boolean;
	envFilePath?: string;
	/** When set, structured pino debug logs are written to this file path. */
	debugLogPath?: string;
}

export interface DevShellManagedProcess {
	pid: number;
	writeStdin(data: string | Uint8Array): void;
	closeStdin(): void;
	kill(signal?: number): void;
	wait(): Promise<number>;
	readonly exitCode: number | null;
}

export interface DevShellHandle {
	pid: number;
	write(data: string | Uint8Array): void;
	onData: ((data: Uint8Array) => void) | null;
	resize(cols: number, rows: number): void;
	kill(signal?: number): void;
	wait(): Promise<number>;
}

export interface DevShellKernel {
	spawn(
		command: string,
		args: string[],
		options?: VmSpawnOptions,
	): DevShellManagedProcess;
	openShell(options?: OpenShellOptions): DevShellHandle;
	connectTerminal(options?: ConnectTerminalOptions): Promise<number>;
	dispose(): Promise<void>;
}

export interface DevShellKernelResult {
	kernel: DevShellKernel;
	workDir: string;
	env: Record<string, string>;
	loadedCommands: string[];
	paths: WorkspacePaths;
	logger: DebugLogger;
	dispose: () => Promise<void>;
}

interface ResolvedCommand {
	command: string;
	args: string[];
	driver: "node" | "wasmvm";
}

interface PreparedSpawn {
	command: string;
	args: string[];
	options?: VmSpawnOptions;
	driver: "node" | "wasmvm";
}

const PI_HELP_TEXT = [
	"pi - AI coding assistant",
	"",
	"Usage:",
	"  pi [options] [@files...] [messages...]",
].join("\n");

function resolvePiCliPath(paths: WorkspacePaths): string | undefined {
	try {
		return moduleRequire.resolve("@mariozechner/pi-coding-agent/dist/cli.js");
	} catch {
		const candidates = [
			path.join(
				paths.hostProjectRoot,
				"node_modules",
				"@mariozechner",
				"pi-coding-agent",
				"dist",
				"cli.js",
			),
			path.join(
				paths.workspaceRoot,
				"registry",
				"agent",
				"pi",
				"node_modules",
				"@mariozechner",
				"pi-coding-agent",
				"dist",
				"cli.js",
			),
		];

		return candidates.find((candidate) => existsSync(candidate));
	}
}

function resolveCommand(
	command: string,
	args: string[],
	piCliPath: string | undefined,
): ResolvedCommand {
	if (command === "pi") {
		if (!piCliPath) {
			throw new Error("pi CLI is not available in this workspace");
		}
		if (args.includes("--help") || args.includes("-h")) {
			return {
				command: "node",
				args: ["-e", `console.log(${JSON.stringify(PI_HELP_TEXT)})`],
				driver: "node",
			};
		}
		return {
			command: "node",
			args: [piCliPath, ...args],
			driver: "node",
		};
	}

	return {
		command,
		args,
		driver: command === "node" ? "node" : "wasmvm",
	};
}

function prepareNodeSpawn(
	command: string,
	args: string[],
	options: VmSpawnOptions | undefined,
): PreparedSpawn {
	const requestedCwd = options?.cwd;
	if (!requestedCwd) {
		return { command, args, options, driver: "node" };
	}

	const env = {
		...(options?.env ?? {}),
		PWD: requestedCwd,
	};

	if (args[0] === "-e") {
		const userCode = args[1] ?? "";
		const wrappedCode = [
			`const __agentOsGuestCwd = ${JSON.stringify(requestedCwd)};`,
			"Object.defineProperty(process, 'cwd', {",
			"  configurable: true,",
			"  value: () => __agentOsGuestCwd,",
			"});",
			"process.env.PWD = __agentOsGuestCwd;",
			userCode,
		].join("\n");

		return {
			command,
			args: ["-e", wrappedCode, ...args.slice(2)],
			options: {
				...options,
				cwd: "/root",
				env,
			},
			driver: "node",
		};
	}

	return {
		command,
		args,
		options: {
			...options,
			cwd: "/root",
			env,
		},
		driver: "node",
	};
}

function createDevShellKernelAdapter(
	vm: AgentOs,
	logger: DebugLogger,
	piCliPath: string | undefined,
): DevShellKernel {
	const spawn = (
		command: string,
		args: string[],
		options?: VmSpawnOptions,
	): DevShellManagedProcess => {
		const resolved = resolveCommand(command, args, piCliPath);
		const prepared =
			resolved.command === "node"
				? prepareNodeSpawn(resolved.command, resolved.args, options)
				: {
						command: resolved.command,
						args: resolved.args,
						options,
						driver: resolved.driver,
					};
		const { pid } = vm.spawn(
			prepared.command,
			prepared.args,
			prepared.options,
		);

		logger.info(
			{
				pid,
				command,
				args,
				driver: prepared.driver,
				resolvedCommand: prepared.command,
				resolvedArgs: prepared.args,
			},
			"process spawned",
		);

		const unsubscribeExit = vm.onProcessExit(pid, (exitCode) => {
			logger.info({ pid, command, exitCode }, "process exited");
			unsubscribeExit();
		});

		return {
			pid,
			writeStdin(data) {
				vm.writeProcessStdin(pid, data);
			},
			closeStdin() {
				vm.closeProcessStdin(pid);
			},
			kill(signal = 15) {
				if (signal === 9) {
					vm.killProcess(pid);
					return;
				}
				vm.stopProcess(pid);
			},
			wait() {
				return vm.waitProcess(pid);
			},
			get exitCode() {
				return vm.getProcess(pid).exitCode;
			},
		};
	};

	const openShell = (options?: OpenShellOptions): DevShellHandle => {
		let onData: ((data: Uint8Array) => void) | null = null;
		const command = options?.command ?? "sh";
		const keepStdinOpen = command === "sh" || command === "bash";
		const proc = spawn(options?.command ?? "sh", options?.args ?? [], {
			cwd: options?.cwd,
			env: options?.env,
			streamStdin: keepStdinOpen,
			onStdout: (data) => {
				onData?.(data);
			},
			onStderr: options?.onStderr,
		});

		return {
			pid: proc.pid,
			write(data) {
				proc.writeStdin(data);
			},
			get onData() {
				return onData;
			},
			set onData(handler) {
				onData = handler;
			},
			resize() {
				// The current native dev-shell path is process-backed rather than PTY-backed.
			},
			kill(signal) {
				proc.kill(signal);
			},
			wait() {
				return proc.wait();
			},
		};
	};

	const connectTerminal = async (
		options?: ConnectTerminalOptions,
	): Promise<number> => {
		const stdin = process.stdin;
		const stdout = process.stdout;
		const { onData, ...shellOptions } = options ?? {};
		const shell = openShell({
			...shellOptions,
			onStderr:
				shellOptions.onStderr ??
				((data) => {
					process.stderr.write(data);
				}),
		});
		const outputHandler =
			onData ??
			((data: Uint8Array) => {
				stdout.write(data);
			});
		const restoreRawMode =
			stdin.isTTY && typeof stdin.setRawMode === "function";
		const onStdinData = (data: Uint8Array | string) => {
			shell.write(data);
		};
		const onResize = () => {
			shell.resize(stdout.columns, stdout.rows);
		};

		let cleanedUp = false;
		const cleanup = () => {
			if (cleanedUp) {
				return;
			}
			cleanedUp = true;
			stdin.removeListener("data", onStdinData);
			stdin.pause();
			if (restoreRawMode) {
				stdin.setRawMode(false);
			}
			if (stdout.isTTY) {
				stdout.removeListener("resize", onResize);
			}
		};

		try {
			if (restoreRawMode) {
				stdin.setRawMode(true);
			}
			stdin.on("data", onStdinData);
			stdin.resume();
			shell.onData = outputHandler;
			if (stdout.isTTY) {
				stdout.on("resize", onResize);
			}
			return await shell.wait();
		} finally {
			cleanup();
		}
	};

	return {
		spawn,
		openShell,
		connectTerminal,
		async dispose() {
			await vm.dispose();
		},
	};
}

export async function createDevShellKernel(
	options: DevShellOptions = {},
): Promise<DevShellKernelResult> {
	const paths = resolveWorkspacePaths(moduleDir);
	const workDir = path.resolve(options.workDir ?? process.cwd());
	const mountWasm = options.mountWasm !== false;
	const env = collectShellEnv(options.envFilePath ?? paths.realProviderEnvFile);

	// Set up structured debug logger (file-only, never stdout/stderr).
	const logger = options.debugLogPath
		? createDebugLogger(options.debugLogPath)
		: createNoopLogger();
	logger.info({ workDir, mountWasm }, "dev-shell session init");

	env.HOME = workDir;
	env.XDG_CONFIG_HOME = path.join(workDir, ".config");
	env.XDG_CACHE_HOME = path.join(workDir, ".cache");
	env.XDG_DATA_HOME = path.join(workDir, ".local", "share");
	env.HISTFILE = "/dev/null";
	env.PATH = "/bin";

	await fsPromises.mkdir(workDir, { recursive: true });
	await fsPromises.mkdir(env.XDG_CONFIG_HOME, { recursive: true });
	await fsPromises.mkdir(env.XDG_CACHE_HOME, { recursive: true });
	await fsPromises.mkdir(env.XDG_DATA_HOME, { recursive: true });

	const piCliPath = resolvePiCliPath(paths);
	const hostHomeDir = homedir();
	const hasWasmCommands =
		mountWasm && existsSync(path.join(paths.wasmCommandsDir, "bash"));
	const software = hasWasmCommands
		? [
				{
					commandDir: paths.wasmCommandsDir,
				},
			]
		: [];

	const vm = await AgentOs.create({
		moduleAccessCwd: paths.hostProjectRoot,
		software,
		mounts: [
			{
				path: hostHomeDir,
				plugin: createHostDirBackend({
					hostPath: hostHomeDir,
					readOnly: false,
				}),
			},
			{
				path: "/tmp",
				plugin: createHostDirBackend({ hostPath: "/tmp", readOnly: false }),
			},
		],
	});

	logger.info(
		{ driver: "node", commands: ["node", "npm", "npx"] },
		"runtime driver mounted",
	);
	if (hasWasmCommands) {
		logger.info(
			{ driver: "wasmvm", commandDir: paths.wasmCommandsDir },
			"runtime driver mounted",
		);
	}
	if (piCliPath) {
		logger.info(
			{ driver: "node", commands: ["pi"], entrypoint: piCliPath },
			"runtime driver mounted",
		);
	}

	const loadedCommands = Array.from(
		new Set([
			"node",
			"npm",
			"npx",
			...(hasWasmCommands ? ["bash", "sh"] : []),
			...(piCliPath ? ["pi"] : []),
		]),
	).sort();
	logger.info({ loadedCommands }, "dev-shell ready");

	const kernel = createDevShellKernelAdapter(vm, logger, piCliPath);

	return {
		kernel,
		workDir,
		env,
		loadedCommands,
		paths,
		logger,
		dispose: async () => {
			logger.info("dev-shell disposing");
			await kernel.dispose();
			await logger.close();
		},
	};
}
