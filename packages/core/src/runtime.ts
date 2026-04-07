export type StdioChannel = "stdout" | "stderr";
export type TimingMitigation = "off" | "freeze";
export type PermissionMode = "allow" | "deny";
export type PermissionDecision = PermissionMode;

export interface VirtualDirEntry {
	name: string;
	isDirectory: boolean;
	isSymbolicLink?: boolean;
}

export interface VirtualStat {
	mode: number;
	size: number;
	blocks: number;
	dev: number;
	rdev: number;
	isDirectory: boolean;
	isSymbolicLink: boolean;
	atimeMs: number;
	mtimeMs: number;
	ctimeMs: number;
	birthtimeMs: number;
	ino: number;
	nlink: number;
	uid: number;
	gid: number;
}

export interface VirtualFileSystem {
	readFile(path: string): Promise<Uint8Array>;
	readTextFile(path: string): Promise<string>;
	readDir(path: string): Promise<string[]>;
	readDirWithTypes(path: string): Promise<VirtualDirEntry[]>;
	writeFile(path: string, content: string | Uint8Array): Promise<void>;
	createDir(path: string): Promise<void>;
	mkdir(path: string, options?: { recursive?: boolean }): Promise<void>;
	exists(path: string): Promise<boolean>;
	stat(path: string): Promise<VirtualStat>;
	removeFile(path: string): Promise<void>;
	removeDir(path: string): Promise<void>;
	rename(oldPath: string, newPath: string): Promise<void>;
	realpath(path: string): Promise<string>;
	symlink(target: string, linkPath: string): Promise<void>;
	readlink(path: string): Promise<string>;
	lstat(path: string): Promise<VirtualStat>;
	link(oldPath: string, newPath: string): Promise<void>;
	chmod(path: string, mode: number): Promise<void>;
	chown(path: string, uid: number, gid: number): Promise<void>;
	utimes(path: string, atime: number, mtime: number): Promise<void>;
	truncate(path: string, length: number): Promise<void>;
	pread(path: string, offset: number, length: number): Promise<Uint8Array>;
	pwrite(path: string, offset: number, data: Uint8Array): Promise<void>;
}

export interface NetworkAccessRequest {
	url?: string;
	host?: string;
	port?: number;
	protocol?: string;
}

export interface FsPermissionRule {
	mode: PermissionMode;
	operations?: string[];
	paths?: string[];
}

export interface PatternPermissionRule {
	mode: PermissionMode;
	operations?: string[];
	patterns?: string[];
}

export interface RulePermissions<TRule> {
	default?: PermissionMode;
	rules: TRule[];
}

export type FsPermissions = PermissionMode | RulePermissions<FsPermissionRule>;
export type NetworkPermissions =
	| PermissionMode
	| RulePermissions<PatternPermissionRule>;
export type ChildProcessPermissions =
	| PermissionMode
	| RulePermissions<PatternPermissionRule>;
export type EnvPermissions =
	| PermissionMode
	| RulePermissions<PatternPermissionRule>;

export interface ProcessInfo {
	pid: number;
	ppid: number;
	pgid: number;
	sid: number;
	driver: string;
	command: string;
	args: string[];
	cwd: string;
	status: "running" | "exited";
	exitCode: number | null;
	startTime: number;
	exitTime: number | null;
}

export interface ManagedProcess {
	pid: number;
	writeStdin(data: Uint8Array | string): void;
	closeStdin(): void;
	kill(signal?: number): void;
	wait(): Promise<number>;
	readonly exitCode: number | null;
}

export interface ShellHandle {
	pid: number;
	write(data: Uint8Array | string): void;
	onData: ((data: Uint8Array) => void) | null;
	resize(cols: number, rows: number): void;
	kill(signal?: number): void;
	wait(): Promise<number>;
}

export interface OpenShellOptions {
	command?: string;
	args?: string[];
	env?: Record<string, string>;
	cwd?: string;
	cols?: number;
	rows?: number;
	onStderr?: (data: Uint8Array) => void;
}

export interface ConnectTerminalOptions extends OpenShellOptions {
	onData?: (data: Uint8Array) => void;
}

export interface ExecOptions {
	env?: Record<string, string>;
	cwd?: string;
	stdin?: string | Uint8Array;
	timeout?: number;
	onStdout?: (data: Uint8Array) => void;
	onStderr?: (data: Uint8Array) => void;
	captureStdio?: boolean;
	filePath?: string;
	cpuTimeLimitMs?: number;
	timingMitigation?: TimingMitigation;
}

export interface ExecResult {
	exitCode: number;
	stdout: string;
	stderr: string;
}

export interface RunResult<T = unknown> {
	value?: T;
	code: number;
	errorMessage?: string;
}

export interface KernelSpawnOptions extends ExecOptions {
	stdio?: "pipe" | "inherit";
	stdinFd?: number;
	stdoutFd?: number;
	stderrFd?: number;
	streamStdin?: boolean;
}

export type KernelExecOptions = ExecOptions;
export type KernelExecResult = ExecResult;
export type StatInfo = VirtualStat;
export type DirEntry = VirtualDirEntry;
export type StdioEvent = { channel: StdioChannel; message: string };
export type StdioHook = (event: StdioEvent) => void;

export interface Permissions {
	fs?: FsPermissions;
	network?: NetworkPermissions;
	childProcess?: ChildProcessPermissions;
	env?: EnvPermissions;
}

export interface ResourceBudgets {
	maxOutputBytes?: number;
	maxBridgeCalls?: number;
	maxTimers?: number;
	maxChildProcesses?: number;
	maxHandles?: number;
}

export interface ProcessConfig {
	cwd?: string;
	env?: Record<string, string>;
	argv?: string[];
	stdinIsTTY?: boolean;
	stdoutIsTTY?: boolean;
	stderrIsTTY?: boolean;
}

export interface OSConfig {
	homedir?: string;
	tmpdir?: string;
}

export interface CommandExecutor {
	spawn(
		command: string,
		args: string[],
		options?: KernelSpawnOptions,
	): ManagedProcess;
}

export interface NetworkAdapter {
	fetch(
		url: string,
		options?: {
			method?: string;
			headers?: Record<string, string>;
			body?: unknown;
		},
	): Promise<{
		ok: boolean;
		status: number;
		statusText: string;
		headers: Record<string, string>;
		body: string;
		url: string;
		redirected: boolean;
	}>;
	dnsLookup(hostname: string): Promise<{
		address?: string;
		family?: number;
		error?: string;
		code?: string;
	}>;
	httpRequest(
		url: string,
		options?: {
			method?: string;
			headers?: Record<string, string>;
			body?: unknown;
		},
	): Promise<{
		status: number;
		statusText: string;
		headers: Record<string, string>;
		body: string;
		url: string;
	}>;
}

export interface SystemDriver {
	filesystem?: VirtualFileSystem;
	network?: NetworkAdapter;
	commandExecutor?: CommandExecutor;
	permissions?: Permissions;
	runtime: {
		process: ProcessConfig;
		os: OSConfig;
	};
}

export interface RuntimeDriverOptions {
	system: SystemDriver;
	runtime: {
		process: ProcessConfig;
		os: OSConfig;
	};
	memoryLimit?: number;
	cpuTimeLimitMs?: number;
	timingMitigation?: TimingMitigation;
	onStdio?: StdioHook;
	payloadLimits?: {
		base64TransferBytes?: number;
		jsonPayloadBytes?: number;
	};
	resourceBudgets?: ResourceBudgets;
}

export interface NodeRuntimeDriver {
	exec(code: string, options?: ExecOptions): Promise<ExecResult>;
	run<T = unknown>(code: string, filePath?: string): Promise<RunResult<T>>;
	dispose(): void;
	terminate?(): Promise<void>;
	readonly network?: Pick<
		NetworkAdapter,
		"fetch" | "dnsLookup" | "httpRequest"
	>;
}

export interface NodeRuntimeDriverFactory {
	createRuntimeDriver(options: RuntimeDriverOptions): NodeRuntimeDriver;
}

export interface KernelInterface {
	vfs: VirtualFileSystem;
}

export interface Kernel extends KernelInterface {
	mount(driver: KernelRuntimeDriver): Promise<void>;
	dispose(): Promise<void>;
	exec(command: string, options?: KernelExecOptions): Promise<KernelExecResult>;
	spawn(
		command: string,
		args: string[],
		options?: KernelSpawnOptions,
	): ManagedProcess;
	openShell(options?: OpenShellOptions): ShellHandle;
	connectTerminal(options?: ConnectTerminalOptions): Promise<number>;
	mountFs(
		path: string,
		fs: VirtualFileSystem,
		options?: { readOnly?: boolean },
	): void;
	unmountFs(path: string): void;
	readFile(path: string): Promise<Uint8Array>;
	writeFile(path: string, content: string | Uint8Array): Promise<void>;
	mkdir(path: string): Promise<void>;
	readdir(path: string): Promise<string[]>;
	stat(path: string): Promise<VirtualStat>;
	exists(path: string): Promise<boolean>;
	removeFile(path: string): Promise<void>;
	removeDir(path: string): Promise<void>;
	rename(oldPath: string, newPath: string): Promise<void>;
	readonly commands: ReadonlyMap<string, string>;
	readonly processes: ReadonlyMap<number, ProcessInfo>;
	readonly env: Record<string, string>;
	readonly cwd: string;
	readonly socketTable: {
		hasHostNetworkAdapter(): boolean;
		findListener(_request: unknown): unknown | null;
		findBoundUdp(_request: unknown): unknown | null;
	};
	readonly processTable: {
		getSignalState(_pid: number): { handlers: Map<number, unknown> };
	};
	readonly timerTable: Record<string, never>;
	readonly zombieTimerCount: number;
}

export interface BindingTree {
	[key: string]: BindingFunction | BindingTree;
}

export type BindingFunction = (...args: unknown[]) => unknown;

export interface ModuleAccessOptions {
	cwd?: string;
}

export interface NodeDriverOptions {
	filesystem?: VirtualFileSystem;
	networkAdapter?: NetworkAdapter;
	commandExecutor?: CommandExecutor;
	permissions?: Permissions;
	processConfig?: ProcessConfig;
	osConfig?: OSConfig;
	moduleAccess?: ModuleAccessOptions;
}

export interface DefaultNetworkAdapterOptions {
	loopbackExemptPorts?: number[];
}

export interface NodeRuntimeOptions {
	systemDriver?: SystemDriver;
	runtimeDriverFactory?: NodeRuntimeDriverFactory;
	permissions?: Partial<Permissions>;
	memoryLimit?: number;
	moduleAccessPaths?: string[];
	bindings?: BindingTree;
	loopbackExemptPorts?: number[];
	moduleAccessCwd?: string;
	packageRoots?: Array<{ hostPath: string; vmPath: string }>;
}

export type NodeRuntimeDriverFactoryOptions = Record<string, never>;
export type NodeExecutionDriverOptions = RuntimeDriverOptions;

export interface KernelRuntimeDriver {
	readonly kind: "node" | "wasmvm";
	readonly name: string;
	readonly commands: string[];
	readonly commandDirs?: string[];
}

export type DriverProcess = ManagedProcess;
export type ProcessContext = Record<string, never>;

export type PermissionTier = "full" | "read-write" | "read-only" | "isolated";

export interface WasmVmRuntimeOptions {
	wasmBinaryPath?: string;
	commandDirs?: string[];
	permissions?: Record<string, PermissionTier>;
}
