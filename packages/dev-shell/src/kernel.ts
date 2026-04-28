import { existsSync } from "node:fs";
import type { Stats } from "node:fs";
import * as fsPromises from "node:fs/promises";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import type {
	Kernel,
	ManagedProcess,
	ShellHandle,
	VirtualFileSystem,
} from "@rivet-dev/agent-os-core/internal/runtime-compat";
import * as runtimeCompat from "@rivet-dev/agent-os-core/internal/runtime-compat";
import type { DebugLogger } from "./debug-logger.js";
import { createDebugLogger, createNoopLogger } from "./debug-logger.js";
import type { WorkspacePaths } from "./shared.js";
import { collectShellEnv, resolveWorkspacePaths } from "./shared.js";

const moduleDir = path.dirname(fileURLToPath(import.meta.url));
const moduleRequire = createRequire(import.meta.url);
const DEV_SHELL_TMP_ROOT_PREFIX = `agent-os-dev-shell-${process.pid}-`;
export interface DevShellOptions {
	workDir?: string;
	mountWasm?: boolean;
	envFilePath?: string;
	/** When set, structured pino debug logs are written to this file path. */
	debugLogPath?: string;
}

export interface DevShellKernelResult {
	kernel: Kernel;
	workDir: string;
	env: Record<string, string>;
	loadedCommands: string[];
	paths: WorkspacePaths;
	logger: DebugLogger;
	dispose: () => Promise<void>;
}

async function createSessionTmpRoot(): Promise<{
	rootDir: string;
	tmpDir: string;
}> {
	const rootDir = await fsPromises.mkdtemp(
		path.join(tmpdir(), DEV_SHELL_TMP_ROOT_PREFIX),
	);
	const tmpDir = path.join(rootDir, "tmp");
	await fsPromises.mkdir(tmpDir, { recursive: true });
	return { rootDir, tmpDir };
}

function isWithinVirtualPath(targetPath: string, prefix: string): boolean {
	const normalizedTarget = path.posix.normalize(targetPath);
	const normalizedPrefix = path.posix.normalize(prefix);
	return (
		normalizedTarget === normalizedPrefix ||
		normalizedTarget.startsWith(`${normalizedPrefix}/`)
	);
}

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
			path.join(
				paths.workspaceRoot,
				"packages",
				"core",
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

function prepareKernelInvocation(
	command: string,
	args: string[],
	piCliPath: string | undefined,
	cwd?: string,
): {
	command: string;
	args: string[];
	driver: string;
	cwd?: string;
	env?: Record<string, string>;
	execCommand?: string;
} {
	if (command === "pi" && piCliPath) {
		if (args.includes("--help") || args.includes("-h")) {
			return {
				command: "node",
				args: [
					"-e",
					[
						'process.stdout.write("Usage: pi [options] [prompt]\\n");',
						'process.stdout.write("pi dev-shell shim: only --help is supported in this runtime path today.\\n");',
					].join("\n"),
				],
				driver: "node",
			};
		}

		return {
			command: "node",
			args: [
				"-e",
				[
					"process.stderr.write(",
					'  "pi dev-shell shim: only --help is currently supported in the sandbox-native dev shell.\\n",',
					");",
					"process.exit(1);",
				].join("\n"),
			],
			driver: "node",
		};
	}

	if (command === "node" && args[0] === "-e") {
		return {
			command: "node",
			args: [
				"-e",
				[
					"const __agentOsFormat = (value) => {",
					'  if (typeof value === "string") return value;',
					"  try {",
					"    return typeof value === 'object' ? JSON.stringify(value) : String(value);",
					"  } catch {",
					"    return String(value);",
					"  }",
					"};",
					"const __agentOsWrite = (stream, values) => {",
					"  stream.write(values.map(__agentOsFormat).join(' ') + '\\n');",
					"};",
					"globalThis.console = {",
					"  ...(globalThis.console ?? {}),",
					"  log: (...values) => __agentOsWrite(process.stdout, values),",
					"  info: (...values) => __agentOsWrite(process.stdout, values),",
					"  warn: (...values) => __agentOsWrite(process.stderr, values),",
					"  error: (...values) => __agentOsWrite(process.stderr, values),",
					"};",
					"(async () => {",
					args[1] ?? "",
					"})().catch((error) => {",
					"  process.stderr.write(",
					'    String(error && error.stack ? error.stack : error) + "\\n",',
					"  );",
					"  process.exit(1);",
					"});",
				].join("\n"),
			],
			driver: "node",
		};
	}

	if (command === "node" && args[0] === "--version") {
		return {
			command: "node",
			args: ["-e", 'process.stdout.write(String(process.version) + "\\n");'],
			driver: "node",
		};
	}

	if (
		(command === "bash" || command === "sh") &&
		(args[0] === "-c" || args[0] === "-lc" || args[0] === "-ic") &&
		args.length === 2
	) {
		return {
			command,
			args,
			driver: `${command}:exec`,
			execCommand: args[1],
		};
	}

	return {
		command,
		args,
		driver: command,
	};
}

function wrapManagedProcess(
	process: ManagedProcess,
	logger: DebugLogger,
	logFields: Record<string, unknown>,
): ManagedProcess {
	let waitPromise: Promise<number> | null = null;

	return {
		pid: process.pid,
		writeStdin(data) {
			process.writeStdin(data);
		},
		closeStdin() {
			process.closeStdin();
		},
		kill(signal) {
			process.kill(signal);
		},
		wait() {
			if (waitPromise !== null) {
				return waitPromise;
			}
			waitPromise = process.wait().then((exitCode) => {
				logger.info({ ...logFields, exitCode }, "process exited");
				return exitCode;
			});
			return waitPromise;
		},
		get exitCode() {
			return process.exitCode;
		},
	};
}

function classifySpawnFailure(error: unknown): {
	exitCode: number;
	stderr: string;
} {
	const message = error instanceof Error ? error.message : String(error);
	const lowerMessage = message.toLowerCase();
	const exitCode =
		message.includes("EACCES") || lowerMessage.includes("permission denied")
			? 126
			: 127;
	return {
		exitCode,
		stderr: message.endsWith("\n") ? message : `${message}\n`,
	};
}

function createFailedManagedProcess(
	pid: number,
	exitCode: number,
): ManagedProcess {
	return {
		pid,
		writeStdin() {},
		closeStdin() {},
		kill() {},
		wait() {
			return Promise.resolve(exitCode);
		},
		get exitCode() {
			return exitCode;
		},
	};
}

function wrapShellHandle(
	handle: ShellHandle,
	logger: DebugLogger,
	logFields: Record<string, unknown>,
): ShellHandle {
	let waitPromise: Promise<number> | null = null;
	let onData: ((data: Uint8Array) => void) | null = null;
	const pendingOutput: Uint8Array[] = [];

	handle.onData = (data) => {
		if (onData) {
			onData(data);
			return;
		}
		pendingOutput.push(data);
	};

	return {
		pid: handle.pid,
		write(data) {
			handle.write(data);
		},
		get onData() {
			return onData;
		},
		set onData(value) {
			onData = value;
			if (!value || pendingOutput.length === 0) {
				return;
			}
			for (const chunk of pendingOutput.splice(0)) {
				value(chunk);
			}
		},
		resize(cols, rows) {
			logger.info({ ...logFields, cols, rows }, "pty resized");
			handle.resize(cols, rows);
		},
		kill(signal) {
			handle.kill(signal);
		},
		wait() {
			if (waitPromise !== null) {
				return waitPromise;
			}
			waitPromise = handle.wait().then((exitCode) => {
				logger.info({ ...logFields, exitCode }, "pty exited");
				return exitCode;
			});
			return waitPromise;
		},
	};
}

function wrapKernel(
	kernel: Kernel,
	logger: DebugLogger,
	piCliPath: string | undefined,
): Kernel {
	const commands = new Map(kernel.commands);
	let syntheticFailurePid = 1_000_000;
	if (piCliPath) {
		commands.set("pi", "node");
	}

	const wrappedKernel = Object.create(kernel) as Kernel;
	Object.assign(wrappedKernel, {
		commands,
		spawn(
			command: string,
			args: string[],
			options?: Parameters<Kernel["spawn"]>[2],
		) {
			const translated = prepareKernelInvocation(
				command,
				args,
				piCliPath,
				options?.cwd,
			);
			try {
				if (
					translated.execCommand !== undefined &&
					options?.streamStdin !== true &&
					options?.stdinFd === undefined &&
					options?.stdoutFd === undefined &&
					options?.stderrFd === undefined
				) {
					const execCommand = translated.execCommand;
					const pid = syntheticFailurePid++;
					let exitCode: number | null = null;
					let waitPromise: Promise<number> | null = null;
					const execOptions = {
						cwd: translated.cwd ?? options?.cwd,
						env: {
							...(options?.env ?? {}),
							...(translated.env ?? {}),
						},
					};
					const logFields = {
						pid,
						command,
						args,
						driver: translated.driver,
						cwd: options?.cwd,
					};
					logger.info(logFields, "process spawned");
					return wrapManagedProcess(
						{
							pid,
							writeStdin() {},
							closeStdin() {},
							kill() {},
							wait() {
									if (waitPromise !== null) {
										return waitPromise;
									}
									waitPromise = wrappedKernel
										.exec(execCommand, execOptions)
									.then((result) => {
										if (result.stdout.length > 0) {
											options?.onStdout?.(Buffer.from(result.stdout, "utf8"));
										}
										if (result.stderr.length > 0) {
											options?.onStderr?.(Buffer.from(result.stderr, "utf8"));
										}
										exitCode = result.exitCode;
										return result.exitCode;
									})
									.catch((error) => {
										const failure = classifySpawnFailure(error);
										if (failure.stderr.length > 0) {
											options?.onStderr?.(Buffer.from(failure.stderr, "utf8"));
										}
										exitCode = failure.exitCode;
										return failure.exitCode;
									});
								return waitPromise;
							},
							get exitCode() {
								return exitCode;
							},
						},
						logger,
						logFields,
					);
				}

				const process = kernel.spawn(translated.command, translated.args, {
					...options,
					cwd: translated.cwd ?? options?.cwd,
					env: {
						...(options?.env ?? {}),
						...(translated.env ?? {}),
					},
				});
				const logFields = {
					pid: process.pid,
					command,
					args,
					driver: translated.driver,
					cwd: options?.cwd,
				};
				logger.info(logFields, "process spawned");
				return wrapManagedProcess(process, logger, logFields);
			} catch (error) {
				const failurePid = syntheticFailurePid++;
				const { exitCode, stderr } = classifySpawnFailure(error);
				if (stderr.length > 0) {
					options?.onStderr?.(Buffer.from(stderr, "utf8"));
				}
				logger.info(
					{
						pid: failurePid,
						command,
						args,
						driver: translated.driver,
						cwd: options?.cwd,
						exitCode,
						error: stderr.trimEnd(),
					},
					"process spawn rejected",
				);
				return createFailedManagedProcess(failurePid, exitCode);
			}
		},
		openShell(options?: Parameters<Kernel["openShell"]>[0]) {
			const requestedCommand = options?.command ?? "sh";
			const requestedArgs =
				options?.args ??
				((requestedCommand === "bash" || requestedCommand === "sh")
					? ["-i"]
					: []);
			const translated = prepareKernelInvocation(
				requestedCommand,
				requestedArgs,
				piCliPath,
				options?.cwd,
			);
			const handle = kernel.openShell({
				...options,
				command: translated.command,
				args: translated.args,
				cwd: translated.cwd ?? options?.cwd,
				env: {
					...(options?.env ?? {}),
					...(translated.env ?? {}),
				},
			});
			const logFields = {
				pid: handle.pid,
				command: requestedCommand,
				args: requestedArgs,
				driver: translated.driver,
				cwd: options?.cwd,
				cols: options?.cols,
				rows: options?.rows,
			};
			logger.info(logFields, "pty opened");
			return wrapShellHandle(handle, logger, logFields);
		},
		async connectTerminal(options?: Parameters<Kernel["connectTerminal"]>[0]) {
			const requestedCommand = options?.command ?? "sh";
			const requestedArgs =
				options?.args ??
				((requestedCommand === "bash" || requestedCommand === "sh")
					? ["-i"]
					: []);
			const translated = prepareKernelInvocation(
				requestedCommand,
				requestedArgs,
				piCliPath,
				options?.cwd,
			);
			logger.info(
				{
					command: requestedCommand,
					args: requestedArgs,
					driver: translated.driver,
					cwd: options?.cwd,
					cols: options?.cols,
					rows: options?.rows,
				},
				"pty connected",
			);
			const exitCode = await kernel.connectTerminal({
				...options,
				command: translated.command,
				args: translated.args,
				cwd: translated.cwd ?? options?.cwd,
				env: {
					...(options?.env ?? {}),
					...(translated.env ?? {}),
				},
			});
			logger.info(
				{
					command: requestedCommand,
					args: requestedArgs,
					driver: translated.driver,
					exitCode,
				},
				"pty exited",
			);
			return exitCode;
		},
	});

	return wrappedKernel;
}

async function seedFilesystemFromHost(
	filesystem: VirtualFileSystem,
	hostPath: string,
	guestPath: string,
): Promise<void> {
	let stats: Stats;
	try {
		stats = await fsPromises.lstat(hostPath);
	} catch (error) {
		const code =
			typeof error === "object" && error !== null && "code" in error
				? String((error as { code?: unknown }).code)
				: undefined;
		if (code === "ENOENT") {
			await filesystem.mkdir(guestPath, { recursive: true });
			return;
		}
		throw error;
	}

	if (stats.isSymbolicLink()) {
		const linkTarget = await fsPromises.readlink(hostPath);
		await filesystem.symlink(linkTarget, guestPath);
		return;
	}

	if (stats.isDirectory()) {
		await filesystem.mkdir(guestPath, { recursive: true });
		const entries = await fsPromises.readdir(hostPath, { withFileTypes: true });
		for (const entry of entries) {
			await seedFilesystemFromHost(
				filesystem,
				path.join(hostPath, entry.name),
				path.posix.join(guestPath, entry.name),
			);
		}
		await filesystem.chmod(guestPath, stats.mode & 0o7777);
		return;
	}

	const fileBytes = await fsPromises.readFile(hostPath);
	await filesystem.writeFile(guestPath, fileBytes);
	await filesystem.chmod(guestPath, stats.mode & 0o7777);
}

export async function createDevShellKernel(
	options: DevShellOptions = {},
): Promise<DevShellKernelResult> {
	const paths = resolveWorkspacePaths(moduleDir);
	const workDir = path.resolve(options.workDir ?? process.cwd());
	const mountWasm = options.mountWasm !== false;
	const sessionTmpRoot = await createSessionTmpRoot();
	const env = collectShellEnv(options.envFilePath ?? paths.realProviderEnvFile);
	if (!process.env.AGENT_OS_NODE_BINARY) {
		process.env.AGENT_OS_NODE_BINARY = process.execPath;
	}

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
	env.TMPDIR = "/tmp";
	env.TMP = "/tmp";
	env.TEMP = "/tmp";
	if (!env.AGENT_OS_NODE_BINARY) {
		env.AGENT_OS_NODE_BINARY = process.execPath;
	}

	const piCliPath = resolvePiCliPath(paths);
	const filesystem = runtimeCompat.createInMemoryFileSystem();
	try {
		await seedFilesystemFromHost(filesystem, workDir, workDir);
		await filesystem.mkdir("/tmp", { recursive: true });
		await filesystem.mkdir(env.XDG_CONFIG_HOME, { recursive: true });
		await filesystem.mkdir(env.XDG_CACHE_HOME, { recursive: true });
		await filesystem.mkdir(env.XDG_DATA_HOME, { recursive: true });

		const sessionTmpFileSystem = new runtimeCompat.NodeFileSystem({
			root: sessionTmpRoot.tmpDir,
		});
		if (workDir.startsWith("/tmp/")) {
			const workDirInTmpMount = workDir.slice("/tmp".length);
			await seedFilesystemFromHost(
				sessionTmpFileSystem,
				workDir,
				workDirInTmpMount,
			);
			if (isWithinVirtualPath(env.XDG_CONFIG_HOME, workDir)) {
				await sessionTmpFileSystem.mkdir(env.XDG_CONFIG_HOME.slice("/tmp".length), {
					recursive: true,
				});
			}
			if (isWithinVirtualPath(env.XDG_CACHE_HOME, workDir)) {
				await sessionTmpFileSystem.mkdir(env.XDG_CACHE_HOME.slice("/tmp".length), {
					recursive: true,
				});
			}
			if (isWithinVirtualPath(env.XDG_DATA_HOME, workDir)) {
				await sessionTmpFileSystem.mkdir(env.XDG_DATA_HOME.slice("/tmp".length), {
					recursive: true,
				});
			}
		}

		const mounts: Array<{
			path: string;
			fs: VirtualFileSystem;
		}> = [
			{
				path: "/tmp",
				fs: sessionTmpFileSystem,
			},
		];

		const kernel = runtimeCompat.createKernel({
			filesystem,
			hostNetworkAdapter: runtimeCompat.createNodeHostNetworkAdapter(),
			permissions: runtimeCompat.allowAll,
			env,
			cwd: workDir,
			logger,
			mounts,
		});

		const loadedCommands: string[] = [];

		if (mountWasm) {
			const wasmRuntime = runtimeCompat.createWasmVmRuntime({
				commandDirs: paths.wasmCommandDirs,
			});
			await kernel.mount(wasmRuntime);
			loadedCommands.push(...wasmRuntime.commands);
			logger.info(
				{ driver: wasmRuntime.name, commands: wasmRuntime.commands },
				"runtime driver mounted",
			);
		}

		const nodeRuntime = runtimeCompat.createNodeRuntime();
		await kernel.mount(nodeRuntime);
		loadedCommands.push(...nodeRuntime.commands);
		logger.info(
			{ driver: nodeRuntime.name, commands: nodeRuntime.commands },
			"runtime driver mounted",
		);

		if (piCliPath) {
			loadedCommands.push("pi");
			logger.info({ command: "pi", piCliPath }, "runtime driver mounted");
		}

		const filteredCommands = Array.from(new Set(loadedCommands))
			.filter((command) => command.trim().length > 0 && !command.startsWith("_"))
			.sort();
		logger.info({ loadedCommands: filteredCommands }, "dev-shell ready");
		const wrappedKernel = wrapKernel(kernel, logger, piCliPath);

		return {
			kernel: wrappedKernel,
			workDir,
			env,
			loadedCommands: filteredCommands,
			paths,
			logger,
			dispose: async () => {
				logger.info("dev-shell disposing");
				try {
					await kernel.dispose();
				} finally {
					await fsPromises.rm(sessionTmpRoot.rootDir, {
						recursive: true,
						force: true,
					});
					await logger.close();
				}
			},
		};
	} catch (error) {
		await fsPromises.rm(sessionTmpRoot.rootDir, {
			recursive: true,
			force: true,
		});
		await logger.close();
		throw error;
	}
}
