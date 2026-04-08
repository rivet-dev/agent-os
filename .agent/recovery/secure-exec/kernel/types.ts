/**
 * Kernel type definitions.
 *
 * The kernel is the shared OS layer. All runtimes make "syscalls" to the
 * kernel for filesystem, process, pipe, and FD operations.
 */

import type { WaitQueue } from "./wait.js";

// Re-export VFS types
export type {
	VirtualFileSystem,
	VirtualDirEntry,
	VirtualStat,
} from "./vfs.js";

// ---------------------------------------------------------------------------
// Kernel
// ---------------------------------------------------------------------------

/**
 * Minimal structured logger interface for kernel diagnostics.
 * Compatible with pino and any logger that supports child loggers.
 * The kernel never depends on pino directly — embedders pass their own logger.
 */
export interface KernelLogger {
	trace(obj: Record<string, unknown>, msg?: string): void;
	debug(obj: Record<string, unknown>, msg?: string): void;
	info(obj: Record<string, unknown>, msg?: string): void;
	warn(obj: Record<string, unknown>, msg?: string): void;
	error(obj: Record<string, unknown>, msg?: string): void;
	child(bindings: Record<string, unknown>): KernelLogger;
}

/** No-op logger that discards all records. */
export const noopKernelLogger: KernelLogger = {
	trace() {},
	debug() {},
	info() {},
	warn() {},
	error() {},
	child() { return noopKernelLogger; },
};

/** A filesystem to mount at a specific path inside the kernel VFS. */
export interface FsMount {
	path: string;
	fs: import("./vfs.js").VirtualFileSystem;
	readOnly?: boolean;
}


export interface KernelOptions {
	filesystem: import("./vfs.js").VirtualFileSystem;
	permissions?: Permissions;
	env?: Record<string, string>;
	cwd?: string;
	/** Maximum number of concurrent processes. Spawn beyond this limit throws EAGAIN. */
	maxProcesses?: number;
	/** Host network adapter for external socket routing (TCP, UDP, DNS). */
	hostNetworkAdapter?: import("./host-adapter.js").HostNetworkAdapter;
	/** Structured debug logger for kernel diagnostics. Defaults to silent no-op. */
	logger?: KernelLogger;
	/** Additional filesystems to mount at boot (after /dev and /proc). */
	mounts?: FsMount[];
}

export interface Kernel {
	/** Mount a runtime driver. Calls driver.init() and registers its commands. */
	mount(driver: RuntimeDriver): Promise<void>;

	/** Dispose the kernel and all mounted drivers. */
	dispose(): Promise<void>;

	/**
	 * Execute a command string through the shell.
	 * Equivalent to: spawn('sh', ['-c', command])
	 * Throws if no shell is mounted (e.g. no WasmVM runtime).
	 */
	exec(command: string, options?: ExecOptions): Promise<ExecResult>;

	/**
	 * Spawn a process directly (no shell interpretation).
	 * The kernel resolves the command via the command registry and delegates
	 * to the appropriate runtime driver.
	 */
	spawn(command: string, args: string[], options?: SpawnOptions): ManagedProcess;

	/**
	 * Flush pending /bin stub entries created by on-demand command discovery.
	 * Ensures VFS is consistent before shell PATH lookups.
	 */
	flushPendingBinEntries(): Promise<void>;

	/**
	 * Open an interactive shell on a PTY.
	 * Wires PTY + process groups + termios for terminal use.
	 */
	openShell(options?: OpenShellOptions): ShellHandle;

	/**
	 * Wire openShell() to process.stdin/stdout for an interactive terminal session.
	 * Sets raw mode, forwards input/output, handles resize, restores terminal on exit.
	 * Returns the shell exit code.
	 */
	connectTerminal(options?: ConnectTerminalOptions): Promise<number>;

	/** Mount a filesystem at the given path. */
	mountFs(path: string, fs: import("./vfs.js").VirtualFileSystem, options?: { readOnly?: boolean }): void;

	/** Unmount the filesystem at the given path. */
	unmountFs(path: string): void;

	// Filesystem convenience wrappers
	readFile(path: string): Promise<Uint8Array>;
	writeFile(path: string, content: string | Uint8Array): Promise<void>;
	mkdir(path: string): Promise<void>;
	readdir(path: string): Promise<string[]>;
	stat(path: string): Promise<import("./vfs.js").VirtualStat>;
	exists(path: string): Promise<boolean>;
	removeFile(path: string): Promise<void>;
	removeDir(path: string): Promise<void>;
	rename(oldPath: string, newPath: string): Promise<void>;

	// Socket table
	readonly socketTable: import("./socket-table.js").SocketTable;
	readonly timerTable: import("./timer-table.js").TimerTable;

	// Introspection
	readonly commands: ReadonlyMap<string, string>;
	readonly processes: ReadonlyMap<number, ProcessInfo>;
	/** Number of pending zombie cleanup timers (test observability). */
	readonly zombieTimerCount: number;
}

export interface ExecOptions {
	env?: Record<string, string>;
	cwd?: string;
	stdin?: string | Uint8Array;
	timeout?: number;
	onStdout?: (data: Uint8Array) => void;
	onStderr?: (data: Uint8Array) => void;
}

export interface ExecResult {
	exitCode: number;
	stdout: string;
	stderr: string;
}

export interface SpawnOptions extends ExecOptions {
	stdio?: "pipe" | "inherit";
	/** FD in caller's table to wire as child's stdin (pipe read end). */
	stdinFd?: number;
	/** FD in caller's table to wire as child's stdout (pipe write end). */
	stdoutFd?: number;
	/** FD in caller's table to wire as child's stderr (pipe write end). */
	stderrFd?: number;
	/** Enable streaming stdin: writeStdin() delivers data immediately instead of buffering until closeStdin(). */
	streamStdin?: boolean;
}

export interface ManagedProcess {
	pid: number;
	writeStdin(data: Uint8Array | string): void;
	closeStdin(): void;
	kill(signal?: number): void;
	wait(): Promise<number>;
	readonly exitCode: number | null;
}

// ---------------------------------------------------------------------------
// Interactive shell
// ---------------------------------------------------------------------------

export interface OpenShellOptions {
	/** Shell command to run (default: "sh"). */
	command?: string;
	/** Arguments to pass to the shell command. */
	args?: string[];
	/** Environment variables for the shell process. */
	env?: Record<string, string>;
	/** Working directory for the shell process. */
	cwd?: string;
	/** Initial terminal columns. */
	cols?: number;
	/** Initial terminal rows. */
	rows?: number;
}

/**
 * Handle returned by kernel.openShell().
 * Provides write/onData/resize/kill/wait for interactive shell use.
 */
export interface ShellHandle {
	/** PID of the shell process. */
	pid: number;
	/** Write data to the shell (goes through PTY line discipline). */
	write(data: Uint8Array | string): void;
	/** Callback for data produced by the shell (program output). */
	onData: ((data: Uint8Array) => void) | null;
	/** Notify terminal resize — delivers SIGWINCH to foreground process group. */
	resize(cols: number, rows: number): void;
	/** Kill the shell process. */
	kill(signal?: number): void;
	/** Wait for the shell to exit. Returns exit code. */
	wait(): Promise<number>;
}

/**
 * Options for connectTerminal().
 * Extends OpenShellOptions with an optional output handler override.
 */
export interface ConnectTerminalOptions extends OpenShellOptions {
	/** Custom output handler. Defaults to writing to process.stdout. */
	onData?: (data: Uint8Array) => void;
}

// ---------------------------------------------------------------------------
// Runtime Driver
// ---------------------------------------------------------------------------

export interface RuntimeDriver {
	/** Driver name (e.g. 'wasmvm', 'node', 'python') */
	name: string;

	/** Commands this driver handles */
	commands: string[];

	/**
	 * Called when the driver is mounted to the kernel.
	 * Use this to initialize resources (compile WASM, load Pyodide, etc.)
	 */
	init(kernel: KernelInterface): Promise<void>;

	/**
	 * Spawn a process for the given command.
	 * The kernel has already resolved the command to this driver.
	 */
	spawn(command: string, args: string[], ctx: ProcessContext): DriverProcess;

	/**
	 * On-demand command discovery. Called by the kernel when a command is not
	 * found in the registry. Returns true if this driver can handle the command
	 * (e.g. found a matching WASM binary on disk). The kernel then registers
	 * the command and retries the spawn.
	 */
	tryResolve?(command: string): boolean;

	/** Cleanup resources */
	dispose(): Promise<void>;
}

export interface ProcessContext {
	pid: number;
	ppid: number;
	env: Record<string, string>;
	cwd: string;
	fds: { stdin: number; stdout: number; stderr: number };
	/** Whether stdin/stdout/stderr are connected to a PTY slave. */
	stdinIsTTY?: boolean;
	stdoutIsTTY?: boolean;
	stderrIsTTY?: boolean;
	/** Enable streaming stdin delivery (writeStdin data arrives immediately). */
	streamStdin?: boolean;
	/** Kernel-provided callback for stdout data emitted during spawn. */
	onStdout?: (data: Uint8Array) => void;
	/** Kernel-provided callback for stderr data emitted during spawn. */
	onStderr?: (data: Uint8Array) => void;
}

export interface DriverProcess {
	/** Called by kernel when data is written to this process's stdin FD */
	writeStdin(data: Uint8Array): void;
	closeStdin(): void;

	/** Called by kernel to terminate the process */
	kill(signal: number): void;

	/** Resolves with exit code when process completes */
	wait(): Promise<number>;

	/** Callbacks for the driver to push data to the kernel */
	onStdout: ((data: Uint8Array) => void) | null;
	onStderr: ((data: Uint8Array) => void) | null;
	onExit: ((code: number) => void) | null;
}

// ---------------------------------------------------------------------------
// Kernel Interface (exposed TO drivers)
// ---------------------------------------------------------------------------

/**
 * Interface the kernel exposes TO drivers.
 * Drivers call these methods for kernel services.
 */
export interface KernelInterface {
	// VFS operations
	vfs: import("./vfs.js").VirtualFileSystem;

	// FD operations (per-PID)
	fdOpen(pid: number, path: string, flags: number, mode?: number): number;
	fdRead(pid: number, fd: number, length: number): Promise<Uint8Array>;
	fdWrite(pid: number, fd: number, data: Uint8Array): number | Promise<number>;
	fdClose(pid: number, fd: number): void;
	fdSeek(
		pid: number,
		fd: number,
		offset: bigint,
		whence: number,
	): Promise<bigint>;
	fdPread(pid: number, fd: number, length: number, offset: bigint): Promise<Uint8Array>;
	fdPwrite(pid: number, fd: number, data: Uint8Array, offset: bigint): Promise<number>;
	fdDup(pid: number, fd: number): number;
	fdDup2(pid: number, oldFd: number, newFd: number): void;
	fdDupMin(pid: number, fd: number, minFd: number): number;
	fdStat(pid: number, fd: number): FDStat;
	/** Query poll state for a file descriptor (pipe, PTY, or regular file). */
	fdPoll(pid: number, fd: number): { readable: boolean; writable: boolean; hangup: boolean; invalid: boolean };
	fdSetCloexec(pid: number, fd: number, value: boolean): void;
	fdGetCloexec(pid: number, fd: number): boolean;
	fcntl(pid: number, fd: number, cmd: number, arg?: number): number;

	// Advisory file locking
	/** Apply or remove an advisory lock on the file referenced by fd. */
	flock(pid: number, fd: number, operation: number): Promise<void>;

	// Process operations
	spawn(
		command: string,
		args: string[],
		ctx: Partial<ProcessContext> & {
			stdinFd?: number;
			stdoutFd?: number;
			stderrFd?: number;
		},
	): ManagedProcess;
	waitpid(
		pid: number,
		options?: number,
	): Promise<{ pid: number; status: number; termSignal: number } | null>;
	kill(pid: number, signal: number): void;
	getpid(pid: number): number;
	getppid(pid: number): number;

	// Process group / session operations
	setpgid(pid: number, pgid: number): void;
	getpgid(pid: number): number;
	setsid(pid: number): number;
	getsid(pid: number): number;

	// Pipe operations
	/** Create a pipe and install both ends in the given process's FD table. */
	pipe(pid: number): { readFd: number; writeFd: number };

	// PTY operations
	/** Allocate a PTY master/slave pair and install FDs in the process's table. */
	openpty(pid: number): { masterFd: number; slaveFd: number; path: string };
	/** Check if an FD refers to a terminal (PTY slave). */
	isatty(pid: number, fd: number): boolean;
	/** Set line discipline configuration on the PTY associated with the given FD. */
	ptySetDiscipline(
		pid: number,
		fd: number,
		config: { canonical?: boolean; echo?: boolean; isig?: boolean },
	): void;
	/** Set the foreground process group for signal delivery on the PTY. */
	ptySetForegroundPgid(pid: number, fd: number, pgid: number): void;

	// Termios operations
	/** Get terminal attributes for the PTY associated with the given FD. */
	tcgetattr(pid: number, fd: number): Termios;
	/** Set terminal attributes for the PTY associated with the given FD. */
	tcsetattr(pid: number, fd: number, termios: Partial<Termios>): void;
	/** Set the foreground process group for the terminal. */
	tcsetpgrp(pid: number, fd: number, pgid: number): void;
	/** Get the foreground process group for the terminal. */
	tcgetpgrp(pid: number, fd: number): number;

	// /dev/fd operations
	/** List open FD numbers for a process (readDir /dev/fd). */
	devFdReadDir(pid: number): string[];
	/** Stat the underlying file for /dev/fd/N. */
	devFdStat(pid: number, fd: number): Promise<import("./vfs.js").VirtualStat>;

	// Environment
	getenv(pid: number): Record<string, string>;
	setenv(pid: number, key: string, value: string): void;
	unsetenv(pid: number, key: string): void;
	getcwd(pid: number): string;

	// Working directory
	chdir(pid: number, path: string): Promise<void>;

	// Alarm (SIGALRM)
	/** Schedule SIGALRM delivery after `seconds`. Returns previous alarm remaining (0 if none). alarm(pid, 0) cancels. */
	alarm(pid: number, seconds: number): number;

	// File mode creation mask
	/** Get/set the process's umask. Returns the previous mask. If newMask is omitted, mask is unchanged. */
	umask(pid: number, newMask?: number): number;

	// Directory creation with umask
	/** Create a directory, applying the process's umask to the given mode. */
	mkdir(pid: number, path: string, mode?: number): Promise<void>;

	// Socket table
	readonly socketTable: import("./socket-table.js").SocketTable;
	readonly timerTable: import("./timer-table.js").TimerTable;

	// Process table
	readonly processTable: import("./process-table.js").ProcessTable;
}

// ---------------------------------------------------------------------------
// FD Table types
// ---------------------------------------------------------------------------

export interface FDStat {
	filetype: number;
	flags: number;
	rights: bigint;
}

export interface FileDescription {
	id: number;
	path: string;
	cursor: bigint;
	flags: number;
	refCount: number;
	/** Mode to apply when the file is first created (set by O_CREAT with umask). */
	creationMode?: number;
}

export interface FDEntry {
	fd: number;
	description: FileDescription;
	rights: bigint;
	filetype: number;
	/** Close-on-exec flag (FD_CLOEXEC). Per-FD, not per-description. */
	cloexec: boolean;
}

// FD open flags
export const O_RDONLY = 0;
export const O_WRONLY = 1;
export const O_RDWR = 2;
export const O_CREAT = 0o100;
export const O_EXCL = 0o200;
export const O_TRUNC = 0o1000;
export const O_APPEND = 0o2000;
export const O_NONBLOCK = 0o4;
export const O_CLOEXEC = 0o2000000;

// fcntl commands
export const F_DUPFD = 0;
export const F_GETFD = 1;
export const F_SETFD = 2;
export const F_GETFL = 3;
export const F_DUPFD_CLOEXEC = 1030;

// FD flags (for F_GETFD / F_SETFD)
export const FD_CLOEXEC = 1;

// Seek whence
export const SEEK_SET = 0;
export const SEEK_CUR = 1;
export const SEEK_END = 2;

// File types
export const FILETYPE_UNKNOWN = 0;
export const FILETYPE_CHARACTER_DEVICE = 2;
export const FILETYPE_DIRECTORY = 3;
export const FILETYPE_REGULAR_FILE = 4;
export const FILETYPE_SYMBOLIC_LINK = 7;
export const FILETYPE_PIPE = 6;

// ---------------------------------------------------------------------------
// Process Table types
// ---------------------------------------------------------------------------

export interface ProcessEntry {
	pid: number;
	ppid: number;
	/** Process group ID. Defaults to parent's pgid, or pid for session leaders. */
	pgid: number;
	/** Session ID. Defaults to parent's sid, or pid for session leaders. */
	sid: number;
	driver: string;
	command: string;
	args: string[];
	status: "running" | "stopped" | "exited";
	exitCode: number | null;
	/** How the process terminated: 'normal' for exit(), 'signal' for kill(). */
	exitReason: "normal" | "signal" | null;
	/** Signal that killed the process (0 = normal exit). */
	termSignal: number;
	/** Epoch ms when the process was registered. */
	startTime: number;
	exitTime: number | null;
	env: Record<string, string>;
	cwd: string;
	/** File mode creation mask (POSIX umask). Inherited from parent, default 0o022. */
	umask: number;
	/** Active handles tracked for this process (id → description). */
	activeHandles: Map<string, string>;
	/** Maximum number of active handles allowed for this process. 0 = unlimited. */
	handleLimit: number;
	/** Signal handling state: registered handlers, blocked signals, pending signals. */
	signalState: ProcessSignalState;
	driverProcess: DriverProcess;
}

export interface ProcessInfo {
	pid: number;
	ppid: number;
	pgid: number;
	sid: number;
	driver: string;
	command: string;
	args: string[];
	cwd: string;
	status: "running" | "stopped" | "exited";
	exitCode: number | null;
	startTime: number;
	exitTime: number | null;
}

// ---------------------------------------------------------------------------
// Kernel error type
// ---------------------------------------------------------------------------

/** POSIX error codes used by the kernel. */
export type KernelErrorCode =
	| "EACCES"
	| "EADDRINUSE"
	| "EAGAIN"
	| "EBADF"
	| "ECONNREFUSED"
	| "EINPROGRESS"
	| "EINTR"
	| "EEXIST"
	| "EINVAL"
	| "ELOOP"
	| "EIO"
	| "EISDIR"
	| "EMFILE"
	| "EMSGSIZE"
	| "ENOENT"
	| "ENOSPC"
	| "ENOSYS"
	| "ENOTCONN"
	| "ENOTEMPTY"
	| "ENOTDIR"
	| "EPERM"
	| "EPIPE"
	| "EROFS"
	| "ESPIPE"
	| "ESRCH"
	| "ETIMEDOUT"
	| "EXDEV";

/**
 * Structured error for kernel operations.
 * Carries a machine-readable `code` so callers can map to errno without
 * string matching.
 */
export class KernelError extends Error {
	readonly code: KernelErrorCode;

	constructor(code: KernelErrorCode, message: string) {
		super(`${code}: ${message}`);
		this.code = code;
		this.name = "KernelError";
	}
}

// ---------------------------------------------------------------------------
// Termios (terminal attributes)
// ---------------------------------------------------------------------------

/** Terminal attributes — controls line discipline behavior on a PTY. */
export interface Termios {
	/** Map CR (0x0d) to NL (0x0a) on input (POSIX ICRNL). */
	icrnl: boolean;
	/** Post-process output (master for ONLCR, etc.). */
	opost: boolean;
	/** Map NL to CR-NL on output (requires opost). */
	onlcr: boolean;
	/** Canonical mode: buffer input until newline, handle backspace. */
	icanon: boolean;
	/** Echo input bytes back through output (master reads them). */
	echo: boolean;
	/** Enable signal generation from control characters (^C, ^Z, ^\). */
	isig: boolean;
	/** Control characters. */
	cc: TermiosCC;
}

export interface TermiosCC {
	vintr: number;   // Default ^C (0x03) → SIGINT
	vquit: number;   // Default ^\ (0x1C) → SIGQUIT
	vsusp: number;   // Default ^Z (0x1A) → SIGTSTP
	veof: number;    // Default ^D (0x04) → EOF
	verase: number;  // Default DEL (0x7F) → erase
}

/** Returns the POSIX-standard default termios: canonical on, echo on, isig on, opost+onlcr on. */
export function defaultTermios(): Termios {
	return {
		icrnl: true,
		opost: true,
		onlcr: true,
		icanon: true,
		echo: true,
		isig: true,
		cc: {
			vintr: 0x03,   // ^C
			vquit: 0x1c,   // ^\
			vsusp: 0x1a,   // ^Z
			veof: 0x04,    // ^D
			verase: 0x7f,  // DEL
		},
	};
}

// Signals
export const SIGHUP = 1;
export const SIGINT = 2;
export const SIGQUIT = 3;
export const SIGKILL = 9;
export const SIGPIPE = 13;
export const SIGALRM = 14;
export const SIGTERM = 15;
export const SIGCHLD = 17;
export const SIGCONT = 18;
export const SIGSTOP = 19;
export const SIGTSTP = 20;
export const SIGWINCH = 28;

// sigaction flags
export const SA_RESTART = 0x10000000;
export const SA_RESETHAND = 0x80000000;
export const SA_NOCLDSTOP = 0x00000001;

// sigprocmask how values
export const SIG_BLOCK = 0;
export const SIG_UNBLOCK = 1;
export const SIG_SETMASK = 2;

// waitpid options (POSIX bitmask)
export const WNOHANG = 1;

// ---------------------------------------------------------------------------
// Signal handler types
// ---------------------------------------------------------------------------

/** Signal disposition: default kernel action, ignore, or user-defined handler. */
export type SignalDisposition = "default" | "ignore" | ((signal: number) => void);

/** Per-signal handler registration (matches POSIX struct sigaction). */
export interface SignalHandler {
	handler: SignalDisposition;
	/** Signals to block during handler execution (sa_mask). */
	mask: Set<number>;
	/** Flags (SA_RESTART, SA_RESETHAND, SA_NOCLDSTOP, etc.). */
	flags: number;
}

/** Per-process signal state. */
export interface ProcessSignalState {
	/** Signal number → registered handler. */
	handlers: Map<number, SignalHandler>;
	/** Currently blocked signals (sigprocmask). */
	blockedSignals: Set<number>;
	/** Signals queued while blocked. Standard signals (1-31) coalesce to max 1. */
	pendingSignals: Set<number>;
	/** Waiters blocked on signal-aware syscalls for this process. */
	signalWaiters: WaitQueue;
	/** Monotonic counter for delivered signals. */
	deliverySeq: number;
	/** Most recently delivered signal number, or null if none. */
	lastDeliveredSignal: number | null;
	/** Flags from the most recently delivered handler registration. */
	lastDeliveredFlags: number;
}

// ---------------------------------------------------------------------------
// Pipe types
// ---------------------------------------------------------------------------

export interface Pipe {
	id: number;
	readFd: number;
	writeFd: number;
	readerPid: number;
	writerPid: number;
	buffer: Uint8Array[];
	closed: { read: boolean; write: boolean };
}

// ---------------------------------------------------------------------------
// Permissions
// ---------------------------------------------------------------------------

export interface PermissionDecision {
	allow: boolean;
	reason?: string;
}

export type PermissionCheck<T> = (request: T) => PermissionDecision;

export interface FsAccessRequest {
	op:
		| "read"
		| "write"
		| "mkdir"
		| "createDir"
		| "readdir"
		| "stat"
		| "rm"
		| "rename"
		| "exists"
		| "symlink"
		| "readlink"
		| "link"
		| "chmod"
		| "chown"
		| "utimes"
		| "truncate";
	path: string;
}

export interface NetworkAccessRequest {
	op: "fetch" | "http" | "dns" | "listen" | "connect";
	url?: string;
	method?: string;
	hostname?: string;
}

export interface ChildProcessAccessRequest {
	command: string;
	args: string[];
	cwd?: string;
	env?: Record<string, string>;
}

export interface EnvAccessRequest {
	op: "read" | "write";
	key: string;
	value?: string;
}

export interface Permissions {
	fs?: PermissionCheck<FsAccessRequest>;
	network?: PermissionCheck<NetworkAccessRequest>;
	childProcess?: PermissionCheck<ChildProcessAccessRequest>;
	env?: PermissionCheck<EnvAccessRequest>;
}
