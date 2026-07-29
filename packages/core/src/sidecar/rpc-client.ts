import { randomUUID } from "node:crypto";
import { rmSync } from "node:fs";
import { constants as osConstants } from "node:os";
import { posix as posixPath } from "node:path";
import type {
	RootFilesystemConfig as VmConfigRootFilesystemConfig,
	RootFilesystemEntry as VmConfigRootFilesystemEntry,
	RootFilesystemLowerDescriptor as VmConfigRootFilesystemLowerDescriptor,
} from "@rivet-dev/agentos-runtime-core/vm-config";
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
	SidecarPermissionsPolicy,
	SidecarProcess,
	SidecarProcessSnapshotEntry,
	SidecarSignalHandlerRegistration,
	SidecarSocketStateEntry,
} from "./native-process-client.js";

const SYNTHETIC_PID_BASE = 1_000_000;
const MISSING_EXIT_EVENT_GRACE_MS = 500;
const PROTECTED_READ_ONLY_GUEST_ROOTS = ["/etc/agentos"] as const;
const TRAILING_OUTPUT_DRAIN_INTERVAL_MS = 10;
const TRAILING_OUTPUT_DRAIN_MAX_MS = 250;
const TRAILING_OUTPUT_DRAIN_QUIET_TURNS = 2;

function shouldLogStructuredSidecarEvent(name: string): boolean {
	const normalized = name.toLowerCase();
	return (
		normalized === "limit_warning" ||
		normalized.startsWith("security.") ||
		normalized.includes("warning") ||
		normalized.includes("failed") ||
		normalized.includes("error")
	);
}

function formatStructuredSidecarDetail(
	detail: Readonly<Record<string, string>>,
): string {
	const entries = Object.entries(detail);
	if (entries.length === 0) {
		return "";
	}
	return entries
		.map(([key, value]) => `${key}=${JSON.stringify(value)}`)
		.join(" ");
}

function logStructuredSidecarEvent(
	name: string,
	detail: Readonly<Record<string, string>>,
): void {
	if (!shouldLogStructuredSidecarEvent(name)) {
		return;
	}
	const formatted = formatStructuredSidecarDetail(detail);
	console.warn(
		formatted
			? `[agent-os] sidecar ${name}: ${formatted}`
			: `[agent-os] sidecar ${name}`,
	);
}

async function drainTrailingProcessOutputTurn(delayMs = 0): Promise<void> {
	// Native-sidecar `process_output` events can lag one macrotask behind the
	// terminal `process_exited` notification for very short-lived processes, and
	// under suite load the sidecar event pump can need a little extra time to
	// flush delayed output through its listener callbacks.
	await new Promise<void>((resolve) => {
		setTimeout(resolve, delayMs);
	});
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
const DOUBLE_QUOTE_ESCAPABLE_CHARACTERS = new Set(['"', "\\", "$", "`"]);
function appendDoubleQuotedEscape(current: string, character: string): string {
	if (DOUBLE_QUOTE_ESCAPABLE_CHARACTERS.has(character)) {
		return current + character;
	}
	if (character === "\n") {
		return current;
	}
	return `${current}\\${character}`;
}

function parseSimpleExecCommand(command: string): string[] | null {
	const tokens: string[] = [];
	let current = "";
	let quote: "'" | '"' | null = null;
	let escaped = false;

	for (const character of command) {
		if (quote === null) {
			if (escaped) {
				current += character;
				escaped = false;
				continue;
			}
			if (character === "\\") {
				escaped = true;
				continue;
			}
			if (character === "'" || character === '"') {
				quote = character;
				continue;
			}
			if (/\s/.test(character)) {
				if (current) {
					tokens.push(current);
					current = "";
				}
				continue;
			}
			if ("|&;<>()$`*?[]{}~!".includes(character)) {
				return null;
			}
			current += character;
			continue;
		}

		if (quote === "'") {
			if (character === "'") {
				quote = null;
				continue;
			}
			current += character;
			continue;
		}

		if (escaped) {
			current = appendDoubleQuotedEscape(current, character);
			escaped = false;
			continue;
		}
		if (character === "\\") {
			escaped = true;
			continue;
		}
		if (character === '"') {
			quote = null;
			continue;
		}
		if (character === "$" || character === "`") {
			return null;
		}
		current += character;
	}

	if (quote !== null || escaped) {
		return null;
	}
	if (current) {
		tokens.push(current);
	}
	if (tokens.length === 0) {
		return null;
	}
	if (tokens.some((token) => token.length === 0)) {
		return null;
	}
	return tokens;
}

function canUseDirectExec(
	driver: string | undefined,
	commandName: string | undefined,
): boolean {
	return (
		driver === "wasmvm" ||
		(driver === "node" && commandName === "node") ||
		(driver === "python" &&
			(commandName === "python" || commandName === "python3"))
	);
}

function shellSingleQuote(value: string): string {
	if (value.length === 0) {
		return "''";
	}
	return `'${value.replace(/'/g, `'"'"'`)}'`;
}

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
	sidecarMount?: SidecarMountDescriptor;
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
	waitWithFallbackPromise: Promise<number> | null;
	hostExitObservedAt: number | null;
	outputGeneration: number;
}

interface NativeSidecarKernelProxyOptions {
	client: SidecarProcess;
	session: AuthenticatedSession;
	vm: CreatedVm;
	env: Record<string, string>;
	cwd: string;
	defaultExecCwd?: string;
	localMounts: LocalCompatMount[];
	sidecarMounts: SidecarMountDescriptor[];
	permissions?: SidecarPermissionsPolicy;
	commandPermissions?: Parameters<
		SidecarProcess["configureVm"]
	>[2]["commandPermissions"];
	loopbackExemptPorts?: number[];
	/**
	 * The boot `configureVm` payload pieces beyond mounts/permissions. Rust
	 * `configure_vm` rebuilds the whole VM configuration from each payload, so
	 * every runtime mount reconfigure must resend these or a post-boot
	 * `mountFs()` silently drops the `/opt/agentos` package projections and
	 * binding shim commands applied at boot.
	 */
	packages?: Parameters<SidecarProcess["configureVm"]>[2]["packages"];
	packagesMountAt?: string;
	bindingShimCommands?: string[];
	commandGuestPaths: ReadonlyMap<string, string>;
	onWasmCommandResolved?: (command: string) => void;
	onDispose?: () => Promise<void>;
	/**
	 * Whether this proxy owns the underlying sidecar process. When VMs share one
	 * sidecar process (the default for VMs leased from an `AgentOsSidecar`
	 * handle), each proxy tears down only its own VM on `dispose()`; the shared
	 * process is disposed when the sidecar handle is disposed. Defaults to true
	 * for the legacy one-process-per-VM path.
	 */
	ownsClient?: boolean;
}

export class NativeSidecarKernelProxy {
	readonly env: Record<string, string>;
	readonly cwd: string;
	readonly commands: ReadonlyMap<string, string>;
	readonly vfs: VirtualFileSystem;
	readonly processes = new Map<number, ProcessInfo>();
	private readonly defaultExecCwd: string | undefined;

	private readonly client: SidecarProcess;
	private readonly session: AuthenticatedSession;
	private readonly vm: CreatedVm;
	private readonly ownsClient: boolean;
	private readonly localMounts: LocalCompatMount[];
	private readonly baseSidecarMounts: SidecarMountDescriptor[];
	private readonly dynamicSidecarMounts: SidecarMountDescriptor[] = [];
	private readonly permissions: SidecarPermissionsPolicy | undefined;
	private readonly commandPermissions:
		| Parameters<SidecarProcess["configureVm"]>[2]["commandPermissions"]
		| undefined;
	private readonly loopbackExemptPorts: number[] | undefined;
	// Mutable: runtime `linkSoftware` appends via `registerLinkedPackage` so
	// later mount reconfigures resend linked packages too, not just boot ones.
	private packages: NonNullable<
		Parameters<SidecarProcess["configureVm"]>[2]["packages"]
	>;
	private readonly packagesMountAt: string | undefined;
	private readonly bindingShimCommands: string[] | undefined;
	private readonly commandDrivers: Map<string, string>;
	private readonly onWasmCommandResolved:
		| ((command: string) => void)
		| undefined;
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
	private mountReconfigurePromise: Promise<void> | null = null;
	private nextSyntheticPid = SYNTHETIC_PID_BASE;
	private readonly eventPumpAbortController = new AbortController();
	private readonly eventPump: Promise<void>;

	constructor(options: NativeSidecarKernelProxyOptions) {
		this.client = options.client;
		this.session = options.session;
		this.vm = options.vm;
		this.ownsClient = options.ownsClient ?? true;
		this.env = { ...options.env };
		this.cwd = options.cwd;
		this.defaultExecCwd = options.defaultExecCwd;
		this.localMounts = [...options.localMounts].sort(
			(left, right) => right.path.length - left.path.length,
		);
		const localMountPaths = new Set(
			this.localMounts.map((mount) => mount.path),
		);
		this.baseSidecarMounts = options.sidecarMounts.filter(
			(mount) =>
				mount.plugin.id !== "js_bridge" ||
				!localMountPaths.has(posixPath.normalize(mount.guestPath)),
		);
		this.permissions = options.permissions;
		this.commandPermissions = options.commandPermissions;
		this.loopbackExemptPorts = options.loopbackExemptPorts;
		this.packages = options.packages ? [...options.packages] : [];
		this.packagesMountAt = options.packagesMountAt;
		this.bindingShimCommands = options.bindingShimCommands;
		this.commandDrivers = buildCommandMap(options.commandGuestPaths);
		this.onWasmCommandResolved = options.onWasmCommandResolved;
		this.onDispose = options.onDispose;
		this.commands = this.commandDrivers;
		this.vfs = this.createFilesystemView(true);
		this.rootView = this.createFilesystemView(false);
		this.eventPump = this.runEventPump();
		void this.eventPump.catch(() => {});
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

	/**
	 * Record a runtime-linked package (`linkSoftware`) so mount reconfigures
	 * resend it. Rust `configure_vm` rebuilds the whole VM configuration from
	 * each payload, so a linked package omitted here would be silently
	 * unprojected from `/opt/agentos` by the next `mountFs`/`unmountFs`.
	 */
	registerLinkedPackage(descriptor: { path: string }): void {
		if (!this.packages.some((pkg) => pkg.path === descriptor.path)) {
			this.packages.push({ path: descriptor.path });
		}
	}

	async dispose(): Promise<void> {
		if (this.disposed) {
			return;
		}
		this.disposed = true;
		this.eventPumpAbortController.abort();
		await this.mountReconfigurePromise?.catch(() => {});

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
		// Only tear down the shared sidecar process when this proxy owns it. VMs
		// leased from an `AgentOsSidecar` handle share one process, which is
		// disposed when the handle is disposed.
		if (this.ownsClient) {
			await this.client.dispose().catch(() => {});
		}
		await this.eventPump.catch(() => {});
		await this.onDispose?.().catch(() => {});

		// Drop all per-VM tracking state so a disposed proxy retains nothing.
		for (const entry of this.trackedProcesses.values()) {
			entry.onStdout.clear();
			entry.onStderr.clear();
		}
		this.trackedProcesses.clear();
		this.trackedProcessesById.clear();
		this.signalStates.clear();
		this.signalRefreshes.clear();
		this.localMounts.length = 0;
	}

	/** Test-only snapshot of the per-VM tracking collection sizes. */
	__trackingSizesForTest(): {
		trackedProcesses: number;
		trackedProcessesById: number;
		signalStates: number;
		signalRefreshes: number;
		localMounts: number;
	} {
		return {
			trackedProcesses: this.trackedProcesses.size,
			trackedProcessesById: this.trackedProcessesById.size,
			signalStates: this.signalStates.size,
			signalRefreshes: this.signalRefreshes.size,
			localMounts: this.localMounts.length,
		};
	}

	/** Test-only handle to a tracked entry so its listener Sets can be inspected. */
	__trackedEntryForTest(
		pid: number,
	):
		| { onStdout: ReadonlySet<unknown>; onStderr: ReadonlySet<unknown> }
		| undefined {
		return this.trackedProcesses.get(pid);
	}

	/** Test-only join point for in-flight signal-state refreshes. */
	async __awaitSignalRefreshesForTest(): Promise<void> {
		await Promise.allSettled([...this.signalRefreshes.values()]);
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
		const effectiveCwd = options?.cwd ?? this.defaultExecCwd ?? this.cwd;
		const parsedCommand = parseSimpleExecCommand(command);
		const runAndCapture = async (
			proc: ManagedProcess,
			stdinOverride?: string | Uint8Array,
			readExitCode?: () => Promise<number>,
		): Promise<KernelExecResult> => {
			if (stdinOverride !== undefined) {
				await proc.writeStdin(stdinOverride);
			} else if (options?.stdin !== undefined) {
				await proc.writeStdin(options.stdin);
			}
			// `kernel.exec()` is a non-interactive run-to-completion API: when the
			// caller does not opt into a streaming stdin handle, the guest process
			// should observe EOF after any provided input so commands like
			// `node -e ...` do not linger behind an inherited open stdin pipe.
			await proc.closeStdin();

			const waitPromise = proc.wait();
			const shellExitCode =
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

			const exitCode = readExitCode
				? await readExitCode().catch(() => shellExitCode)
				: shellExitCode;

			await drainTrailingProcessOutputTurn();

			return {
				exitCode,
				stdout: Buffer.concat(
					stdoutChunks.map((chunk) => Buffer.from(chunk)),
				).toString("utf8"),
				stderr: Buffer.concat(
					stderrChunks.map((chunk) => Buffer.from(chunk)),
				).toString("utf8"),
			};
		};
		const parsedCommandDriver = parsedCommand
			? this.commands.get(parsedCommand[0])
			: undefined;
		const requiresShellWrappedWasmCwd =
			parsedCommandDriver === "wasmvm" && parsedCommand?.[0] === "pwd";
		if (
			parsedCommand &&
			parsedCommandDriver &&
			canUseDirectExec(parsedCommandDriver, parsedCommand[0]) &&
			!requiresShellWrappedWasmCwd
		) {
			if (parsedCommandDriver === "wasmvm") {
				this.onWasmCommandResolved?.(parsedCommand[0]);
			}
			return runAndCapture(
				this.spawn(parsedCommand[0], parsedCommand.slice(1), {
					...options,
					cwd: effectiveCwd,
					onStdout: (chunk) => {
						stdoutChunks.push(chunk);
						options?.onStdout?.(chunk);
					},
					onStderr: (chunk) => {
						stderrChunks.push(chunk);
						options?.onStderr?.(chunk);
					},
				}),
			);
		}
		const proc = this.spawn("sh", ["-c", command], {
			...options,
			cwd: effectiveCwd,
			onStdout: (chunk) => {
				stdoutChunks.push(chunk);
				options?.onStdout?.(chunk);
			},
			onStderr: (chunk) => {
				stderrChunks.push(chunk);
				options?.onStderr?.(chunk);
			},
		});
		return runAndCapture(proc);
	}

	async execArgv(
		command: string,
		args: readonly string[] = [],
		options?: KernelExecOptions,
	): Promise<KernelExecResult> {
		const stdoutChunks: Uint8Array[] = [];
		const stderrChunks: Uint8Array[] = [];
		const effectiveCwd = options?.cwd ?? this.defaultExecCwd ?? this.cwd;
		const runAndCapture = async (
			proc: ManagedProcess,
		): Promise<KernelExecResult> => {
			if (options?.stdin !== undefined) {
				await proc.writeStdin(options.stdin);
			}
			await proc.closeStdin();

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

			await drainTrailingProcessOutputTurn();

			return {
				exitCode,
				stdout: Buffer.concat(
					stdoutChunks.map((chunk) => Buffer.from(chunk)),
				).toString("utf8"),
				stderr: Buffer.concat(
					stderrChunks.map((chunk) => Buffer.from(chunk)),
				).toString("utf8"),
			};
		};

		if (this.commands.get(command) === "wasmvm") {
			this.onWasmCommandResolved?.(command);
		}

		return runAndCapture(
			this.spawn(command, [...args], {
				...options,
				cwd: effectiveCwd,
				onStdout: (chunk) => {
					stdoutChunks.push(chunk);
					options?.onStdout?.(chunk);
				},
				onStderr: (chunk) => {
					stderrChunks.push(chunk);
					options?.onStderr?.(chunk);
				},
			}),
		);
	}

	spawn(
		command: string,
		args: string[],
		options?: KernelSpawnOptions,
	): ManagedProcess {
		let spawnCommand = command;
		let spawnArgs = [...args];
		const shellOption = (
			options as ({ shell?: unknown } & KernelSpawnOptions) | undefined
		)?.shell;
		if (shellOption === true || typeof shellOption === "string") {
			// Node's shell mode hands the raw command line to the shell. Shell
			// grammar belongs to the guest shell, so the bridge never parses it.
			if (!this.commands.has("sh")) {
				throw new Error(
					`native sidecar shell-mode spawn requires guest shell command 'sh': ${command}`,
				);
			}
			spawnCommand = "sh";
			spawnArgs = ["-c", [command, ...args].join(" ")];
		}
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
			command: spawnCommand,
			args: spawnArgs,
			driver:
				spawnCommand === "node"
					? "node"
					: spawnCommand === "python" || spawnCommand === "python3"
						? "python"
						: "wasmvm",
			cwd: options?.cwd ?? this.cwd,
			env: {
				...(options?.env ?? {}),
				...(options?.streamStdin ? { AGENTOS_KEEP_STDIN_OPEN: "1" } : {}),
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
			pendingStdin: options?.stdin === undefined ? [] : [options.stdin],
			stdinFlushPromise: null,
			pendingCloseStdin: !options?.streamStdin,
			pendingKillSignal: null,
			waitWithFallbackPromise: null,
			hostExitObservedAt: null,
			outputGeneration: 0,
		};
		this.trackedProcesses.set(pid, entry);
		this.trackedProcessesById.set(processId, entry);
		this.updateTrackedProcessSnapshot(entry);

		const proc: ManagedProcess = {
			pid,
			writeStdin: (data) => {
				if (entry.exitCode !== null) {
					return Promise.resolve();
				}
				entry.pendingStdin.push(data);
				return this.flushPendingStdin(entry).catch((error) => {
					this.handleBackgroundProcessError(entry, error);
				});
			},
			closeStdin: () => {
				entry.pendingCloseStdin = true;
				return this.closeTrackedStdin(entry).catch((error) => {
					this.handleBackgroundProcessError(entry, error);
				});
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
			wait: async () => {
				const exitCode = await this.waitForTrackedProcess(entry);
				await this.drainTrailingProcessOutput(entry);
				return exitCode;
			},
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
		const terminalHandlers = new Set<(data: Uint8Array) => void>();
		const stderrHandlers = new Set<(data: Uint8Array) => void>();
		const command = options?.command ?? "sh";
		const args =
			options?.args ??
			(command === "sh" || command === "/bin/sh" ? ["-i"] : []);
		const synthesizePrompt = !options?.command && !options?.args;
		const promptText = "sh-0.4$ ";
		const textEncoder = new TextEncoder();
		const textDecoder = new TextDecoder();
		const execCommand = this.exec.bind(this);
		const spawnCommand = this.spawn.bind(this);
		const sanitizeSyntheticShellText = (value: string) =>
			value
				.replace(/\u001b\[[0-9;]*m/g, "")
				.replace(/^.*WARN could not retrieve pid for child process\n?/gm, "")
				.replace(/^ProcessExitError:.*\n(?:\s+at .*\n)*/gm, "");
		const sanitizeNativeShellOutput = (chunk: Uint8Array) => {
			const text = textDecoder.decode(chunk);
			const sanitized = text.replace(
				/^.*WARN could not retrieve pid for child process\n?/gm,
				"",
			);
			return sanitized.length > 0 ? textEncoder.encode(sanitized) : null;
		};
		let bufferedInput = "";
		let bufferedCommand = "";
		let activeForegroundProcess: ManagedProcess | null = null;
		let shellEnv = { ...(options?.env ?? {}), AGENTOS_EXEC_TTY: "1" };
		let shellCwd = options?.cwd ?? this.cwd;
		let syntheticCommandQueue = Promise.resolve();
		let promptTimer: ReturnType<typeof setTimeout> | null = null;
		let commandInFlight = false;
		let syntheticCursorAtLineStart = true;
		const syntheticPid = this.nextSyntheticPid++;
		let syntheticExitCode: number | null = null;
		let resolveSyntheticWait!: (exitCode: number) => void;
		const syntheticWaitPromise = new Promise<number>((resolve) => {
			resolveSyntheticWait = resolve;
		});
		const clearPromptTimer = () => {
			if (promptTimer !== null) {
				clearTimeout(promptTimer);
				promptTimer = null;
			}
		};
		const normalizeSyntheticTerminalText = (text: string) =>
			text.replace(/\r?\n/g, "\r\n");
		const updateSyntheticCursor = (text: string) => {
			if (!text) {
				return;
			}
			syntheticCursorAtLineStart = /(?:\r\n)$/.test(text);
		};
		const emitSyntheticStdout = (text: string) => {
			if (!text) {
				return;
			}
			const normalized = normalizeSyntheticTerminalText(text);
			updateSyntheticCursor(normalized);
			const chunk = textEncoder.encode(normalized);
			for (const handler of terminalHandlers) {
				handler(chunk);
			}
		};
		const emitSyntheticTerminal = (text: string) => {
			if (!text) {
				return;
			}
			const normalized = normalizeSyntheticTerminalText(text);
			updateSyntheticCursor(normalized);
			const chunk = textEncoder.encode(normalized);
			for (const handler of terminalHandlers) {
				handler(chunk);
			}
		};
		const finishSyntheticShell = (exitCode: number) => {
			if (syntheticExitCode !== null) {
				return;
			}
			syntheticExitCode = exitCode;
			clearPromptTimer();
			resolveSyntheticWait(exitCode);
		};
		const commandNeedsContinuation = (source: string) => {
			let singleQuoted = false;
			let doubleQuoted = false;
			let escaped = false;
			for (const character of source) {
				if (escaped) {
					escaped = false;
					continue;
				}
				if (character === "\\") {
					escaped = true;
					continue;
				}
				if (!doubleQuoted && character === "'") {
					singleQuoted = !singleQuoted;
					continue;
				}
				if (!singleQuoted && character === '"') {
					doubleQuoted = !doubleQuoted;
				}
			}
			return singleQuoted || doubleQuoted || escaped;
		};
		const emitPrompt = () => {
			if (!synthesizePrompt) {
				return;
			}
			if (syntheticExitCode !== null) {
				return;
			}
			commandInFlight = false;
			const promptPrefix = syntheticCursorAtLineStart ? "" : "\r\n";
			const promptChunk = textEncoder.encode(`${promptPrefix}${promptText}`);
			for (const handler of terminalHandlers) {
				handler(promptChunk);
			}
			syntheticCursorAtLineStart = false;
		};
		const schedulePrompt = (delayMs: number) => {
			if (!synthesizePrompt) {
				return;
			}
			clearPromptTimer();
			promptTimer = setTimeout(() => {
				promptTimer = null;
				emitPrompt();
			}, delayMs);
		};
		const parseForegroundCommand = (source: string) => {
			const parsed = parseSimpleExecCommand(source);
			const driver = parsed ? this.commands.get(parsed[0]) : undefined;
			if (
				!parsed ||
				!canUseDirectExec(driver, parsed[0]) ||
				(driver === "wasmvm" && parsed[0] === "pwd")
			) {
				return null;
			}
			return parsed;
		};
		const writeForegroundInput = async (
			proc: ManagedProcess,
			data: string | Uint8Array,
		) => {
			if (typeof data === "string") {
				for (const character of data) {
					await proc.writeStdin(character);
				}
				return;
			}
			for (const byte of data) {
				await proc.writeStdin(new Uint8Array([byte]));
			}
		};
		const appendSyntheticInput = (input: string) => {
			for (const character of input) {
				if (character === "\u0004") {
					continue;
				}
				if (character === "\b" || character === "\u007f") {
					if (bufferedInput.length > 0) {
						bufferedInput = bufferedInput.slice(0, -1);
						emitSyntheticTerminal("\b \b");
					}
					continue;
				}
				bufferedInput += character;
				if (character === "\n") {
					emitSyntheticTerminal("\n");
				} else if (character >= " ") {
					emitSyntheticTerminal(character);
				}
			}
		};

		let onData: ((data: Uint8Array) => void) | null = null;
		terminalHandlers.add((data) => onData?.(data));
		if (options?.onStderr) {
			stderrHandlers.add(options.onStderr);
		}
		if (synthesizePrompt) {
			schedulePrompt(0);
			return {
				pid: syntheticPid,
				async write(data) {
					if (syntheticExitCode !== null) {
						return;
					}
					if (activeForegroundProcess) {
						const rawText =
							typeof data === "string"
								? data
								: Buffer.from(data).toString("utf8");
						if (rawText.includes("\u0003")) {
							const [beforeInterrupt] = rawText.split("\u0003");
							if (beforeInterrupt) {
								await writeForegroundInput(
									activeForegroundProcess,
									beforeInterrupt,
								);
							}
							emitSyntheticTerminal("^C\n");
							activeForegroundProcess.kill(2);
							return;
						}
						await writeForegroundInput(activeForegroundProcess, data);
						return;
					}
					const rawText =
						typeof data === "string"
							? data
							: Buffer.from(data).toString("utf8");
					let text = rawText;
					if (rawText.includes("\u0003")) {
						const segments = rawText.split("\u0003");
						bufferedInput = "";
						bufferedCommand = "";
						for (let index = 0; index < segments.length - 1; index += 1) {
							emitSyntheticTerminal("^C\n");
							emitPrompt();
						}
						text = segments[segments.length - 1] ?? "";
					}
					if (
						text.includes("\u0004") &&
						bufferedInput.length === 0 &&
						bufferedCommand.length === 0
					) {
						finishSyntheticShell(0);
						return;
					}
					appendSyntheticInput(
						text.replace(/\r\n/g, "\n").replace(/\r/g, "\n"),
					);
					while (true) {
						const newlineIndex = bufferedInput.indexOf("\n");
						if (newlineIndex < 0) {
							break;
						}
						const line = bufferedInput
							.slice(0, newlineIndex)
							.replace(/\r$/, "");
						bufferedInput = bufferedInput.slice(newlineIndex + 1);
						const nextCommand = bufferedCommand
							? `${bufferedCommand}\n${line}`
							: line;
						if (commandNeedsContinuation(nextCommand)) {
							bufferedCommand = nextCommand;
							continue;
						}
						bufferedCommand = "";
						syntheticCommandQueue = syntheticCommandQueue
							.then(async () => {
								const trimmed = nextCommand.trim();
								if (!trimmed) {
									emitPrompt();
									return;
								}
								const exitMatch = trimmed.match(/^exit(?:\s+(-?\d+))?$/);
								if (exitMatch) {
									finishSyntheticShell(
										Number.parseInt(exitMatch[1] ?? "0", 10),
									);
									return;
								}
								const exportMatch = trimmed.match(
									/^export\s+([A-Za-z_][A-Za-z0-9_]*)=(.*)$/,
								);
								if (exportMatch) {
									shellEnv = {
										...shellEnv,
										[exportMatch[1]]: exportMatch[2],
									};
									emitPrompt();
									return;
								}
								const cdMatch = trimmed.match(/^cd(?:\s+(.*))?$/);
								if (cdMatch) {
									const target = cdMatch[1]?.trim() || "/";
									shellCwd = target.startsWith("/")
										? posixPath.normalize(target)
										: posixPath.normalize(posixPath.join(shellCwd, target));
									emitPrompt();
									return;
								}
								const foregroundCommand = parseForegroundCommand(trimmed);
								if (foregroundCommand) {
									const proc = spawnCommand(
										foregroundCommand[0],
										foregroundCommand.slice(1),
										{
											env: shellEnv,
											cwd: shellCwd,
											streamStdin: true,
											onStdout: (chunk) =>
												emitSyntheticTerminal(textDecoder.decode(chunk)),
											onStderr: (chunk) =>
												emitSyntheticTerminal(textDecoder.decode(chunk)),
										},
									);
									activeForegroundProcess = proc;
									try {
										await proc.wait();
									} finally {
										if (activeForegroundProcess === proc) {
											activeForegroundProcess = null;
										}
									}
									emitPrompt();
									return;
								}
								const result = await execCommand(nextCommand, {
									env: shellEnv,
									cwd: shellCwd,
								});
								const sanitizedStdout = sanitizeSyntheticShellText(
									result.stdout,
								);
								if (sanitizedStdout) {
									emitSyntheticStdout(sanitizedStdout);
								}
								const sanitizedStderr = sanitizeSyntheticShellText(
									result.stderr,
								).replace(
									/^error: failed to execute command '([^']+)': .*$/gm,
									"error: command not found: $1",
								);
								if (sanitizedStderr) {
									emitSyntheticTerminal(sanitizedStderr);
								}
								emitPrompt();
							})
							.catch((error) => {
								const message =
									error instanceof Error ? error.message : String(error);
								emitSyntheticTerminal(`${message}\n`);
								emitPrompt();
							});
					}
				},
				get onData() {
					return onData;
				},
				set onData(handler) {
					onData = handler;
				},
				resize() {
					// Synthetic shells are terminal-less.
				},
				kill(signal = 15) {
					finishSyntheticShell(128 + signal);
				},
				wait() {
					return syntheticWaitPromise;
				},
			};
		}

		const proc = this.spawn(command, args, {
			env: {
				...(options?.env ?? {}),
				...(options?.cols ? { COLUMNS: String(Math.trunc(options.cols)) } : {}),
				...(options?.rows ? { LINES: String(Math.trunc(options.rows)) } : {}),
				AGENTOS_EXEC_TTY: "1",
			},
			cwd: options?.cwd,
			streamStdin: true,
			onStdout: (chunk) => {
				const sanitized = sanitizeNativeShellOutput(chunk);
				if (!sanitized) {
					return;
				}
				for (const handler of terminalHandlers) {
					handler(sanitized);
				}
				if (commandInFlight) {
					schedulePrompt(120);
				}
			},
			onStderr: (chunk) => {
				const sanitized = sanitizeNativeShellOutput(chunk);
				if (!sanitized) {
					return;
				}
				// `onData` is the ordered PTY rendering stream. `onStderr` remains an
				// optional channel-specific diagnostic tap and must not also be rendered.
				for (const handler of terminalHandlers) {
					handler(sanitized);
				}
				for (const handler of stderrHandlers) {
					handler(sanitized);
				}
				if (commandInFlight) {
					schedulePrompt(120);
				}
			},
		});

		return {
			pid: proc.pid,
			async write(data) {
				if (synthesizePrompt) {
					return;
				}
				await proc.writeStdin(data);
				if (
					synthesizePrompt &&
					typeof data === "string" &&
					(data.includes("\n") || data.includes("\r"))
				) {
					commandInFlight = true;
					schedulePrompt(120);
				}
			},
			get onData() {
				return onData;
			},
			set onData(handler) {
				onData = handler;
			},
			resize: (cols, rows) => {
				const entry = this.trackedProcesses.get(proc.pid);
				if (!entry || entry.exitCode !== null) {
					return;
				}
				void entry.startPromise
					.then(() =>
						this.client.resizePty(
							this.session,
							this.vm,
							entry.processId,
							Math.trunc(cols),
							Math.trunc(rows),
						),
					)
					.catch((error) => {
						this.handleBackgroundProcessError(entry, error);
					});
			},
			kill(signal) {
				clearPromptTimer();
				proc.kill(signal);
			},
			wait() {
				clearPromptTimer();
				return proc.wait();
			},
		};
	}

	async connectTerminal(options?: ConnectTerminalOptions): Promise<number> {
		const stdin = process.stdin;
		const stdout = process.stdout;
		const { onData, ...shellOptions } = options ?? {};
		const shell = this.openShell(shellOptions);
		const outputHandler =
			onData ??
			((data: Uint8Array) => {
				stdout.write(data);
			});
		const restoreRawMode =
			stdin.isTTY && typeof stdin.setRawMode === "function";
		const onStdinData = (data: Uint8Array | string) => {
			void shell.write(data).catch((error) => {
				console.error("[agentos] failed to forward terminal stdin:", error);
			});
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

	async readdirRecursive(
		path: string,
		options?: { maxDepth?: number },
	): Promise<
		Array<{
			name: string;
			path: string;
			isDirectory: boolean;
			isSymbolicLink: boolean;
			size: number;
		}>
	> {
		const local = this.resolveLocalMount(path);
		if (local) {
			return this.readdirRecursiveLocal(
				local.mount.fs,
				local.relativePath,
				options?.maxDepth,
			);
		}
		return (
			await this.client.readdirRecursive(this.session, this.vm, path, options)
		).map((entry) => ({ ...entry, size: Number(entry.size) }));
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

	async removePath(
		path: string,
		options?: { recursive?: boolean },
	): Promise<void> {
		return this.dispatchWrite(
			path,
			(mount, relativePath) =>
				this.removePathLocal(
					mount.fs,
					relativePath,
					options?.recursive ?? false,
				),
			() =>
				this.client.removePath(this.session, this.vm, path, {
					recursive: options?.recursive ?? false,
				}),
		);
	}

	async copyPath(
		fromPath: string,
		toPath: string,
		options?: { recursive?: boolean },
	): Promise<void> {
		const from = this.resolveLocalMount(fromPath);
		const to = this.resolveLocalMount(toPath);
		if (!!from !== !!to) {
			throw errnoError("EXDEV", "cross-device link not permitted");
		}
		if (from && to) {
			if (from.mount.path !== to.mount.path) {
				throw errnoError("EXDEV", "cross-device link not permitted");
			}
			this.assertLocalWritable(to.mount);
			return this.copyPathLocal(
				from.mount.fs,
				from.relativePath,
				to.relativePath,
				options?.recursive ?? false,
			);
		}
		return this.client.copyPath(this.session, this.vm, fromPath, toPath, {
			recursive: options?.recursive ?? false,
		});
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

	async movePath(oldPath: string, newPath: string): Promise<void> {
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

		return this.client.movePath(this.session, this.vm, oldPath, newPath);
	}

	mountFs(
		path: string,
		driver: VirtualFileSystem,
		options?: { readOnly?: boolean; sidecarMount?: SidecarMountDescriptor },
	): Promise<void> {
		this.localMounts.unshift({
			path: posixPath.normalize(path),
			fs: driver,
			readOnly: options?.readOnly ?? false,
			sidecarMount: options?.sidecarMount,
		});
		this.localMounts.sort(
			(left, right) => right.path.length - left.path.length,
		);
		// Resolves once the sidecar has the mount; a swallowed rejection here
		// used to turn reconfigure failures into silently missing guest mounts.
		// The local catch only guards callers that drop the promise — awaiting
		// callers still observe the rejection.
		const applied = this.reconfigureSidecarMounts();
		applied.catch(() => {});
		return applied;
	}

	unmountFs(path: string): Promise<void> {
		const normalized = posixPath.normalize(path);
		const index = this.localMounts.findIndex(
			(mount) => mount.path === normalized,
		);
		if (index < 0) {
			return Promise.resolve();
		}
		this.localMounts.splice(index, 1);
		const applied = this.reconfigureSidecarMounts();
		applied.catch(() => {});
		return applied;
	}

	mountDescriptor(mount: SidecarMountDescriptor): Promise<void> {
		const normalized = posixPath.normalize(mount.guestPath);
		if (
			this.desiredSidecarMounts().some(
				(existing) => posixPath.normalize(existing.guestPath) === normalized,
			)
		) {
			return Promise.reject(new Error(`mount already exists: ${normalized}`));
		}
		this.dynamicSidecarMounts.push({ ...mount, guestPath: normalized });
		const applied = this.reconfigureSidecarMounts();
		applied.catch(() => {});
		return applied;
	}

	unmountDescriptor(path: string): Promise<void> {
		const normalized = posixPath.normalize(path);
		const index = this.dynamicSidecarMounts.findIndex(
			(mount) => posixPath.normalize(mount.guestPath) === normalized,
		);
		if (index < 0) return Promise.resolve();
		this.dynamicSidecarMounts.splice(index, 1);
		const applied = this.reconfigureSidecarMounts();
		applied.catch(() => {});
		return applied;
	}

	async listMounts(): Promise<
		Array<{ path: string; kind: string; readOnly: boolean }>
	> {
		await this.waitForMountReconfigure();
		return this.client.listMounts(this.session, this.vm);
	}

	private desiredSidecarMounts(): SidecarMountDescriptor[] {
		return [
			...this.baseSidecarMounts,
			...this.dynamicSidecarMounts,
			...this.localMounts.map(
				(mount) =>
					mount.sidecarMount ?? {
						guestPath: mount.path,
						readOnly: mount.readOnly,
						plugin: {
							id: "js_bridge",
							config: {},
						},
					},
			),
		];
	}

	private reconfigureSidecarMounts(): Promise<void> {
		const run = async () => {
			if (this.disposed) {
				return;
			}
			// Rust `configure_vm` rebuilds the whole VM configuration from this
			// payload, so resend the boot packages / binding shim commands too —
			// omitting them here strips the `/opt/agentos` projections and binding
			// shims from the VM as a side effect of a runtime mount change.
			await this.client.configureVm(this.session, this.vm, {
				mounts: this.desiredSidecarMounts(),
				permissions: this.permissions,
				commandPermissions: this.commandPermissions,
				loopbackExemptPorts: this.loopbackExemptPorts,
				packages: this.packages,
				packagesMountAt: this.packagesMountAt,
				bindingShimCommands: this.bindingShimCommands,
			});
		};
		const previous = this.mountReconfigurePromise ?? Promise.resolve();
		const next = previous.then(run, run);
		const tracked = next.finally(() => {
			if (this.mountReconfigurePromise === tracked) {
				this.mountReconfigurePromise = null;
			}
		});
		this.mountReconfigurePromise = tracked;
		return tracked;
	}

	private async waitForMountReconfigure(): Promise<void> {
		if (this.mountReconfigurePromise) {
			await this.mountReconfigurePromise;
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
		if (entry) this.scheduleSignalStateRefresh(entry);
		return this.signalStates.get(pid) ?? { handlers: new Map() };
	}

	private scheduleSignalStateRefresh(entry: TrackedProcessEntry): void {
		if (this.signalRefreshes.has(entry.pid)) return;
		const refresh = this.refreshSignalState(entry);
		this.signalRefreshes.set(entry.pid, refresh);
		void refresh.catch(() => {});
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

	private async drainTrailingProcessOutput(
		entry: TrackedProcessEntry,
	): Promise<void> {
		if (entry.onStdout.size === 0 && entry.onStderr.size === 0) {
			return;
		}

		let observedGeneration = entry.outputGeneration;
		let quietTurns = 0;
		let delayMs = 0;
		const deadline = Date.now() + TRAILING_OUTPUT_DRAIN_MAX_MS;

		while (quietTurns < TRAILING_OUTPUT_DRAIN_QUIET_TURNS) {
			const remainingMs = deadline - Date.now();
			if (remainingMs <= 0) {
				return;
			}

			await drainTrailingProcessOutputTurn(Math.min(delayMs, remainingMs));
			if (entry.outputGeneration === observedGeneration) {
				quietTurns += 1;
			} else {
				observedGeneration = entry.outputGeneration;
				quietTurns = 0;
			}
			delayMs = TRAILING_OUTPUT_DRAIN_INTERVAL_MS;
		}
	}

	private async startTrackedProcess(entry: TrackedProcessEntry): Promise<void> {
		await this.waitForMountReconfigure();
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
		// Signal metadata is advisory and must not hold process startup behind a
		// second sidecar RPC. Very short binding commands can finish while the
		// original execute response is still in flight; synchronously refreshing
		// here creates a circular wait with their output/exit delivery.
		this.scheduleSignalStateRefresh(entry);

		void this.flushPendingStdin(entry).catch((error) => {
			this.handleBackgroundProcessError(entry, error);
		});
		void this.closeTrackedStdin(entry).catch((error) => {
			this.handleBackgroundProcessError(entry, error);
		});

		if (entry.pendingKillSignal !== null) {
			const signal = entry.pendingKillSignal;
			entry.pendingKillSignal = null;
			await this.signalProcess(entry, signal);
		}
	}

	private async runEventPump(): Promise<void> {
		// Scope the pump to THIS VM's ownership so multiple proxies can share one
		// sidecar process: events for other VMs stay buffered for their own pumps
		// rather than being consumed (and dropped) here.
		const vmId = this.vm.vmId;
		while (!this.disposed) {
			try {
				const event = await this.client.waitForEvent(
					(frame) =>
						frame.ownership.scope === "vm" && frame.ownership.vm_id === vmId,
					undefined,
					{ signal: this.eventPumpAbortController.signal },
				);
				if (event.payload.type === "process_output") {
					const entry = this.trackedProcessesById.get(event.payload.process_id);
					if (!entry) {
						continue;
					}
					entry.outputGeneration += 1;
					void this.refreshProcessSnapshot().catch(() => {});
					this.scheduleSignalStateRefresh(entry);
					const chunk = event.payload.chunk;
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
					this.finishProcess(entry, event.payload.exit_code);
					continue;
				}

				if (event.payload.type === "structured") {
					logStructuredSidecarEvent(event.payload.name, event.payload.detail);
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
		// Release per-process tracking now that the process has terminated so these
		// maps/Sets don't grow without bound. Defer the release until trailing
		// output has drained: `wait()`'s drain and late `process_output` events
		// still need the entry + its listeners during the drain window. The exited
		// record lives on in `processes` for listing.
		void this.releaseProcessTrackingAfterDrain(entry);
	}

	private async releaseProcessTrackingAfterDrain(
		entry: TrackedProcessEntry,
	): Promise<void> {
		try {
			await this.drainTrailingProcessOutput(entry);
		} finally {
			this.trackedProcesses.delete(entry.pid);
			this.trackedProcessesById.delete(entry.processId);
			this.signalRefreshes.delete(entry.pid);
			this.signalStates.delete(entry.pid);
			entry.onStdout.clear();
			entry.onStderr.clear();
		}
	}

	private waitForTrackedProcess(entry: TrackedProcessEntry): Promise<number> {
		if (entry.exitCode !== null) {
			return Promise.resolve(entry.exitCode);
		}
		if (entry.waitWithFallbackPromise !== null) {
			return entry.waitWithFallbackPromise;
		}

		entry.waitWithFallbackPromise = (async () => {
			await entry.startPromise.catch(() => {});
			while (entry.exitCode === null && !this.disposed) {
				const maybeExit = await Promise.race<number | null>([
					entry.waitPromise.then((exitCode) => exitCode),
					new Promise<null>((resolve) => setTimeout(() => resolve(null), 50)),
				]);
				if (maybeExit !== null) {
					return maybeExit;
				}

				try {
					await this.refreshProcessSnapshot();
					const snapshot = this.sidecarProcessSnapshot.find(
						(candidate) => candidate.processId === entry.processId,
					);
					if (snapshot?.status === "exited") {
						this.finishProcess(entry, snapshot.exitCode ?? 0);
						break;
					}
					if (snapshot) {
						entry.hostExitObservedAt = null;
						continue;
					}

					// Fast guest processes can exit before the sidecar emits a
					// `process_exited` event. Once a started process disappears from the
					// authoritative VM snapshot for a full grace window, treat it as
					// reaped even if the `pid` returned at launch was only a kernel/shared
					// runtime identifier rather than a probeable host PID.
					if (!snapshot) {
						const now = Date.now();
						if (entry.hostExitObservedAt === null) {
							entry.hostExitObservedAt = now;
							continue;
						}
						if (now - entry.hostExitObservedAt >= MISSING_EXIT_EVENT_GRACE_MS) {
							this.finishProcess(entry, 0);
							break;
						}
					}
				} catch {
					// Fall back to the next wait interval if the sidecar snapshot query fails.
				}
			}

			return entry.waitPromise;
		})().finally(() => {
			entry.waitWithFallbackPromise = null;
		});

		return entry.waitWithFallbackPromise;
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
			if (isNoSuchProcessError(error) || isUnknownVmError(error)) {
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
			.catch((error) => {
				if (isNoSuchProcessError(error) || isUnknownVmError(error)) {
					return;
				}
				throw error;
			})
			.finally(() => {
				entry.stdinFlushPromise = null;
				if (entry.pendingStdin.length > 0 && entry.exitCode === null) {
					void this.flushPendingStdin(entry).catch((error) => {
						this.handleBackgroundProcessError(entry, error);
					});
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
			if (isNoSuchProcessError(error) || isUnknownVmError(error)) {
				return;
			}
			throw error;
		}
	}

	private handleBackgroundProcessError(
		entry: TrackedProcessEntry,
		error: unknown,
	): void {
		if (
			this.disposed ||
			isNoSuchProcessError(error) ||
			isUnknownVmError(error)
		) {
			return;
		}
		if (entry.exitCode !== null) {
			this.recordCompletedProcessError(entry, error);
			return;
		}
		this.emitBackgroundProcessError(entry, error);
		this.finishProcess(entry, 1);
	}

	private recordCompletedProcessError(
		entry: TrackedProcessEntry,
		error: unknown,
	): number {
		if (
			this.disposed ||
			isNoSuchProcessError(error) ||
			isUnknownVmError(error)
		) {
			return entry.exitCode ?? 1;
		}
		this.emitBackgroundProcessError(entry, error);
		entry.exitCode =
			entry.exitCode === null || entry.exitCode === 0 ? 1 : entry.exitCode;
		entry.exitTime ??= Date.now();
		this.updateTrackedProcessSnapshot(entry);
		return entry.exitCode;
	}

	private emitBackgroundProcessError(
		entry: TrackedProcessEntry,
		error: unknown,
	): void {
		const normalized =
			error instanceof Error ? error : new Error(String(error));
		const stderr = new TextEncoder().encode(`${normalized.message}\n`);
		for (const handler of entry.onStderr) {
			handler(stderr);
		}
	}

	private async readdirRecursiveLocal(
		fs: VirtualFileSystem,
		path: string,
		maxDepth: number | undefined,
	): Promise<
		Array<{
			name: string;
			path: string;
			isDirectory: boolean;
			isSymbolicLink: boolean;
			size: number;
		}>
	> {
		const results: Array<{
			name: string;
			path: string;
			isDirectory: boolean;
			isSymbolicLink: boolean;
			size: number;
		}> = [];
		const queue: Array<{ path: string; depth: number }> = [{ path, depth: 0 }];
		while (queue.length > 0) {
			const current = queue.shift();
			if (!current) break;
			for (const name of await fs.readDir(current.path)) {
				if (name === "." || name === "..") continue;
				const child = posixPath.join(current.path, name);
				const stat = await fs.lstat(child);
				results.push({
					name,
					path: child,
					isDirectory: stat.isDirectory,
					isSymbolicLink: stat.isSymbolicLink,
					size: stat.size,
				});
				if (
					stat.isDirectory &&
					!stat.isSymbolicLink &&
					(maxDepth === undefined || current.depth < maxDepth)
				) {
					queue.push({ path: child, depth: current.depth + 1 });
				}
			}
		}
		return results;
	}

	private async removePathLocal(
		fs: VirtualFileSystem,
		path: string,
		recursive: boolean,
	): Promise<void> {
		const stat = await fs.lstat(path);
		if (stat.isDirectory && !stat.isSymbolicLink) {
			if (recursive) {
				for (const name of await fs.readDir(path)) {
					if (name === "." || name === "..") continue;
					await this.removePathLocal(fs, posixPath.join(path, name), true);
				}
			}
			return fs.removeDir(path);
		}
		return fs.removeFile(path);
	}

	private async copyPathLocal(
		fs: VirtualFileSystem,
		fromPath: string,
		toPath: string,
		recursive: boolean,
	): Promise<void> {
		const stat = await fs.lstat(fromPath);
		if (stat.isSymbolicLink) {
			return fs.symlink(await fs.readlink(fromPath), toPath);
		}
		if (stat.isDirectory) {
			if (!recursive) {
				throw errnoError("EISDIR", "illegal operation on a directory");
			}
			await fs.mkdir(posixPath.dirname(toPath), { recursive: true });
			if (!(await fs.exists(toPath))) {
				await fs.createDir(toPath);
			}
			await fs.chmod(toPath, stat.mode);
			await fs.chown(toPath, stat.uid, stat.gid);
			for (const name of await fs.readDir(fromPath)) {
				if (name === "." || name === "..") continue;
				await this.copyPathLocal(
					fs,
					posixPath.join(fromPath, name),
					posixPath.join(toPath, name),
					true,
				);
			}
			return;
		}
		await fs.writeFile(toPath, await fs.readFile(fromPath));
		await fs.chmod(toPath, stat.mode);
		await fs.chown(toPath, stat.uid, stat.gid);
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
					async () => {
						try {
							await this.client.mkdir(this.session, this.vm, path, {
								recursive: false,
							});
						} catch (error) {
							if (!isAlreadyExistsError(error)) {
								throw error;
							}
						}
					},
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
				const local = includeLocalMounts ? this.resolveLocalMount(path) : null;
				if (local) {
					return local.mount.fs.pread(local.relativePath, offset, length);
				}
				return this.client.pread(this.session, this.vm, path, offset, length);
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
							: entry.status === "exited"
								? "exited"
								: "running",
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

	private async dispatchNativeRead(path: string): Promise<Uint8Array> {
		await this.waitForMountReconfigure();
		return this.client.readFile(this.session, this.vm, path);
	}

	private async dispatchWrite(
		path: string,
		handler: (mount: LocalCompatMount, relativePath: string) => Promise<void>,
		nativeHandler: () => Promise<void>,
		includeLocalMounts = true,
	): Promise<void> {
		this.assertGuestPathWritable(path);
		const local = includeLocalMounts ? this.resolveLocalMount(path) : null;
		if (local) {
			this.assertLocalWritable(local.mount);
			await handler(local.mount, local.relativePath);
			return;
		}
		await this.waitForMountReconfigure();
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

	private assertGuestPathWritable(path: string): void {
		const normalizedPath = posixPath.normalize(path);
		for (const root of PROTECTED_READ_ONLY_GUEST_ROOTS) {
			if (normalizedPath === root || normalizedPath.startsWith(`${root}/`)) {
				throw errnoError("EROFS", "read-only file system");
			}
		}
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
		// `python` / `python3` are served by the embedded Pyodide runtime,
		// mirroring how `node` is served by the embedded V8 runtime.
		["python", "python"],
		["python3", "python"],
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

function isUnknownVmError(error: unknown): boolean {
	if (!(error instanceof Error)) {
		return false;
	}
	return error.message.toLowerCase().includes("unknown sidecar vm");
}

function isAlreadyExistsError(error: unknown): boolean {
	if (!(error instanceof Error)) {
		return false;
	}
	const message = error.message.toLowerCase();
	return error.message.includes("EEXIST") || message.includes("file exists");
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

// VirtualStat is a numeric, Node-default-shaped view: u64 fields above
// Number.MAX_SAFE_INTEGER lose precision here, same as Node's non-bigint
// fs.stat on the host.
function toVirtualStat(stat: GuestFilesystemStat): VirtualStat {
	return {
		mode: stat.mode,
		size: Number(stat.size),
		sizeExact: stat.size,
		blocks: Number(stat.blocks),
		dev: Number(stat.dev),
		rdev: Number(stat.rdev),
		isDirectory: stat.is_directory,
		isSymbolicLink: stat.is_symbolic_link,
		atimeMs: stat.atime_ms,
		mtimeMs: stat.mtime_ms,
		ctimeMs: stat.ctime_ms,
		birthtimeMs: stat.birthtime_ms,
		ino: Number(stat.ino),
		inoExact: stat.ino,
		nlink: Number(stat.nlink),
		nlinkExact: stat.nlink,
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
	RootFilesystemEntry,
	SidecarConfigureVmResult,
	SidecarEventSelector,
	SidecarLinkPackageResult,
	SidecarPermissionsPolicy,
	SidecarProjectedAgent,
	SidecarRegisteredHostCallbackDefinition,
	SidecarRequestFrame,
	SidecarResponsePayload,
	SidecarSessionState,
	SidecarSignalHandlerRegistration,
	SidecarSocketStateEntry,
	SidecarSpawnOptions,
} from "./native-process-client.js";
export {
	NativeSidecarProcessClient,
	SidecarEventBufferOverflow,
	SidecarProcess,
	SidecarProcessError,
	SidecarProcessExited,
} from "./native-process-client.js";

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
	createOwnershipTransport(
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
	private readonly createOwnershipTransport: AgentOsSidecarClientOptions["createOwnershipTransport"];
	private readonly createId: () => string;
	private readonly now: () => number;
	private readonly sessions = new Map<string, AgentOsSidecarSessionEntry>();
	private disposed = false;

	constructor(options: AgentOsSidecarClientOptions) {
		this.createOwnershipTransport = options.createOwnershipTransport;
		this.createId = options.createId ?? randomUUID;
		this.now = options.now ?? Date.now;
	}

	/** Open a physical sidecar ownership scope, not a durable ACP session. */
	async createOwnershipSession(
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
			entry.transport = await this.createOwnershipTransport({
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
	guestSource?: string;
	guestFstype?: string;
	readOnly: boolean;
	plugin: SidecarMountPluginDescriptor;
}

export function serializeMountConfigForSidecar(
	mount: PlainMountConfig | NativeMountConfig,
): SidecarMountDescriptor {
	if ("driver" in mount) {
		return {
			guestPath: mount.path,
			guestSource: mount.guestSource ?? "js_bridge",
			guestFstype: mount.guestFstype ?? "js_bridge",
			readOnly: mount.readOnly ?? false,
			plugin: {
				id: "js_bridge",
				config: {},
			},
		};
	}

	return {
		guestPath: mount.path,
		guestSource: mount.guestSource ?? mount.plugin.id,
		guestFstype: mount.guestFstype ?? mount.plugin.id,
		readOnly: mount.readOnly ?? false,
		plugin: {
			id: mount.plugin.id,
			config: mount.plugin.config ?? {},
		},
	};
}

export type SidecarRootFilesystemDescriptor = VmConfigRootFilesystemConfig;
export type SidecarRootFilesystemLowerDescriptor =
	VmConfigRootFilesystemLowerDescriptor;
export type SidecarRootFilesystemEntry = VmConfigRootFilesystemEntry;

export function serializeRootFilesystemForSidecar(
	config?: RootFilesystemConfig,
	bootstrapLower?: RootSnapshotExport | null,
): SidecarRootFilesystemDescriptor {
	if (config?.type === "native") {
		return {
			mode: "ephemeral",
			disableDefaultBaseLayer: true,
			lowers: [],
			bootstrapEntries: [],
		};
	}
	const lowerInputs = [
		...(config?.lowers ?? []),
		...(bootstrapLower ? [bootstrapLower] : []),
	];

	return {
		mode: config?.mode === "read-only" ? "read-only" : "ephemeral",
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
			kind: "bundledBaseFilesystem",
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
