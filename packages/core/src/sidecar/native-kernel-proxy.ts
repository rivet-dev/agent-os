import { execFileSync } from "node:child_process";
import {
	existsSync,
	mkdirSync,
	mkdtempSync,
	realpathSync,
	rmSync,
	symlinkSync,
	writeFileSync,
} from "node:fs";
import { constants as osConstants, tmpdir } from "node:os";
import {
	basename as basenameHostPath,
	dirname as dirnameHostPath,
	join as joinHostPath,
	posix as posixPath,
} from "node:path";
import type {
	ConnectTerminalOptions,
	Kernel,
	KernelExecOptions,
	KernelExecResult,
	KernelSpawnOptions,
	ManagedProcess,
	OpenShellOptions,
	ProcessInfo,
	ShellHandle,
	VirtualFileSystem,
	VirtualStat,
} from "../runtime-compat.js";
import type {
	AuthenticatedSession,
	CreatedVm,
	GuestFilesystemStat,
	NativeSidecarProcessClient,
	SidecarSignalHandlerRegistration,
	SidecarSocketStateEntry,
} from "./native-process-client.js";

const SYNTHETIC_PID_BASE = 1_000_000;
const EVENT_PUMP_TIMEOUT_MS = 86_400_000;
const GUEST_PATH_MAPPINGS_ENV = "AGENT_OS_GUEST_PATH_MAPPINGS";
const EXTRA_FS_READ_PATHS_ENV = "AGENT_OS_EXTRA_FS_READ_PATHS";
const EXTRA_FS_WRITE_PATHS_ENV = "AGENT_OS_EXTRA_FS_WRITE_PATHS";
const ALLOWED_NODE_BUILTINS_ENV = "AGENT_OS_ALLOWED_NODE_BUILTINS";
const LOOPBACK_EXEMPT_PORTS_ENV = "AGENT_OS_LOOPBACK_EXEMPT_PORTS";
const DEFAULT_ALLOWED_NODE_BUILTINS = [
	"assert",
	"buffer",
	"console",
	"child_process",
	"crypto",
	"dns",
	"events",
	"fs",
	"os",
	"path",
	"querystring",
	"stream",
	"string_decoder",
	"timers",
	"tls",
	"url",
	"util",
	"zlib",
] as const;

function normalizeAllowedNodeBuiltins(
	allowedNodeBuiltins?: readonly string[],
): string[] {
	if (allowedNodeBuiltins === undefined) {
		return [...DEFAULT_ALLOWED_NODE_BUILTINS];
	}

	return [
		...new Set(
			allowedNodeBuiltins.filter((value) => typeof value === "string"),
		),
	];
}

const PREFERRED_SIGNAL_NAMES = [
	"SIGHUP",
	"SIGINT",
	"SIGQUIT",
	"SIGILL",
	"SIGTRAP",
	"SIGABRT",
	"SIGBUS",
	"SIGFPE",
	"SIGKILL",
	"SIGUSR1",
	"SIGSEGV",
	"SIGUSR2",
	"SIGPIPE",
	"SIGALRM",
	"SIGTERM",
	"SIGSTKFLT",
	"SIGCHLD",
	"SIGCONT",
	"SIGSTOP",
	"SIGTSTP",
	"SIGTTIN",
	"SIGTTOU",
	"SIGURG",
	"SIGXCPU",
	"SIGXFSZ",
	"SIGVTALRM",
	"SIGPROF",
	"SIGWINCH",
	"SIGIO",
	"SIGPWR",
	"SIGSYS",
	"SIGEMT",
	"SIGINFO",
] as const;
const NON_CANONICAL_SIGNAL_NAMES = new Set([
	"SIGCLD",
	"SIGIOT",
	"SIGPOLL",
	"SIGUNUSED",
]);
const SIGNAL_NAME_BY_NUMBER = buildSignalNameByNumber();

function buildSignalNameByNumber(): Map<number, string> {
	const signals = osConstants.signals as Record<string, number | undefined>;
	const names = new Map<number, string>();
	for (const name of PREFERRED_SIGNAL_NAMES) {
		const value = signals[name];
		if (typeof value === "number") {
			names.set(value, name);
		}
	}
	for (const [name, value] of Object.entries(signals)) {
		if (
			typeof value === "number" &&
			!NON_CANONICAL_SIGNAL_NAMES.has(name) &&
			!names.has(value)
		) {
			names.set(value, name);
		}
	}
	return names;
}

export function toSidecarSignalName(signal: number): string {
	return SIGNAL_NAME_BY_NUMBER.get(signal) ?? String(signal);
}

export interface LocalCompatMount {
	path: string;
	fs: VirtualFileSystem;
	readOnly: boolean;
}

interface HostPathMapping {
	guestPath: string;
	hostPath: string;
}

interface KernelSocketSnapshot {
	processId: string;
	host?: string;
	port?: number;
	path?: string;
}

interface KernelSignalState {
	handlers: Map<
		number,
		{
			action: SidecarSignalHandlerRegistration["action"];
			mask: Set<number>;
			flags: number;
		}
	>;
}

interface SocketLookupCacheEntry {
	value: KernelSocketSnapshot | null;
	pending: Promise<void> | null;
}

export interface NativeKernelHostPathMapping {
	guestPath: string;
	hostPath: string;
}

interface TrackedProcessEntry {
	pid: number;
	processId: string;
	command: string;
	args: string[];
	driver: string;
	cwd: string;
	env: Record<string, string>;
	startTime: number;
	exitTime: number | null;
	hostPid: number | null;
	exitCode: number | null;
	started: boolean;
	startPromise: Promise<void>;
	waitPromise: Promise<number>;
	resolveWait: (exitCode: number) => void;
	rejectWait: (error: Error) => void;
	onStdout: Set<(data: Uint8Array) => void>;
	onStderr: Set<(data: Uint8Array) => void>;
	pendingStdin: Array<string | Uint8Array>;
	stdinFlushPromise: Promise<void> | null;
	pendingCloseStdin: boolean;
	pendingKillSignal: number | null;
}

interface HostProcessRow {
	pid: number;
	ppid: number;
	command: string;
}

interface NativeSidecarKernelProxyOptions {
	client: NativeSidecarProcessClient;
	session: AuthenticatedSession;
	vm: CreatedVm;
	env: Record<string, string>;
	cwd: string;
	localMounts: LocalCompatMount[];
	commandGuestPaths: ReadonlyMap<string, string>;
	hostPathMappings: HostPathMapping[];
	allowedNodeBuiltins?: readonly string[];
	loopbackExemptPorts?: number[];
	nodeExecutionCwd: string;
	onDispose?: () => Promise<void>;
}

export class NativeSidecarKernelProxy {
	readonly env: Record<string, string>;
	readonly cwd: string;
	readonly commands: ReadonlyMap<string, string>;
	readonly vfs: VirtualFileSystem;
	readonly processes = new Map<number, ProcessInfo>();

	private readonly client: NativeSidecarProcessClient;
	private readonly session: AuthenticatedSession;
	private readonly vm: CreatedVm;
	private readonly localMounts: LocalCompatMount[];
	private readonly commandGuestPaths: Map<string, string>;
	private readonly hostPathMappings: HostPathMapping[];
	private readonly allowedNodeBuiltins: readonly string[];
	private readonly loopbackExemptPorts: readonly number[];
	private readonly nodeExecutionCwd: string;
	private readonly onDispose: (() => Promise<void>) | undefined;
	private readonly trackedProcesses = new Map<number, TrackedProcessEntry>();
	private readonly trackedProcessesById = new Map<
		string,
		TrackedProcessEntry
	>();
	private readonly listenerLookups = new Map<string, SocketLookupCacheEntry>();
	private readonly boundUdpLookups = new Map<string, SocketLookupCacheEntry>();
	private readonly signalStates = new Map<number, KernelSignalState>();
	private readonly signalRefreshes = new Map<number, Promise<void>>();
	private readonly rootView: VirtualFileSystem;
	private zombieTimerCountValue = 0;
	private zombieTimerCountRefresh: Promise<void> | null = null;
	private disposed = false;
	private pumpError: Error | null = null;
	private nextSyntheticPid = SYNTHETIC_PID_BASE;
	private readonly eventPump: Promise<void>;
	private readonly shadowRoot: string;

	constructor(options: NativeSidecarKernelProxyOptions) {
		this.client = options.client;
		this.session = options.session;
		this.vm = options.vm;
		this.env = { ...options.env };
		this.cwd = options.cwd;
		this.localMounts = [...options.localMounts].sort(
			(left, right) => right.path.length - left.path.length,
		);
		this.commandGuestPaths = new Map(options.commandGuestPaths);
		this.hostPathMappings = [...options.hostPathMappings].sort(
			(left, right) => right.guestPath.length - left.guestPath.length,
		);
		this.allowedNodeBuiltins = normalizeAllowedNodeBuiltins(
			options.allowedNodeBuiltins,
		);
		this.loopbackExemptPorts = [...(options.loopbackExemptPorts ?? [])];
		this.nodeExecutionCwd = options.nodeExecutionCwd;
		this.onDispose = options.onDispose;
		this.shadowRoot = mkdtempSync(
			joinHostPath(tmpdir(), "agent-os-native-shadow-"),
		);
		this.materializeHostPathMappings();
		this.commands = buildCommandMap(this.commandGuestPaths);
		this.vfs = this.createFilesystemView(true);
		this.rootView = this.createFilesystemView(false);
		this.eventPump = this.runEventPump();
	}

	createRootView(): VirtualFileSystem {
		return this.rootView;
	}

	get zombieTimerCount(): number {
		if (!this.zombieTimerCountRefresh) {
			this.zombieTimerCountRefresh = this.refreshZombieTimerCount();
		}
		return this.zombieTimerCountValue;
	}

	registerCommandGuestPaths(
		commandGuestPaths: ReadonlyMap<string, string>,
	): void {
		for (const [name, guestPath] of commandGuestPaths) {
			this.commandGuestPaths.set(name, guestPath);
			(this.commands as Map<string, string>).set(name, "wasmvm");
		}
	}

	async dispose(): Promise<void> {
		if (this.disposed) {
			return;
		}
		this.disposed = true;

		const liveProcesses = [...this.trackedProcesses.values()].filter(
			(entry) => entry.exitCode === null,
		);
		await Promise.allSettled(
			liveProcesses.map((entry) => this.signalProcess(entry, 15)),
		);

		await this.client.disposeVm(this.session, this.vm).catch(() => {});
		for (const entry of liveProcesses) {
			if (entry.exitCode === null) {
				// The sidecar dispose path already performs TERM/KILL escalation for any
				// guest executions that are still live. Resolve local waiters eagerly so
				// VM teardown does not hang on killed ACP adapter processes that never
				// surface a terminal process_exited event back to the JS bridge.
				this.finishProcess(entry, 143);
			}
		}
		await this.client.dispose().catch(() => {});
		await this.eventPump.catch(() => {});
		rmSync(this.shadowRoot, { recursive: true, force: true });
		await this.onDispose?.().catch(() => {});
	}

	async exec(
		command: string,
		options?: KernelExecOptions,
	): Promise<KernelExecResult> {
		const stdoutChunks: Uint8Array[] = [];
		const stderrChunks: Uint8Array[] = [];

		const parsed = this.resolveExecCommand(command);
		const proc = this.spawn(parsed.command, parsed.args, {
			...options,
			onStdout: (chunk) => {
				stdoutChunks.push(chunk);
				options?.onStdout?.(chunk);
			},
			onStderr: (chunk) => {
				stderrChunks.push(chunk);
				options?.onStderr?.(chunk);
			},
		});

		if (options?.stdin !== undefined) {
			proc.writeStdin(options.stdin);
			proc.closeStdin();
		}

		const waitPromise = proc.wait();
		const exitCode =
			typeof options?.timeout === "number"
				? await new Promise<number>((resolve) => {
						const timer = setTimeout(() => {
							proc.kill(9);
							void proc.wait().then(resolve);
						}, options.timeout);
						void waitPromise.then((code) => {
							clearTimeout(timer);
							resolve(code);
						});
					})
				: await waitPromise;

		return {
			exitCode,
			stdout: Buffer.concat(
				stdoutChunks.map((chunk) => Buffer.from(chunk)),
			).toString("utf8"),
			stderr: Buffer.concat(
				stderrChunks.map((chunk) => Buffer.from(chunk)),
			).toString("utf8"),
		};
	}

	spawn(
		command: string,
		args: string[],
		options?: KernelSpawnOptions,
	): ManagedProcess {
		const pid = this.nextSyntheticPid++;
		const processId = `proc-${pid}`;
		let resolveWait!: (exitCode: number) => void;
		let rejectWait!: (error: Error) => void;
		const waitPromise = new Promise<number>((resolve, reject) => {
			resolveWait = resolve;
			rejectWait = reject;
		});

		const entry: TrackedProcessEntry = {
			pid,
			processId,
			command,
			args: [...args],
			driver: command === "node" ? "node" : "wasmvm",
			cwd: options?.cwd ?? this.cwd,
			env: {
				...(options?.env ?? {}),
				...(options?.streamStdin ? { AGENT_OS_KEEP_STDIN_OPEN: "1" } : {}),
			},
			startTime: Date.now(),
			exitTime: null,
			hostPid: null,
			exitCode: null,
			started: false,
			startPromise: Promise.resolve(),
			waitPromise,
			resolveWait,
			rejectWait,
			onStdout: new Set(options?.onStdout ? [options.onStdout] : []),
			onStderr: new Set(options?.onStderr ? [options.onStderr] : []),
			pendingStdin: [],
			stdinFlushPromise: null,
			pendingCloseStdin: false,
			pendingKillSignal: null,
		};
		this.trackedProcesses.set(pid, entry);
		this.trackedProcessesById.set(processId, entry);
		this.updateTrackedProcessSnapshot(entry);

		const proc: ManagedProcess = {
			pid,
			writeStdin: (data) => {
				if (entry.exitCode !== null) {
					return;
				}
				entry.pendingStdin.push(data);
				void this.flushPendingStdin(entry);
			},
			closeStdin: () => {
				entry.pendingCloseStdin = true;
				void this.closeTrackedStdin(entry);
			},
			kill: (signal = 15) => {
				if (entry.exitCode !== null) {
					return;
				}
				entry.pendingKillSignal = signal;
				void entry.startPromise.then(async () => {
					if (entry.exitCode !== null || entry.pendingKillSignal === null) {
						return;
					}
					const pendingSignal = entry.pendingKillSignal;
					entry.pendingKillSignal = null;
					await this.signalProcess(entry, pendingSignal);
				});
			},
			wait: () => entry.waitPromise,
			get exitCode() {
				return entry.exitCode;
			},
		};

		entry.startPromise = this.startTrackedProcess(entry).catch((error) => {
			const normalized =
				error instanceof Error ? error : new Error(String(error));
			const stderr = new TextEncoder().encode(`${normalized.message}\n`);
			for (const handler of entry.onStderr) {
				handler(stderr);
			}
			this.finishProcess(entry, 1);
		});

		return proc;
	}

	openShell(options?: OpenShellOptions): ShellHandle {
		const stdoutHandlers = new Set<(data: Uint8Array) => void>();
		const stderrHandlers = new Set<(data: Uint8Array) => void>();
		const proc = this.spawn(options?.command ?? "sh", options?.args ?? [], {
			env: options?.env,
			cwd: options?.cwd,
			onStdout: (chunk) => {
				for (const handler of stdoutHandlers) {
					handler(chunk);
				}
			},
			onStderr: (chunk) => {
				for (const handler of stderrHandlers) {
					handler(chunk);
				}
			},
		});

		let onData: ((data: Uint8Array) => void) | null = null;
		stdoutHandlers.add((data) => onData?.(data));
		if (options?.onStderr) {
			stderrHandlers.add(options.onStderr);
		}

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
				// The current stdio-native path is process-backed rather than PTY-backed.
			},
			kill(signal) {
				proc.kill(signal);
			},
			wait() {
				return proc.wait();
			},
		};
	}

	async connectTerminal(options?: ConnectTerminalOptions): Promise<number> {
		const stdin = process.stdin;
		const stdout = process.stdout;
		const { onData, ...shellOptions } = options ?? {};
		const shell = this.openShell({
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
				shell.resize(stdout.columns, stdout.rows);
			}
		} catch (error) {
			cleanup();
			shell.kill();
			throw error;
		}

		void shell.wait().finally(cleanup);
		return shell.pid;
	}

	readFile(path: string): Promise<Uint8Array> {
		return this.dispatchRead(path, (mount, relativePath) =>
			mount.fs.readFile(relativePath),
		);
	}

	writeFile(path: string, content: string | Uint8Array): Promise<void> {
		return this.dispatchWrite(
			path,
			(mount, relativePath) => mount.fs.writeFile(relativePath, content),
			async () => {
				await this.client.writeFile(this.session, this.vm, path, content);
				this.mirrorGuestFile(path, content);
			},
		);
	}

	async mkdir(path: string, recursive = true): Promise<void> {
		return this.dispatchWrite(
			path,
			(mount, relativePath) => mount.fs.mkdir(relativePath, { recursive }),
			() => this.client.mkdir(this.session, this.vm, path, { recursive }),
		);
	}

	async exists(path: string): Promise<boolean> {
		const local = this.resolveLocalMount(path);
		if (local) {
			return local.mount.fs.exists(local.relativePath);
		}
		return this.client.exists(this.session, this.vm, path);
	}

	async stat(path: string): Promise<VirtualStat> {
		const local = this.resolveLocalMount(path);
		if (local) {
			return local.mount.fs.stat(local.relativePath);
		}
		return toVirtualStat(await this.client.stat(this.session, this.vm, path));
	}

	async readdir(path: string): Promise<string[]> {
		const local = this.resolveLocalMount(path);
		if (local) {
			return local.mount.fs.readDir(local.relativePath);
		}

		const entries = await this.client.readdir(this.session, this.vm, path);
		return [...new Set([...entries, ...this.mountedChildNames(path)])].sort(
			(a, b) => a.localeCompare(b),
		);
	}

	async removeFile(path: string): Promise<void> {
		return this.dispatchWrite(
			path,
			(mount, relativePath) => mount.fs.removeFile(relativePath),
			() => this.client.removeFile(this.session, this.vm, path),
		);
	}

	async removeDir(path: string): Promise<void> {
		return this.dispatchWrite(
			path,
			(mount, relativePath) => mount.fs.removeDir(relativePath),
			() => this.client.removeDir(this.session, this.vm, path),
		);
	}

	async rename(oldPath: string, newPath: string): Promise<void> {
		const from = this.resolveLocalMount(oldPath);
		const to = this.resolveLocalMount(newPath);

		if (!!from !== !!to) {
			throw errnoError("EXDEV", "cross-device link not permitted");
		}
		if (from && to) {
			if (from.mount.path !== to.mount.path) {
				throw errnoError("EXDEV", "cross-device link not permitted");
			}
			this.assertLocalWritable(from.mount);
			return from.mount.fs.rename(from.relativePath, to.relativePath);
		}

		return this.client.rename(this.session, this.vm, oldPath, newPath);
	}

	mountFs(
		path: string,
		driver: VirtualFileSystem,
		options?: { readOnly?: boolean },
	): void {
		this.localMounts.unshift({
			path: posixPath.normalize(path),
			fs: driver,
			readOnly: options?.readOnly ?? false,
		});
		this.localMounts.sort(
			(left, right) => right.path.length - left.path.length,
		);
	}

	unmountFs(path: string): void {
		const normalized = posixPath.normalize(path);
		const index = this.localMounts.findIndex(
			(mount) => mount.path === normalized,
		);
		if (index >= 0) {
			this.localMounts.splice(index, 1);
		}
	}

	snapshotProcesses(): ProcessInfo[] {
		return this.buildProcessSnapshot();
	}

	findListener(request: {
		host?: string;
		port?: number;
		path?: string;
	}): KernelSocketSnapshot | null {
		const key = socketLookupKey("listener", request);
		const cached = this.listenerLookups.get(key);
		if (!cached?.pending) {
			this.listenerLookups.set(key, {
				value: cached?.value ?? null,
				pending: this.refreshSocketLookup(this.listenerLookups, key, () =>
					this.client.findListener(this.session, this.vm, request),
				),
			});
		}
		return this.listenerLookups.get(key)?.value ?? null;
	}

	findBoundUdp(request: {
		host?: string;
		port?: number;
	}): KernelSocketSnapshot | null {
		const key = socketLookupKey("udp", request);
		const cached = this.boundUdpLookups.get(key);
		if (!cached?.pending) {
			this.boundUdpLookups.set(key, {
				value: cached?.value ?? null,
				pending: this.refreshSocketLookup(this.boundUdpLookups, key, () =>
					this.client.findBoundUdp(this.session, this.vm, request),
				),
			});
		}
		return this.boundUdpLookups.get(key)?.value ?? null;
	}

	getSignalState(pid: number): KernelSignalState {
		const entry = this.trackedProcesses.get(pid);
		if (entry && !this.signalRefreshes.has(pid)) {
			this.signalRefreshes.set(pid, this.refreshSignalState(entry));
		}
		return this.signalStates.get(pid) ?? { handlers: new Map() };
	}

	private async refreshSocketLookup(
		cache: Map<string, SocketLookupCacheEntry>,
		key: string,
		lookup: () => Promise<SidecarSocketStateEntry | null>,
	): Promise<void> {
		try {
			const socket = await lookup();
			cache.set(key, {
				value: socket ? toKernelSocketSnapshot(socket) : null,
				pending: null,
			});
		} catch {
			cache.set(key, {
				value: cache.get(key)?.value ?? null,
				pending: null,
			});
		}
	}

	private async refreshSignalState(entry: TrackedProcessEntry): Promise<void> {
		try {
			const signalState = await this.client.getSignalState(
				this.session,
				this.vm,
				entry.processId,
			);
			this.signalStates.set(
				entry.pid,
				toKernelSignalState(signalState.handlers),
			);
		} catch {
			this.signalStates.set(
				entry.pid,
				this.signalStates.get(entry.pid) ?? { handlers: new Map() },
			);
		} finally {
			this.signalRefreshes.delete(entry.pid);
		}
	}

	private async refreshZombieTimerCount(): Promise<void> {
		try {
			const snapshot = await this.client.getZombieTimerCount(
				this.session,
				this.vm,
			);
			this.zombieTimerCountValue = snapshot.count;
		} catch {
			// Keep the last known value if the sidecar query fails.
		} finally {
			this.zombieTimerCountRefresh = null;
		}
	}

	private async startTrackedProcess(entry: TrackedProcessEntry): Promise<void> {
		const execution = await this.resolveExecution(entry);
		if (execution.bootstrap) {
			await execution.bootstrap();
		}
		const started = await this.client.execute(this.session, this.vm, {
			processId: entry.processId,
			runtime: execution.runtime,
			entrypoint: execution.entrypoint,
			args: execution.args,
			env: execution.env,
			cwd: execution.cwd,
		});
		entry.hostPid = started.pid;
		entry.started = true;
		this.updateTrackedProcessSnapshot(entry);
		await this.refreshSignalState(entry);

		void this.flushPendingStdin(entry);
		void this.closeTrackedStdin(entry);

		if (entry.pendingKillSignal !== null) {
			const signal = entry.pendingKillSignal;
			entry.pendingKillSignal = null;
			await this.signalProcess(entry, signal);
		}
	}

	private async runEventPump(): Promise<void> {
		while (!this.disposed) {
			try {
				const event = await this.client.waitForEvent(
					() => true,
					EVENT_PUMP_TIMEOUT_MS,
				);
				if (event.payload.type === "process_output") {
					const entry = this.trackedProcessesById.get(event.payload.process_id);
					if (!entry) {
						continue;
					}
					if (!this.signalRefreshes.has(entry.pid)) {
						this.signalRefreshes.set(entry.pid, this.refreshSignalState(entry));
						await this.signalRefreshes.get(entry.pid);
					}
					const chunk = new TextEncoder().encode(event.payload.chunk);
					const listeners =
						event.payload.channel === "stdout"
							? entry.onStdout
							: entry.onStderr;
					for (const listener of listeners) {
						listener(chunk);
					}
					continue;
				}

				if (event.payload.type === "process_exited") {
					const entry = this.trackedProcessesById.get(event.payload.process_id);
					if (!entry) {
						continue;
					}
					this.signalRefreshes.delete(entry.pid);
					this.finishProcess(entry, event.payload.exit_code);
				}
			} catch (error) {
				if (this.disposed) {
					return;
				}
				this.pumpError =
					error instanceof Error ? error : new Error(String(error));
				for (const entry of this.trackedProcesses.values()) {
					if (entry.exitCode !== null) {
						continue;
					}
					const stderr = new TextEncoder().encode(
						`${this.pumpError.message}\n`,
					);
					for (const listener of entry.onStderr) {
						listener(stderr);
					}
					this.finishProcess(entry, 1);
				}
				return;
			}
		}
	}

	private finishProcess(entry: TrackedProcessEntry, exitCode: number): void {
		if (entry.exitCode !== null) {
			return;
		}
		entry.exitCode = exitCode;
		entry.exitTime = Date.now();
		this.updateTrackedProcessSnapshot(entry);
		entry.resolveWait(exitCode);
	}

	private async signalProcess(
		entry: TrackedProcessEntry,
		signal: number,
	): Promise<void> {
		if (entry.hostPid !== null) {
			try {
				process.kill(entry.hostPid, signal);
				return;
			} catch (error) {
				if (isMissingHostProcessError(error)) {
					return;
				}
				throw error;
			}
		}

		try {
			await this.client.killProcess(
				this.session,
				this.vm,
				entry.processId,
				toSidecarSignalName(signal),
			);
		} catch (error) {
			if (isNoSuchProcessError(error)) {
				return;
			}
			throw error;
		}
	}

	private flushPendingStdin(entry: TrackedProcessEntry): Promise<void> {
		if (entry.stdinFlushPromise) {
			return entry.stdinFlushPromise;
		}

		entry.stdinFlushPromise = entry.startPromise
			.then(async () => {
				if (entry.exitCode !== null) {
					return;
				}
				while (entry.pendingStdin.length > 0) {
					const chunk = entry.pendingStdin.shift();
					if (chunk === undefined) {
						break;
					}
					await this.client.writeStdin(
						this.session,
						this.vm,
						entry.processId,
						chunk,
					);
				}
			})
			.finally(() => {
				entry.stdinFlushPromise = null;
				if (entry.pendingStdin.length > 0 && entry.exitCode === null) {
					void this.flushPendingStdin(entry);
				}
			});
		return entry.stdinFlushPromise;
	}

	private async closeTrackedStdin(entry: TrackedProcessEntry): Promise<void> {
		await entry.startPromise;
		await this.flushPendingStdin(entry);
		if (entry.exitCode !== null || !entry.pendingCloseStdin) {
			return;
		}
		entry.pendingCloseStdin = false;
		try {
			await this.client.closeStdin(this.session, this.vm, entry.processId);
		} catch (error) {
			if (isNoSuchProcessError(error)) {
				return;
			}
			throw error;
		}
	}

	private resolveExecCommand(command: string): {
		command: string;
		args: string[];
	} {
		if (this.commandGuestPaths.has("sh")) {
			return {
				command: "sh",
				args: ["-c", command],
			};
		}

		const tokens = tokenizeCommand(command);
		if (tokens.length >= 2 && tokens[0] === "node") {
			return {
				command: "node",
				args: tokens.slice(1),
			};
		}

		throw new Error(
			`native sidecar exec requires a shell command driver: ${command}`,
		);
	}

	private async resolveExecution(entry: TrackedProcessEntry): Promise<{
		runtime: "java_script" | "web_assembly";
		entrypoint: string;
		args: string[];
		cwd?: string;
		env?: Record<string, string>;
		bootstrap?: () => Promise<void>;
	}> {
		if (entry.command === "node") {
			if (entry.args.length === 0) {
				throw new Error("node spawn requires an entrypoint");
			}
			if (entry.args[0] === "-e") {
				const source = entry.args[1] ?? "";
				const guestEntrypoint = `/tmp/agent-os-inline-${entry.pid}.mjs`;
				const entrypoint = this.shadowPathForGuest(guestEntrypoint, false);
				return {
					runtime: "java_script",
					entrypoint: guestEntrypoint,
					args: entry.args.slice(2),
					cwd: this.resolveNodeCwd(entry.cwd),
					env: this.buildNodeExecutionEnv(entry, guestEntrypoint),
					bootstrap: async () => {
						mkdirSync(dirnameHostPath(entrypoint), { recursive: true });
						writeFileSync(entrypoint, source);
					},
				};
			}
			const entrypoint = await this.resolveNodeEntrypoint(
				entry.args[0],
				entry.cwd,
			);
			return {
				runtime: "java_script",
				entrypoint,
				args: entry.args.slice(1),
				cwd: this.resolveNodeCwd(entry.cwd),
				env: this.buildNodeExecutionEnv(entry, entrypoint),
			};
		}

		const wasmEntrypoint = this.commandGuestPaths.get(entry.command);
		if (wasmEntrypoint) {
			return {
				runtime: "web_assembly",
				entrypoint: wasmEntrypoint,
				args: entry.args,
				cwd: entry.cwd,
				env: entry.env,
			};
		}

		throw new Error(
			`command not found on native sidecar path: ${entry.command}`,
		);
	}

	private async resolveNodeEntrypoint(
		entrypoint: string,
		cwd: string,
	): Promise<string> {
		if (!isPathLikeSpecifier(entrypoint)) {
			return entrypoint;
		}

		if (entrypoint.startsWith("file:")) {
			return entrypoint;
		}

		const guestPath = entrypoint.startsWith("/")
			? posixPath.normalize(entrypoint)
			: posixPath.normalize(posixPath.join(cwd, entrypoint));
		if (!this.resolveHostPath(guestPath)) {
			await this.materializeGuestFile(guestPath);
		}
		return guestPath;
	}

	private resolveNodeCwd(cwd: string): string {
		return this.resolveHostPath(cwd) ?? this.shadowPathForGuest(cwd, true);
	}

	private resolveHostPath(guestPath: string): string | null {
		const normalized = posixPath.normalize(guestPath);
		for (const mapping of this.hostPathMappings) {
			if (
				normalized !== mapping.guestPath &&
				!normalized.startsWith(`${mapping.guestPath}/`)
			) {
				continue;
			}
			const suffix =
				normalized === mapping.guestPath
					? ""
					: normalized.slice(mapping.guestPath.length + 1);
			return suffix.length === 0
				? mapping.hostPath
				: joinHostPath(mapping.hostPath, suffix);
		}
		return null;
	}

	private shadowPathForGuest(guestPath: string, directory: boolean): string {
		const relativePath = posixPath.normalize(guestPath).replace(/^\/+/, "");
		const hostPath = joinHostPath(this.shadowRoot, relativePath);
		mkdirSync(directory ? hostPath : dirnameHostPath(hostPath), {
			recursive: true,
		});
		return hostPath;
	}

	private async materializeGuestFile(guestPath: string): Promise<string> {
		const hostPath = joinHostPath(
			this.shadowRoot,
			posixPath.normalize(guestPath).replace(/^\/+/, ""),
		);
		mkdirSync(dirnameHostPath(hostPath), { recursive: true });
		writeFileSync(hostPath, Buffer.from(await this.readFile(guestPath)));
		return hostPath;
	}

	private materializeHostPathMappings(): void {
		for (const mapping of this.hostPathMappings) {
			const linkPath = this.shadowPathForGuest(mapping.guestPath, false);
			rmSync(linkPath, { recursive: true, force: true });
			symlinkSync(mapping.hostPath, linkPath);
		}
	}

	private buildNodeExecutionEnv(
		entry: TrackedProcessEntry,
		guestEntrypoint: string,
	): Record<string, string> {
		const pathMappings = [
			...this.hostPathMappings,
			{ guestPath: "/", hostPath: this.shadowRoot },
		];
		const guestLiteralPaths = [
			entry.cwd,
			entry.env.HOME ?? this.env.HOME,
			...this.hostPathMappings.map((mapping) => mapping.guestPath),
		].filter(
			(candidate): candidate is string =>
				typeof candidate === "string" && candidate.startsWith("/"),
		);
		const extraReadPaths = dedupePaths([
			...expandHostAccessPaths([
				this.shadowRoot,
				...pathMappings.map((mapping) => mapping.hostPath),
			]),
			...guestLiteralPaths,
		]);
		const extraWritePaths = dedupePaths([
			this.shadowRoot,
			...guestLiteralPaths,
		]);

		return {
			...entry.env,
			[GUEST_PATH_MAPPINGS_ENV]: JSON.stringify(pathMappings),
			[EXTRA_FS_READ_PATHS_ENV]: JSON.stringify(extraReadPaths),
			[EXTRA_FS_WRITE_PATHS_ENV]: JSON.stringify(extraWritePaths),
			[ALLOWED_NODE_BUILTINS_ENV]: JSON.stringify(this.allowedNodeBuiltins),
			[LOOPBACK_EXEMPT_PORTS_ENV]: JSON.stringify(
				this.loopbackExemptPorts.map((port) => String(port)),
			),
			AGENT_OS_GUEST_ENTRYPOINT: guestEntrypoint,
		};
	}

	private createFilesystemView(includeLocalMounts: boolean): VirtualFileSystem {
		return {
			readFile: (path) =>
				this.dispatchRead(
					path,
					(mount, relativePath) => mount.fs.readFile(relativePath),
					includeLocalMounts,
				),
			readTextFile: async (path) =>
				new TextDecoder().decode(
					await this.dispatchRead(
						path,
						(mount, relativePath) => mount.fs.readFile(relativePath),
						includeLocalMounts,
					),
				),
			readDir: async (path) => {
				const local = includeLocalMounts ? this.resolveLocalMount(path) : null;
				if (local) {
					return local.mount.fs.readDir(local.relativePath);
				}
				const entries = await this.client.readdir(this.session, this.vm, path);
				return includeLocalMounts
					? [...new Set([...entries, ...this.mountedChildNames(path)])].sort(
							(a, b) => a.localeCompare(b),
						)
					: entries;
			},
			readDirWithTypes: async (path) => {
				const entries =
					await this.createFilesystemView(includeLocalMounts).readDir(path);
				return Promise.all(
					entries.map(async (name) => {
						const stat = await this.createFilesystemView(
							includeLocalMounts,
						).lstat(posixPath.join(path, name));
						return {
							name,
							isDirectory: stat.isDirectory,
							isSymbolicLink: stat.isSymbolicLink,
						};
					}),
				);
			},
			writeFile: (path, content) =>
				this.dispatchWrite(
					path,
					(mount, relativePath) => mount.fs.writeFile(relativePath, content),
					async () => {
						await this.client.writeFile(this.session, this.vm, path, content);
						this.mirrorGuestFile(path, content);
					},
					includeLocalMounts,
				),
			createDir: (path) =>
				this.dispatchWrite(
					path,
					(mount, relativePath) => mount.fs.createDir(relativePath),
					() =>
						this.client.mkdir(this.session, this.vm, path, {
							recursive: false,
						}),
					includeLocalMounts,
				),
			mkdir: (path, options) =>
				this.dispatchWrite(
					path,
					(mount, relativePath) =>
						mount.fs.mkdir(relativePath, {
							recursive: options?.recursive ?? true,
						}),
					() =>
						this.client.mkdir(this.session, this.vm, path, {
							recursive: options?.recursive ?? true,
						}),
					includeLocalMounts,
				),
			exists: async (path) => {
				const local = includeLocalMounts ? this.resolveLocalMount(path) : null;
				if (local) {
					return local.mount.fs.exists(local.relativePath);
				}
				return this.client.exists(this.session, this.vm, path);
			},
			stat: async (path) => {
				const local = includeLocalMounts ? this.resolveLocalMount(path) : null;
				if (local) {
					return local.mount.fs.stat(local.relativePath);
				}
				return toVirtualStat(
					await this.client.stat(this.session, this.vm, path),
				);
			},
			removeFile: (path) =>
				this.dispatchWrite(
					path,
					(mount, relativePath) => mount.fs.removeFile(relativePath),
					() => this.client.removeFile(this.session, this.vm, path),
					includeLocalMounts,
				),
			removeDir: (path) =>
				this.dispatchWrite(
					path,
					(mount, relativePath) => mount.fs.removeDir(relativePath),
					() => this.client.removeDir(this.session, this.vm, path),
					includeLocalMounts,
				),
			rename: async (oldPath, newPath) => {
				const from = includeLocalMounts
					? this.resolveLocalMount(oldPath)
					: null;
				const to = includeLocalMounts ? this.resolveLocalMount(newPath) : null;
				if (!!from !== !!to) {
					throw errnoError("EXDEV", "cross-device link not permitted");
				}
				if (from && to) {
					if (from.mount.path !== to.mount.path) {
						throw errnoError("EXDEV", "cross-device link not permitted");
					}
					this.assertLocalWritable(from.mount);
					return from.mount.fs.rename(from.relativePath, to.relativePath);
				}
				return this.client.rename(this.session, this.vm, oldPath, newPath);
			},
			realpath: async (path) => {
				const local = includeLocalMounts ? this.resolveLocalMount(path) : null;
				if (local) {
					return local.mount.fs.realpath(local.relativePath);
				}
				return this.client.realpath(this.session, this.vm, path);
			},
			symlink: (target, linkPath) =>
				this.dispatchWrite(
					linkPath,
					(mount, relativePath) => mount.fs.symlink(target, relativePath),
					() => this.client.symlink(this.session, this.vm, target, linkPath),
					includeLocalMounts,
				),
			readlink: async (path) => {
				const local = includeLocalMounts ? this.resolveLocalMount(path) : null;
				if (local) {
					return local.mount.fs.readlink(local.relativePath);
				}
				return this.client.readLink(this.session, this.vm, path);
			},
			lstat: async (path) => {
				const local = includeLocalMounts ? this.resolveLocalMount(path) : null;
				if (local) {
					return local.mount.fs.lstat(local.relativePath);
				}
				return toVirtualStat(
					await this.client.lstat(this.session, this.vm, path),
				);
			},
			link: async (oldPath, newPath) => {
				const from = includeLocalMounts
					? this.resolveLocalMount(oldPath)
					: null;
				const to = includeLocalMounts ? this.resolveLocalMount(newPath) : null;
				if (!!from !== !!to) {
					throw errnoError("EXDEV", "cross-device link not permitted");
				}
				if (from && to) {
					if (from.mount.path !== to.mount.path) {
						throw errnoError("EXDEV", "cross-device link not permitted");
					}
					this.assertLocalWritable(from.mount);
					return from.mount.fs.link(from.relativePath, to.relativePath);
				}
				return this.client.link(this.session, this.vm, oldPath, newPath);
			},
			chmod: (path, mode) =>
				this.dispatchWrite(
					path,
					(mount, relativePath) => mount.fs.chmod(relativePath, mode),
					() => this.client.chmod(this.session, this.vm, path, mode),
					includeLocalMounts,
				),
			chown: (path, uid, gid) =>
				this.dispatchWrite(
					path,
					(mount, relativePath) => mount.fs.chown(relativePath, uid, gid),
					() => this.client.chown(this.session, this.vm, path, uid, gid),
					includeLocalMounts,
				),
			utimes: (path, atimeMs, mtimeMs) =>
				this.dispatchWrite(
					path,
					(mount, relativePath) =>
						mount.fs.utimes(relativePath, atimeMs, mtimeMs),
					() =>
						this.client.utimes(this.session, this.vm, path, atimeMs, mtimeMs),
					includeLocalMounts,
				),
			truncate: (path, length) =>
				this.dispatchWrite(
					path,
					(mount, relativePath) => mount.fs.truncate(relativePath, length),
					() => this.client.truncate(this.session, this.vm, path, length),
					includeLocalMounts,
				),
			pread: async (path, offset, length) => {
				const bytes =
					await this.createFilesystemView(includeLocalMounts).readFile(path);
				return bytes.subarray(offset, offset + length);
			},
			pwrite: async (path, offset, data) => {
				const bytes =
					await this.createFilesystemView(includeLocalMounts).readFile(path);
				const nextSize = Math.max(bytes.length, offset + data.length);
				const updated = new Uint8Array(nextSize);
				updated.set(bytes);
				updated.set(data, offset);
				await this.createFilesystemView(includeLocalMounts).writeFile(
					path,
					updated,
				);
			},
		};
	}

	private buildProcessSnapshot(): ProcessInfo[] {
		const processMap = new Map<number, ProcessInfo>();
		const hostRoots = new Map<number, TrackedProcessEntry>();

		for (const entry of this.trackedProcesses.values()) {
			processMap.set(entry.pid, {
				pid: entry.pid,
				ppid: 0,
				pgid: entry.pid,
				sid: entry.pid,
				driver: entry.driver,
				command: entry.command,
				args: entry.args,
				cwd: entry.cwd,
				status: entry.exitCode === null ? "running" : "exited",
				exitCode: entry.exitCode,
				startTime: entry.startTime,
				exitTime: entry.exitTime,
			});
			if (entry.hostPid !== null && entry.exitCode === null) {
				hostRoots.set(entry.hostPid, entry);
			}
		}

		if (hostRoots.size === 0) {
			return [...processMap.values()];
		}

		const rows = readHostProcesses();
		const childrenByParent = new Map<number, HostProcessRow[]>();
		for (const row of rows) {
			const children = childrenByParent.get(row.ppid);
			if (children) {
				children.push(row);
				continue;
			}
			childrenByParent.set(row.ppid, [row]);
		}

		const displayPidByHostPid = new Map<number, number>();
		for (const [hostPid, entry] of hostRoots) {
			displayPidByHostPid.set(hostPid, entry.pid);
		}

		const queue = [...hostRoots.keys()];
		while (queue.length > 0) {
			const hostPid = queue.shift();
			if (hostPid === undefined) {
				break;
			}
			for (const child of childrenByParent.get(hostPid) ?? []) {
				const displayPid = child.pid;
				const displayPpid = displayPidByHostPid.get(child.ppid) ?? child.ppid;
				processMap.set(displayPid, {
					pid: displayPid,
					ppid: displayPpid,
					pgid: displayPid,
					sid: displayPid,
					driver: "node",
					command: child.command,
					args: [],
					cwd: "/",
					status: "running",
					exitCode: null,
					startTime: Date.now(),
					exitTime: null,
				});
				displayPidByHostPid.set(child.pid, displayPid);
				queue.push(child.pid);
			}
		}

		return [...processMap.values()];
	}

	private dispatchRead<T>(
		path: string,
		handler: (mount: LocalCompatMount, relativePath: string) => Promise<T>,
		includeLocalMounts = true,
	): Promise<T> {
		const local = includeLocalMounts ? this.resolveLocalMount(path) : null;
		if (local) {
			return handler(local.mount, local.relativePath);
		}
		return this.dispatchNativeRead(path) as Promise<T>;
	}

	private dispatchNativeRead(path: string): Promise<Uint8Array> {
		return this.client.readFile(this.session, this.vm, path);
	}

	private async dispatchWrite(
		path: string,
		handler: (mount: LocalCompatMount, relativePath: string) => Promise<void>,
		nativeHandler: () => Promise<void>,
		includeLocalMounts = true,
	): Promise<void> {
		const local = includeLocalMounts ? this.resolveLocalMount(path) : null;
		if (local) {
			this.assertLocalWritable(local.mount);
			await handler(local.mount, local.relativePath);
			return;
		}
		await nativeHandler();
	}

	private resolveLocalMount(
		path: string,
	): { mount: LocalCompatMount; relativePath: string } | null {
		const normalizedPath = posixPath.normalize(path);
		for (const mount of this.localMounts) {
			if (
				normalizedPath !== mount.path &&
				!normalizedPath.startsWith(`${mount.path}/`)
			) {
				continue;
			}
			const relativePath =
				normalizedPath === mount.path
					? "/"
					: `/${normalizedPath.slice(mount.path.length + 1)}`;
			return {
				mount,
				relativePath,
			};
		}
		return null;
	}

	private mountedChildNames(path: string): string[] {
		const normalizedPath = posixPath.normalize(path);
		const names = new Set<string>();
		for (const mount of this.localMounts) {
			if (mount.path === normalizedPath) {
				continue;
			}
			if (
				!mount.path.startsWith(`${normalizedPath}/`) &&
				normalizedPath !== "/"
			) {
				continue;
			}
			const relative =
				normalizedPath === "/"
					? mount.path.slice(1)
					: mount.path.slice(normalizedPath.length + 1);
			const name = relative.split("/").find(Boolean);
			if (name) {
				names.add(name);
			}
		}
		return [...names];
	}

	private assertLocalWritable(mount: LocalCompatMount): void {
		if (mount.readOnly) {
			throw errnoError("EROFS", "read-only file system");
		}
	}

	private mirrorGuestFile(path: string, content: string | Uint8Array): void {
		if (this.resolveHostPath(path)) {
			return;
		}
		const hostPath = this.shadowPathForGuest(path, false);
		writeFileSync(
			hostPath,
			typeof content === "string" ? content : Buffer.from(content),
		);
	}

	private updateTrackedProcessSnapshot(entry: TrackedProcessEntry): void {
		this.processes.set(entry.pid, {
			pid: entry.pid,
			ppid: 0,
			pgid: entry.pid,
			sid: entry.pid,
			driver: entry.driver,
			command: entry.command,
			args: entry.args,
			cwd: entry.cwd,
			status: entry.exitCode === null ? "running" : "exited",
			exitCode: entry.exitCode,
			startTime: entry.startTime,
			exitTime: entry.exitTime,
		});
	}
}

function buildCommandMap(
	commandGuestPaths: ReadonlyMap<string, string>,
): ReadonlyMap<string, string> {
	const commands = new Map<string, string>([
		["node", "node"],
		["npm", "node"],
		["npx", "node"],
	]);
	for (const name of commandGuestPaths.keys()) {
		commands.set(name, "wasmvm");
	}
	return commands;
}

function isPathLikeSpecifier(specifier: string): boolean {
	return (
		specifier.startsWith("/") ||
		specifier.startsWith("./") ||
		specifier.startsWith("../") ||
		specifier.startsWith("file:")
	);
}

function isNoSuchProcessError(error: unknown): boolean {
	if (!(error instanceof Error)) {
		return false;
	}
	const message = error.message.toLowerCase();
	return (
		error.message.includes("ESRCH") ||
		message.includes("no such process") ||
		message.includes("has no active process")
	);
}

function isMissingHostProcessError(error: unknown): boolean {
	return (
		typeof error === "object" &&
		error !== null &&
		"code" in error &&
		(error as { code?: unknown }).code === "ESRCH"
	);
}

function errnoError(code: string, message: string): Error {
	return Object.assign(new Error(`${code}: ${message}`), { code });
}

function toVirtualStat(stat: GuestFilesystemStat): VirtualStat {
	return {
		mode: stat.mode,
		size: stat.size,
		isDirectory: stat.is_directory,
		isSymbolicLink: stat.is_symbolic_link,
		atimeMs: stat.atime_ms,
		mtimeMs: stat.mtime_ms,
		ctimeMs: stat.ctime_ms,
		birthtimeMs: stat.birthtime_ms,
		ino: stat.ino,
		nlink: stat.nlink,
		uid: stat.uid,
		gid: stat.gid,
	};
}

function toKernelSocketSnapshot(
	socket: SidecarSocketStateEntry,
): KernelSocketSnapshot {
	return {
		processId: socket.processId,
		...(socket.host !== undefined ? { host: socket.host } : {}),
		...(socket.port !== undefined ? { port: socket.port } : {}),
		...(socket.path !== undefined ? { path: socket.path } : {}),
	};
}

function toKernelSignalState(
	handlers: ReadonlyMap<number, SidecarSignalHandlerRegistration>,
): KernelSignalState {
	return {
		handlers: new Map(
			[...handlers.entries()].map(([signal, registration]) => [
				signal,
				{
					action: registration.action,
					mask: new Set(registration.mask),
					flags: registration.flags,
				},
			]),
		),
	};
}

function socketLookupKey(
	kind: "listener" | "udp",
	request: { host?: string; port?: number; path?: string },
): string {
	return JSON.stringify({
		kind,
		host: request.host ?? null,
		port: request.port ?? null,
		path: request.path ?? null,
	});
}

function tokenizeCommand(command: string): string[] {
	const tokens: string[] = [];
	let current = "";
	let quote: "'" | '"' | null = null;
	let escaping = false;

	for (const char of command) {
		if (escaping) {
			current += char;
			escaping = false;
			continue;
		}
		if (char === "\\") {
			escaping = true;
			continue;
		}
		if (quote) {
			if (char === quote) {
				quote = null;
				continue;
			}
			current += char;
			continue;
		}
		if (char === "'" || char === '"') {
			quote = char;
			continue;
		}
		if (/\s/.test(char)) {
			if (current.length > 0) {
				tokens.push(current);
				current = "";
			}
			continue;
		}
		current += char;
	}

	if (current.length > 0) {
		tokens.push(current);
	}

	return tokens;
}

function readHostProcesses(): HostProcessRow[] {
	try {
		const output = execFileSync("ps", ["-eo", "pid=,ppid=,comm="], {
			encoding: "utf8",
		});
		return output
			.split("\n")
			.map((line) => line.trim())
			.filter(Boolean)
			.map((line) => {
				const [pid, ppid, ...commandParts] = line.split(/\s+/);
				return {
					pid: Number(pid),
					ppid: Number(ppid),
					command: commandParts.join(" "),
				};
			})
			.filter((row) => Number.isFinite(row.pid) && Number.isFinite(row.ppid));
	} catch {
		return [];
	}
}

function expandHostAccessPaths(paths: readonly string[]): string[] {
	const expanded: string[] = [];
	const seen = new Set<string>();

	const addPath = (candidate: string | null): void => {
		if (!candidate || seen.has(candidate)) {
			return;
		}
		seen.add(candidate);
		expanded.push(candidate);
	};

	for (const hostPath of paths) {
		addPath(hostPath);
		addPath(safeRealpathSync(hostPath));

		if (basenameHostPath(hostPath) !== "node_modules") {
			continue;
		}

		let current = dirnameHostPath(hostPath);
		while (true) {
			const candidate = joinHostPath(current, "node_modules");
			if (existsSync(candidate)) {
				addPath(candidate);
				addPath(safeRealpathSync(candidate));
			}

			const parent = dirnameHostPath(current);
			if (parent === current) {
				break;
			}
			current = parent;
		}
	}

	return expanded;
}

function safeRealpathSync(path: string): string | null {
	try {
		return realpathSync.native(path);
	} catch {
		return null;
	}
}

function dedupePaths(paths: readonly string[]): string[] {
	return [...new Set(paths)];
}
