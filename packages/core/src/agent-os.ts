import { spawn as spawnChildProcess } from "node:child_process";
import {
	mkdtempSync,
	readdirSync,
	readFileSync,
	rmSync,
	statSync,
	writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import {
	sep as hostPathSeparator,
	join,
	posix as posixPath,
	relative as relativeHostPath,
	resolve as resolveHostPath,
} from "node:path";
import {
	allowAll,
	createInMemoryFileSystem,
	createKernel,
	type Kernel,
	type KernelExecOptions,
	type KernelExecResult,
	type ProcessInfo as KernelProcessInfo,
	type KernelSpawnOptions,
	type ManagedProcess,
	type OpenShellOptions,
	type Permissions,
	type ShellHandle,
	type VirtualFileSystem,
	type VirtualStat,
} from "@secure-exec/core";
import { type ToolKit, validateToolkits } from "./host-tools.js";
import { generateToolReference } from "./host-tools-prompt.js";
import {
	type HostToolsServer,
	startHostToolsServer,
} from "./host-tools-server.js";
import {
	createShimFilesystem,
	generateMasterShim,
	generateToolkitShim,
} from "./host-tools-shims.js";

/** Process tree node: extends kernel ProcessInfo with child references. */
export interface ProcessTreeNode extends KernelProcessInfo {
	children: ProcessTreeNode[];
}

/** A directory entry with metadata. */
export interface DirEntry {
	/** Absolute path to the entry. */
	path: string;
	type: "file" | "directory" | "symlink";
	size: number;
}

/** Options for readdirRecursive(). */
export interface ReaddirRecursiveOptions {
	/** Maximum depth to recurse (0 = only immediate children). */
	maxDepth?: number;
	/** Directory names to skip. */
	exclude?: string[];
}

/** Entry for batch write operations. */
export interface BatchWriteEntry {
	path: string;
	content: string | Uint8Array;
}

/** Result of a single file in a batch write. */
export interface BatchWriteResult {
	path: string;
	success: boolean;
	error?: string;
}

/** Result of a single file in a batch read. */
export interface BatchReadResult {
	path: string;
	content: Uint8Array | null;
	error?: string;
}

/** Entry in the agent registry, describing an available agent type. */
export interface AgentRegistryEntry {
	id: AgentType;
	acpAdapter: string;
	agentPackage: string;
	installed: boolean;
}

import { createWasmVmRuntime } from "@rivet-dev/agent-os-posix";
import { createPythonRuntime } from "@rivet-dev/agent-os-python";
import {
	createNodeHostNetworkAdapter,
	createNodeRuntime,
} from "@secure-exec/nodejs";
import { AcpClient } from "./acp-client.js";
import { AGENT_CONFIGS, type AgentConfig, type AgentType } from "./agents.js";
import { getHostDirBackendMeta } from "./backends/host-dir-backend.js";
import {
	createBootstrapAwareFilesystem,
	getBaseEnvironment,
	getBaseFilesystemEntries,
} from "./base-filesystem.js";
import { CronManager } from "./cron/cron-manager.js";
import type { ScheduleDriver } from "./cron/schedule-driver.js";
import { TimerScheduleDriver } from "./cron/timer-driver.js";
import type {
	CronEvent,
	CronEventHandler,
	CronJob,
	CronJobInfo,
	CronJobOptions,
} from "./cron/types.js";
import {
	type FilesystemEntry,
	snapshotVirtualFilesystem,
} from "./filesystem-snapshot.js";
import {
	createDefaultRootLowerInput,
	createInMemoryLayerStore,
	createSnapshotExport,
	type LayerStore,
	type OverlayFilesystemMode,
	type RootSnapshotExport,
	type SnapshotLayerHandle,
} from "./layers.js";
import { getOsInstructions } from "./os-instructions.js";
import {
	processSoftware,
	type SoftwareInput,
	type SoftwareRoot,
} from "./packages.js";
import type { JsonRpcRequest, JsonRpcResponse } from "./protocol.js";
import {
	type AgentCapabilities,
	type AgentInfo,
	type GetEventsOptions,
	type PermissionReply,
	type PermissionRequestHandler,
	type SequencedEvent,
	Session,
	type SessionConfigOption,
	type SessionEventHandler,
	type SessionInitData,
	type SessionModeState,
} from "./session.js";
import { createSqliteBindings } from "./sqlite-bindings.js";
import { createStdoutLineIterable } from "./stdout-lines.js";

interface HostMountInfo {
	vmPath: string;
	hostPath: string;
	readOnly: boolean;
}

interface AcpTerminalState {
	sessionId: string;
	pid: number;
	output: string;
	truncated: boolean;
	outputByteLimit: number;
}

export type RootLowerInput =
	| { kind: "bundled-base-filesystem" }
	| RootSnapshotExport;

export interface RootFilesystemConfig {
	type?: "overlay";
	mode?: OverlayFilesystemMode;
	disableDefaultBaseLayer?: boolean;
	lowers?: RootLowerInput[];
}

/** Configuration for mounting a filesystem driver at a path. */
export interface PlainMountConfig {
	/** Path inside the VM to mount at. */
	path: string;
	/** The filesystem driver to mount. */
	driver: VirtualFileSystem;
	/** If true, write operations throw EROFS. */
	readOnly?: boolean;
}

export interface OverlayMountConfig {
	path: string;
	filesystem: {
		type: "overlay";
		store: LayerStore;
		mode?: OverlayFilesystemMode;
		lowers: SnapshotLayerHandle[];
	};
}

export type MountConfig = PlainMountConfig | OverlayMountConfig;

export interface AgentOsOptions {
	/**
	 * Software to install in the VM. Each entry provides agents, tools,
	 * or WASM commands. Any object with a `commandDir` property (e.g.,
	 * registry packages like @rivet-dev/agent-os-coreutils) is treated
	 * as a WASM command source automatically. Arrays are flattened, so
	 * meta-packages that export arrays of sub-packages work directly.
	 */
	software?: SoftwareInput[];
	/** Loopback ports to exempt from SSRF checks (for testing with host-side mock servers). */
	loopbackExemptPorts?: number[];
	/**
	 * Host-side CWD for module access resolution. Sets the directory whose
	 * node_modules are projected into the VM at /root/node_modules/.
	 * Defaults to process.cwd().
	 */
	moduleAccessCwd?: string;
	/** Root filesystem configuration. Defaults to an overlay with the bundled base snapshot as its deepest lower. */
	rootFilesystem?: RootFilesystemConfig;
	/** Filesystems to mount at boot time. */
	mounts?: MountConfig[];
	/** Additional instructions appended to the base OS instructions written to /etc/agentos/instructions.md. */
	additionalInstructions?: string;
	/** Custom schedule driver for cron jobs. Defaults to TimerScheduleDriver. */
	scheduleDriver?: ScheduleDriver;
	/** Host-side toolkits available to agents inside the VM. */
	toolKits?: ToolKit[];
	/**
	 * Custom permission policy for the kernel. Controls access to filesystem,
	 * network, child process, and environment operations. Defaults to allowAll.
	 */
	permissions?: Permissions;
}

/** Configuration for a local MCP server (spawned as a child process). */
export interface McpServerConfigLocal {
	type: "local";
	/** Command to launch the MCP server. */
	command: string;
	/** Arguments for the command. */
	args?: string[];
	/** Environment variables for the server process. */
	env?: Record<string, string>;
}

/** Configuration for a remote MCP server (connected via URL). */
export interface McpServerConfigRemote {
	type: "remote";
	/** URL of the remote MCP server. */
	url: string;
	/** HTTP headers to include in requests to the server. */
	headers?: Record<string, string>;
}

export type McpServerConfig = McpServerConfigLocal | McpServerConfigRemote;

export interface CreateSessionOptions {
	/** Working directory for the agent session inside the VM. */
	cwd?: string;
	/** Environment variables to pass to the agent process. */
	env?: Record<string, string>;
	/** MCP servers to make available to the agent during the session. */
	mcpServers?: McpServerConfig[];
	/** Skip OS instructions injection entirely (default false). */
	skipOsInstructions?: boolean;
	/** Additional instructions appended to the base OS instructions. */
	additionalInstructions?: string;
}

export interface SessionInfo {
	sessionId: string;
	agentType: string;
}

/** Result from AgentOs.prompt(). */
export interface PromptResult {
	/** Raw JSON-RPC response from the ACP adapter. */
	response: JsonRpcResponse;
	/** Accumulated agent text output from streamed message chunks. */
	text: string;
}

/** Information about a process spawned via AgentOs.spawn(). */
export interface SpawnedProcessInfo {
	pid: number;
	command: string;
	args: string[];
	running: boolean;
	exitCode: number | null;
}

function isOverlayMountConfig(
	config: MountConfig,
): config is OverlayMountConfig {
	return "filesystem" in config;
}

const KERNEL_POSIX_BOOTSTRAP_DIRS = [
	"/dev",
	"/proc",
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
	"/usr/local",
	"/usr/local/bin",
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
	"/etc/agentos",
] as const;

const NODE_RUNTIME_BOOTSTRAP_COMMANDS = ["node", "npm", "npx"] as const;
const PYTHON_RUNTIME_BOOTSTRAP_COMMANDS = ["python", "python3", "pip"] as const;
const KERNEL_COMMAND_STUB = "#!/bin/sh\n# kernel command stub\n";

function isWasmBinaryFile(path: string): boolean {
	try {
		const header = readFileSync(path);
		return (
			header.length >= 4 &&
			header[0] === 0x00 &&
			header[1] === 0x61 &&
			header[2] === 0x73 &&
			header[3] === 0x6d
		);
	} catch {
		return false;
	}
}

function collectBootstrapWasmCommands(commandDirs: string[]): string[] {
	const commands: string[] = [];
	const seen = new Set<string>();

	for (const dir of commandDirs) {
		let entries: string[];
		try {
			entries = readdirSync(dir).sort((a, b) => a.localeCompare(b));
		} catch {
			continue;
		}

		for (const entry of entries) {
			if (entry.startsWith(".")) {
				continue;
			}

			const fullPath = join(dir, entry);
			try {
				if (statSync(fullPath).isDirectory()) {
					continue;
				}
			} catch {
				continue;
			}

			if (!isWasmBinaryFile(fullPath) || seen.has(entry)) {
				continue;
			}

			seen.add(entry);
			commands.push(entry);
		}
	}

	return commands;
}

function collectConfiguredLowerPaths(
	config?: RootFilesystemConfig,
): Set<string> {
	const paths = new Set<string>();

	for (const lower of config?.lowers ?? []) {
		if (lower.kind !== "snapshot-export") {
			continue;
		}
		for (const entry of lower.source.filesystem.entries) {
			paths.add(entry.path);
		}
	}

	if (!config?.disableDefaultBaseLayer) {
		for (const entry of getBaseFilesystemEntries()) {
			paths.add(entry.path);
		}
	}

	return paths;
}

function createKernelBootstrapLower(
	config: RootFilesystemConfig | undefined,
	commandNames: string[],
): RootSnapshotExport | null {
	const existingPaths = collectConfiguredLowerPaths(config);
	const entries: FilesystemEntry[] = [
		{
			path: "/",
			type: "directory",
			mode: "755",
			uid: 0,
			gid: 0,
		},
	];

	for (const dir of KERNEL_POSIX_BOOTSTRAP_DIRS) {
		if (existingPaths.has(dir)) {
			continue;
		}
		entries.push({
			path: dir,
			type: "directory",
			mode: "755",
			uid: 0,
			gid: 0,
		});
	}

	if (!existingPaths.has("/usr/bin/env")) {
		entries.push({
			path: "/usr/bin/env",
			type: "file",
			mode: "644",
			uid: 0,
			gid: 0,
			content: "AA==",
			encoding: "base64",
		});
	}

	const uniqueCommands = [...new Set(commandNames)].sort((a, b) =>
		a.localeCompare(b),
	);
	for (const command of uniqueCommands) {
		const stubPath = `/bin/${command}`;
		if (existingPaths.has(stubPath)) {
			continue;
		}
		entries.push({
			path: stubPath,
			type: "file",
			mode: "755",
			uid: 0,
			gid: 0,
			content: KERNEL_COMMAND_STUB,
			encoding: "utf8",
		});
	}

	return entries.length > 1 ? createSnapshotExport(entries) : null;
}

async function createRootFilesystem(
	config?: RootFilesystemConfig,
	bootstrapLower?: RootSnapshotExport | null,
): Promise<{
	filesystem: VirtualFileSystem;
	finishKernelBootstrap: () => void;
	rootView: VirtualFileSystem;
}> {
	const rootStore = createInMemoryLayerStore();
	const normalizedConfig = config ?? {};
	const lowerInputs = normalizedConfig.lowers
		? [...normalizedConfig.lowers]
		: [];

	if (bootstrapLower) {
		lowerInputs.push(bootstrapLower);
	}

	if (!normalizedConfig.disableDefaultBaseLayer) {
		lowerInputs.push({ kind: "bundled-base-filesystem" });
	}

	const lowers = await Promise.all(
		lowerInputs.map((lower) =>
			rootStore.importSnapshot(
				lower.kind === "bundled-base-filesystem"
					? createDefaultRootLowerInput()
					: lower,
			),
		),
	);

	const rootView =
		normalizedConfig.mode === "read-only"
			? rootStore.createOverlayFilesystem({
					mode: "read-only",
					lowers,
				})
			: rootStore.createOverlayFilesystem({
					upper: await rootStore.createWritableLayer(),
					lowers,
				});

	if (normalizedConfig.mode === "read-only") {
		return {
			filesystem: rootView,
			finishKernelBootstrap: () => {},
			rootView,
		};
	}

	const { filesystem, finishKernelBootstrap } = createBootstrapAwareFilesystem(
		rootView,
		rootView,
	);

	return {
		filesystem,
		finishKernelBootstrap,
		rootView,
	};
}

async function resolveMounts(
	mounts?: MountConfig[],
): Promise<Array<{ path: string; fs: VirtualFileSystem; readOnly?: boolean }>> {
	if (!mounts) {
		return [];
	}

	return Promise.all(
		mounts.map(async (mount) => {
			if (!isOverlayMountConfig(mount)) {
				return {
					path: mount.path,
					fs: mount.driver,
					readOnly: mount.readOnly,
				};
			}

			const mode = mount.filesystem.mode ?? "ephemeral";
			const fs =
				mode === "read-only"
					? mount.filesystem.store.createOverlayFilesystem({
							mode: "read-only",
							lowers: mount.filesystem.lowers,
						})
					: mount.filesystem.store.createOverlayFilesystem({
							upper: await mount.filesystem.store.createWritableLayer(),
							lowers: mount.filesystem.lowers,
						});

			return {
				path: mount.path,
				fs,
				readOnly: mode === "read-only",
			};
		}),
	);
}

export class AgentOs {
	readonly kernel: Kernel;
	private _sessions = new Map<string, Session>();
	private _processes = new Map<
		number,
		{
			proc: ManagedProcess;
			command: string;
			args: string[];
			stdoutHandlers: Set<(data: Uint8Array) => void>;
			stderrHandlers: Set<(data: Uint8Array) => void>;
			exitHandlers: Set<(exitCode: number) => void>;
		}
	>();
	private _shells = new Map<
		string,
		{
			handle: ShellHandle;
			dataHandlers: Set<(data: Uint8Array) => void>;
		}
	>();
	private _shellCounter = 0;
	private _moduleAccessCwd: string;
	private _softwareRoots: SoftwareRoot[];
	private _softwareAgentConfigs: Map<string, AgentConfig>;
	private _cronManager!: CronManager;
	private _toolsServer: HostToolsServer | null = null;
	private _toolKits: ToolKit[] = [];
	private _shimFs: ReturnType<typeof createInMemoryFileSystem> | null = null;
	private _hostMounts: HostMountInfo[];
	private _acpTerminals = new Map<string, AcpTerminalState>();
	private _acpTerminalCounter = 0;
	private _env: Record<string, string>;
	private _rootFilesystem: VirtualFileSystem;

	private constructor(
		kernel: Kernel,
		moduleAccessCwd: string,
		softwareRoots: SoftwareRoot[],
		softwareAgentConfigs: Map<string, AgentConfig>,
		hostMounts: HostMountInfo[],
		env: Record<string, string>,
		rootFilesystem: VirtualFileSystem,
	) {
		this.kernel = kernel;
		this._moduleAccessCwd = moduleAccessCwd;
		this._softwareRoots = softwareRoots;
		this._softwareAgentConfigs = softwareAgentConfigs;
		this._hostMounts = hostMounts;
		this._env = env;
		this._rootFilesystem = rootFilesystem;
	}

	static async create(options?: AgentOsOptions): Promise<AgentOs> {
		// Process software descriptors first so the root lower can include the
		// exact command stubs Secure Exec will register during boot.
		const processed = processSoftware(options?.software ?? []);
		const bootstrapLower = createKernelBootstrapLower(options?.rootFilesystem, [
			...collectBootstrapWasmCommands(processed.commandDirs),
			...NODE_RUNTIME_BOOTSTRAP_COMMANDS,
			...PYTHON_RUNTIME_BOOTSTRAP_COMMANDS,
		]);
		const { filesystem, finishKernelBootstrap, rootView } =
			await createRootFilesystem(options?.rootFilesystem, bootstrapLower);
		const hostNetworkAdapter = createNodeHostNetworkAdapter();
		const moduleAccessCwd = options?.moduleAccessCwd ?? process.cwd();

		const mounts = await resolveMounts(options?.mounts);
		const hostMounts = (options?.mounts ?? [])
			.flatMap((mount) => {
				if (isOverlayMountConfig(mount)) {
					return [];
				}
				const meta = getHostDirBackendMeta(mount.driver);
				if (!meta) {
					return [];
				}
				return [
					{
						vmPath: posixPath.normalize(mount.path),
						hostPath: meta.hostPath,
						readOnly: mount.readOnly ?? meta.readOnly,
					},
				];
			})
			.sort((a, b) => b.vmPath.length - a.vmPath.length);

		// Start host tools RPC server before kernel creation so the port
		// can be included in the kernel env and loopback exemptions.
		let toolsServer: HostToolsServer | null = null;
		const toolKits = options?.toolKits;
		if (toolKits && toolKits.length > 0) {
			validateToolkits(toolKits);
			toolsServer = await startHostToolsServer(toolKits);
		}

		const loopbackExemptPorts = [
			...(options?.loopbackExemptPorts ?? []),
			...(toolsServer ? [toolsServer.port] : []),
		];

		const env: Record<string, string> = getBaseEnvironment();
		if (toolsServer) {
			env.AGENTOS_TOOLS_PORT = String(toolsServer.port);
		}

		const kernel = createKernel({
			filesystem,
			hostNetworkAdapter,
			permissions: options?.permissions ?? allowAll,
			env,
			cwd: "/home/user",
			mounts,
		});

		// Mount OS instructions at /etc/agentos/ as a read-only filesystem
		// so agents cannot tamper with their own instructions.
		const etcAgentosFs = createInMemoryFileSystem();
		const instructions = getOsInstructions(options?.additionalInstructions);
		await etcAgentosFs.writeFile("instructions.md", instructions);
		kernel.mountFs("/etc/agentos", etcAgentosFs, { readOnly: true });

		// Mount CLI shims for host tools at /usr/local/bin so agents can
		// invoke tools via shell commands (agentos-{name} <tool> ...).
		let shimFs: ReturnType<typeof createInMemoryFileSystem> | null = null;
		if (toolKits && toolKits.length > 0) {
			shimFs = await createShimFilesystem(toolKits);
			kernel.mountFs("/usr/local/bin", shimFs, { readOnly: true });
		}

		await kernel.mount(
			createWasmVmRuntime(
				processed.commandDirs.length > 0
					? {
							commandDirs: processed.commandDirs,
							permissions: processed.commandPermissions,
						}
					: undefined,
			),
		);
		await kernel.mount(
			createNodeRuntime({
				bindings: await createSqliteBindings(kernel),
				loopbackExemptPorts,
				moduleAccessCwd,
				packageRoots:
					processed.softwareRoots.length > 0
						? processed.softwareRoots
						: undefined,
			}),
		);
		await kernel.mount(createPythonRuntime());
		finishKernelBootstrap();

		const vm = new AgentOs(
			kernel,
			moduleAccessCwd,
			processed.softwareRoots,
			processed.agentConfigs,
			hostMounts,
			env,
			rootView,
		);
		vm._toolsServer = toolsServer;
		vm._toolKits = toolKits ?? [];
		vm._shimFs = shimFs;
		vm._cronManager = new CronManager(
			vm,
			options?.scheduleDriver ?? new TimerScheduleDriver(),
		);

		return vm;
	}

	async exec(
		command: string,
		options?: KernelExecOptions,
	): Promise<KernelExecResult> {
		return this.kernel.exec(command, options);
	}

	private _trackProcess(
		proc: ManagedProcess,
		command: string,
		args: string[],
		stdoutHandlers: Set<(data: Uint8Array) => void>,
		stderrHandlers: Set<(data: Uint8Array) => void>,
		exitHandlers: Set<(exitCode: number) => void>,
	): { pid: number } {
		const entry = {
			proc,
			command,
			args,
			stdoutHandlers,
			stderrHandlers,
			exitHandlers,
		};
		this._processes.set(proc.pid, entry);

		proc.wait().then((code) => {
			for (const h of exitHandlers) h(code);
		});

		return { pid: proc.pid };
	}

	spawn(
		command: string,
		args: string[],
		options?: KernelSpawnOptions,
	): { pid: number } {
		const stdoutHandlers = new Set<(data: Uint8Array) => void>();
		const stderrHandlers = new Set<(data: Uint8Array) => void>();
		const exitHandlers = new Set<(exitCode: number) => void>();

		// Include caller-provided callbacks in the initial handler sets.
		if (options?.onStdout) stdoutHandlers.add(options.onStdout);
		if (options?.onStderr) stderrHandlers.add(options.onStderr);

		const proc = this.kernel.spawn(command, args, {
			...options,
			onStdout: (data) => {
				for (const h of stdoutHandlers) h(data);
			},
			onStderr: (data) => {
				for (const h of stderrHandlers) h(data);
			},
		});

		return this._trackProcess(
			proc,
			command,
			args,
			stdoutHandlers,
			stderrHandlers,
			exitHandlers,
		);
	}

	/** Write data to a process's stdin. */
	writeProcessStdin(pid: number, data: string | Uint8Array): void {
		const entry = this._processes.get(pid);
		if (!entry) throw new Error(`Process not found: ${pid}`);
		entry.proc.writeStdin(data);
	}

	/** Close a process's stdin stream. */
	closeProcessStdin(pid: number): void {
		const entry = this._processes.get(pid);
		if (!entry) throw new Error(`Process not found: ${pid}`);
		entry.proc.closeStdin();
	}

	/** Subscribe to stdout data from a process. Returns an unsubscribe function. */
	onProcessStdout(
		pid: number,
		handler: (data: Uint8Array) => void,
	): () => void {
		const entry = this._processes.get(pid);
		if (!entry) throw new Error(`Process not found: ${pid}`);
		entry.stdoutHandlers.add(handler);
		return () => {
			entry.stdoutHandlers.delete(handler);
		};
	}

	/** Subscribe to stderr data from a process. Returns an unsubscribe function. */
	onProcessStderr(
		pid: number,
		handler: (data: Uint8Array) => void,
	): () => void {
		const entry = this._processes.get(pid);
		if (!entry) throw new Error(`Process not found: ${pid}`);
		entry.stderrHandlers.add(handler);
		return () => {
			entry.stderrHandlers.delete(handler);
		};
	}

	/** Subscribe to process exit. Returns an unsubscribe function. */
	onProcessExit(pid: number, handler: (exitCode: number) => void): () => void {
		const entry = this._processes.get(pid);
		if (!entry) throw new Error(`Process not found: ${pid}`);
		// If already exited, call immediately.
		if (entry.proc.exitCode !== null) {
			handler(entry.proc.exitCode);
			return () => {};
		}
		entry.exitHandlers.add(handler);
		return () => {
			entry.exitHandlers.delete(handler);
		};
	}

	/** Wait for a process to exit. Returns the exit code. */
	waitProcess(pid: number): Promise<number> {
		const entry = this._processes.get(pid);
		if (!entry) throw new Error(`Process not found: ${pid}`);
		return entry.proc.wait();
	}

	private _assertSafeAbsolutePath(path: string): void {
		if (!path.startsWith("/")) {
			throw new Error(`Path must be absolute: ${path}`);
		}
		if (posixPath.normalize(path) !== path) {
			throw new Error(`Path must be normalized: ${path}`);
		}
	}

	private _vfs(): VirtualFileSystem {
		return (this.kernel as unknown as { vfs: VirtualFileSystem }).vfs;
	}

	private async _copyPath(from: string, to: string): Promise<void> {
		const stat = await this._vfs().lstat(from);
		if (stat.isSymbolicLink) {
			const target = await this._vfs().readlink(from);
			await this._vfs().symlink(target, to);
			return;
		}
		if (stat.isDirectory) {
			await this._mkdirp(posixPath.dirname(to));
			if (!(await this.kernel.exists(to))) {
				await this.kernel.mkdir(to);
			}
			await this._vfs().chmod(to, stat.mode);
			await this._vfs().chown(to, stat.uid, stat.gid);
			const entries = await this.kernel.readdir(from);
			for (const entry of entries) {
				if (entry === "." || entry === "..") continue;
				const fromPath = from === "/" ? `/${entry}` : `${from}/${entry}`;
				const toPath = to === "/" ? `/${entry}` : `${to}/${entry}`;
				await this._copyPath(fromPath, toPath);
			}
			return;
		}
		const content = await this.kernel.readFile(from);
		await this.writeFile(to, content);
		await this._vfs().chmod(to, stat.mode);
		await this._vfs().chown(to, stat.uid, stat.gid);
	}

	async readFile(path: string): Promise<Uint8Array> {
		this._assertSafeAbsolutePath(path);
		return this.kernel.readFile(path);
	}

	async writeFile(path: string, content: string | Uint8Array): Promise<void> {
		this._assertSafeAbsolutePath(path);
		return this.kernel.writeFile(path, content);
	}

	async writeFiles(entries: BatchWriteEntry[]): Promise<BatchWriteResult[]> {
		const results: BatchWriteResult[] = [];
		for (const entry of entries) {
			try {
				this._assertSafeAbsolutePath(entry.path);
				// Create parent directories as needed
				const parentDir = entry.path.substring(0, entry.path.lastIndexOf("/"));
				if (parentDir) {
					await this._mkdirp(parentDir);
				}
				await this.kernel.writeFile(entry.path, entry.content);
				results.push({ path: entry.path, success: true });
			} catch (err: unknown) {
				results.push({
					path: entry.path,
					success: false,
					error: err instanceof Error ? err.message : String(err),
				});
			}
		}
		return results;
	}

	async readFiles(paths: string[]): Promise<BatchReadResult[]> {
		const results: BatchReadResult[] = [];
		for (const path of paths) {
			try {
				this._assertSafeAbsolutePath(path);
				const content = await this.kernel.readFile(path);
				results.push({ path, content });
			} catch (err: unknown) {
				results.push({
					path,
					content: null,
					error: err instanceof Error ? err.message : String(err),
				});
			}
		}
		return results;
	}

	/** Recursively create directories (mkdir -p). */
	private async _mkdirp(path: string): Promise<void> {
		this._assertSafeAbsolutePath(path);
		const parts = path.split("/").filter(Boolean);
		let current = "";
		for (const part of parts) {
			current += `/${part}`;
			if (!(await this.kernel.exists(current))) {
				await this.kernel.mkdir(current);
			}
		}
	}

	async mkdir(path: string, options?: { recursive?: boolean }): Promise<void> {
		if (options?.recursive) {
			return this._mkdirp(path);
		}
		this._assertSafeAbsolutePath(path);
		return this.kernel.mkdir(path);
	}

	async readdir(path: string): Promise<string[]> {
		this._assertSafeAbsolutePath(path);
		return this.kernel.readdir(path);
	}

	async readdirRecursive(
		path: string,
		options?: ReaddirRecursiveOptions,
	): Promise<DirEntry[]> {
		this._assertSafeAbsolutePath(path);
		const maxDepth = options?.maxDepth;
		const exclude = options?.exclude ? new Set(options.exclude) : undefined;
		const results: DirEntry[] = [];

		// BFS queue: [dirPath, currentDepth]
		const queue: [string, number][] = [[path, 0]];

		while (queue.length > 0) {
			const item = queue.shift();
			if (!item) break;
			const [dirPath, depth] = item;
			const entries = await this.kernel.readdir(dirPath);

			for (const name of entries) {
				if (name === "." || name === "..") continue;
				if (exclude?.has(name)) continue;

				const fullPath = dirPath === "/" ? `/${name}` : `${dirPath}/${name}`;
				const s = await this.kernel.stat(fullPath);

				if (s.isSymbolicLink) {
					results.push({
						path: fullPath,
						type: "symlink",
						size: s.size,
					});
				} else if (s.isDirectory) {
					results.push({
						path: fullPath,
						type: "directory",
						size: s.size,
					});
					if (maxDepth === undefined || depth < maxDepth) {
						queue.push([fullPath, depth + 1]);
					}
				} else {
					results.push({
						path: fullPath,
						type: "file",
						size: s.size,
					});
				}
			}
		}

		return results;
	}

	async stat(path: string): Promise<VirtualStat> {
		this._assertSafeAbsolutePath(path);
		return this.kernel.stat(path);
	}

	async exists(path: string): Promise<boolean> {
		this._assertSafeAbsolutePath(path);
		return this.kernel.exists(path);
	}

	async snapshotRootFilesystem(): Promise<RootSnapshotExport> {
		return createSnapshotExport(
			await snapshotVirtualFilesystem(this._rootFilesystem),
		);
	}

	mountFs(
		path: string,
		driver: VirtualFileSystem,
		options?: { readOnly?: boolean },
	): void {
		this._assertSafeAbsolutePath(path);
		this.kernel.mountFs(path, driver, { readOnly: options?.readOnly });
	}

	unmountFs(path: string): void {
		this._assertSafeAbsolutePath(path);
		this.kernel.unmountFs(path);
	}

	async move(from: string, to: string): Promise<void> {
		this._assertSafeAbsolutePath(from);
		this._assertSafeAbsolutePath(to);
		const sourceStat = await this._vfs().lstat(from);
		if (!sourceStat.isDirectory || sourceStat.isSymbolicLink) {
			return this.kernel.rename(from, to);
		}
		await this._copyPath(from, to);
		await this.delete(from, { recursive: true });
	}

	async delete(path: string, options?: { recursive?: boolean }): Promise<void> {
		this._assertSafeAbsolutePath(path);
		const s = await this.kernel.stat(path);
		if (s.isDirectory) {
			if (options?.recursive) {
				const entries = await this.kernel.readdir(path);
				for (const entry of entries) {
					if (entry === "." || entry === "..") continue;
					await this.delete(`${path}/${entry}`, { recursive: true });
				}
			}
			return this.kernel.removeDir(path);
		}
		return this.kernel.removeFile(path);
	}

	async fetch(port: number, request: Request): Promise<Response> {
		const url = new URL(request.url);
		url.hostname = "127.0.0.1";
		url.port = String(port);
		url.protocol = "http:";

		return globalThis.fetch(
			new Request(url, {
				method: request.method,
				headers: request.headers,
				body: request.body,
				redirect: request.redirect,
				signal: request.signal,
			}),
		);
	}

	openShell(options?: OpenShellOptions): { shellId: string } {
		const shellId = `shell-${++this._shellCounter}`;
		const dataHandlers = new Set<(data: Uint8Array) => void>();

		const handle = this.kernel.openShell(options);
		handle.onData = (data) => {
			for (const h of dataHandlers) h(data);
		};

		this._shells.set(shellId, { handle, dataHandlers });
		return { shellId };
	}

	/** Write data to a shell's PTY input. */
	writeShell(shellId: string, data: string | Uint8Array): void {
		const entry = this._shells.get(shellId);
		if (!entry) throw new Error(`Shell not found: ${shellId}`);
		entry.handle.write(data);
	}

	/** Subscribe to data output from a shell. Returns an unsubscribe function. */
	onShellData(
		shellId: string,
		handler: (data: Uint8Array) => void,
	): () => void {
		const entry = this._shells.get(shellId);
		if (!entry) throw new Error(`Shell not found: ${shellId}`);
		entry.dataHandlers.add(handler);
		return () => {
			entry.dataHandlers.delete(handler);
		};
	}

	/** Notify a shell of terminal resize. */
	resizeShell(shellId: string, cols: number, rows: number): void {
		const entry = this._shells.get(shellId);
		if (!entry) throw new Error(`Shell not found: ${shellId}`);
		entry.handle.resize(cols, rows);
	}

	/** Kill a shell process and remove it from tracking. */
	closeShell(shellId: string): void {
		const entry = this._shells.get(shellId);
		if (!entry) throw new Error(`Shell not found: ${shellId}`);
		entry.handle.kill();
		this._shells.delete(shellId);
	}

	private _resolveVmPathToHostPath(vmPath: string): string | null {
		const normalizedVmPath = posixPath.normalize(vmPath);
		for (const mount of this._hostMounts) {
			if (
				normalizedVmPath === mount.vmPath ||
				normalizedVmPath.startsWith(`${mount.vmPath}/`)
			) {
				const relativePath = posixPath.relative(mount.vmPath, normalizedVmPath);
				if (!relativePath) {
					return mount.hostPath;
				}
				return join(mount.hostPath, ...relativePath.split("/").filter(Boolean));
			}
		}
		return null;
	}

	private _resolveHostPathToVmPath(hostPath: string): string | null {
		const normalizedHostPath = resolveHostPath(hostPath);
		for (const mount of this._hostMounts) {
			if (
				normalizedHostPath === mount.hostPath ||
				normalizedHostPath.startsWith(`${mount.hostPath}${hostPathSeparator}`)
			) {
				const relativePath = relativeHostPath(
					mount.hostPath,
					normalizedHostPath,
				);
				if (!relativePath) {
					return mount.vmPath;
				}
				return posixPath.join(
					mount.vmPath,
					...relativePath.split(hostPathSeparator).filter(Boolean),
				);
			}
		}
		return null;
	}

	private _normalizeClientPathToVmPath(clientPath: string): string {
		if (!clientPath.startsWith("/")) {
			throw new Error(`ACP path must be absolute: ${clientPath}`);
		}
		return (
			this._resolveHostPathToVmPath(clientPath) ??
			posixPath.normalize(clientPath)
		);
	}

	private _appendTerminalOutput(
		terminal: AcpTerminalState,
		data: Uint8Array,
	): void {
		terminal.output += new TextDecoder().decode(data);
		if (terminal.outputByteLimit <= 0) {
			terminal.output = "";
			terminal.truncated = true;
			return;
		}

		while (
			Buffer.byteLength(terminal.output, "utf8") > terminal.outputByteLimit
		) {
			terminal.output = terminal.output.slice(1);
			terminal.truncated = true;
		}
	}

	private async _handleInboundAcpRequest(
		request: JsonRpcRequest,
	): Promise<{ result?: unknown } | null> {
		const params =
			request.params && typeof request.params === "object"
				? (request.params as Record<string, unknown>)
				: {};

		switch (request.method) {
			case "fs/read_text_file": {
				const path = params.path;
				if (typeof path !== "string") {
					throw new Error("fs/read_text_file requires a string path");
				}
				const vmPath = this._normalizeClientPathToVmPath(path);
				const content = new TextDecoder().decode(await this.readFile(vmPath));
				const startLine = Math.max(
					1,
					typeof params.line === "number" ? params.line : 1,
				);
				const limit =
					typeof params.limit === "number" ? params.limit : undefined;
				const lines = content.split("\n");
				const sliced = lines.slice(
					startLine - 1,
					limit === undefined ? undefined : startLine - 1 + limit,
				);
				return { result: { content: sliced.join("\n") } };
			}
			case "fs/write_text_file": {
				const path = params.path;
				const content = params.content;
				if (typeof path !== "string" || typeof content !== "string") {
					throw new Error(
						"fs/write_text_file requires string path and content",
					);
				}
				await this.writeFile(this._normalizeClientPathToVmPath(path), content);
				return { result: null };
			}
			case "terminal/create": {
				const command = params.command;
				if (typeof command !== "string") {
					throw new Error("terminal/create requires a command");
				}
				const args = Array.isArray(params.args)
					? params.args.filter((arg): arg is string => typeof arg === "string")
					: [];
				const env = Array.isArray(params.env)
					? Object.fromEntries(
							params.env
								.map((entry) => {
									if (
										!entry ||
										typeof entry !== "object" ||
										typeof (entry as { name?: unknown }).name !== "string" ||
										typeof (entry as { value?: unknown }).value !== "string"
									) {
										return null;
									}
									return [
										(entry as { name: string }).name,
										(entry as { value: string }).value,
									];
								})
								.filter((entry): entry is [string, string] =>
									Array.isArray(entry),
								),
						)
					: undefined;
				const cwd =
					typeof params.cwd === "string"
						? this._normalizeClientPathToVmPath(params.cwd)
						: undefined;
				const outputByteLimit =
					typeof params.outputByteLimit === "number"
						? params.outputByteLimit
						: 1_048_576;
				const terminalId = `acp-term-${++this._acpTerminalCounter}`;
				const { pid } = this.spawn(command, args, {
					cwd,
					env,
					onStdout: (data) => {
						const terminal = this._acpTerminals.get(terminalId);
						if (terminal) {
							this._appendTerminalOutput(terminal, data);
						}
					},
					onStderr: (data) => {
						const terminal = this._acpTerminals.get(terminalId);
						if (terminal) {
							this._appendTerminalOutput(terminal, data);
						}
					},
				});
				this._acpTerminals.set(terminalId, {
					sessionId:
						typeof params.sessionId === "string" ? params.sessionId : "",
					pid,
					output: "",
					truncated: false,
					outputByteLimit,
				});
				return { result: { terminalId } };
			}
			case "terminal/output": {
				const terminalId = params.terminalId;
				if (typeof terminalId !== "string") {
					throw new Error("terminal/output requires a terminalId");
				}
				const terminal = this._acpTerminals.get(terminalId);
				if (!terminal) {
					throw new Error(`ACP terminal not found: ${terminalId}`);
				}
				const proc = this.getProcess(terminal.pid);
				return {
					result: {
						output: terminal.output,
						truncated: terminal.truncated,
						exitStatus:
							proc.exitCode === null
								? undefined
								: { exitCode: proc.exitCode, signal: null },
					},
				};
			}
			case "terminal/wait_for_exit": {
				const terminalId = params.terminalId;
				if (typeof terminalId !== "string") {
					throw new Error("terminal/wait_for_exit requires a terminalId");
				}
				const terminal = this._acpTerminals.get(terminalId);
				if (!terminal) {
					throw new Error(`ACP terminal not found: ${terminalId}`);
				}
				const exitCode = await this.waitProcess(terminal.pid);
				return { result: { exitCode, signal: null } };
			}
			case "terminal/kill": {
				const terminalId = params.terminalId;
				if (typeof terminalId !== "string") {
					throw new Error("terminal/kill requires a terminalId");
				}
				const terminal = this._acpTerminals.get(terminalId);
				if (!terminal) {
					throw new Error(`ACP terminal not found: ${terminalId}`);
				}
				this.killProcess(terminal.pid);
				return { result: null };
			}
			case "terminal/release": {
				const terminalId = params.terminalId;
				if (typeof terminalId !== "string") {
					throw new Error("terminal/release requires a terminalId");
				}
				const terminal = this._acpTerminals.get(terminalId);
				if (!terminal) {
					throw new Error(`ACP terminal not found: ${terminalId}`);
				}
				if (this.getProcess(terminal.pid).exitCode === null) {
					this.killProcess(terminal.pid);
				}
				this._acpTerminals.delete(terminalId);
				return { result: null };
			}
			default:
				return null;
		}
	}

	/** Returns info about all processes spawned via spawn(). */
	listProcesses(): SpawnedProcessInfo[] {
		return [...this._processes.values()].map(({ proc, command, args }) => ({
			pid: proc.pid,
			command,
			args,
			running: proc.exitCode === null,
			exitCode: proc.exitCode,
		}));
	}

	/** Returns all kernel processes across all runtimes (WASM, Node, Python). */
	allProcesses(): KernelProcessInfo[] {
		return [...this.kernel.processes.values()];
	}

	/** Returns processes organized as a tree using ppid relationships. */
	processTree(): ProcessTreeNode[] {
		const all = this.allProcesses();
		const nodeMap = new Map<number, ProcessTreeNode>();

		// Index: create a tree node for each process
		for (const proc of all) {
			nodeMap.set(proc.pid, { ...proc, children: [] });
		}

		// Wire: attach each node to its parent
		const roots: ProcessTreeNode[] = [];
		for (const node of nodeMap.values()) {
			const parent = nodeMap.get(node.ppid);
			if (parent) {
				parent.children.push(node);
			} else {
				roots.push(node);
			}
		}

		return roots;
	}

	/** Returns info about a specific process by PID. Throws if not found. */
	getProcess(pid: number): SpawnedProcessInfo {
		const entry = this._processes.get(pid);
		if (!entry) {
			throw new Error(`Process not found: ${pid}`);
		}
		return {
			pid: entry.proc.pid,
			command: entry.command,
			args: entry.args,
			running: entry.proc.exitCode === null,
			exitCode: entry.proc.exitCode,
		};
	}

	/** Send SIGTERM to gracefully stop a process. No-op if already exited. */
	stopProcess(pid: number): void {
		const entry = this._processes.get(pid);
		if (!entry) {
			throw new Error(`Process not found: ${pid}`);
		}
		if (entry.proc.exitCode !== null) return;
		entry.proc.kill();
	}

	/** Send SIGKILL to force-kill a process. No-op if already exited. */
	killProcess(pid: number): void {
		const entry = this._processes.get(pid);
		if (!entry) {
			throw new Error(`Process not found: ${pid}`);
		}
		if (entry.proc.exitCode !== null) return;
		entry.proc.kill(9);
	}

	/** Returns all active sessions with their IDs and agent types. */
	listSessions(): SessionInfo[] {
		return [...this._sessions.values()].map((s) => ({
			sessionId: s.sessionId,
			agentType: s.agentType,
		}));
	}

	/** Internal helper: retrieve a session or throw. */
	private _requireSession(sessionId: string): Session {
		const session = this._sessions.get(sessionId);
		if (!session) {
			throw new Error(`Session not found: ${sessionId}`);
		}
		return session;
	}

	/** Returns all registered agents with their installation status. */
	listAgents(): AgentRegistryEntry[] {
		// Collect all agent IDs from both package configs and hardcoded configs.
		const allIds = new Set<string>([
			...this._softwareAgentConfigs.keys(),
			...Object.keys(AGENT_CONFIGS),
		]);

		return [...allIds]
			.map((id) => {
				const config = this._resolveAgentConfig(id);
				if (!config) return null;

				let installed = false;
				try {
					// Check package roots first, then CWD-based node_modules.
					const vmPrefix = `/root/node_modules/${config.acpAdapter}`;
					let hostPkgJsonPath: string | null = null;
					for (const root of this._softwareRoots) {
						if (root.vmPath === vmPrefix) {
							hostPkgJsonPath = join(root.hostPath, "package.json");
							break;
						}
					}
					if (!hostPkgJsonPath) {
						hostPkgJsonPath = join(
							this._moduleAccessCwd,
							"node_modules",
							config.acpAdapter,
							"package.json",
						);
					}
					readFileSync(hostPkgJsonPath);
					installed = true;
				} catch {
					// Package not installed
				}
				return {
					id: id as AgentType,
					acpAdapter: config.acpAdapter,
					agentPackage: config.agentPackage,
					installed,
				};
			})
			.filter((entry): entry is AgentRegistryEntry => entry !== null);
	}

	private _deriveSessionConfigOptions(
		agentType: string,
		sessionResult: Record<string, unknown> | undefined,
	): SessionConfigOption[] {
		const models =
			sessionResult?.models && typeof sessionResult.models === "object"
				? (sessionResult.models as Record<string, unknown>)
				: null;
		if (!models) {
			return [];
		}

		const currentModelId =
			typeof models.currentModelId === "string"
				? models.currentModelId
				: undefined;
		const allowedValues = Array.isArray(models.availableModels)
			? models.availableModels.reduce<Array<{ id: string; label?: string }>>(
					(acc, model) => {
						if (!model || typeof model !== "object") {
							return acc;
						}
						const modelId = (model as { modelId?: unknown }).modelId;
						const name = (model as { name?: unknown }).name;
						if (typeof modelId !== "string") {
							return acc;
						}
						acc.push({
							id: modelId,
							label: typeof name === "string" ? name : undefined,
						});
						return acc;
					},
					[],
				)
			: [];

		if (!currentModelId && allowedValues.length === 0) {
			return [];
		}

		return [
			{
				id: "model",
				category: "model",
				label: "Model",
				description:
					agentType === "opencode"
						? "Available models reported by OpenCode. Model switching must be configured before createSession() because ACP session/set_config_option is not implemented."
						: undefined,
				currentValue: currentModelId,
				allowedValues,
				readOnly: agentType === "opencode",
			},
		];
	}

	/**
	 * Spawn an ACP-compatible coding agent inside the VM and return a Session.
	 *
	 * 1. Resolves the adapter binary from mounted node_modules
	 * 2. Spawns it with streaming stdin and stdout capture
	 * 3. Sends initialize + session/new
	 * 4. Returns a Session for prompt/cancel/close
	 */
	async createSession(
		agentType: AgentType | string,
		options?: CreateSessionOptions,
	): Promise<{ sessionId: string }> {
		const config = this._resolveAgentConfig(agentType);
		if (!config) {
			throw new Error(`Unknown agent type: ${agentType}`);
		}

		// Generate tool reference from VM-level toolkits. This is always
		// injected into the agent prompt, even when skipOsInstructions is true.
		const toolReference =
			this._toolKits.length > 0
				? generateToolReference(this._toolKits)
				: undefined;

		// Prepare OS instructions injection. When skipOsInstructions is true,
		// the base OS instructions are skipped but tool docs are still injected.
		let extraArgs: string[] = [];
		let extraEnv: Record<string, string> = {};
		if (config.prepareInstructions) {
			const cwd = options?.cwd ?? "/home/user";
			const skipBase = options?.skipOsInstructions ?? false;
			const hasToolRef = !!toolReference;

			if (!skipBase || hasToolRef) {
				const prepared = await config.prepareInstructions(
					this.kernel,
					cwd,
					skipBase ? undefined : options?.additionalInstructions,
					{ toolReference, skipBase },
				);
				if (prepared.args) extraArgs = prepared.args;
				if (prepared.env) extraEnv = prepared.env;
			}
		}

		// Create stdout line iterable wired via onStdout callback
		const { iterable, onStdout } = createStdoutLineIterable();
		const launchArgs = [...(config.launchArgs ?? []), ...extraArgs];
		const launchEnv = { ...config.defaultEnv, ...extraEnv, ...options?.env };
		const sessionCwd = options?.cwd ?? "/home/user";
		const binPath = this._resolveAdapterBin(config.acpAdapter);
		const pid = this.spawn("node", [binPath, ...launchArgs], {
			streamStdin: true,
			onStdout,
			env: launchEnv,
			cwd: options?.cwd,
		}).pid;

		const proc = this._processes.get(pid)!.proc;
		const client = new AcpClient(proc, iterable, {
			requestHandler: (request) => this._handleInboundAcpRequest(request),
		});

		let initResponse: JsonRpcResponse;
		let sessionResponse: JsonRpcResponse;
		try {
			initResponse = await client.request("initialize", {
				protocolVersion: 1,
				clientCapabilities: {
					fs: {
						readTextFile: true,
						writeTextFile: true,
					},
					terminal: true,
				},
			});
			if (initResponse.error) {
				throw new Error(`ACP initialize failed: ${initResponse.error.message}`);
			}

			sessionResponse = await client.request("session/new", {
				cwd: sessionCwd,
				mcpServers: options?.mcpServers ?? [],
			});
			if (sessionResponse.error) {
				throw new Error(
					`ACP session/new failed: ${sessionResponse.error.message}`,
				);
			}
		} catch (error) {
			client.close();
			throw error;
		}

		const sessionId = (sessionResponse.result as { sessionId: string })
			.sessionId;

		// Extract initialize-scoped metadata, then allow session/new to
		// override with session-scoped modes/config options when present.
		const initResult = initResponse.result as
			| Record<string, unknown>
			| undefined;
		const sessionResult = sessionResponse.result as
			| Record<string, unknown>
			| undefined;
		const initData: SessionInitData = {};
		if (initResult) {
			if (initResult.modes) {
				initData.modes = initResult.modes as SessionInitData["modes"];
			}
			if (initResult.configOptions) {
				initData.configOptions =
					initResult.configOptions as SessionInitData["configOptions"];
			}
			if (initResult.agentCapabilities) {
				initData.capabilities =
					initResult.agentCapabilities as SessionInitData["capabilities"];
			}
			if (initResult.agentInfo) {
				initData.agentInfo =
					initResult.agentInfo as SessionInitData["agentInfo"];
			}
		}
		if (sessionResult) {
			if (sessionResult.modes) {
				initData.modes = sessionResult.modes as SessionInitData["modes"];
			}
			if (sessionResult.configOptions) {
				initData.configOptions =
					sessionResult.configOptions as SessionInitData["configOptions"];
			}
		}
		const derivedConfigOptions = this._deriveSessionConfigOptions(
			agentType,
			sessionResult,
		);
		if (derivedConfigOptions.length > 0) {
			initData.configOptions = [
				...(initData.configOptions ?? []),
				...derivedConfigOptions,
			];
		}

		const session = new Session(client, sessionId, agentType, initData, () => {
			for (const [terminalId, terminal] of this._acpTerminals) {
				if (terminal.sessionId !== sessionId) {
					continue;
				}
				if (this.getProcess(terminal.pid).exitCode === null) {
					this.killProcess(terminal.pid);
				}
				this._acpTerminals.delete(terminalId);
			}
			this._sessions.delete(sessionId);
		});
		this._sessions.set(sessionId, session);

		return { sessionId };
	}

	/**
	 * Resolve the VM bin entry point of an ACP adapter package.
	 * Reads from the host filesystem since kernel.readFile() does NOT see
	 * the ModuleAccessFileSystem overlay.
	 */
	private _resolveAdapterBin(adapterPackage: string): string {
		const vmPrefix = `/root/node_modules/${adapterPackage}`;
		let hostPkgJsonPath: string | null = null;
		for (const root of this._softwareRoots) {
			if (root.vmPath === vmPrefix) {
				hostPkgJsonPath = join(root.hostPath, "package.json");
				break;
			}
		}
		// Fall back to CWD-based node_modules.
		if (!hostPkgJsonPath) {
			hostPkgJsonPath = join(
				this._moduleAccessCwd,
				"node_modules",
				adapterPackage,
				"package.json",
			);
		}
		const pkg = JSON.parse(readFileSync(hostPkgJsonPath, "utf-8"));

		let binEntry: string | undefined;
		if (typeof pkg.bin === "string") {
			binEntry = pkg.bin;
		} else if (typeof pkg.bin === "object" && pkg.bin !== null) {
			binEntry =
				(pkg.bin as Record<string, string>)[adapterPackage] ??
				Object.values(pkg.bin)[0];
		}

		if (!binEntry) {
			throw new Error(`No bin entry found in ${adapterPackage}/package.json`);
		}

		return `${vmPrefix}/${binEntry}`;
	}

	/**
	 * Resolve an agent config by ID. Package-provided configs take
	 * precedence over the hardcoded AGENT_CONFIGS.
	 */
	private _resolveAgentConfig(agentType: string): AgentConfig | undefined {
		return (
			this._softwareAgentConfigs.get(agentType) ??
			(AGENT_CONFIGS as Record<string, AgentConfig>)[agentType]
		);
	}

	/**
	 * Verify a session exists and is active.
	 * Throws if the session is not found.
	 */
	resumeSession(sessionId: string): { sessionId: string } {
		this._requireSession(sessionId);
		return { sessionId };
	}

	/**
	 * Gracefully destroy a session: cancel any pending work, close the client,
	 * and remove from tracking. Unlike close() which is abrupt, this attempts
	 * a graceful shutdown sequence.
	 */
	async destroySession(sessionId: string): Promise<void> {
		const session = this._sessions.get(sessionId);
		if (!session) {
			throw new Error(`Session not found: ${sessionId}`);
		}

		// Attempt graceful cancel before closing (ignore errors)
		try {
			await session.cancel();
		} catch {
			// No pending work or already closed — ignore
		}

		session.close();
	}

	// ── Flat session API (ID-based) ───────────────────────────────

	/** Send a prompt to the agent and wait for the final response.
	 *  Returns the raw JSON-RPC response and the accumulated agent text. */
	async prompt(sessionId: string, text: string): Promise<PromptResult> {
		const session = this._requireSession(sessionId);

		// Collect streamed text while the prompt is running
		let agentText = "";
		const handler: SessionEventHandler = (event) => {
			const params = event.params as Record<string, unknown> | undefined;
			const update = params?.update as Record<string, unknown> | undefined;
			if (update?.sessionUpdate === "agent_message_chunk") {
				const content = update.content as { text?: string } | undefined;
				if (content?.text) agentText += content.text;
			}
		};
		session.onSessionEvent(handler);

		try {
			const response = await session.prompt(text);
			return { response, text: agentText };
		} finally {
			session.removeSessionEventHandler(handler);
		}
	}

	/** Cancel ongoing agent work for a session. */
	async cancelSession(sessionId: string): Promise<JsonRpcResponse> {
		return this._requireSession(sessionId).cancel();
	}

	/** Kill the agent process and clear event history for a session. */
	closeSession(sessionId: string): void {
		this._requireSession(sessionId).close();
	}

	/** Returns the sequenced event history for a session. */
	getSessionEvents(
		sessionId: string,
		options?: GetEventsOptions,
	): SequencedEvent[] {
		return this._requireSession(sessionId).getSequencedEvents(options);
	}

	/** Respond to a permission request from an agent. */
	async respondPermission(
		sessionId: string,
		permissionId: string,
		reply: PermissionReply,
	): Promise<JsonRpcResponse> {
		return this._requireSession(sessionId).respondPermission(
			permissionId,
			reply,
		);
	}

	/** Set the session mode (e.g., "plan", "normal"). */
	async setSessionMode(
		sessionId: string,
		modeId: string,
	): Promise<JsonRpcResponse> {
		return this._requireSession(sessionId).setMode(modeId);
	}

	/** Returns available modes from the agent's reported capabilities. */
	getSessionModes(sessionId: string): SessionModeState | null {
		return this._requireSession(sessionId).getModes();
	}

	/** Set the model for a session. */
	async setSessionModel(
		sessionId: string,
		model: string,
	): Promise<JsonRpcResponse> {
		return this._requireSession(sessionId).setModel(model);
	}

	/** Set the thought/reasoning level for a session. */
	async setSessionThoughtLevel(
		sessionId: string,
		level: string,
	): Promise<JsonRpcResponse> {
		return this._requireSession(sessionId).setThoughtLevel(level);
	}

	/** Returns available config options for a session. */
	getSessionConfigOptions(sessionId: string): SessionConfigOption[] {
		return this._requireSession(sessionId).getConfigOptions();
	}

	/** Returns the agent's capability flags for a session. */
	getSessionCapabilities(sessionId: string): AgentCapabilities | null {
		const caps = this._requireSession(sessionId).capabilities;
		return Object.keys(caps).length > 0 ? caps : null;
	}

	/** Returns agent identity information for a session. */
	getSessionAgentInfo(sessionId: string): AgentInfo | null {
		return this._requireSession(sessionId).agentInfo;
	}

	/** Send an arbitrary JSON-RPC request to a session's agent. */
	async rawSessionSend(
		sessionId: string,
		method: string,
		params?: Record<string, unknown>,
	): Promise<JsonRpcResponse> {
		return this._requireSession(sessionId).rawSend(method, params);
	}

	/** Subscribe to session/update notifications for a session. Returns an unsubscribe function. */
	onSessionEvent(sessionId: string, handler: SessionEventHandler): () => void {
		const session = this._requireSession(sessionId);
		session.onSessionEvent(handler);
		return () => {
			session.removeSessionEventHandler(handler);
		};
	}

	/** Subscribe to permission requests for a session. Returns an unsubscribe function. */
	onPermissionRequest(
		sessionId: string,
		handler: PermissionRequestHandler,
	): () => void {
		const session = this._requireSession(sessionId);
		session.onPermissionRequest(handler);
		return () => {
			session.removePermissionRequestHandler(handler);
		};
	}

	// ── Cron ────────────────────────────────────────────────────

	/** Schedule a cron job. Returns a handle with the job ID and a cancel method. */
	scheduleCron(options: CronJobOptions): CronJob {
		return this._cronManager.schedule(options);
	}

	/** List all registered cron jobs. */
	listCronJobs(): CronJobInfo[] {
		return this._cronManager.list();
	}

	/** Cancel a cron job by ID. */
	cancelCronJob(id: string): void {
		this._cronManager.cancel(id);
	}

	/** Subscribe to cron lifecycle events (fire, complete, error). */
	onCronEvent(handler: CronEventHandler): void {
		this._cronManager.onEvent(handler);
	}

	async dispose(): Promise<void> {
		// Cancel all cron jobs first
		this._cronManager.dispose();

		// Close all active sessions before disposing the kernel
		for (const session of this._sessions.values()) {
			session.close();
		}
		this._sessions.clear();

		// Kill all tracked shells
		for (const [id, entry] of this._shells) {
			entry.handle.kill();
		}
		this._shells.clear();

		for (const terminal of this._acpTerminals.values()) {
			if (this.getProcess(terminal.pid).exitCode === null) {
				this.killProcess(terminal.pid);
			}
		}
		this._acpTerminals.clear();

		// Shut down the host tools RPC server
		if (this._toolsServer) {
			await this._toolsServer.close();
			this._toolsServer = null;
		}

		return this.kernel.dispose();
	}
}
