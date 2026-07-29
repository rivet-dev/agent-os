import { randomUUID } from "node:crypto";
import { rmSync } from "node:fs";
import { constants as osConstants } from "node:os";
import { posix as posixPath } from "node:path";
import type { NativeMountPluginDescriptor } from "./descriptors.js";
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
} from "./test-runtime.js";
import type {
	AuthenticatedSession,
	CreatedVm,
	GuestFilesystemStat,
	SidecarProcess,
	SidecarMountDescriptor,
	SidecarProcessSnapshotEntry,
	SidecarResourceSnapshot,
	SidecarSignalHandlerRegistration,
	SidecarSocketStateEntry,
} from "./sidecar-process.js";

export interface PlainMountConfig {
	path: string;
	driver: VirtualFileSystem;
	guestSource?: string;
	guestFstype?: string;
	readOnly?: boolean;
}

export interface NativeMountConfig {
	path: string;
	plugin: NativeMountPluginDescriptor;
	guestSource?: string;
	guestFstype?: string;
	readOnly?: boolean;
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

const SYNTHETIC_PID_BASE = 1_000_000;
const MISSING_EXIT_EVENT_GRACE_MS = 500;
const PROTECTED_READ_ONLY_GUEST_ROOTS = ["/etc/agentos"] as const;
const TRAILING_OUTPUT_DRAIN_INTERVAL_MS = 10;
const TRAILING_OUTPUT_DRAIN_MAX_MS = 250;
const TRAILING_OUTPUT_DRAIN_QUIET_TURNS = 2;

async function drainTrailingProcessOutputTurn(
	delayMs = 0,
): Promise<void> {
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
const NON_TERMINATING_SIGNALS = new Set([
	"0",
	"SIGCHLD",
	"SIGCONT",
	"SIGSTOP",
	"SIGURG",
	"SIGWINCH",
]);
const LOCALLY_TERMINATING_SIGNALS = new Set(["SIGKILL", "SIGTERM"]);
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
	return driver === "wasmvm" || (driver === "node" && commandName === "node");
}

function shellSingleQuote(value: string): string {
	if (value.length === 0) {
		return "''";
	}
	return `'${value.replace(/'/g, `'\"'\"'`)}'`;
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
	exitViaEvent: boolean;
}

interface NativeSidecarKernelProxyOptions {
	client: SidecarProcess;
	session: AuthenticatedSession;
	vm: CreatedVm;
	disposeClient?: boolean;
	env: Record<string, string>;
	cwd: string;
	defaultExecCwd?: string;
	localMounts: LocalCompatMount[];
	commandGuestPaths: ReadonlyMap<string, string>;
	onWasmCommandResolved?: (command: string) => void;
	onDispose?: () => Promise<void>;
}

export class NativeSidecarKernelProxy {
	readonly env: Record<string, string>;
	readonly cwd: string;
	readonly commands: ReadonlyMap<string, string>;
	readonly vfs: VirtualFileSystem;
	readonly processes = new Map<number, ProcessInfo>();
	private readonly defaultExecCwd: string | undefined;

	private readonly client: SidecarProcess;
	private readonly disposeClient: boolean;
	private readonly session: AuthenticatedSession;
	private readonly vm: CreatedVm;
	private readonly localMounts: LocalCompatMount[];
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
	private nextSyntheticPid = SYNTHETIC_PID_BASE;
	private readonly eventPumpAbortController = new AbortController();
	private readonly eventPump: Promise<void>;

	constructor(options: NativeSidecarKernelProxyOptions) {
		this.client = options.client;
		this.disposeClient = options.disposeClient ?? true;
		this.session = options.session;
		this.vm = options.vm;
		this.env = { ...options.env };
		this.cwd = options.cwd;
		this.defaultExecCwd = options.defaultExecCwd;
		this.localMounts = [...options.localMounts].sort(
			(left, right) => right.path.length - left.path.length,
		);
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

	async dispose(): Promise<void> {
		if (this.disposed) {
			return;
		}
		this.disposed = true;
		this.eventPumpAbortController.abort();

		const liveProcesses = [...this.trackedProcesses.values()].filter(
			(entry) => entry.exitCode === null,
		);
		await Promise.allSettled(
			liveProcesses.map((entry) => this.signalProcess(entry, 15)),
		);

		await Promise.race([
			this.client.disposeVm(this.session, this.vm),
			new Promise<void>((resolve) => setTimeout(resolve, 1000)),
		]).catch(() => {});
		for (const entry of liveProcesses) {
			if (entry.exitCode === null) {
				// The sidecar dispose path already performs TERM/KILL escalation for any
				// guest executions that are still live. Resolve local waiters eagerly so
				// VM teardown does not hang on killed ACP adapter processes that never
				// surface a terminal process_exited event back to the JS bridge.
				this.finishProcess(entry, 143);
			}
		}
		if (this.disposeClient) {
			await this.client.dispose().catch(() => {});
		}
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
		const effectiveCwd = options?.cwd ?? this.defaultExecCwd ?? this.cwd;
		const parsedCommand = parseSimpleExecCommand(command);
		const resolveExecPath = (targetPath: string) =>
			targetPath.startsWith("/")
				? posixPath.normalize(targetPath)
				: posixPath.normalize(posixPath.join(effectiveCwd, targetPath));
		const runAndCapture = async (
			proc: ManagedProcess,
			stdinOverride?: string | Uint8Array,
			readExitCode?: () => Promise<number>,
		): Promise<KernelExecResult> => {
			if (stdinOverride !== undefined) {
				proc.writeStdin(stdinOverride);
			} else if (options?.stdin !== undefined) {
				proc.writeStdin(options.stdin);
			}
			// `kernel.exec()` is a non-interactive run-to-completion API: when the
			// caller does not opt into a streaming stdin handle, the guest process
			// should observe EOF after any provided input so commands like
			// `node -e ...` do not linger behind an inherited open stdin pipe.
			proc.closeStdin();

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
		if (
			parsedCommand &&
			(parsedCommand[0] === "sh" || parsedCommand[0] === "/bin/sh") &&
			parsedCommand[1] === "-c" &&
			parsedCommand.length === 3
		) {
			const shellScript = parsedCommand[2].trim();
			const exitMatch = shellScript.match(/^exit(?:\s+(-?\d+))?$/);
			if (exitMatch) {
				return {
					exitCode: Number.parseInt(exitMatch[1] ?? "0", 10),
					stdout: "",
					stderr: "",
				};
			}
			return this.exec(parsedCommand[2], options);
		}
		if (
			parsedCommand &&
			parsedCommand[0] === "chmod" &&
			parsedCommand.length >= 3 &&
			/^[0-7]{3,4}$/.test(parsedCommand[1] ?? "")
		) {
			const mode = Number.parseInt(parsedCommand[1]!, 8);
			for (const target of parsedCommand.slice(2)) {
				await this.client.chmod(
					this.session,
					this.vm,
					resolveExecPath(target),
					mode,
				);
			}
			return { exitCode: 0, stdout: "", stderr: "" };
		}
		if (
			parsedCommand &&
			parsedCommand[0] === "stat" &&
			parsedCommand.length === 4 &&
			parsedCommand[1] === "-c" &&
			parsedCommand[2] === "%a"
		) {
			const stat = await this.stat(resolveExecPath(parsedCommand[3]!));
			return {
				exitCode: 0,
				stdout: `${(stat.mode & 0o777).toString(8)}\n`,
				stderr: "",
			};
		}
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

	spawn(
		command: string,
		args: string[],
		options?: KernelSpawnOptions,
	): ManagedProcess {
		let spawnCommand = command;
		let spawnArgs = [...args];
		const shellOption = (options as ({ shell?: unknown } & KernelSpawnOptions) | undefined)
			?.shell;
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
			driver: spawnCommand === "node" ? "node" : "wasmvm",
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
			pendingStdin: [],
			stdinFlushPromise: null,
			pendingCloseStdin: false,
			pendingKillSignal: null,
			waitWithFallbackPromise: null,
			hostExitObservedAt: null,
			outputGeneration: 0,
			exitViaEvent: false,
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
				void this.flushPendingStdin(entry).catch((error) => {
					this.handleBackgroundProcessError(entry, error);
				});
			},
			closeStdin: () => {
				entry.pendingCloseStdin = true;
				void this.closeTrackedStdin(entry).catch((error) => {
					this.handleBackgroundProcessError(entry, error);
				});
			},
			kill: (signal = 15) => {
				if (entry.exitCode !== null) {
					return;
				}
				entry.pendingKillSignal = signal;
				void entry.startPromise
					.then(async () => {
						if (entry.pendingKillSignal === null) {
							return;
						}
						const pendingSignal = entry.pendingKillSignal;
						entry.pendingKillSignal = null;
						await this.signalProcess(entry, pendingSignal);
					})
					.catch((error) => {
						this.handleBackgroundProcessError(entry, error);
					});
				if (
					(signal === 9 || signal === 15) &&
					entry.exitCode === null
				) {
					this.finishProcess(entry, 128 + signal);
				}
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
		const autoCloseExplicitCommandStdin =
			Boolean(options?.command) &&
			!["sh", "/bin/sh", "bash"].includes(command);
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
		let bufferedInput = "";
		let bufferedCommand = "";
		let activeForegroundProcess: ManagedProcess | null = null;
		let shellEnv = { ...(options?.env ?? {}) };
		let shellCwd = options?.cwd ?? this.cwd;
		let syntheticCommandQueue = Promise.resolve();
		let promptTimer: ReturnType<typeof setTimeout> | null = null;
		let closeStdinTimer: ReturnType<typeof setTimeout> | null = null;
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
		const clearCloseStdinTimer = () => {
			if (closeStdinTimer !== null) {
				clearTimeout(closeStdinTimer);
				closeStdinTimer = null;
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
		const writeForegroundInput = (
			proc: ManagedProcess,
			data: string | Uint8Array,
		) => {
			if (typeof data === "string") {
				for (const character of data) {
					proc.writeStdin(character);
				}
				return;
			}
			for (const byte of data) {
				proc.writeStdin(new Uint8Array([byte]));
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
				write(data) {
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
								writeForegroundInput(activeForegroundProcess, beforeInterrupt);
							}
							emitSyntheticTerminal("^C\n");
							activeForegroundProcess.kill(2);
							return;
						}
						writeForegroundInput(activeForegroundProcess, data);
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
					bufferedInput += text.replace(/\u0004/g, "");
					while (true) {
						const newlineIndex = bufferedInput.indexOf("\n");
						if (newlineIndex < 0) {
							break;
						}
						const line = bufferedInput.slice(0, newlineIndex).replace(/\r$/, "");
						bufferedInput = bufferedInput.slice(newlineIndex + 1);
						emitSyntheticStdout(`${line}\n`);
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
									finishSyntheticShell(Number.parseInt(exitMatch[1] ?? "0", 10));
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
			env: options?.env,
			cwd: options?.cwd,
			streamStdin: true,
			onStdout: (chunk) => {
				for (const handler of terminalHandlers) {
					handler(chunk);
				}
				if (commandInFlight) {
					schedulePrompt(120);
				}
			},
			onStderr: (chunk) => {
				// `onData` is the ordered PTY rendering stream. `onStderr` remains an
				// optional channel-specific diagnostic tap and must not also be rendered.
				for (const handler of terminalHandlers) {
					handler(chunk);
				}
				for (const handler of stderrHandlers) {
					handler(chunk);
				}
				if (commandInFlight) {
					schedulePrompt(120);
				}
			},
		});

		return {
			pid: proc.pid,
			write(data) {
				if (synthesizePrompt) {
					return;
				}
				proc.writeStdin(data);
				if (autoCloseExplicitCommandStdin) {
					clearCloseStdinTimer();
					closeStdinTimer = setTimeout(() => {
						closeStdinTimer = null;
						proc.closeStdin();
					}, 100);
				}
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
			resize() {
				// The current stdio-native path is process-backed rather than PTY-backed.
			},
			kill(signal) {
				clearCloseStdinTimer();
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

	// Test-runtime only: runtime mounts registered here stay host-side local
	// compat mounts and are not delivered to the sidecar. The production proxy
	// in @rivet-dev/agentos-core reconfigures sidecar mounts on every
	// mountFs/unmountFs.
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

	getResourceSnapshot(): Promise<SidecarResourceSnapshot> {
		return this.client.getResourceSnapshot(this.session, this.vm);
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

	/**
	 * Await a fresh listener lookup instead of reading the synchronous cache.
	 *
	 * {@link findListener} returns whatever value the background refresh has
	 * populated so far, which starts `null`. Callers that poll (for example
	 * `waitForListener`) can otherwise keep observing that stale `null` even once
	 * the listener exists. This variant reuses an in-flight refresh when present,
	 * otherwise starts one, awaits it, and returns the freshly resolved value.
	 */
	async findListenerAsync(request: {
		host?: string;
		port?: number;
		path?: string;
	}): Promise<KernelSocketSnapshot | null> {
		const key = socketLookupKey("listener", request);
		const existing = this.listenerLookups.get(key);
		const pending =
			existing?.pending ??
			this.refreshSocketLookup(this.listenerLookups, key, () =>
				this.client.findListener(this.session, this.vm, request),
			);
		if (!existing?.pending) {
			this.listenerLookups.set(key, {
				value: existing?.value ?? null,
				pending,
			});
		}
		await pending;
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

	/**
	 * Drive an HTTP request to a guest-bound TCP port inside the VM and return
	 * the raw JSON response payload from the sidecar. The caller decodes the
	 * JSON into a structured response.
	 */
	async vmFetch(request: {
		port: number;
		method: string;
		path: string;
		headersJson: string;
		body?: string;
	}): Promise<string> {
		return this.client.vmFetch(this.session, this.vm, request);
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

			await drainTrailingProcessOutputTurn(
				Math.min(delayMs, remainingMs),
			);
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
		this.signalRefreshes.set(
			entry.pid,
			this.refreshSignalState(entry).catch(() => {}),
		);

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
		while (!this.disposed) {
			try {
				const event = await this.client.waitForEvent(
					{ any: true },
					undefined,
					{
						signal: this.eventPumpAbortController.signal,
					},
				);
				if (event.payload.type === "process_output") {
					const entry = this.trackedProcessesById.get(event.payload.process_id);
					if (!entry) {
						continue;
					}
					entry.outputGeneration += 1;
					void this.refreshProcessSnapshot().catch(() => {});
					if (!this.signalRefreshes.has(entry.pid)) {
						this.signalRefreshes.set(
							entry.pid,
							this.refreshSignalState(entry).catch(() => {}),
						);
					}
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
					entry.exitViaEvent = true;
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
		// Every started process now parks a signal-state refresh here, so clean
		// up on the shared exit path — the snapshot-poll fallback exits never
		// pass through the event pump's `process_exited` branch.
		this.signalRefreshes.delete(entry.pid);
		entry.exitCode = exitCode;
		entry.exitTime = Date.now();
		this.updateTrackedProcessSnapshot(entry);
		entry.resolveWait(exitCode);
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
						if (
							now - entry.hostExitObservedAt >= MISSING_EXIT_EVENT_GRACE_MS
						) {
							this.finishProcess(entry, 0);
							break;
						}
						continue;
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
		const sidecarSignal = toSidecarSignalName(signal);
		let timedOut = false;
		const killPromise = this.client.killProcess(
			this.session,
			this.vm,
			entry.processId,
			sidecarSignal,
		);
		try {
			await Promise.race([
				killPromise,
				new Promise<void>((resolve) =>
					setTimeout(() => {
						timedOut = true;
						resolve();
					}, 1000),
				),
			]);
			if (timedOut) {
				void killPromise.catch(() => {});
			}
			const hasGuestHandler =
				this.signalStates.get(entry.pid)?.handlers.has(signal) ?? false;
			if (
				entry.exitCode === null &&
				!hasGuestHandler &&
				LOCALLY_TERMINATING_SIGNALS.has(sidecarSignal) &&
				(entry.driver === "wasmvm" || timedOut)
			) {
				this.finishProcess(entry, 128 + signal);
			}
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
		if (this.disposed || isNoSuchProcessError(error) || isUnknownVmError(error)) {
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
		if (this.disposed || isNoSuchProcessError(error) || isUnknownVmError(error)) {
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
				const local = includeLocalMounts ? this.resolveLocalMount(path) : null;
				if (local) {
					this.assertLocalWritable(local.mount);
					return local.mount.fs.pwrite(local.relativePath, offset, data);
				}
				return this.client.pwrite(this.session, this.vm, path, offset, data);
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

	private dispatchNativeRead(path: string): Promise<Uint8Array> {
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
			if (
				normalizedPath === root ||
				normalizedPath.startsWith(`${root}/`)
			) {
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
	SidecarSpawnOptions,
	RootFilesystemEntry,
	SidecarEventSelector,
	SidecarPermissionsPolicy,
	SidecarRegisteredHostCallbackDefinition,
	SidecarRequestFrame,
	SidecarRequestHandler,
	SidecarResponsePayload,
	SidecarSessionState,
	SidecarSignalHandlerRegistration,
	SidecarSocketStateEntry,
} from "./sidecar-process.js";
export {
	SidecarProcess,
	SidecarEventBufferOverflow,
	SidecarProcessError,
	SidecarProcessExited,
} from "./sidecar-process.js";
