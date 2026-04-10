import { randomUUID } from "node:crypto";
import { rmSync } from "node:fs";
import { constants as osConstants } from "node:os";
import { posix as posixPath } from "node:path";
import type {
	NativeMountConfig,
	PlainMountConfig,
	RootFilesystemConfig,
	RootLowerInput,
} from "../agent-os.js";
import type { FilesystemEntry } from "../filesystem-snapshot.js";
import type { RootSnapshotExport } from "../layers.js";
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
	SidecarProcessSnapshotEntry,
	SidecarSignalHandlerRegistration,
	SidecarSocketStateEntry,
} from "./native-process-client.js";

const SYNTHETIC_PID_BASE = 1_000_000;
const EVENT_PUMP_TIMEOUT_MS = 86_400_000;

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

interface NativeSidecarKernelProxyOptions {
	client: NativeSidecarProcessClient;
	session: AuthenticatedSession;
	vm: CreatedVm;
	env: Record<string, string>;
	cwd: string;
	localMounts: LocalCompatMount[];
	commandGuestPaths: ReadonlyMap<string, string>;
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
	private readonly commandDrivers: Map<string, string>;
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
	private sidecarProcessSnapshot: SidecarProcessSnapshotEntry[] = [];
	private processSnapshotRefresh: Promise<void> | null = null;
	private readonly observedProcessStartTimes = new Map<string, number>();
	private readonly rootView: VirtualFileSystem;
	private zombieTimerCountValue = 0;
	private zombieTimerCountRefresh: Promise<void> | null = null;
	private disposed = false;
	private pumpError: Error | null = null;
	private nextSyntheticPid = SYNTHETIC_PID_BASE;
	private readonly eventPump: Promise<void>;

	constructor(options: NativeSidecarKernelProxyOptions) {
		this.client = options.client;
		this.session = options.session;
		this.vm = options.vm;
		this.env = { ...options.env };
		this.cwd = options.cwd;
		this.localMounts = [...options.localMounts].sort(
			(left, right) => right.path.length - left.path.length,
		);
		this.commandDrivers = buildCommandMap(options.commandGuestPaths);
		this.onDispose = options.onDispose;
		this.commands = this.commandDrivers;
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
		for (const name of commandGuestPaths.keys()) {
			this.commandDrivers.set(name, "wasmvm");
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
		await this.onDispose?.().catch(() => {});
	}

	async exec(
		command: string,
		options?: KernelExecOptions,
	): Promise<KernelExecResult> {
		if (!this.commands.has("sh")) {
			throw new Error(
				`native sidecar exec requires guest shell command 'sh': ${command}`,
			);
		}

		const stdoutChunks: Uint8Array[] = [];
		const stderrChunks: Uint8Array[] = [];
		const proc = this.spawn("sh", ["-c", command], {
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
		void shell.wait().finally(() => {
			cleanup();
		});
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
			() => this.client.writeFile(this.session, this.vm, path, content),
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

	private async refreshProcessSnapshot(): Promise<void> {
		if (this.processSnapshotRefresh) {
			await this.processSnapshotRefresh;
			return;
		}

		this.processSnapshotRefresh = (async () => {
			try {
				this.sidecarProcessSnapshot = await this.client.getProcessSnapshot(
					this.session,
					this.vm,
				);
			} finally {
				this.processSnapshotRefresh = null;
			}
		})();

		await this.processSnapshotRefresh;
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
		const started = await this.client.execute(this.session, this.vm, {
			processId: entry.processId,
			command: entry.command,
			args: entry.args,
			env: entry.env,
			cwd: entry.cwd,
		});
		entry.hostPid = started.pid;
		entry.started = true;
		this.updateTrackedProcessSnapshot(entry);
		void this.refreshProcessSnapshot().catch(() => {});
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
					void this.refreshProcessSnapshot().catch(() => {});
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
					void this.refreshProcessSnapshot().catch(() => {});
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
		if (entry.stdinFlushPromise !== null) {
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
					() => this.client.writeFile(this.session, this.vm, path, content),
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
		void this.refreshProcessSnapshot().catch(() => {});
		const processMap = new Map<number, ProcessInfo>();
		const displayPidByKernelPid = new Map<number, number>();

		for (const entry of this.sidecarProcessSnapshot) {
			const tracked = this.trackedProcessesById.get(entry.processId);
			if (tracked) {
				displayPidByKernelPid.set(entry.pid, tracked.pid);
			}
		}

		for (const entry of this.sidecarProcessSnapshot) {
			const tracked = this.trackedProcessesById.get(entry.processId);
			const displayPid = displayPidByKernelPid.get(entry.pid) ?? entry.pid;
			const displayPpid = displayPidByKernelPid.get(entry.ppid) ?? entry.ppid;
			const displayPgid = displayPidByKernelPid.get(entry.pgid) ?? entry.pgid;
			const displaySid = displayPidByKernelPid.get(entry.sid) ?? entry.sid;
			const processKey = `${entry.processId}:${entry.pid}`;
			const startTime =
				tracked?.startTime ??
				this.observedProcessStartTimes.get(processKey) ??
				Date.now();
			this.observedProcessStartTimes.set(processKey, startTime);

			processMap.set(displayPid, {
				pid: displayPid,
				ppid: displayPpid,
				pgid: displayPgid,
				sid: displaySid,
				driver: tracked?.driver ?? entry.driver,
				command: tracked?.command ?? entry.command,
				args: tracked?.args ?? entry.args,
				cwd: tracked?.cwd ?? entry.cwd,
				status:
					tracked?.exitCode !== null
						? "exited"
						: tracked
							? "running"
							: entry.status,
				exitCode: tracked?.exitCode ?? entry.exitCode,
				startTime,
				exitTime: tracked?.exitTime ?? null,
			});
		}

		for (const entry of this.trackedProcesses.values()) {
			if (processMap.has(entry.pid)) {
				continue;
			}
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
		}

		this.processes.clear();
		for (const process of processMap.values()) {
			this.processes.set(process.pid, process);
		}

		return [...processMap.values()].sort((left, right) => left.pid - right.pid);
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
): Map<string, string> {
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
		blocks: stat.blocks,
		dev: stat.dev,
		rdev: stat.rdev,
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

export type {
	AuthenticatedSession,
	CreatedVm,
	GuestFilesystemStat,
	NativeSidecarSpawnOptions,
	RootFilesystemEntry,
	SidecarPermissionsPolicy,
	SidecarRegisteredToolDefinition,
	SidecarRequestFrame,
	SidecarResponsePayload,
	SidecarSessionState,
	SidecarSignalHandlerRegistration,
	SidecarSocketStateEntry,
} from "./native-process-client.js";
export { NativeSidecarProcessClient } from "./native-process-client.js";

export type AgentOsSidecarPlacement =
	| { kind: "shared"; pool?: string }
	| { kind: "explicit"; sidecarId: string };

export type AgentOsSidecarSessionState =
	| "connecting"
	| "ready"
	| "disposing"
	| "disposed"
	| "failed";

export type AgentOsSidecarVmState =
	| "creating"
	| "ready"
	| "disposing"
	| "disposed"
	| "failed";

export interface AgentOsSidecarSessionLifecycle {
	sessionId: string;
	placement: AgentOsSidecarPlacement;
	state: AgentOsSidecarSessionState;
	createdAt: number;
	connectedAt?: number;
	disposedAt?: number;
	lastError?: string;
	metadata: Record<string, string>;
	vmIds: string[];
}

export interface AgentOsSidecarVmLifecycle {
	vmId: string;
	sessionId: string;
	state: AgentOsSidecarVmState;
	createdAt: number;
	readyAt?: number;
	disposedAt?: number;
	lastError?: string;
	metadata: Record<string, string>;
}

export interface AgentOsSidecarSessionOptions {
	placement?: AgentOsSidecarPlacement;
	metadata?: Record<string, string>;
	signal?: AbortSignal;
}

export interface AgentOsSidecarVmOptions {
	metadata?: Record<string, string>;
}

export interface AgentOsSidecarSessionBootstrap {
	sessionId: string;
	placement: AgentOsSidecarPlacement;
	metadata: Record<string, string>;
	signal?: AbortSignal;
}

export interface AgentOsSidecarVmBootstrap {
	vmId: string;
	sessionId: string;
	metadata: Record<string, string>;
}

export interface AgentOsSidecarTransport {
	createVm?(bootstrap: AgentOsSidecarVmBootstrap): Promise<void>;
	disposeVm?(vmId: string): Promise<void>;
	dispose(): Promise<void>;
}

export interface AgentOsSidecarClientOptions {
	createSessionTransport(
		bootstrap: AgentOsSidecarSessionBootstrap,
	): Promise<AgentOsSidecarTransport>;
	createId?: () => string;
	now?: () => number;
}

interface AgentOsSidecarVmEntry {
	lifecycle: AgentOsSidecarVmLifecycle;
}

interface AgentOsSidecarSessionEntry {
	lifecycle: AgentOsSidecarSessionLifecycle;
	transport?: AgentOsSidecarTransport;
	vms: Map<string, AgentOsSidecarVmEntry>;
}

export class AgentOsSidecarVmHandle {
	constructor(
		private readonly client: AgentOsSidecarClient,
		readonly sessionId: string,
		readonly vmId: string,
	) {}

	describe(): AgentOsSidecarVmLifecycle {
		return this.client.requireVmLifecycle(this.sessionId, this.vmId);
	}

	async dispose(): Promise<void> {
		await this.client.disposeVm(this.sessionId, this.vmId);
	}
}

export class AgentOsSidecarSessionHandle {
	constructor(
		private readonly client: AgentOsSidecarClient,
		readonly sessionId: string,
	) {}

	describe(): AgentOsSidecarSessionLifecycle {
		return this.client.requireSessionLifecycle(this.sessionId);
	}

	listVms(): AgentOsSidecarVmLifecycle[] {
		return this.client.listVms(this.sessionId);
	}

	async createVm(
		options?: AgentOsSidecarVmOptions,
	): Promise<AgentOsSidecarVmHandle> {
		return this.client.createVm(this.sessionId, options);
	}

	async dispose(): Promise<void> {
		await this.client.disposeSession(this.sessionId);
	}
}

export class AgentOsSidecarClient {
	private readonly createSessionTransport: AgentOsSidecarClientOptions["createSessionTransport"];
	private readonly createId: () => string;
	private readonly now: () => number;
	private readonly sessions = new Map<string, AgentOsSidecarSessionEntry>();
	private disposed = false;

	constructor(options: AgentOsSidecarClientOptions) {
		this.createSessionTransport = options.createSessionTransport;
		this.createId = options.createId ?? randomUUID;
		this.now = options.now ?? Date.now;
	}

	async createSession(
		options: AgentOsSidecarSessionOptions = {},
	): Promise<AgentOsSidecarSessionHandle> {
		this.assertActive();

		const sessionId = this.createId();
		const placement = clonePlacement(options.placement);
		const metadata = cloneMetadata(options.metadata);
		const lifecycle: AgentOsSidecarSessionLifecycle = {
			sessionId,
			placement,
			state: "connecting",
			createdAt: this.now(),
			metadata,
			vmIds: [],
		};
		const entry: AgentOsSidecarSessionEntry = {
			lifecycle,
			vms: new Map(),
		};
		this.sessions.set(sessionId, entry);

		try {
			entry.transport = await this.createSessionTransport({
				sessionId,
				placement: clonePlacement(placement),
				metadata: cloneMetadata(metadata),
				signal: options.signal,
			});
			entry.lifecycle.state = "ready";
			entry.lifecycle.connectedAt = this.now();
			return new AgentOsSidecarSessionHandle(this, sessionId);
		} catch (error) {
			entry.lifecycle.state = "failed";
			entry.lifecycle.lastError = toErrorMessage(error);
			throw toError(error);
		}
	}

	listSessions(): AgentOsSidecarSessionLifecycle[] {
		return [...this.sessions.values()].map((entry) =>
			cloneSessionLifecycle(entry.lifecycle),
		);
	}

	requireSessionLifecycle(sessionId: string): AgentOsSidecarSessionLifecycle {
		const entry = this.getSessionEntry(sessionId);
		return cloneSessionLifecycle(entry.lifecycle);
	}

	listVms(sessionId: string): AgentOsSidecarVmLifecycle[] {
		const entry = this.getSessionEntry(sessionId);
		return [...entry.vms.values()].map((vmEntry) =>
			cloneVmLifecycle(vmEntry.lifecycle),
		);
	}

	requireVmLifecycle(
		sessionId: string,
		vmId: string,
	): AgentOsSidecarVmLifecycle {
		const vmEntry = this.getVmEntry(sessionId, vmId);
		return cloneVmLifecycle(vmEntry.lifecycle);
	}

	async createVm(
		sessionId: string,
		options: AgentOsSidecarVmOptions = {},
	): Promise<AgentOsSidecarVmHandle> {
		this.assertActive();

		const entry = this.getSessionEntry(sessionId);
		if (entry.lifecycle.state !== "ready" || !entry.transport) {
			throw new Error(
				`Cannot create VM for sidecar session ${sessionId} while it is ${entry.lifecycle.state}`,
			);
		}

		const vmId = this.createId();
		const metadata = cloneMetadata(options.metadata);
		const vmEntry: AgentOsSidecarVmEntry = {
			lifecycle: {
				vmId,
				sessionId,
				state: "creating",
				createdAt: this.now(),
				metadata,
			},
		};
		entry.vms.set(vmId, vmEntry);
		entry.lifecycle.vmIds = [...entry.vms.keys()];

		try {
			await entry.transport.createVm?.({
				vmId,
				sessionId,
				metadata: cloneMetadata(metadata),
			});
			vmEntry.lifecycle.state = "ready";
			vmEntry.lifecycle.readyAt = this.now();
			return new AgentOsSidecarVmHandle(this, sessionId, vmId);
		} catch (error) {
			vmEntry.lifecycle.state = "failed";
			vmEntry.lifecycle.lastError = toErrorMessage(error);
			throw toError(error);
		}
	}

	async disposeVm(sessionId: string, vmId: string): Promise<void> {
		const sessionEntry = this.getSessionEntry(sessionId);
		const vmEntry = this.getVmEntry(sessionId, vmId);
		await this.disposeVmEntry(sessionEntry, vmEntry);
	}

	async disposeSession(sessionId: string): Promise<void> {
		const entry = this.getSessionEntry(sessionId);
		if (
			entry.lifecycle.state === "disposed" ||
			entry.lifecycle.state === "disposing"
		) {
			return;
		}

		entry.lifecycle.state = "disposing";

		const errors: Error[] = [];
		for (const vmEntry of entry.vms.values()) {
			try {
				await this.disposeVmEntry(entry, vmEntry);
			} catch (error) {
				errors.push(toError(error));
			}
		}

		try {
			await entry.transport?.dispose();
		} catch (error) {
			errors.push(toError(error));
		}

		if (errors.length > 0) {
			entry.lifecycle.state = "failed";
			entry.lifecycle.lastError = errors
				.map((error) => error.message)
				.join("; ");
			throw new Error(entry.lifecycle.lastError);
		}

		entry.lifecycle.state = "disposed";
		entry.lifecycle.disposedAt = this.now();
	}

	async dispose(): Promise<void> {
		if (this.disposed) {
			return;
		}

		const errors: Error[] = [];
		for (const sessionId of this.sessions.keys()) {
			try {
				await this.disposeSession(sessionId);
			} catch (error) {
				errors.push(toError(error));
			}
		}

		this.disposed = true;

		if (errors.length > 0) {
			throw new Error(errors.map((error) => error.message).join("; "));
		}
	}

	private async disposeVmEntry(
		sessionEntry: AgentOsSidecarSessionEntry,
		vmEntry: AgentOsSidecarVmEntry,
	): Promise<void> {
		if (
			vmEntry.lifecycle.state === "disposed" ||
			vmEntry.lifecycle.state === "disposing"
		) {
			return;
		}

		vmEntry.lifecycle.state = "disposing";
		try {
			await sessionEntry.transport?.disposeVm?.(vmEntry.lifecycle.vmId);
			vmEntry.lifecycle.state = "disposed";
			vmEntry.lifecycle.disposedAt = this.now();
		} catch (error) {
			vmEntry.lifecycle.state = "failed";
			vmEntry.lifecycle.lastError = toErrorMessage(error);
			throw toError(error);
		}
	}

	private getSessionEntry(sessionId: string): AgentOsSidecarSessionEntry {
		const entry = this.sessions.get(sessionId);
		if (!entry) {
			throw new Error(`Unknown sidecar session: ${sessionId}`);
		}
		return entry;
	}

	private getVmEntry(sessionId: string, vmId: string): AgentOsSidecarVmEntry {
		const entry = this.getSessionEntry(sessionId);
		const vmEntry = entry.vms.get(vmId);
		if (!vmEntry) {
			throw new Error(`Unknown sidecar VM ${vmId} for session ${sessionId}`);
		}
		return vmEntry;
	}

	private assertActive(): void {
		if (this.disposed) {
			throw new Error("Agent OS sidecar client has already been disposed");
		}
	}
}

export function createAgentOsSidecarClient(
	options: AgentOsSidecarClientOptions,
): AgentOsSidecarClient {
	return new AgentOsSidecarClient(options);
}

export type MountConfigJsonValue =
	| string
	| number
	| boolean
	| null
	| MountConfigJsonObject
	| MountConfigJsonValue[];

export interface MountConfigJsonObject {
	[key: string]: MountConfigJsonValue;
}

export interface SidecarMountPluginDescriptor {
	id: string;
	config: MountConfigJsonObject;
}

export interface SidecarMountDescriptor {
	guestPath: string;
	readOnly: boolean;
	plugin: SidecarMountPluginDescriptor;
}

export function serializeMountConfigForSidecar(
	mount: PlainMountConfig | NativeMountConfig,
): SidecarMountDescriptor {
	if ("driver" in mount) {
		return {
			guestPath: mount.path,
			readOnly: mount.readOnly ?? false,
			plugin: {
				id: "js_bridge",
				config: {},
			},
		};
	}

	return {
		guestPath: mount.path,
		readOnly: mount.readOnly ?? false,
		plugin: {
			id: mount.plugin.id,
			config: mount.plugin.config ?? {},
		},
	};
}

export interface SidecarRootFilesystemDescriptor {
	mode: "ephemeral" | "read_only";
	disableDefaultBaseLayer: boolean;
	lowers: SidecarRootFilesystemLowerDescriptor[];
	bootstrapEntries: SidecarRootFilesystemEntry[];
}

export interface SidecarRootFilesystemLowerDescriptor {
	kind: "snapshot" | "bundled_base_filesystem";
	entries?: SidecarRootFilesystemEntry[];
}

export interface SidecarRootFilesystemEntry {
	path: string;
	kind: "file" | "directory" | "symlink";
	mode?: number;
	uid?: number;
	gid?: number;
	content?: string;
	encoding?: "utf8" | "base64";
	target?: string;
	executable: boolean;
}

export function serializeRootFilesystemForSidecar(
	config?: RootFilesystemConfig,
	bootstrapLower?: RootSnapshotExport | null,
): SidecarRootFilesystemDescriptor {
	const lowerInputs = [
		...(config?.lowers ?? []),
		...(bootstrapLower ? [bootstrapLower] : []),
	];

	return {
		mode: config?.mode === "read-only" ? "read_only" : "ephemeral",
		disableDefaultBaseLayer: config?.disableDefaultBaseLayer ?? false,
		lowers: lowerInputs.map(serializeRootLowerForSidecar),
		bootstrapEntries: [],
	};
}

function clonePlacement(
	placement: AgentOsSidecarPlacement | undefined,
): AgentOsSidecarPlacement {
	if (!placement || placement.kind === "shared") {
		return {
			kind: "shared",
			...(placement?.pool ? { pool: placement.pool } : {}),
		};
	}

	return {
		kind: "explicit",
		sidecarId: placement.sidecarId,
	};
}

function cloneMetadata(
	metadata: Record<string, string> | undefined,
): Record<string, string> {
	return { ...(metadata ?? {}) };
}

function cloneSessionLifecycle(
	lifecycle: AgentOsSidecarSessionLifecycle,
): AgentOsSidecarSessionLifecycle {
	return {
		...lifecycle,
		placement: clonePlacement(lifecycle.placement),
		metadata: cloneMetadata(lifecycle.metadata),
		vmIds: [...lifecycle.vmIds],
	};
}

function cloneVmLifecycle(
	lifecycle: AgentOsSidecarVmLifecycle,
): AgentOsSidecarVmLifecycle {
	return {
		...lifecycle,
		metadata: cloneMetadata(lifecycle.metadata),
	};
}

function serializeRootLowerForSidecar(
	lower: RootLowerInput,
): SidecarRootFilesystemLowerDescriptor {
	if (lower.kind === "bundled-base-filesystem") {
		return {
			kind: "bundled_base_filesystem",
		};
	}

	return {
		kind: "snapshot",
		entries: lower.source.filesystem.entries.map(
			serializeFilesystemEntryForSidecar,
		),
	};
}

function serializeFilesystemEntryForSidecar(
	entry: FilesystemEntry,
): SidecarRootFilesystemEntry {
	const mode = Number.parseInt(entry.mode, 8);
	return {
		path: entry.path,
		kind: entry.type,
		mode,
		uid: entry.uid,
		gid: entry.gid,
		content: entry.content,
		encoding: entry.encoding,
		target: entry.target,
		executable: entry.type === "file" && (mode & 0o111) !== 0,
	};
}

function toError(error: unknown): Error {
	return error instanceof Error ? error : new Error(String(error));
}

function toErrorMessage(error: unknown): string {
	return toError(error).message;
}
