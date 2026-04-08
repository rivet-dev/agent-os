/**
 * Kernel implementation.
 *
 * The kernel is the OS. It owns VFS, FD table, process table, device layer,
 * pipe manager, command registry, and permissions. Runtimes are execution
 * engines that make "syscalls" to the kernel.
 */

import type {
	Kernel,
	KernelInterface,
	KernelOptions,
	KernelLogger,
	ExecOptions,
	ExecResult,
	SpawnOptions,
	ManagedProcess,
	RuntimeDriver,
	ProcessContext,
	ProcessInfo,
	FDStat,
	FDEntry,
	OpenShellOptions,
	ShellHandle,
	ConnectTerminalOptions,
} from "./types.js";
import type { VirtualFileSystem, VirtualStat } from "./vfs.js";
import { createDeviceBackend } from "./device-backend.js";
import { createProcBackend } from "./proc-backend.js";
import { MountTable } from "./mount-table.js";
import { FDTableManager, ProcessFDTable } from "./fd-table.js";
import { ProcessTable } from "./process-table.js";
import { PipeManager } from "./pipe-manager.js";
import { PtyManager } from "./pty.js";
import { FileLockManager } from "./file-lock.js";
import { CommandRegistry } from "./command-registry.js";
import { wrapFileSystem, checkChildProcess } from "./permissions.js";
import { UserManager } from "./user.js";
import { SocketTable } from "./socket-table.js";
import { TimerTable } from "./timer-table.js";
import {
	FILETYPE_REGULAR_FILE,
	FILETYPE_DIRECTORY,
	FILETYPE_PIPE,
	FILETYPE_CHARACTER_DEVICE,
	SEEK_SET,
	SEEK_CUR,
	SEEK_END,
	O_APPEND,
	O_CREAT,
	O_EXCL,
	O_TRUNC,
	SIGTERM,
	SIGPIPE,
	SIGWINCH,
	F_DUPFD,
	F_GETFD,
	F_SETFD,
	F_GETFL,
	F_DUPFD_CLOEXEC,
	FD_CLOEXEC,
	KernelError,
	noopKernelLogger,
} from "./types.js";

export function createKernel(options: KernelOptions): Kernel {
	return new KernelImpl(options);
}

class KernelImpl implements Kernel {
	private vfs: VirtualFileSystem;
	private mountTable: MountTable;
	private fdTableManager = new FDTableManager();
	private processTable!: ProcessTable;
	private pipeManager = new PipeManager();
	private ptyManager!: PtyManager;
	private fileLockManager = new FileLockManager();
	private commandRegistry = new CommandRegistry();
	readonly socketTable: SocketTable;
	readonly timerTable: TimerTable;
	private userManager: UserManager;
	private drivers: RuntimeDriver[] = [];
	private driverPids = new Map<string, Set<number>>();
	private permissions?: import("./types.js").Permissions;
	private maxProcesses?: number;
	private env: Record<string, string>;
	private cwd: string;
	private disposed = false;
	private pendingBinEntries: Promise<void>[] = [];
	private posixDirsReady: Promise<void>;
	private log: KernelLogger;

	constructor(options: KernelOptions) {
		this.log = options.logger ?? noopKernelLogger;
		this.processTable = new ProcessTable(this.log.child({ component: "process" }));
		this.ptyManager = new PtyManager(
			(pgid, signal, excludeLeaders) => {
				try {
					if (excludeLeaders) {
						return this.processTable.killGroupExcludeLeaders(pgid, signal);
					}
					this.processTable.kill(-pgid, signal);
				} catch { /* no-op if pgid gone */ }
				return 0;
			},
			this.log.child({ component: "pty" }),
		);
		// Build mount table: root FS → /dev → /proc → user mounts.
		const mt = new MountTable(options.filesystem);
		mt.mount("/dev", createDeviceBackend());
		mt.mount("/proc", createProcBackend({
			processTable: this.processTable,
			fdTableManager: this.fdTableManager,
			hostname: options.env?.HOSTNAME,
			mountTable: mt,
		}));

		// Mount user-supplied filesystems
		if (options.mounts) {
			for (const m of options.mounts) {
				mt.mount(m.path, m.fs, { readOnly: m.readOnly });
			}
		}

		this.mountTable = mt;

		// Apply permission wrapping on top of the mount table
		let fs: VirtualFileSystem = mt;
		if (options.permissions) {
			fs = wrapFileSystem(fs, options.permissions);
		}

		this.vfs = fs;
		this.permissions = options.permissions;
		this.maxProcesses = options.maxProcesses;
		this.env = { ...options.env };
		this.cwd = options.cwd ?? "/home/user";
		this.userManager = new UserManager();
		this.socketTable = new SocketTable({
			vfs: this.vfs,
			networkCheck: options.permissions?.network,
			hostAdapter: options.hostNetworkAdapter,
			getSignalState: (pid) => this.processTable.getSignalState(pid),
			processExists: (pid) => this.processTable.get(pid) !== undefined,
		});
		this.timerTable = new TimerTable();

		// Clean up FD table and sockets when a process exits
		this.processTable.onProcessExit = (pid) => {
			this.log.debug({ pid }, "process exit cleanup");
			this.cleanupProcessFDs(pid);
			this.socketTable.closeAllForProcess(pid);
			this.timerTable.clearAllForProcess(pid);
		};
		// Clean up driver PID ownership when zombie is reaped
		this.processTable.onProcessReap = (pid) => {
			const entry = this.processTable.get(pid);
			if (entry) this.driverPids.get(entry.driver)?.delete(pid);
		};

		// Deliver SIGPIPE default action: terminate writer with 128+SIGPIPE
		this.pipeManager.onBrokenPipe = (pid) => {
			try {
				this.processTable.kill(pid, SIGPIPE);
			} catch {
				// Process may already be exited
			}
		};

		// Create standard POSIX directory hierarchy so all programs see /tmp,
		// /usr, /etc, etc. — matching a real Linux root filesystem layout.
		this.posixDirsReady = this.initPosixDirs();
	}

	private async initPosixDirs(): Promise<void> {
		// /dev and /proc are auto-created by MountTable mounts — don't create them here.
		const dirs = [
			"/tmp",
			"/bin",
			"/lib",
			"/sbin",
			"/boot",
			"/etc",
			"/root",
			"/run",
			"/srv",
			"/sys",
			"/opt",
			"/mnt",
			"/media",
			"/home",
			"/usr",
			"/usr/bin",
			"/usr/games",
			"/usr/include",
			"/usr/lib",
			"/usr/libexec",
			"/usr/man",
			"/usr/sbin",
			"/usr/share",
			"/usr/share/man",
			"/var",
			"/var/cache",
			"/var/empty",
			"/var/lib",
			"/var/lock",
			"/var/log",
			"/var/run",
			"/var/spool",
			"/var/tmp",
		];
		for (const dir of dirs) {
			try {
				await this.vfs.mkdir(dir, { recursive: true });
			} catch {
				// Directory may already exist
			}
		}
		// Standard utility that many scripts expect
		try {
			await this.vfs.writeFile("/usr/bin/env", new Uint8Array(1));
		} catch {
			// File may already exist
		}
	}

	// -----------------------------------------------------------------------
	// Kernel public API
	// -----------------------------------------------------------------------

	async mount(driver: RuntimeDriver): Promise<void> {
		this.assertNotDisposed();
		await this.posixDirsReady;
		this.log.debug({ driver: driver.name, commands: driver.commands }, "mounting runtime driver");

		// Track PIDs owned by this driver
		if (!this.driverPids.has(driver.name)) {
			this.driverPids.set(driver.name, new Set());
		}

		// Initialize the driver with a scoped kernel interface
		await driver.init(this.createKernelInterface(driver.name));

		// Register commands
		this.commandRegistry.register(driver);
		this.drivers.push(driver);

		// Populate /bin stubs for shell PATH lookup
		await this.commandRegistry.populateBin(this.vfs);
		this.log.info({ driver: driver.name, commands: driver.commands }, "runtime driver mounted");
	}

	mountFs(path: string, fs: VirtualFileSystem, options?: { readOnly?: boolean }): void {
		this.assertNotDisposed();
		this.mountTable.mount(path, fs, options);
	}

	unmountFs(path: string): void {
		this.assertNotDisposed();
		this.mountTable.unmount(path);
	}

	async dispose(): Promise<void> {
		if (this.disposed) return;
		this.disposed = true;
		this.log.info({}, "kernel disposing");

		// Terminate all running processes
		await this.processTable.terminateAll();

		// Clean up all sockets
		this.socketTable.disposeAll();
		this.timerTable.disposeAll();

		// Dispose all drivers (reverse mount order)
		for (let i = this.drivers.length - 1; i >= 0; i--) {
			try {
				await this.drivers[i].dispose();
			} catch {
				// Best effort cleanup
			}
		}
		this.drivers.length = 0;
	}

	/**
	 * Flush pending /bin stub entries created by on-demand command discovery.
	 * Ensures VFS is consistent before shell PATH lookups.
	 */
	async flushPendingBinEntries(): Promise<void> {
		if (this.pendingBinEntries.length > 0) {
			await Promise.all(this.pendingBinEntries);
			this.pendingBinEntries.length = 0;
		}
	}

	async exec(command: string, options?: ExecOptions): Promise<ExecResult> {
		this.assertNotDisposed();
		this.log.debug({ command, timeout: options?.timeout, cwd: options?.cwd }, "exec start");

		// Flush pending /bin stubs before shell PATH lookup
		await this.flushPendingBinEntries();

		// Route through shell
		const shell = this.commandRegistry.resolve("sh");
		if (!shell) {
			throw new Error(
				"No shell available. Mount a WasmVM runtime to enable exec().",
			);
		}

		const proc = this.spawnInternal("sh", ["-c", command], options);

		// Write stdin if provided
		if (options?.stdin) {
			const data =
				typeof options.stdin === "string"
					? new TextEncoder().encode(options.stdin)
					: options.stdin;
			proc.writeStdin(data);
			proc.closeStdin();
		}

		// Collect output
		const stdoutChunks: Uint8Array[] = [];
		const stderrChunks: Uint8Array[] = [];

		proc.onStdout = (data) => {
			stdoutChunks.push(data);
			options?.onStdout?.(data);
		};
		proc.onStderr = (data) => {
			stderrChunks.push(data);
			options?.onStderr?.(data);
		};

		// Wait with optional timeout
		let exitCode: number;
		if (options?.timeout) {
			let timer: ReturnType<typeof setTimeout> | undefined;
			try {
				exitCode = await Promise.race([
					proc.wait().then((code) => {
						clearTimeout(timer);
						return code;
					}),
					new Promise<number>((_, reject) => {
						timer = setTimeout(() => {
							// Kill process and detach output callbacks
							this.log.warn({ command, timeout: options.timeout }, "exec timeout, sending SIGTERM");
							proc.onStdout = null;
							proc.onStderr = null;
							proc.kill(SIGTERM);
							reject(new KernelError("ETIMEDOUT", "exec timeout"));
						}, options.timeout);
					}),
				]);
			} catch (err) {
				clearTimeout(timer);
				throw err;
			}
		} else {
			exitCode = await proc.wait();
		}

		return {
			exitCode,
			stdout: concatUint8(stdoutChunks),
			stderr: concatUint8(stderrChunks),
		};
	}

	spawn(
		command: string,
		args: string[],
		options?: SpawnOptions,
	): ManagedProcess {
		this.assertNotDisposed();
		return this.spawnManaged(command, args, options);
	}

	openShell(options?: OpenShellOptions): ShellHandle {
		this.assertNotDisposed();

		const command = options?.command ?? "sh";
		const args = options?.args ?? [];
		this.log.debug({ command, args, cols: options?.cols, rows: options?.rows, cwd: options?.cwd }, "openShell start");

		// Allocate a controller PID with an FD table to hold the PTY master
		const controllerPid = this.processTable.allocatePid();
		const controllerTable = this.fdTableManager.create(controllerPid);

		// Create PTY pair in the controller's FD table
		const { masterFd, slaveFd } = this.ptyManager.createPtyFDs(controllerTable);
		const masterDescId = controllerTable.get(masterFd)!.description.id;

		// Spawn shell with PTY slave as stdin/stdout/stderr
		// Propagate terminal dimensions as POSIX COLUMNS/LINES env vars
		const cols = options?.cols;
		const rows = options?.rows;
		const dimEnv: Record<string, string> = {};
		if (cols !== undefined) dimEnv.COLUMNS = String(cols);
		if (rows !== undefined) dimEnv.LINES = String(rows);

		const proc = this.spawnInternal(command, args, {
			env: { ...options?.env, ...dimEnv },
			cwd: options?.cwd,
			stdinFd: slaveFd,
			stdoutFd: slaveFd,
			stderrFd: slaveFd,
		}, controllerPid);

		// Shell becomes its own process group leader, set as PTY foreground
		this.processTable.setpgid(proc.pid, proc.pid);
		this.ptyManager.setForegroundPgid(masterDescId, proc.pid);
		this.ptyManager.setSessionLeader(masterDescId, proc.pid);
		this.log.debug({ shellPid: proc.pid, controllerPid, masterFd, masterDescId }, "openShell PTY attached");

		// Close controller's copy of slave FD (child inherited its own copy via fork).
		// Without this, slave refCount stays >0 after shell exits, preventing EOF on master.
		const slaveEntry = controllerTable.get(slaveFd);
		const slaveDescId = slaveEntry!.description.id;
		controllerTable.close(slaveFd);
		if (slaveEntry!.description.refCount <= 0) {
			this.ptyManager.close(slaveDescId);
		}

		// Start read pump: master reads → onData callback
		// Use object wrapper so TypeScript doesn't narrow to null in the async closure
		const pump = { onData: null as ((data: Uint8Array) => void) | null, exited: false };

		const pumpPromise = (async () => {
			try {
				while (!pump.exited) {
					const data = await this.ptyManager.read(masterDescId, 4096);
					if (!data || data.length === 0) break;
					try {
						pump.onData?.(data);
					} catch (cbErr) {
						// Propagate callback errors — don't silently swallow
						console.error("openShell readPump: onData callback error:", cbErr);
					}
				}
			} catch (err) {
				// Master closed or PTY gone — expected when shell exits
				if (pump.exited) return;
				console.error("openShell readPump: PTY read error:", err);
			}
		})();

		// wait() resolves after both shell exit AND pump drain
		const waitPromise = proc.wait().then(async (exitCode) => {
			pump.exited = true;
			// Wait for pump to finish delivering remaining data
			await pumpPromise;
			// Clean up controller PID's FD table (incl. PTY master)
			this.cleanupProcessFDs(controllerPid);
			return exitCode;
		});

		return {
			pid: proc.pid,
			write: (data) => {
				const bytes = typeof data === "string"
					? new TextEncoder().encode(data)
					: data;
				this.ptyManager.write(masterDescId, bytes);
			},
			get onData() { return pump.onData; },
			set onData(fn) { pump.onData = fn; },
			resize: (_cols, _rows) => {
				const fgPgid = this.ptyManager.getForegroundPgid(masterDescId);
				this.log.trace({ shellPid: proc.pid, cols: _cols, rows: _rows, fgPgid }, "PTY resize");
				if (fgPgid > 0) {
					try { this.processTable.kill(-fgPgid, SIGWINCH); } catch { /* pgid may be gone */ }
				}
			},
			kill: (signal) => {
				proc.kill(signal ?? SIGTERM);
			},
			wait: () => waitPromise,
		};
	}

	async connectTerminal(options?: ConnectTerminalOptions): Promise<number> {
		this.assertNotDisposed();
		this.log.debug({ command: options?.command, cols: options?.cols, rows: options?.rows }, "connectTerminal start");

		const stdin = process.stdin;
		const stdout = process.stdout;
		const isTTY = stdin.isTTY;

		let onStdinData: ((data: Buffer) => void) | undefined;
		let onResize: (() => void) | undefined;

		try {
			const shell = this.openShell(options);

			// Set raw mode so keypresses pass through directly
			if (isTTY) stdin.setRawMode(true);

			// Forward stdin to shell
			onStdinData = (data: Buffer) => shell.write(data);
			stdin.on("data", onStdinData);
			stdin.resume();

			// Forward shell output to stdout or custom handler
			const outputHandler = options?.onData
				?? ((data: Uint8Array) => { stdout.write(data); });
			shell.onData = outputHandler;

			// Forward terminal resize → PTY SIGWINCH
			if (stdout.isTTY) {
				onResize = () => {
					shell.resize(stdout.columns, stdout.rows);
				};
				stdout.on("resize", onResize);
			}

			return await shell.wait();
		} finally {
			// Restore terminal — guard each cleanup since setup may have partially completed
			if (onStdinData) stdin.removeListener("data", onStdinData);
			stdin.pause();
			if (isTTY) stdin.setRawMode(false);
			if (onResize && stdout.isTTY) stdout.removeListener("resize", onResize);
		}
	}

	// Filesystem convenience wrappers
	readFile(path: string): Promise<Uint8Array> { return this.vfs.readFile(path); }
	writeFile(path: string, content: string | Uint8Array): Promise<void> { return this.vfs.writeFile(path, content); }
	mkdir(path: string): Promise<void> { return this.vfs.mkdir(path); }
	readdir(path: string): Promise<string[]> { return this.vfs.readDir(path); }
	stat(path: string): Promise<VirtualStat> { return this.vfs.stat(path); }
	exists(path: string): Promise<boolean> { return this.vfs.exists(path); }
	removeFile(path: string): Promise<void> { return this.vfs.removeFile(path); }
	removeDir(path: string): Promise<void> { return this.vfs.removeDir(path); }
	rename(oldPath: string, newPath: string): Promise<void> { return this.vfs.rename(oldPath, newPath); }

	// Introspection
	get commands(): ReadonlyMap<string, string> {
		return this.commandRegistry.list();
	}

	get processes(): ReadonlyMap<number, ProcessInfo> {
		return this.processTable.listProcesses();
	}

	get zombieTimerCount(): number {
		return this.processTable.zombieTimerCount;
	}

	// -----------------------------------------------------------------------
	// Internal spawn
	// -----------------------------------------------------------------------

	private spawnInternal(
		command: string,
		args: string[],
		options?: SpawnOptions,
		callerPid?: number,
	): InternalProcess {
		this.log.debug({ command, args, callerPid, cwd: options?.cwd }, "spawn start");
		let driver = this.commandRegistry.resolve(command);

		// On-demand discovery: ask mounted drivers to resolve unknown commands
		if (!driver) {
			const basename = command.includes("/")
				? command.split("/").pop()!
				: command;
			if (basename) {
				for (const d of this.drivers) {
					if (d.tryResolve?.(basename)) {
						this.commandRegistry.registerCommand(basename, d);
						// Store pending promise so exec() can flush before shell PATH lookup
						const p = this.commandRegistry.populateBinEntry(this.vfs, basename);
						this.pendingBinEntries.push(p);
						p.then(() => {
							const idx = this.pendingBinEntries.indexOf(p);
							if (idx >= 0) this.pendingBinEntries.splice(idx, 1);
						});
						driver = d;
						break;
					}
				}
			}
		}

		if (!driver) {
			this.log.warn({ command }, "command not found");
			throw new KernelError("ENOENT", `command not found: ${command}`);
		}

		// Check childProcess permission
		try {
			checkChildProcess(this.permissions, command, args, options?.cwd);
		} catch (err) {
			this.log.warn({ command, args }, "spawn permission denied");
			throw err;
		}

		// Enforce maxProcesses budget
		if (this.maxProcesses !== undefined && this.processTable.runningCount() >= this.maxProcesses) {
			this.log.warn({ command, running: this.processTable.runningCount(), max: this.maxProcesses }, "process limit reached");
			throw new KernelError("EAGAIN", "maximum process limit reached");
		}

		// Allocate PID atomically
		const pid = this.processTable.allocatePid();

		// Register PID ownership before driver.spawn() so the driver can use it
		this.driverPids.get(driver.name)?.add(pid);

		// Cross-runtime spawn: parent driver must also track child PID so
		// it can waitpid/kill/interact with the child process
		if (callerPid !== undefined) {
			for (const [name, pids] of this.driverPids) {
				if (name !== driver.name && pids.has(callerPid)) {
					pids.add(pid);
					break;
				}
			}
		}

		// Create FD table — wire pipe FDs when overrides are provided
		const table = this.createChildFDTable(pid, options, callerPid);

		// Check which stdio channels are piped (data flows through kernel, not callbacks)
		const stdoutPiped = this.isStdioPiped(table, 1);
		const stderrPiped = this.isStdioPiped(table, 2);

		// Buffer stdout/stderr — wired before spawn so nothing is lost
		const stdoutBuf: Uint8Array[] = [];
		const stderrBuf: Uint8Array[] = [];

		// Resolve output callbacks.  Drivers invoke BOTH ctx.onStdout and
		// proc.onStdout per message, so the two must never point at the same
		// callback — otherwise the host sees every chunk twice.
		//
		//   ctx callbacks  — kernel-internal routing (pipes, parent forwarding)
		//                    + temporary buffer during spawn() to catch any
		//                    synchronous output (disabled right after spawn).
		//   proc callbacks — user / host callback (options.onStdout) or buffer
		//                    for later replay.  Set AFTER spawn returns.
		let ctxStdoutCb: ((data: Uint8Array) => void) | undefined;
		let ctxStderrCb: ((data: Uint8Array) => void) | undefined;
		if (stdoutPiped) {
			ctxStdoutCb = this.createPipedOutputCallback(table, 1, pid);
		} else if (!options?.onStdout && callerPid !== undefined) {
			const parent = this.processTable.get(callerPid);
			if (parent?.driverProcess.onStdout) {
				ctxStdoutCb = parent.driverProcess.onStdout;
			}
		}
		if (stderrPiped) {
			ctxStderrCb = this.createPipedOutputCallback(table, 2, pid);
		} else if (!options?.onStderr && callerPid !== undefined) {
			const parent = this.processTable.get(callerPid);
			if (parent?.driverProcess.onStderr) {
				ctxStderrCb = parent.driverProcess.onStderr;
			}
		}

		// Inherit env from parent process if spawned by another process, else use kernel defaults
		const parentEntry = callerPid ? this.processTable.get(callerPid) : undefined;
		const baseEnv = parentEntry?.env ?? this.env;

		// Detect PTY slave on stdio FDs
		const stdinIsTTY = this.isFdPtySlave(table, 0);
		const stdoutIsTTY = this.isFdPtySlave(table, 1);
		const stderrIsTTY = this.isFdPtySlave(table, 2);

		// Build process context with pre-wired callbacks.
		// When not piped/forwarded, ctx gets a temporary buffer so that any
		// data emitted synchronously during driver.spawn() is captured.
		const resolvedCwd = options?.cwd ?? this.cwd;
		const ctx: ProcessContext = {
			pid,
			ppid: callerPid ?? 0,
			env: { ...baseEnv, ...options?.env, PWD: resolvedCwd },
			cwd: resolvedCwd,
			fds: { stdin: 0, stdout: 1, stderr: 2 },
			stdinIsTTY,
			stdoutIsTTY,
			stderrIsTTY,
			streamStdin: options?.streamStdin,
			onStdout: ctxStdoutCb ?? (stdoutPiped ? undefined : (data) => stdoutBuf.push(data)),
			onStderr: ctxStderrCb ?? (stderrPiped ? undefined : (data) => stderrBuf.push(data)),
		};

		// Spawn via driver
		const driverProcess = driver.spawn(command, args, ctx);
		this.log.debug({
			pid, command, driver: driver.name, callerPid,
			stdinIsTTY, stdoutIsTTY, stderrIsTTY,
		}, "process spawned");

		// After spawn, disable the temporary ctx buffer so that async output
		// flows only through proc.onStdout — prevents double-delivery.
		// Pipe/parent-forwarding callbacks stay active (they live in ctxStdoutCb).
		if (!stdoutPiped) {
			ctx.onStdout = ctxStdoutCb;
		}
		if (!stderrPiped) {
			ctx.onStderr = ctxStderrCb;
		}

		// User/host callback goes ONLY on driverProcess (never on ctx) to
		// avoid double-delivery — drivers invoke both ctx and proc callbacks.
		if (!stdoutPiped) {
			driverProcess.onStdout = options?.onStdout ?? ((data) => stdoutBuf.push(data));
		}
		if (!stderrPiped) {
			driverProcess.onStderr = options?.onStderr ?? ((data) => stderrBuf.push(data));
		}

		// Register in process table
		const entry = this.processTable.register(
			pid,
			driver.name,
			command,
			args,
			ctx,
			driverProcess,
		);

		return {
			pid: entry.pid,
			driverProcess,
			wait: () => driverProcess.wait(),
			writeStdin: (data) => driverProcess.writeStdin(data),
			closeStdin: () => driverProcess.closeStdin(),
			kill: (signal) => driverProcess.kill(signal ?? 15),
			get onStdout() { return driverProcess.onStdout; },
			set onStdout(fn) {
				driverProcess.onStdout = fn;
				// Replay buffered data
				if (fn) for (const chunk of stdoutBuf) fn(chunk);
				stdoutBuf.length = 0;
			},
			get onStderr() { return driverProcess.onStderr; },
			set onStderr(fn) {
				driverProcess.onStderr = fn;
				if (fn) for (const chunk of stderrBuf) fn(chunk);
				stderrBuf.length = 0;
			},
		};
	}

	private spawnManaged(
		command: string,
		args: string[],
		options?: SpawnOptions,
		callerPid?: number,
	): ManagedProcess {
		const internal = this.spawnInternal(command, args, options, callerPid);
		let exitCode: number | null = null;

		// Note: options.onStdout/onStderr are already wired through ctx.onStdout
		// by spawnInternal. Do NOT also set them on driverProcess.onStdout here —
		// the driver calls both ctx.onStdout and proc.onStdout per message, so
		// setting both to the same callback would double-deliver output.

		internal.driverProcess.wait().then((code) => {
			exitCode = code;
		});

		return {
			pid: internal.pid,
			writeStdin: (data) => {
				const bytes = typeof data === "string"
					? new TextEncoder().encode(data)
					: data;
				internal.writeStdin(bytes);
			},
			closeStdin: () => internal.closeStdin(),
			kill: (signal) => this.processTable.kill(internal.pid, signal ?? 15),
			wait: () => internal.driverProcess.wait(),
			get exitCode() { return exitCode; },
		};
	}

	// -----------------------------------------------------------------------
	// Kernel interface (exposed to drivers)
	// -----------------------------------------------------------------------

	private createKernelInterface(driverName: string): KernelInterface {
		// Validate that the calling driver owns the target PID
		const assertOwns = (pid: number) => {
			if (this.driverPids.get(driverName)?.has(pid)) return;

			// Check if any driver owns this PID — if not, the PID doesn't exist
			for (const pids of this.driverPids.values()) {
				if (pids.has(pid)) {
					throw new KernelError("EPERM", `driver "${driverName}" does not own PID ${pid}`);
				}
			}
			throw new KernelError("ESRCH", `no such process ${pid}`);
		};

		const kernelInterface: KernelInterface & {
			fdPollWait: (pid: number, fd: number, timeoutMs?: number) => Promise<void>;
		} = {
			vfs: this.vfs,

			// FD operations
			fdOpen: (pid, path, flags, mode) => {
				assertOwns(pid);
				// /dev/fd/N → dup(N): equivalent to open() on the underlying FD
				if (path.startsWith("/dev/fd/")) {
					const raw = path.slice(8);
					const n = parseInt(raw, 10);
					if (isNaN(n) || n < 0 || String(n) !== raw) throw new KernelError("EBADF", `bad file descriptor: ${path}`);
					const table = this.getTable(pid);
					const entry = table.get(n);
					if (!entry) throw new KernelError("EBADF", `bad file descriptor ${n}`);
					return table.dup(n);
				}
				const created = (flags & (O_CREAT | O_EXCL | O_TRUNC)) !== 0
					? this.prepareOpenSync(path, flags)
					: false;
				const table = this.getTable(pid);
				const filetype = FILETYPE_REGULAR_FILE;
				const fd = table.open(path, flags, filetype);
				const fdEntry = table.get(fd);

				// Stash the effective mode for the first write that materializes a new file.
				if (created && (flags & O_CREAT)) {
					const entry = this.processTable.get(pid);
					const umask = entry?.umask ?? 0o022;
					const requestedMode = mode ?? 0o666;
					if (fdEntry) {
						fdEntry.description.creationMode = requestedMode & ~umask;
					}
				}

				return fd;
			},
			fdRead: async (pid, fd, length) => {
				assertOwns(pid);
				const table = this.getTable(pid);
				const entry = table.get(fd);
				if (!entry) throw new KernelError("EBADF", `bad file descriptor ${fd}`);

				// Pipe reads route through PipeManager
				if (this.pipeManager.isPipe(entry.description.id)) {
					const data = await this.pipeManager.read(entry.description.id, length);
					return data ?? new Uint8Array(0);
				}

				// PTY reads route through PtyManager
				if (this.ptyManager.isPty(entry.description.id)) {
					const data = await this.ptyManager.read(entry.description.id, length);
					return data ?? new Uint8Array(0);
				}

				// Positional read from VFS — avoids loading entire file
				const cursor = Number(entry.description.cursor);
				const slice = await this.preadDescription(entry.description, cursor, length);
				entry.description.cursor += BigInt(slice.length);
				return slice;
			},
			fdWrite: (pid, fd, data) => {
				assertOwns(pid);
				const table = this.getTable(pid);
				const entry = table.get(fd);
				if (!entry) throw new KernelError("EBADF", `bad file descriptor ${fd}`);

				if (this.pipeManager.isPipe(entry.description.id)) {
					return this.pipeManager.write(entry.description.id, data, pid);
				}

				if (this.ptyManager.isPty(entry.description.id)) {
					return this.ptyManager.write(entry.description.id, data);
				}

				// Write to VFS at cursor position (async — returns Promise)
				return this.vfsWrite(entry, data);
			},
			fdClose: (pid, fd) => {
				assertOwns(pid);
				const table = this.getTable(pid);
				const entry = table.get(fd);
				if (!entry) return;

				const descId = entry.description.id;
				const isPipe = this.pipeManager.isPipe(descId);
				const isPty = this.ptyManager.isPty(descId);

				// Close FD first (decrements refCount on shared FileDescription)
				table.close(fd);

				// Only signal pipe/pty/lock closure when last reference is dropped
				if (entry.description.refCount <= 0) {
					this.releaseDescriptionInode(entry.description);
					if (isPipe) this.pipeManager.close(descId);
					if (isPty) this.ptyManager.close(descId);
					this.fileLockManager.releaseByDescription(descId);
				}
			},
			fdSeek: async (pid, fd, offset, whence) => {
				assertOwns(pid);
				const table = this.getTable(pid);
				const entry = table.get(fd);
				if (!entry) throw new KernelError("EBADF", `bad file descriptor ${fd}`);

				// Pipes and PTYs are not seekable
				if (this.pipeManager.isPipe(entry.description.id) || this.ptyManager.isPty(entry.description.id)) {
					throw new KernelError("ESPIPE", "illegal seek");
				}

				let newCursor: bigint;
				switch (whence) {
					case SEEK_SET:
						newCursor = offset;
						break;
					case SEEK_CUR:
						newCursor = entry.description.cursor + offset;
						break;
					case SEEK_END: {
						newCursor = BigInt(await this.getDescriptionSize(entry.description)) + offset;
						break;
					}
					default:
						throw new KernelError("EINVAL", `invalid whence ${whence}`);
				}

				if (newCursor < 0n) throw new KernelError("EINVAL", "negative seek position");

				entry.description.cursor = newCursor;
				return newCursor;
			},
			fdPread: async (pid, fd, length, offset) => {
				assertOwns(pid);
				const table = this.getTable(pid);
				const entry = table.get(fd);
				if (!entry) throw new KernelError("EBADF", `bad file descriptor ${fd}`);

				// Pipes and PTYs are not seekable
				if (this.pipeManager.isPipe(entry.description.id) || this.ptyManager.isPty(entry.description.id)) {
					throw new KernelError("ESPIPE", "illegal seek");
				}

				// Read from VFS at given offset without moving cursor
				return this.preadDescription(entry.description, Number(offset), length);
			},
			fdPwrite: async (pid, fd, data, offset) => {
				assertOwns(pid);
				const table = this.getTable(pid);
				const entry = table.get(fd);
				if (!entry) throw new KernelError("EBADF", `bad file descriptor ${fd}`);

				// Pipes and PTYs are not seekable
				if (this.pipeManager.isPipe(entry.description.id) || this.ptyManager.isPty(entry.description.id)) {
					throw new KernelError("ESPIPE", "illegal seek");
				}

				// Delegate positional write to VFS.
				await this.pwriteDescription(entry.description, Number(offset), data);
				return data.length;
			},
			fdDup: (pid, fd) => {
				assertOwns(pid);
				return this.getTable(pid).dup(fd);
			},
			fdDup2: (pid, oldFd, newFd) => {
				assertOwns(pid);
				const table = this.getTable(pid);
				const targetEntry = table.get(newFd);
				const targetDesc = targetEntry?.description;
				const targetDescId = targetDesc?.id;
				table.dup2(oldFd, newFd);
				if (targetDesc && targetDesc.refCount <= 0) {
					this.releaseDescriptionInode(targetDesc);
					if (targetDescId !== undefined) {
						if (this.pipeManager.isPipe(targetDescId)) this.pipeManager.close(targetDescId);
						if (this.ptyManager.isPty(targetDescId)) this.ptyManager.close(targetDescId);
						this.fileLockManager.releaseByDescription(targetDescId);
					}
				}
			},
			fdDupMin: (pid, fd, minFd) => {
				assertOwns(pid);
				return this.getTable(pid).dupMinFd(fd, minFd);
			},
			fdStat: (pid, fd) => {
				assertOwns(pid);
				return this.getTable(pid).stat(fd);
			},
			fdPoll: (pid, fd) => {
				try {
					const table = this.getTable(pid);
					const entry = table.get(fd);
					if (!entry) return { readable: false, writable: false, hangup: false, invalid: true };
					const descId = entry.description.id;
					if (this.pipeManager.isPipe(descId)) {
						const ps = this.pipeManager.pollState(descId);
						return ps ? { ...ps, invalid: false } : { readable: false, writable: false, hangup: false, invalid: true };
					}
					// Regular files are always readable/writable
					return { readable: true, writable: true, hangup: false, invalid: false };
				} catch {
					return { readable: false, writable: false, hangup: false, invalid: true };
				}
			},
			fdPollWait: async (pid, fd, timeoutMs) => {
				assertOwns(pid);
				const table = this.getTable(pid);
				const entry = table.get(fd);
				if (!entry) throw new KernelError("EBADF", `bad file descriptor ${fd}`);
				const descId = entry.description.id;
				if (this.pipeManager.isPipe(descId)) {
					await this.pipeManager.waitForPoll(descId, timeoutMs);
				}
			},
			fdSetCloexec: (pid, fd, value) => {
				assertOwns(pid);
				const table = this.getTable(pid);
				const entry = table.get(fd);
				if (!entry) throw new KernelError("EBADF", `bad file descriptor ${fd}`);
				entry.cloexec = value;
			},
			fdGetCloexec: (pid, fd) => {
				assertOwns(pid);
				const table = this.getTable(pid);
				const entry = table.get(fd);
				if (!entry) throw new KernelError("EBADF", `bad file descriptor ${fd}`);
				return entry.cloexec;
			},
			fcntl: (pid, fd, cmd, arg) => {
				assertOwns(pid);
				const table = this.getTable(pid);
				const entry = table.get(fd);
				if (!entry) throw new KernelError("EBADF", `bad file descriptor ${fd}`);
				switch (cmd) {
					case F_DUPFD:
						return table.dupMinFd(fd, arg ?? 0);
					case F_DUPFD_CLOEXEC: {
						const newFd = table.dupMinFd(fd, arg ?? 0);
						table.get(newFd)!.cloexec = true;
						return newFd;
					}
					case F_GETFD:
						return entry.cloexec ? FD_CLOEXEC : 0;
					case F_SETFD:
						entry.cloexec = ((arg ?? 0) & FD_CLOEXEC) !== 0;
						return 0;
					case F_GETFL:
						return entry.description.flags;
					default:
						throw new KernelError("EINVAL", `unsupported fcntl command ${cmd}`);
				}
			},

			// Advisory file locking
			flock: async (pid, fd, operation) => {
				assertOwns(pid);
				const table = this.getTable(pid);
				const entry = table.get(fd);
				if (!entry) throw new KernelError("EBADF", `bad file descriptor ${fd}`);
				await this.fileLockManager.flock(entry.description.path, entry.description.id, operation);
			},

			// Process operations
			spawn: (command, args, ctx) => {
				if (ctx.ppid) assertOwns(ctx.ppid);
				return this.spawnManaged(command, args, {
					env: ctx.env,
					cwd: ctx.cwd,
					streamStdin: ctx.streamStdin,
					onStdout: ctx.onStdout,
					onStderr: ctx.onStderr,
					stdinFd: ctx.stdinFd,
					stdoutFd: ctx.stdoutFd,
					stderrFd: ctx.stderrFd,
				}, ctx.ppid);
			},
			waitpid: (pid, options) => {
				try { assertOwns(pid); } catch (e) { return Promise.reject(e); }
				return this.processTable.waitpid(pid, options);
			},
			kill: (pid, signal) => {
				// Negative PID = process group kill, handled by kernel directly
				if (pid >= 0) assertOwns(pid);
				this.log.debug({ pid, signal }, "signal delivery");
				this.processTable.kill(pid, signal);
			},
			getpid: (pid) => {
				assertOwns(pid);
				return pid;
			},
			getppid: (pid) => {
				assertOwns(pid);
				return this.processTable.getppid(pid);
			},

			// Process group / session
			setpgid: (pid, pgid) => {
				assertOwns(pid);
				this.processTable.setpgid(pid, pgid);
			},
			getpgid: (pid) => {
				assertOwns(pid);
				return this.processTable.getpgid(pid);
			},
			setsid: (pid) => {
				assertOwns(pid);
				return this.processTable.setsid(pid);
			},
			getsid: (pid) => {
				assertOwns(pid);
				return this.processTable.getsid(pid);
			},

			// Pipe operations
			pipe: (pid) => {
				assertOwns(pid);
				const table = this.getTable(pid);
				return this.pipeManager.createPipeFDs(table);
			},

			// PTY operations
			openpty: (pid) => {
				assertOwns(pid);
				const table = this.getTable(pid);
				return this.ptyManager.createPtyFDs(table);
			},
			isatty: (pid, fd) => {
				assertOwns(pid);
				const table = this.getTable(pid);
				const entry = table.get(fd);
				if (!entry) return false;
				return this.ptyManager.isSlave(entry.description.id);
			},
			ptySetDiscipline: (pid, fd, config) => {
				assertOwns(pid);
				const table = this.getTable(pid);
				const entry = table.get(fd);
				if (!entry) throw new KernelError("EBADF", `bad file descriptor ${fd}`);
				this.ptyManager.setDiscipline(entry.description.id, config);
			},
			ptySetForegroundPgid: (pid, fd, pgid) => {
				assertOwns(pid);
				const table = this.getTable(pid);
				const entry = table.get(fd);
				if (!entry) throw new KernelError("EBADF", `bad file descriptor ${fd}`);
				this.ptyManager.setForegroundPgid(entry.description.id, pgid);
			},

			// Termios operations
			tcgetattr: (pid, fd) => {
				assertOwns(pid);
				const table = this.getTable(pid);
				const entry = table.get(fd);
				if (!entry) throw new KernelError("EBADF", `bad file descriptor ${fd}`);
				return this.ptyManager.getTermios(entry.description.id);
			},
			tcsetattr: (pid, fd, termios) => {
				assertOwns(pid);
				const table = this.getTable(pid);
				const entry = table.get(fd);
				if (!entry) throw new KernelError("EBADF", `bad file descriptor ${fd}`);
				this.ptyManager.setTermios(entry.description.id, termios);
			},
			tcsetpgrp: (pid, fd, pgid) => {
				assertOwns(pid);
				const table = this.getTable(pid);
				const entry = table.get(fd);
				if (!entry) throw new KernelError("EBADF", `bad file descriptor ${fd}`);
				// Validate target PGID refers to an existing process group
				if (!this.processTable.hasProcessGroup(pgid)) {
					throw new KernelError("ESRCH", `no such process group ${pgid}`);
				}
				this.ptyManager.setForegroundPgid(entry.description.id, pgid);
			},
			tcgetpgrp: (pid, fd) => {
				assertOwns(pid);
				const table = this.getTable(pid);
				const entry = table.get(fd);
				if (!entry) throw new KernelError("EBADF", `bad file descriptor ${fd}`);
				return this.ptyManager.getForegroundPgid(entry.description.id);
			},

			// /dev/fd operations
			devFdReadDir: (pid) => {
				assertOwns(pid);
				const table = this.fdTableManager.get(pid);
				if (!table) return [];
				const fds: number[] = [];
				for (const entry of table) fds.push(entry.fd);
				return fds.sort((a, b) => a - b).map(String);
			},
			devFdStat: async (pid, fd) => {
				assertOwns(pid);
				const table = this.getTable(pid);
				const entry = table.get(fd);
				if (!entry) throw new KernelError("EBADF", `bad file descriptor ${fd}`);

				// Pipe/PTY FDs return a synthetic character device stat
				if (this.pipeManager.isPipe(entry.description.id) || this.ptyManager.isPty(entry.description.id)) {
					const now = Date.now();
					return {
						mode: 0o666,
						size: 0,
						isDirectory: false,
						isSymbolicLink: false,
						atimeMs: now,
						mtimeMs: now,
						ctimeMs: now,
						birthtimeMs: now,
						ino: entry.description.id,
						nlink: 1,
						uid: 0,
						gid: 0,
					};
				}

				// Regular file — stat the underlying path
				return this.statDescription(entry.description);
			},

			// Environment
			getenv: (pid) => {
				assertOwns(pid);
				const entry = this.processTable.get(pid);
				return entry?.env ?? { ...this.env };
			},
			setenv: (pid, key, value) => {
				assertOwns(pid);
				const entry = this.processTable.get(pid);
				if (!entry) throw new KernelError("ESRCH", `no such process ${pid}`);
				entry.env[key] = value;
			},
			unsetenv: (pid, key) => {
				assertOwns(pid);
				const entry = this.processTable.get(pid);
				if (!entry) throw new KernelError("ESRCH", `no such process ${pid}`);
				delete entry.env[key];
			},
			getcwd: (pid) => {
				assertOwns(pid);
				const entry = this.processTable.get(pid);
				return entry?.cwd ?? this.cwd;
			},

			// Working directory
			chdir: async (pid, path) => {
				assertOwns(pid);
				const entry = this.processTable.get(pid);
				if (!entry) throw new KernelError("ESRCH", `no such process ${pid}`);

				// Validate path exists and is a directory
				let st: VirtualStat;
				try {
					st = await this.vfs.stat(path);
				} catch {
					throw new KernelError("ENOENT", `no such file or directory: ${path}`);
				}
				if (!st.isDirectory) {
					throw new KernelError("ENOTDIR", `not a directory: ${path}`);
				}

				entry.cwd = path;
				entry.env.PWD = path;
			},

			// Alarm (SIGALRM)
			alarm: (pid, seconds) => {
				assertOwns(pid);
				return this.processTable.alarm(pid, seconds);
			},

			// File mode creation mask
			umask: (pid, newMask?) => {
				assertOwns(pid);
				const entry = this.processTable.get(pid);
				if (!entry) throw new KernelError("ESRCH", `no such process ${pid}`);
				const old = entry.umask;
				if (newMask !== undefined) {
					entry.umask = newMask & 0o777;
				}
				return old;
			},

			// Directory creation with umask
			mkdir: async (pid, path, mode?) => {
				assertOwns(pid);
				const entry = this.processTable.get(pid);
				const umask = entry?.umask ?? 0o022;
				const requestedMode = mode ?? 0o777;
				const effectiveMode = requestedMode & ~umask;
				await this.vfs.mkdir(path);
				await this.vfs.chmod(path, effectiveMode);
			},

			// Socket table (shared across runtimes)
			socketTable: this.socketTable,
			timerTable: this.timerTable,

			// Process table (shared across runtimes)
			processTable: this.processTable,
		};

		return kernelInterface;
	}

	/**
	 * Create FD table for a child process via fork + optional FD overrides.
	 *
	 * When callerPid exists, forks the parent's FD table so the child inherits
	 * all open FDs (shared cursors via refcounted FileDescription). Then applies
	 * stdinFd/stdoutFd/stderrFd overrides on top of the forked table.
	 */
	private createChildFDTable(
		childPid: number,
		options?: SpawnOptions,
		callerPid?: number,
	): ProcessFDTable {
		// Fork parent's FD table if parent exists
		if (callerPid && this.fdTableManager.get(callerPid)) {
			const table = this.fdTableManager.fork(callerPid, childPid);

			// Apply FD overrides on top of the forked table
			const hasFdOverrides =
				options?.stdinFd !== undefined ||
				options?.stdoutFd !== undefined ||
				options?.stderrFd !== undefined;

			if (hasFdOverrides) {
				const callerTable = this.fdTableManager.get(callerPid)!;
				this.applyStdioOverride(table, callerTable, 0, options!.stdinFd);
				this.applyStdioOverride(table, callerTable, 1, options!.stdoutFd);
				this.applyStdioOverride(table, callerTable, 2, options!.stderrFd);
			}

			// Close inherited pipe FDs above stdio that share a pipe with an
			// overridden stdio FD — prevents pipe deadlocks (close-on-exec for
			// counterpart pipe ends only, so tests that intentionally inherit pipe
			// FDs without overrides are not affected).
			if (hasFdOverrides) {
				const overridePipeIds = new Set<number>();
				for (const fd of [0, 1, 2]) {
					const e = table.get(fd);
					if (e && this.pipeManager.isPipe(e.description.id)) {
						const pipeId = this.pipeManager.pipeIdFor(e.description.id);
						if (pipeId !== undefined) overridePipeIds.add(pipeId);
					}
				}
				if (overridePipeIds.size > 0) {
					const toClose: number[] = [];
					for (const entry of table) {
						if (entry.fd > 2 && this.pipeManager.isPipe(entry.description.id)) {
							const pid2 = this.pipeManager.pipeIdFor(entry.description.id);
							if (pid2 !== undefined && overridePipeIds.has(pid2)) {
								toClose.push(entry.fd);
							}
						}
					}
					for (const fd of toClose) {
						table.close(fd);
					}
				}
			}

			return table;
		}

		return this.fdTableManager.create(childPid);
	}

	/** Close inherited stdio FD and install an override from the caller's table. */
	private applyStdioOverride(
		childTable: ProcessFDTable,
		callerTable: ProcessFDTable,
		targetFd: number,
		overrideFd: number | undefined,
	): void {
		if (overrideFd === undefined) return;
		if (overrideFd === 0xFFFFFFFF) return; // /dev/null sentinel — keep inherited

		const entry = callerTable.get(overrideFd);
		if (!entry) return;

		// Close the inherited FD and install the override
		const existing = childTable.get(targetFd);
		childTable.close(targetFd);
		if (existing && existing.description.refCount <= 0) {
			this.releaseDescriptionInode(existing.description);
			const descId = existing.description.id;
			if (this.pipeManager.isPipe(descId)) this.pipeManager.close(descId);
			if (this.ptyManager.isPty(descId)) this.ptyManager.close(descId);
			this.fileLockManager.releaseByDescription(descId);
		}
		childTable.openWith(entry.description, entry.filetype, targetFd);
	}

	/** Check if a stdio FD (0/1/2) in a process's table is a pipe or PTY. */
	private isStdioPiped(table: ProcessFDTable, fd: number): boolean {
		const entry = table.get(fd);
		if (!entry) return false;
		return this.pipeManager.isPipe(entry.description.id) || this.ptyManager.isPty(entry.description.id);
	}

	/** Check if an FD in the given table refers to a PTY slave (terminal). */
	private isFdPtySlave(table: ProcessFDTable, fd: number): boolean {
		const entry = table.get(fd);
		if (!entry) return false;
		return this.ptyManager.isSlave(entry.description.id);
	}

	/**
	 * Create a callback that forwards data through a piped stdio FD.
	 * Needed for drivers (like Node) that emit output via callbacks rather
	 * than kernel FD writes (like WasmVM does via WASI fd_write).
	 */
	private createPipedOutputCallback(
		table: ProcessFDTable,
		fd: number,
		pid?: number,
	): ((data: Uint8Array) => void) | undefined {
		const entry = table.get(fd);
		if (!entry) return undefined;

		const descId = entry.description.id;
		if (this.pipeManager.isPipe(descId)) {
			return (data) => {
				try { this.pipeManager.write(descId, data, pid); } catch { /* pipe closed */ }
			};
		}
		if (this.ptyManager.isPty(descId)) {
			return (data) => {
				try { this.ptyManager.write(descId, data); } catch { /* pty closed */ }
			};
		}
		return undefined;
	}

	/** Clean up all FDs for a process, closing pipe/PTY ends when last reference drops. */
	private cleanupProcessFDs(pid: number): void {
		const table = this.fdTableManager.get(pid);
		if (!table) return;

		// Collect descriptions before closing so we can check refCounts after.
		const descriptions = new Map<number, import("./types.js").FileDescription>();
		const managedDescs: { id: number; description: import("./types.js").FileDescription; type: "pipe" | "pty" | "lock" }[] = [];
		for (const entry of table) {
			descriptions.set(entry.description.id, entry.description);
			const descId = entry.description.id;
			if (this.pipeManager.isPipe(descId)) {
				managedDescs.push({ id: descId, description: entry.description, type: "pipe" });
			} else if (this.ptyManager.isPty(descId)) {
				managedDescs.push({ id: descId, description: entry.description, type: "pty" });
			} else if (this.fileLockManager.hasLock(descId)) {
				managedDescs.push({ id: descId, description: entry.description, type: "lock" });
			}
		}

		// Close all FDs and remove the table
		this.fdTableManager.remove(pid);

		// Flush buffered writes when the last shared reference closes.
		for (const description of descriptions.values()) {
			if (description.refCount <= 0) {
				this.releaseDescriptionInode(description);
			}
		}

		// Signal closure for managed descriptions whose last reference was dropped.
		for (const { id, description, type } of managedDescs) {
			if (description.refCount <= 0) {
				if (type === "pipe") this.pipeManager.close(id);
				else if (type === "pty") this.ptyManager.close(id);
				else if (type === "lock") this.fileLockManager.releaseByDescription(id);
			}
		}
	}

	private async vfsWrite(entry: FDEntry, data: Uint8Array): Promise<number> {
		let content: Uint8Array;
		try {
			content = await this.readDescriptionFile(entry.description);
		} catch {
			content = new Uint8Array(0);
		}

		// O_APPEND: every write seeks to end of file first (POSIX)
		const cursor = (entry.description.flags & O_APPEND)
			? content.length
			: Number(entry.description.cursor);
		const endPos = cursor + data.length;
		const newContent = new Uint8Array(Math.max(content.length, endPos));
		newContent.set(content);
		newContent.set(data, cursor);
		await this.writeDescriptionFile(entry.description, newContent);

		// Apply creation mode once the descriptor's newly created file is materialized.
		if (entry.description.creationMode !== undefined) {
			await this.vfs.chmod(entry.description.path, entry.description.creationMode);
			entry.description.creationMode = undefined;
		}

		entry.description.cursor = BigInt(endPos);
		return data.length;
	}

	private releaseDescriptionInode(
		description: import("./types.js").FileDescription,
	): void {
		// Flush buffered writes to durable storage when the last FD is closed.
		void this.vfs.fsync?.(description.path);
	}

	private async readDescriptionFile(
		description: import("./types.js").FileDescription,
	): Promise<Uint8Array> {
		return this.vfs.readFile(description.path);
	}

	private async writeDescriptionFile(
		description: import("./types.js").FileDescription,
		content: Uint8Array,
	): Promise<void> {
		await this.vfs.writeFile(description.path, content);
	}

	private prepareOpenSync(path: string, flags: number): boolean {
		const syncVfs = this.vfs as VirtualFileSystem & {
			prepareOpenSync?: (targetPath: string, openFlags: number) => boolean;
		};
		return syncVfs.prepareOpenSync?.(path, flags) ?? false;
	}

	private async preadDescription(
		description: import("./types.js").FileDescription,
		offset: number,
		length: number,
	): Promise<Uint8Array> {
		return this.vfs.pread(description.path, offset, length);
	}

	private async pwriteDescription(
		description: import("./types.js").FileDescription,
		offset: number,
		data: Uint8Array,
	): Promise<void> {
		await this.vfs.pwrite(description.path, offset, data);
	}

	private async getDescriptionSize(
		description: import("./types.js").FileDescription,
	): Promise<number> {
		return (await this.statDescription(description)).size;
	}

	private async statDescription(
		description: import("./types.js").FileDescription,
	): Promise<VirtualStat> {
		return this.vfs.stat(description.path);
	}

	private getTable(pid: number): ProcessFDTable {
		const table = this.fdTableManager.get(pid);
		if (!table) throw new KernelError("ESRCH", `no FD table for PID ${pid}`);
		return table;
	}

	private assertNotDisposed(): void {
		if (this.disposed) throw new Error("Kernel is disposed");
	}
}

interface InternalProcess {
	pid: number;
	driverProcess: import("./types.js").DriverProcess;
	wait(): Promise<number>;
	writeStdin(data: Uint8Array): void;
	closeStdin(): void;
	kill(signal: number): void;
	onStdout: ((data: Uint8Array) => void) | null;
	onStderr: ((data: Uint8Array) => void) | null;
}

function concatUint8(chunks: Uint8Array[]): string {
	if (chunks.length === 0) return "";
	const total = chunks.reduce((sum, c) => sum + c.length, 0);
	const buf = new Uint8Array(total);
	let offset = 0;
	for (const chunk of chunks) {
		buf.set(chunk, offset);
		offset += chunk.length;
	}
	return new TextDecoder().decode(buf);
}
