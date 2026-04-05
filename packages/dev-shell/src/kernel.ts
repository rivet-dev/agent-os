import { existsSync } from "node:fs";
import * as fsPromises from "node:fs/promises";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";
import {
	allowAll,
	createInMemoryFileSystem,
	createKernel,
	createProcessScopedFileSystem,
	createDefaultNetworkAdapter,
	createHostFallbackVfs,
	createKernelCommandExecutor,
	createKernelVfsAdapter,
	createNodeDriver,
	createNodeHostNetworkAdapter,
	createNodeRuntime,
	type DriverProcess,
	type Kernel,
	type KernelInterface,
	type KernelRuntimeDriver as RuntimeDriver,
	type Permissions,
	type ProcessContext,
	type VirtualFileSystem,
	NodeExecutionDriver,
} from "@rivet-dev/agent-os/internal/runtime-compat";
import { createWasmVmRuntime } from "@rivet-dev/agent-os/test/runtime";
import type { DebugLogger } from "./debug-logger.js";
import { createDebugLogger, createNoopLogger } from "./debug-logger.js";
import type { WorkspacePaths } from "./shared.js";
import { collectShellEnv, resolveWorkspacePaths } from "./shared.js";

const moduleDir = path.dirname(fileURLToPath(import.meta.url));
const moduleRequire = createRequire(import.meta.url);

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

function normalizeHostRoots(roots: string[]): string[] {
	return Array.from(
		new Set(
			roots.filter((root) => root.length > 0).map((root) => path.resolve(root)),
		),
	).sort((left, right) => right.length - left.length);
}

function isWithinHostRoots(targetPath: string, roots: string[]): boolean {
	const resolved = path.resolve(targetPath);
	return roots.some(
		(root) => resolved === root || resolved.startsWith(`${root}${path.sep}`),
	);
}

function toIntegerTimestamp(value: number): number {
	return Math.trunc(value);
}

function createHybridVfs(hostRoots: string[]): VirtualFileSystem {
	const memfs = createInMemoryFileSystem();
	const normalizedRoots = normalizeHostRoots(hostRoots);

	const withHostFallback = async <T>(
		targetPath: string,
		op: () => Promise<T>,
	): Promise<T> => {
		try {
			return await op();
		} catch {
			if (!isWithinHostRoots(targetPath, normalizedRoots)) {
				throw new Error(`ENOENT: ${targetPath}`);
			}
			throw new Error("__HOST_FALLBACK__");
		}
	};

	return {
		readFile: async (targetPath) => {
			try {
				return await withHostFallback(targetPath, () =>
					memfs.readFile(targetPath),
				);
			} catch (error) {
				if ((error as Error).message !== "__HOST_FALLBACK__") throw error;
				return new Uint8Array(await fsPromises.readFile(targetPath));
			}
		},
		readTextFile: async (targetPath) => {
			try {
				return await withHostFallback(targetPath, () =>
					memfs.readTextFile(targetPath),
				);
			} catch (error) {
				if ((error as Error).message !== "__HOST_FALLBACK__") throw error;
				return await fsPromises.readFile(targetPath, "utf8");
			}
		},
		readDir: async (targetPath) => {
			try {
				return await withHostFallback(targetPath, () =>
					memfs.readDir(targetPath),
				);
			} catch (error) {
				if ((error as Error).message !== "__HOST_FALLBACK__") throw error;
				return await fsPromises.readdir(targetPath);
			}
		},
		readDirWithTypes: async (targetPath) => {
			try {
				return await withHostFallback(targetPath, () =>
					memfs.readDirWithTypes(targetPath),
				);
			} catch (error) {
				if ((error as Error).message !== "__HOST_FALLBACK__") throw error;
				const entries = await fsPromises.readdir(targetPath, {
					withFileTypes: true,
				});
				return entries.map((entry) => ({
					name: entry.name,
					isDirectory: entry.isDirectory(),
					isSymbolicLink: entry.isSymbolicLink(),
				}));
			}
		},
		exists: async (targetPath) => {
			if (await memfs.exists(targetPath)) return true;
			if (!isWithinHostRoots(targetPath, normalizedRoots)) return false;
			try {
				await fsPromises.access(targetPath);
				return true;
			} catch {
				return false;
			}
		},
		stat: async (targetPath) => {
			try {
				return await withHostFallback(targetPath, () => memfs.stat(targetPath));
			} catch (error) {
				if ((error as Error).message !== "__HOST_FALLBACK__") throw error;
				const info = await fsPromises.stat(targetPath);
				return {
					mode: info.mode,
					size: info.size,
					isDirectory: info.isDirectory(),
					isSymbolicLink: false,
					atimeMs: toIntegerTimestamp(info.atimeMs),
					mtimeMs: toIntegerTimestamp(info.mtimeMs),
					ctimeMs: toIntegerTimestamp(info.ctimeMs),
					birthtimeMs: toIntegerTimestamp(info.birthtimeMs),
					ino: info.ino,
					nlink: info.nlink,
					uid: info.uid,
					gid: info.gid,
				};
			}
		},
		lstat: async (targetPath) => {
			try {
				return await withHostFallback(targetPath, () =>
					memfs.lstat(targetPath),
				);
			} catch (error) {
				if ((error as Error).message !== "__HOST_FALLBACK__") throw error;
				const info = await fsPromises.lstat(targetPath);
				return {
					mode: info.mode,
					size: info.size,
					isDirectory: info.isDirectory(),
					isSymbolicLink: info.isSymbolicLink(),
					atimeMs: toIntegerTimestamp(info.atimeMs),
					mtimeMs: toIntegerTimestamp(info.mtimeMs),
					ctimeMs: toIntegerTimestamp(info.ctimeMs),
					birthtimeMs: toIntegerTimestamp(info.birthtimeMs),
					ino: info.ino,
					nlink: info.nlink,
					uid: info.uid,
					gid: info.gid,
				};
			}
		},
		realpath: async (targetPath) => {
			try {
				return await withHostFallback(targetPath, () =>
					memfs.realpath(targetPath),
				);
			} catch (error) {
				if ((error as Error).message !== "__HOST_FALLBACK__") throw error;
				return await fsPromises.realpath(targetPath);
			}
		},
		readlink: async (targetPath) => {
			try {
				return await withHostFallback(targetPath, () =>
					memfs.readlink(targetPath),
				);
			} catch (error) {
				if ((error as Error).message !== "__HOST_FALLBACK__") throw error;
				return await fsPromises.readlink(targetPath);
			}
		},
		pread: async (targetPath, offset, length) => {
			try {
				return await withHostFallback(targetPath, () =>
					memfs.pread(targetPath, offset, length),
				);
			} catch (error) {
				if ((error as Error).message !== "__HOST_FALLBACK__") throw error;
				const handle = await fsPromises.open(targetPath, "r");
				try {
					const buffer = Buffer.alloc(length);
					const { bytesRead } = await handle.read(buffer, 0, length, offset);
					return new Uint8Array(buffer.buffer, buffer.byteOffset, bytesRead);
				} finally {
					await handle.close();
				}
			}
		},
		pwrite: async (targetPath, offset, data) => {
			try {
				return await withHostFallback(targetPath, () =>
					memfs.pwrite(targetPath, offset, data),
				);
			} catch (error) {
				if ((error as Error).message !== "__HOST_FALLBACK__") throw error;
				const handle = await fsPromises.open(targetPath, "r+");
				try {
					await handle.write(data, 0, data.length, offset);
				} finally {
					await handle.close();
				}
			}
		},
		writeFile: (targetPath, content) =>
			isWithinHostRoots(targetPath, normalizedRoots)
				? fsPromises.writeFile(targetPath, content)
				: memfs.writeFile(targetPath, content),
		createDir: (targetPath) =>
			isWithinHostRoots(targetPath, normalizedRoots)
				? fsPromises.mkdir(targetPath).then(() => {})
				: memfs.createDir(targetPath),
		mkdir: (targetPath, options) =>
			isWithinHostRoots(targetPath, normalizedRoots)
				? fsPromises
						.mkdir(targetPath, { recursive: options?.recursive ?? true })
						.then(() => {})
				: memfs.mkdir(targetPath, options),
		removeFile: (targetPath) =>
			isWithinHostRoots(targetPath, normalizedRoots)
				? fsPromises.unlink(targetPath)
				: memfs.removeFile(targetPath),
		removeDir: (targetPath) =>
			isWithinHostRoots(targetPath, normalizedRoots)
				? fsPromises.rm(targetPath, { recursive: true, force: false })
				: memfs.removeDir(targetPath),
		rename: (oldPath, newPath) =>
			isWithinHostRoots(oldPath, normalizedRoots) ||
			isWithinHostRoots(newPath, normalizedRoots)
				? fsPromises.rename(oldPath, newPath)
				: memfs.rename(oldPath, newPath),
		symlink: (target, linkPath) =>
			isWithinHostRoots(linkPath, normalizedRoots)
				? fsPromises.symlink(target, linkPath)
				: memfs.symlink(target, linkPath),
		link: (oldPath, newPath) =>
			isWithinHostRoots(oldPath, normalizedRoots) ||
			isWithinHostRoots(newPath, normalizedRoots)
				? fsPromises.link(oldPath, newPath)
				: memfs.link(oldPath, newPath),
		chmod: (targetPath, mode) =>
			isWithinHostRoots(targetPath, normalizedRoots)
				? fsPromises.chmod(targetPath, mode)
				: memfs.chmod(targetPath, mode),
		chown: (targetPath, uid, gid) =>
			isWithinHostRoots(targetPath, normalizedRoots)
				? fsPromises.chown(targetPath, uid, gid)
				: memfs.chown(targetPath, uid, gid),
		utimes: (targetPath, atime, mtime) =>
			isWithinHostRoots(targetPath, normalizedRoots)
				? fsPromises.utimes(targetPath, atime, mtime)
				: memfs.utimes(targetPath, atime, mtime),
		truncate: (targetPath, length) =>
			isWithinHostRoots(targetPath, normalizedRoots)
				? fsPromises.truncate(targetPath, length)
				: memfs.truncate(targetPath, length),
	};
}

class SandboxNodeScriptDriver implements RuntimeDriver {
	readonly name: string;
	readonly commands: string[];
	private readonly entryPath: string;
	private readonly moduleAccessCwd: string;
	private readonly permissions: Partial<Permissions>;
	private readonly launchMode: "file" | "import";
	private kernel: KernelInterface | null = null;
	private activeDrivers = new Map<number, NodeExecutionDriver>();

	constructor(
		command: string,
		entryPath: string,
		permissions: Partial<Permissions>,
		moduleAccessCwd?: string,
		launchMode: "file" | "import" = "file",
	) {
		this.name = `${command}-driver`;
		this.commands = [command];
		this.entryPath = entryPath;
		this.moduleAccessCwd = moduleAccessCwd ?? path.dirname(entryPath);
		this.permissions = permissions;
		this.launchMode = launchMode;
	}

	async init(kernel: KernelInterface): Promise<void> {
		this.kernel = kernel;
	}

	spawn(_command: string, args: string[], ctx: ProcessContext): DriverProcess {
		const kernel = this.kernel;
		if (!kernel) throw new Error("SandboxNodeScriptDriver not initialized");

		let resolveExit!: (code: number) => void;
		let exitResolved = false;
		const exitPromise = new Promise<number>((resolve) => {
			resolveExit = (code) => {
				if (exitResolved) return;
				exitResolved = true;
				resolve(code);
			};
		});

		const stdinChunks: Uint8Array[] = [];
		let stdinResolve: ((value: string | undefined) => void) | null = null;
		const stdinPromise = new Promise<string | undefined>((resolve) => {
			stdinResolve = resolve;
			queueMicrotask(() => {
				if (stdinResolve && stdinChunks.length === 0) {
					stdinResolve = null;
					resolve(undefined);
				}
			});
		});

		let killedSignal: number | null = null;

		const proc: DriverProcess = {
			onStdout: null,
			onStderr: null,
			onExit: null,
			writeStdin: (data) => {
				stdinChunks.push(data);
			},
			closeStdin: () => {
				if (!stdinResolve) return;
				if (stdinChunks.length === 0) {
					stdinResolve(undefined);
				} else {
					const totalLength = stdinChunks.reduce(
						(sum, chunk) => sum + chunk.length,
						0,
					);
					const merged = new Uint8Array(totalLength);
					let offset = 0;
					for (const chunk of stdinChunks) {
						merged.set(chunk, offset);
						offset += chunk.length;
					}
					stdinResolve(new TextDecoder().decode(merged));
				}
				stdinResolve = null;
			},
			kill: (signal) => {
				if (exitResolved) return;
				killedSignal = signal > 0 ? signal : 15;
				const driver = this.activeDrivers.get(ctx.pid);
				if (!driver) {
					const exitCode = 128 + killedSignal;
					resolveExit(exitCode);
					proc.onExit?.(exitCode);
					return;
				}
				this.activeDrivers.delete(ctx.pid);
				void driver
					.terminate()
					.catch(() => {
						driver.dispose();
					})
					.finally(() => {
						const exitCode = 128 + (killedSignal ?? 15);
						resolveExit(exitCode);
						proc.onExit?.(exitCode);
					});
			},
			wait: () => exitPromise,
		};

		void this.executeAsync(
			kernel,
			args,
			ctx,
			proc,
			resolveExit,
			stdinPromise,
			() => killedSignal,
		);

		return proc;
	}

	async dispose(): Promise<void> {
		for (const driver of this.activeDrivers.values()) {
			try {
				driver.dispose();
			} catch {
				// best effort
			}
		}
		this.activeDrivers.clear();
		this.kernel = null;
	}

	private async executeAsync(
		kernel: KernelInterface,
		args: string[],
		ctx: ProcessContext,
		proc: DriverProcess,
		resolveExit: (code: number) => void,
		stdinPromise: Promise<string | undefined>,
		getKilledSignal: () => number | null,
	): Promise<void> {
		try {
			const code =
				this.launchMode === "import"
					? [
							"(async () => {",
							`  process.argv = ${JSON.stringify([process.execPath, this.commands[0], ...args])};`,
							`  await import(${JSON.stringify(this.entryPath)});`,
							"})().catch((error) => {",
							"  const message = error && error.stack ? error.stack : String(error);",
							"  const exitMatch = /process\\.exit\\((\\d+)\\)/.exec(message);",
							"  if (exitMatch) {",
							"    process.exit(Number.parseInt(exitMatch[1], 10));",
							"  }",
							"  console.error(message);",
							"  process.exit(1);",
							"});",
						].join("\n")
					: await kernel.vfs.readTextFile(this.entryPath);
			const stdinData = await stdinPromise;
			if (getKilledSignal() !== null) return;

			let filesystem: VirtualFileSystem = createProcessScopedFileSystem(
				createKernelVfsAdapter(kernel.vfs),
				ctx.pid,
			);
			filesystem = createHostFallbackVfs(filesystem);

			const systemDriver = createNodeDriver({
				filesystem,
				moduleAccess: { cwd: this.moduleAccessCwd },
				networkAdapter: kernel.socketTable.hasHostNetworkAdapter()
					? createDefaultNetworkAdapter()
					: undefined,
				commandExecutor: createKernelCommandExecutor(kernel, ctx.pid),
				permissions: this.permissions,
				processConfig: {
					cwd: ctx.cwd,
					env: ctx.env,
					argv: [process.execPath, this.entryPath, ...args],
					stdinIsTTY: ctx.stdinIsTTY ?? false,
					stdoutIsTTY: ctx.stdoutIsTTY ?? false,
					stderrIsTTY: ctx.stderrIsTTY ?? false,
				},
				osConfig: {
					homedir: ctx.env.HOME || "/root",
					tmpdir: ctx.env.TMPDIR || "/tmp",
				},
			});

			const onPtySetRawMode = ctx.stdinIsTTY
				? (mode: boolean) => {
						kernel.tcsetattr(ctx.pid, 0, {
							icanon: !mode,
							echo: !mode,
							isig: !mode,
							icrnl: !mode,
						});
					}
				: undefined;

			const liveStdinSource = ctx.stdinIsTTY
				? {
						async read() {
							try {
								const chunk = await kernel.fdRead(ctx.pid, 0, 4096);
								return chunk.length === 0 ? null : chunk;
							} catch {
								return null;
							}
						},
					}
				: undefined;

			const executionDriver = new NodeExecutionDriver({
				system: systemDriver,
				runtime: systemDriver.runtime,
				memoryLimit: 128,
				onPtySetRawMode,
				socketTable: kernel.socketTable,
				processTable: kernel.processTable,
				timerTable: kernel.timerTable,
				pid: ctx.pid,
				liveStdinSource,
			});

			this.activeDrivers.set(ctx.pid, executionDriver);
			if (getKilledSignal() !== null) {
				this.activeDrivers.delete(ctx.pid);
				try {
					await executionDriver.terminate();
				} catch {
					executionDriver.dispose();
				}
				return;
			}

			const result = await executionDriver.exec(code, {
				filePath:
					this.launchMode === "import"
						? path.join(ctx.cwd, `.${this.commands[0]}-launcher.cjs`)
						: this.entryPath,
				env: ctx.env,
				cwd: ctx.cwd,
				stdin: stdinData,
				onStdio: (event) => {
					const bytes = new TextEncoder().encode(event.message);
					if (event.channel === "stdout") {
						ctx.onStdout?.(bytes);
						proc.onStdout?.(bytes);
					} else {
						ctx.onStderr?.(bytes);
						proc.onStderr?.(bytes);
					}
				},
			});

			if (result.errorMessage) {
				const bytes = new TextEncoder().encode(`${result.errorMessage}\n`);
				ctx.onStderr?.(bytes);
				proc.onStderr?.(bytes);
			}

			executionDriver.dispose();
			this.activeDrivers.delete(ctx.pid);
			resolveExit(result.code);
			proc.onExit?.(result.code);
		} catch (error) {
			const message = error instanceof Error ? error.message : String(error);
			const bytes = new TextEncoder().encode(`pi: ${message}\n`);
			ctx.onStderr?.(bytes);
			proc.onStderr?.(bytes);
			resolveExit(1);
			proc.onExit?.(1);
		}
	}
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
		];

		return candidates.find((candidate) => existsSync(candidate));
	}
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

	const filesystem = createHybridVfs([
		workDir,
		paths.workspaceRoot,
		paths.hostProjectRoot,
		"/tmp",
	]);

	const kernel = createKernel({
		filesystem,
		hostNetworkAdapter: createNodeHostNetworkAdapter(),
		permissions: allowAll,
		env,
		cwd: workDir,
		logger,
	});

	const loadedCommands: string[] = [];

	// Mount shell/runtime drivers in the same order as the integration tests.
	if (mountWasm) {
		const wasmRuntime = createWasmVmRuntime({
			commandDirs: [paths.wasmCommandsDir],
		});
		await kernel.mount(wasmRuntime);
		loadedCommands.push(...wasmRuntime.commands);
		logger.info({ commands: wasmRuntime.commands }, "mounted wasmvm runtime");
	}

	const nodeRuntime = createNodeRuntime({ permissions: allowAll });
	await kernel.mount(nodeRuntime);
	loadedCommands.push(...nodeRuntime.commands);
	logger.info({ commands: nodeRuntime.commands }, "mounted node runtime");

	const piCliPath = resolvePiCliPath(paths);
	if (piCliPath) {
		await kernel.mount(
			new SandboxNodeScriptDriver(
				"pi",
				piCliPath,
				allowAll,
				paths.hostProjectRoot,
				"import",
			),
		);
		loadedCommands.push("pi");
		logger.info({ piCliPath }, "mounted pi driver");
	}

	const filteredCommands = Array.from(new Set(loadedCommands))
		.filter((command) => command.trim().length > 0 && !command.startsWith("_"))
		.sort();
	logger.info({ loadedCommands: filteredCommands }, "dev-shell ready");

	return {
		kernel,
		workDir,
		env,
		loadedCommands: filteredCommands,
		paths,
		logger,
		dispose: async () => {
			logger.info("dev-shell disposing");
			await kernel.dispose();
			await logger.close();
		},
	};
}
