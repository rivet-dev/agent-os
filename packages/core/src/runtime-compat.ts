import { execFileSync } from "node:child_process";
import * as fsSync from "node:fs";
import * as fs from "node:fs/promises";
import * as path from "node:path";
import * as posixPath from "node:path/posix";
import { fileURLToPath } from "node:url";
import type {
	CreateVmConfig,
	RootFilesystemEntry as VmConfigRootFilesystemEntry,
} from "@rivet-dev/agentos-runtime-core/vm-config";
import type { NodeModulesMountConfig } from "./host-dir-mount.js";
import { resolvePublishedSidecarBinary } from "./sidecar/binary.js";
import { findCargoBinary, resolveCargoBinary } from "./sidecar/cargo.js";
import { serializePermissionsForSidecar } from "./sidecar/permissions.js";
import {
	type AuthenticatedSession,
	type CreatedVm,
	type LocalCompatMount,
	NativeSidecarKernelProxy,
	type RootFilesystemEntry,
	type SidecarMountDescriptor,
	SidecarProcess,
	serializeMountConfigForSidecar,
} from "./sidecar/rpc-client.js";

export const AF_INET = 2;
export const AF_UNIX = 1;
export const SOCK_STREAM = 1;
export const SOCK_DGRAM = 2;
export const SIGTERM = 15;

const NODE_RUNTIME_BOOTSTRAP_COMMANDS = [
	"node",
	"npm",
	"npx",
	"python",
	"python3",
] as const;
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
	"/home/agentos",
	"/workspace",
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
] as const;
const REPO_ROOT = fileURLToPath(new URL("../../..", import.meta.url));
const SIDECAR_BINARY = path.join(REPO_ROOT, "target/debug/agentos-sidecar");
const SIDECAR_BUILD_INPUTS = [
	path.join(REPO_ROOT, "Cargo.toml"),
	path.join(REPO_ROOT, "Cargo.lock"),
	path.join(REPO_ROOT, "crates/bridge"),
	path.join(REPO_ROOT, "crates/build-support"),
	path.join(REPO_ROOT, "crates/execution"),
	path.join(REPO_ROOT, "crates/kernel"),
	path.join(REPO_ROOT, "crates/agentos-protocol"),
	path.join(REPO_ROOT, "crates/agentos-sidecar"),
	path.join(REPO_ROOT, "crates/native-sidecar"),
	path.join(REPO_ROOT, "crates/native-sidecar-core"),
	path.join(REPO_ROOT, "crates/sidecar-protocol"),
	path.join(REPO_ROOT, "crates/v8-runtime"),
	path.join(REPO_ROOT, "crates/vfs"),
	path.join(REPO_ROOT, "crates/vm-config"),
	path.join(REPO_ROOT, "packages/build-tools/bridge-src"),
	path.join(REPO_ROOT, "packages/build-tools/package.json"),
	path.join(REPO_ROOT, "packages/build-tools/scripts/build-v8-bridge.mjs"),
	path.join(REPO_ROOT, "packages/core/fixtures/base-filesystem.json"),
	path.join(REPO_ROOT, "packages/runtime-core/fixtures/base-filesystem.json"),
	path.join(REPO_ROOT, "pnpm-lock.yaml"),
] as const;
let ensuredSidecarBinary: string | null = null;

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
	sizeExact?: bigint;
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
	inoExact?: bigint;
	nlink: number;
	nlinkExact?: bigint;
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
	chown(
		path: string,
		uid: number,
		gid: number,
		options?: { followSymlinks?: boolean },
	): Promise<void>;
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
export type ProcessPermissions =
	| PermissionMode
	| RulePermissions<PatternPermissionRule>;
export type EnvPermissions =
	| PermissionMode
	| RulePermissions<PatternPermissionRule>;
export type BindingPermissions =
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
	writeStdin(data: Uint8Array | string): Promise<void>;
	closeStdin(): Promise<void>;
	kill(signal?: number): void;
	wait(): Promise<number>;
	readonly exitCode: number | null;
}

export interface ShellHandle {
	pid: number;
	write(data: Uint8Array | string): Promise<void>;
	/** Ordered PTY output containing stdout and stderr exactly once. */
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
	/** Optional stderr-only diagnostic tap; do not render it alongside `onData`. */
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
	process?: ProcessPermissions;
	env?: EnvPermissions;
	binding?: BindingPermissions;
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
	mounts: readonly NodeModulesMountConfig[];
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

export interface KernelRecursiveDirEntry {
	name: string;
	path: string;
	isDirectory: boolean;
	isSymbolicLink: boolean;
	size: number;
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
	): void | Promise<void>;
	unmountFs(path: string): void | Promise<void>;
	readFile(path: string): Promise<Uint8Array>;
	writeFile(path: string, content: string | Uint8Array): Promise<void>;
	mkdir(path: string): Promise<void>;
	readdir(path: string): Promise<string[]>;
	readdirRecursive(
		path: string,
		options?: { maxDepth?: number },
	): Promise<KernelRecursiveDirEntry[]>;
	stat(path: string): Promise<VirtualStat>;
	exists(path: string): Promise<boolean>;
	removeFile(path: string): Promise<void>;
	removeDir(path: string): Promise<void>;
	removePath(path: string, options?: { recursive?: boolean }): Promise<void>;
	rename(oldPath: string, newPath: string): Promise<void>;
	movePath(oldPath: string, newPath: string): Promise<void>;
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

export interface NodeDriverOptions {
	filesystem?: VirtualFileSystem;
	networkAdapter?: NetworkAdapter;
	commandExecutor?: CommandExecutor;
	permissions?: Permissions;
	mounts?: readonly NodeModulesMountConfig[];
	processConfig?: ProcessConfig;
	osConfig?: OSConfig;
}

export interface DefaultNetworkAdapterOptions {
	loopbackExemptPorts?: number[];
}

export interface NodeRuntimeOptions {
	systemDriver?: SystemDriver;
	runtimeDriverFactory?: NodeRuntimeDriverFactory;
	permissions?: Partial<Permissions>;
	memoryLimit?: number;
	bindings?: BindingTree;
	loopbackExemptPorts?: number[];
}

export type NodeRuntimeDriverFactoryOptions = Record<string, never>;
export type NodeExecutionDriverOptions = RuntimeDriverOptions;

export interface KernelRuntimeDriver {
	readonly kind: "node" | "wasmvm";
	readonly name: string;
	readonly commands: string[];
	readonly commandDirs?: string[];
	init?(kernel: KernelInterface): Promise<void> | void;
	tryResolve?(command: string): boolean;
	getGuestCommandPaths?(startIndex: number): ReadonlyMap<string, string>;
	recordModuleExecution?(command: string): void;
}

export type DriverProcess = ManagedProcess;
export type ProcessContext = Record<string, never>;

export class KernelError extends Error {
	readonly code: string;

	constructor(code: string, message: string) {
		super(message.startsWith(`${code}:`) ? message : `${code}: ${message}`);
		this.name = "KernelError";
		this.code = code;
	}
}

function normalizePath(inputPath: string): string {
	if (!inputPath) return "/";
	let normalized = inputPath.startsWith("/") ? inputPath : `/${inputPath}`;
	normalized = normalized.replace(/\/+/g, "/");
	if (normalized.length > 1 && normalized.endsWith("/")) {
		normalized = normalized.slice(0, -1);
	}
	const parts = normalized.split("/");
	const resolved: string[] = [];
	for (const part of parts) {
		if (part === "" || part === ".") continue;
		if (part === "..") {
			resolved.pop();
			continue;
		}
		resolved.push(part);
	}
	return resolved.length === 0 ? "/" : `/${resolved.join("/")}`;
}

function dirnameVirtual(inputPath: string): string {
	const normalized = normalizePath(inputPath);
	if (normalized === "/") return "/";
	const parts = normalized.split("/").filter(Boolean);
	return parts.length <= 1 ? "/" : `/${parts.slice(0, -1).join("/")}`;
}

export class NodeFileSystem implements VirtualFileSystem {
	readonly rootPath: string;

	constructor(options: { root: string }) {
		this.rootPath = fsSync.realpathSync(options.root);
	}

	private normalizeTarget(targetPath: string): string {
		const normalized = normalizePath(targetPath).replace(/^\/+/, "");
		const resolved = path.resolve(this.rootPath, normalized);
		if (
			resolved !== this.rootPath &&
			!resolved.startsWith(`${this.rootPath}${path.sep}`)
		) {
			throw errnoError("EACCES", `path escapes root '${targetPath}'`);
		}
		return resolved;
	}

	private toStat(stat: fsSync.Stats): VirtualStat {
		const posixStat = stat as fsSync.Stats & {
			blocks?: number;
			dev?: number;
			rdev?: number;
		};
		return {
			mode: stat.mode,
			size: stat.size,
			blocks:
				posixStat.blocks ?? (stat.size === 0 ? 0 : Math.ceil(stat.size / 512)),
			dev: posixStat.dev ?? 1,
			rdev: posixStat.rdev ?? 0,
			isDirectory: stat.isDirectory(),
			isSymbolicLink: stat.isSymbolicLink(),
			atimeMs: Math.trunc(stat.atimeMs),
			mtimeMs: Math.trunc(stat.mtimeMs),
			ctimeMs: Math.trunc(stat.ctimeMs),
			birthtimeMs: Math.trunc(stat.birthtimeMs),
			ino: stat.ino,
			nlink: stat.nlink,
			uid: stat.uid,
			gid: stat.gid,
		};
	}

	async readFile(targetPath: string): Promise<Uint8Array> {
		return new Uint8Array(await fs.readFile(this.normalizeTarget(targetPath)));
	}

	async readTextFile(targetPath: string): Promise<string> {
		return fs.readFile(this.normalizeTarget(targetPath), "utf8");
	}

	async readDir(targetPath: string): Promise<string[]> {
		return fs.readdir(this.normalizeTarget(targetPath));
	}

	async readDirWithTypes(targetPath: string): Promise<VirtualDirEntry[]> {
		const entries = await fs.readdir(this.normalizeTarget(targetPath), {
			withFileTypes: true,
		});
		return entries.map((entry) => ({
			name: entry.name,
			isDirectory: entry.isDirectory(),
			isSymbolicLink: entry.isSymbolicLink(),
		}));
	}

	async writeFile(
		targetPath: string,
		content: string | Uint8Array,
	): Promise<void> {
		const resolved = this.normalizeTarget(targetPath);
		await fs.mkdir(path.dirname(resolved), { recursive: true });
		await fs.writeFile(resolved, content);
	}

	async createDir(targetPath: string): Promise<void> {
		await fs.mkdir(this.normalizeTarget(targetPath));
	}

	async mkdir(
		targetPath: string,
		options?: { recursive?: boolean },
	): Promise<void> {
		await fs.mkdir(this.normalizeTarget(targetPath), {
			recursive: options?.recursive ?? true,
		});
	}

	async exists(targetPath: string): Promise<boolean> {
		try {
			await fs.access(this.normalizeTarget(targetPath));
			return true;
		} catch {
			return false;
		}
	}

	async stat(targetPath: string): Promise<VirtualStat> {
		return this.toStat(await fs.stat(this.normalizeTarget(targetPath)));
	}

	async removeFile(targetPath: string): Promise<void> {
		await fs.unlink(this.normalizeTarget(targetPath));
	}

	async removeDir(targetPath: string): Promise<void> {
		await fs.rmdir(this.normalizeTarget(targetPath));
	}

	async rename(oldPath: string, newPath: string): Promise<void> {
		const nextPath = this.normalizeTarget(newPath);
		await fs.mkdir(path.dirname(nextPath), { recursive: true });
		await fs.rename(this.normalizeTarget(oldPath), nextPath);
	}

	async realpath(targetPath: string): Promise<string> {
		const real = await fs.realpath(this.normalizeTarget(targetPath));
		const relative = path.relative(this.rootPath, real);
		return relative ? `/${relative.split(path.sep).join("/")}` : "/";
	}

	async symlink(target: string, linkPath: string): Promise<void> {
		const resolvedLink = this.normalizeTarget(linkPath);
		await fs.mkdir(path.dirname(resolvedLink), { recursive: true });
		await fs.symlink(target, resolvedLink);
	}

	async readlink(targetPath: string): Promise<string> {
		return fs.readlink(this.normalizeTarget(targetPath));
	}

	async lstat(targetPath: string): Promise<VirtualStat> {
		return this.toStat(await fs.lstat(this.normalizeTarget(targetPath)));
	}

	async link(oldPath: string, newPath: string): Promise<void> {
		await fs.link(this.normalizeTarget(oldPath), this.normalizeTarget(newPath));
	}

	async chmod(targetPath: string, mode: number): Promise<void> {
		await fs.chmod(this.normalizeTarget(targetPath), mode);
	}

	async chown(targetPath: string, uid: number, gid: number): Promise<void> {
		await fs.chown(this.normalizeTarget(targetPath), uid, gid);
	}

	async utimes(
		targetPath: string,
		atime: number,
		mtime: number,
	): Promise<void> {
		await fs.utimes(
			this.normalizeTarget(targetPath),
			atime / 1000,
			mtime / 1000,
		);
	}

	async truncate(targetPath: string, length: number): Promise<void> {
		await fs.truncate(this.normalizeTarget(targetPath), length);
	}

	async pread(
		targetPath: string,
		offset: number,
		length: number,
	): Promise<Uint8Array> {
		const handle = await fs.open(this.normalizeTarget(targetPath), "r");
		try {
			const buffer = Buffer.alloc(length);
			const { bytesRead } = await handle.read(buffer, 0, length, offset);
			return new Uint8Array(buffer.buffer, buffer.byteOffset, bytesRead);
		} finally {
			await handle.close();
		}
	}

	async pwrite(
		targetPath: string,
		offset: number,
		data: Uint8Array,
	): Promise<void> {
		const handle = await fs.open(this.normalizeTarget(targetPath), "r+");
		try {
			await handle.write(data, 0, data.length, offset);
		} finally {
			await handle.close();
		}
	}
}

function permissionAllows(mode: PermissionMode | undefined): boolean {
	return mode !== "deny";
}

function globMatches(pattern: string, value: string): boolean {
	const escaped = pattern.replace(/[|\\{}()[\]^$+?.]/g, "\\$&");
	const expression = escaped.replace(/\*/g, ".*");
	return new RegExp(`^${expression}$`).test(value);
}

function envPolicyAllows(
	policy: EnvPermissions | undefined,
	name: string,
): boolean {
	if (policy === undefined) {
		return true;
	}
	if (typeof policy === "string") {
		return permissionAllows(policy);
	}
	let mode = policy.default ?? "deny";
	for (const rule of policy.rules) {
		const operationsMatch =
			!rule.operations ||
			rule.operations.length === 0 ||
			rule.operations.includes("read");
		const patternsMatch =
			!rule.patterns ||
			rule.patterns.length === 0 ||
			rule.patterns.some((pattern) => globMatches(pattern, name));
		if (operationsMatch && patternsMatch) {
			mode = rule.mode;
		}
	}
	return permissionAllows(mode);
}

export const allowAllFs: FsPermissions = "allow";
export const allowAllNetwork: NetworkPermissions = "allow";
export const allowAllChildProcess: ChildProcessPermissions = "allow";
export const allowAllProcess: ProcessPermissions = "allow";
export const allowAllEnv: EnvPermissions = "allow";
export const allowAll: Permissions = {
	fs: allowAllFs,
	network: allowAllNetwork,
	childProcess: allowAllChildProcess,
	process: allowAllProcess,
	env: allowAllEnv,
};

export function filterEnv(
	env: Record<string, string> | undefined,
	permissions?: Permissions,
): Record<string, string> {
	const input = env ?? {};
	if (!permissions?.env) return { ...input };
	const output: Record<string, string> = {};
	for (const [name, value] of Object.entries(input)) {
		if (envPolicyAllows(permissions.env, name)) {
			output[name] = value;
		}
	}
	return output;
}

export function createProcessScopedFileSystem(
	filesystem: VirtualFileSystem,
): VirtualFileSystem {
	return filesystem;
}

export async function exists(
	filesystem: VirtualFileSystem,
	targetPath: string,
): Promise<boolean> {
	return filesystem.exists(targetPath);
}

export async function stat(
	filesystem: VirtualFileSystem,
	targetPath: string,
): Promise<VirtualStat> {
	return filesystem.stat(targetPath);
}

export async function rename(
	filesystem: VirtualFileSystem,
	oldPath: string,
	newPath: string,
): Promise<void> {
	return filesystem.rename(oldPath, newPath);
}

export async function readDirWithTypes(
	filesystem: VirtualFileSystem,
	targetPath: string,
): Promise<VirtualDirEntry[]> {
	return filesystem.readDirWithTypes(targetPath);
}

export async function mkdir(
	filesystem: VirtualFileSystem,
	targetPath: string,
	options?: { recursive?: boolean },
): Promise<void> {
	return filesystem.mkdir(targetPath, options);
}

export function createNodeHostCommandExecutor(): CommandExecutor {
	return {
		spawn() {
			throw new Error(
				"createNodeHostCommandExecutor is not supported on the native runtime path",
			);
		},
	};
}

export function createKernelCommandExecutor(kernel: Kernel): CommandExecutor {
	return {
		spawn(command, args, options) {
			return kernel.spawn(command, args, options);
		},
	};
}

export function createKernelVfsAdapter(
	kernelVfs: VirtualFileSystem,
): VirtualFileSystem {
	return kernelVfs;
}

export function createHostFallbackVfs(
	base: VirtualFileSystem,
): VirtualFileSystem {
	return base;
}

export function isPrivateIp(host: string): boolean {
	return (
		host === "localhost" ||
		host === "127.0.0.1" ||
		host.startsWith("10.") ||
		host.startsWith("192.168.") ||
		/^172\.(1[6-9]|2\d|3[0-1])\./.test(host)
	);
}

export function createNodeHostNetworkAdapter(): NetworkAdapter {
	return createDefaultNetworkAdapter();
}

export function createDefaultNetworkAdapter(): NetworkAdapter {
	return {
		async fetch(url, options) {
			const response = await globalThis.fetch(url, {
				method: options?.method ?? "GET",
				headers: options?.headers,
				body: options?.body as RequestInit["body"],
			});
			const headers: Record<string, string> = {};
			response.headers.forEach((value, key) => {
				headers[key] = value;
			});
			return {
				ok: response.ok,
				status: response.status,
				statusText: response.statusText,
				headers,
				body: await response.text(),
				url: response.url,
				redirected: response.redirected,
			};
		},
		async dnsLookup(hostname) {
			return { address: hostname, family: hostname.includes(":") ? 6 : 4 };
		},
		async httpRequest(url, options) {
			const response = await globalThis.fetch(url, {
				method: options?.method ?? "GET",
				headers: options?.headers,
				body: options?.body as RequestInit["body"],
			});
			const headers: Record<string, string> = {};
			response.headers.forEach((value, key) => {
				headers[key] = value;
			});
			return {
				status: response.status,
				statusText: response.statusText,
				headers,
				body: await response.text(),
				url: response.url,
			};
		},
	};
}

export function createNodeDriver(
	options: NodeDriverOptions = {},
): SystemDriver {
	return {
		filesystem: options.filesystem,
		network: options.networkAdapter,
		commandExecutor: options.commandExecutor,
		permissions: options.permissions,
		mounts: options.mounts ?? [],
		runtime: {
			process: options.processConfig ?? {},
			os: options.osConfig ?? {},
		},
	};
}

export class NodeExecutionDriver implements NodeRuntimeDriver {
	readonly network?: Pick<
		NetworkAdapter,
		"fetch" | "dnsLookup" | "httpRequest"
	>;

	constructor(private readonly options: RuntimeDriverOptions) {
		this.network = options.system.network;
	}

	async exec(): Promise<ExecResult> {
		throw new Error(
			"NodeExecutionDriver is not available after the native runtime migration",
		);
	}

	async run<T = unknown>(): Promise<RunResult<T>> {
		throw new Error(
			"NodeExecutionDriver is not available after the native runtime migration",
		);
	}

	dispose(): void {
		void this.options;
	}

	async terminate(): Promise<void> {}
}

export class NodeRuntime extends NodeExecutionDriver {}

export function createNodeRuntimeDriverFactory(): NodeRuntimeDriverFactory {
	return {
		createRuntimeDriver(options) {
			return new NodeRuntime(options);
		},
	};
}

export const WASMVM_COMMANDS = Object.freeze([
	"sh",
	"bash",
	"grep",
	"egrep",
	"fgrep",
	"rg",
	"sed",
	"awk",
	"jq",
	"yq",
	"find",
	"fd",
	"cat",
	"chmod",
	"column",
	"cp",
	"dd",
	"diff",
	"du",
	"expr",
	"file",
	"head",
	"ln",
	"logname",
	"ls",
	"mkdir",
	"mktemp",
	"mv",
	"pathchk",
	"rev",
	"rm",
	"sleep",
	"sort",
	"split",
	"stat",
	"strings",
	"tac",
	"tail",
	"test",
	"[",
	"touch",
	"tree",
	"tsort",
	"whoami",
	"gzip",
	"gunzip",
	"zcat",
	"tar",
	"zip",
	"unzip",
	"sqlite3",
	"curl",
	"git",
	"git-remote-http",
	"git-remote-https",
	"env",
	"envsubst",
	"nice",
	"nohup",
	"stdbuf",
	"timeout",
	"xargs",
	"base32",
	"base64",
	"basenc",
	"basename",
	"comm",
	"cut",
	"dircolors",
	"dirname",
	"echo",
	"expand",
	"factor",
	"false",
	"fmt",
	"fold",
	"join",
	"nl",
	"numfmt",
	"od",
	"paste",
	"printenv",
	"printf",
	"ptx",
	"seq",
	"shuf",
	"tr",
	"true",
	"unexpand",
	"uniq",
	"wc",
	"yes",
	"b2sum",
	"cksum",
	"md5sum",
	"sha1sum",
	"sha224sum",
	"sha256sum",
	"sha384sum",
	"sha512sum",
	"sum",
	"link",
	"pwd",
	"readlink",
	"realpath",
	"rmdir",
	"shred",
	"tee",
	"truncate",
	"unlink",
	"arch",
	"date",
	"nproc",
	"uname",
	"dir",
	"vdir",
	"hostname",
	"hostid",
	"more",
	"sync",
	"tty",
	"chcon",
	"runcon",
	"chgrp",
	"chown",
	"chroot",
	"df",
	"groups",
	"id",
	"install",
	"kill",
	"mkfifo",
	"mknod",
	"pinky",
	"who",
	"users",
	"uptime",
	"stty",
	"codex",
	"codex-exec",
]) as readonly string[];

export type PermissionTier = "full" | "read-write" | "read-only" | "isolated";

export const DEFAULT_FIRST_PARTY_TIERS: Readonly<
	Record<string, PermissionTier>
> = Object.freeze({
	sh: "full",
	bash: "full",
	env: "full",
	timeout: "full",
	xargs: "full",
	nice: "full",
	nohup: "full",
	stdbuf: "full",
	codex: "full",
	"codex-exec": "full",
	git: "full",
	"git-remote-http": "full",
	"git-remote-https": "full",
	grep: "read-only",
	egrep: "read-only",
	fgrep: "read-only",
	rg: "read-only",
	cat: "read-only",
	head: "read-only",
	tail: "read-only",
	wc: "read-only",
	sort: "read-only",
	uniq: "read-only",
	diff: "read-only",
	find: "read-only",
	fd: "read-only",
	tree: "read-only",
	file: "read-only",
	du: "read-only",
	ls: "read-only",
	dir: "read-only",
	vdir: "read-only",
	strings: "read-only",
	stat: "read-only",
	rev: "read-only",
	column: "read-only",
	cut: "read-only",
	tr: "read-only",
	paste: "read-only",
	join: "read-only",
	fold: "read-only",
	expand: "read-only",
	nl: "read-only",
	od: "read-only",
	comm: "read-only",
	basename: "read-only",
	dirname: "read-only",
	realpath: "read-only",
	readlink: "read-only",
	pwd: "read-only",
	echo: "read-only",
	envsubst: "read-only",
	printf: "read-only",
	true: "read-only",
	false: "read-only",
	yes: "read-only",
	seq: "read-only",
	test: "read-only",
	"[": "read-only",
	expr: "read-only",
	factor: "read-only",
	date: "read-only",
	uname: "read-only",
	nproc: "read-only",
	whoami: "read-only",
	id: "read-only",
	groups: "read-only",
	base64: "read-only",
	md5sum: "read-only",
	sha256sum: "read-only",
	tac: "read-only",
	tsort: "read-only",
	curl: "full",
	sqlite3: "read-write",
});

export interface WasmVmRuntimeOptions {
	wasmBinaryPath?: string;
	commandDirs?: string[];
	permissions?: Record<string, PermissionTier>;
}

class NativeRuntimeDescriptor implements KernelRuntimeDriver {
	constructor(
		readonly kind: "node" | "wasmvm",
		readonly name: string,
		readonly commands: string[],
		readonly commandDirs?: string[],
	) {}
}

function normalizeCommandLookup(command: string): string {
	return path.posix.basename(command);
}

interface DiscoveredWasmCommandEntry {
	name: string;
	hostPath: string;
}

function isWasmBinaryFile(filePath: string): boolean {
	try {
		const header = fsSync.readFileSync(filePath, { encoding: null });
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

function discoverWasmCommandEntries(
	commandDirs: string[],
): DiscoveredWasmCommandEntry[] {
	const discovered: DiscoveredWasmCommandEntry[] = [];
	const seen = new Set<string>();
	for (const commandDir of commandDirs) {
		let entries: string[];
		try {
			entries = fsSync
				.readdirSync(commandDir)
				.sort((left, right) => left.localeCompare(right));
		} catch {
			continue;
		}
		for (const entry of entries) {
			if (entry.startsWith(".")) continue;
			if (seen.has(entry)) continue;
			const fullPath = path.join(commandDir, entry);
			if (isWasmBinaryFile(fullPath)) {
				seen.add(entry);
				discovered.push({
					name: entry,
					hostPath: fullPath,
				});
				continue;
			}
			try {
				const realPath = fsSync.realpathSync(fullPath);
				if (isWasmBinaryFile(realPath)) {
					seen.add(entry);
					discovered.push({
						name: entry,
						hostPath: fullPath,
					});
				}
			} catch {}
		}
	}
	return discovered;
}

class WasmVmRuntimeDescriptor implements KernelRuntimeDriver {
	readonly kind = "wasmvm" as const;
	readonly name = "wasmvm" as const;
	readonly commands: string[] = [];
	readonly commandDirs?: string[];
	readonly _commandPaths = new Map<string, string>();
	readonly _moduleCache = new Map<string, true>();

	constructor(options: WasmVmRuntimeOptions) {
		this.commandDirs =
			options.commandDirs && options.commandDirs.length > 0
				? [...options.commandDirs]
				: undefined;
		if (options.commandDirs && options.commandDirs.length > 0) {
			this.refreshDiscovery();
			return;
		}
		this.commands.push(...WASMVM_COMMANDS);
		if (options.wasmBinaryPath) {
			console.warn(
				"createWasmVmRuntime({ wasmBinaryPath }) is deprecated; use commandDirs instead.",
			);
		}
	}

	init(_kernel: KernelInterface): void {
		if (this.commandDirs && this.commandDirs.length > 0) {
			this.refreshDiscovery();
		}
	}

	tryResolve(command: string): boolean {
		if (!this.commandDirs || this.commandDirs.length === 0) {
			return false;
		}
		const normalized = normalizeCommandLookup(command);
		if (this._commandPaths.has(normalized)) {
			return true;
		}
		this.refreshDiscovery();
		return this._commandPaths.has(normalized);
	}

	recordModuleExecution(command: string): void {
		const normalized = normalizeCommandLookup(command);
		if (
			this._commandPaths.has(normalized) ||
			((!this.commandDirs || this.commandDirs.length === 0) &&
				this.commands.includes(normalized))
		) {
			this._moduleCache.set(normalized, true);
		}
	}

	private refreshDiscovery(): void {
		if (!this.commandDirs || this.commandDirs.length === 0) {
			return;
		}
		const discovered = discoverWasmCommandEntries(this.commandDirs);
		this.commands.length = 0;
		this._commandPaths.clear();
		for (const entry of discovered) {
			this.commands.push(entry.name);
			this._commandPaths.set(entry.name, entry.hostPath);
		}
	}
}

export function createWasmVmRuntime(
	options: WasmVmRuntimeOptions = {},
): KernelRuntimeDriver {
	return new WasmVmRuntimeDescriptor(options);
}

export function createNodeRuntime(): KernelRuntimeDriver {
	return new NativeRuntimeDescriptor("node", "node", ["node", "npm", "npx"]);
}

function latestMtimeMs(targetPath: string): number {
	try {
		const stats = fsSync.statSync(targetPath);
		if (!stats.isDirectory()) {
			return stats.mtimeMs;
		}
		let latest = stats.mtimeMs;
		for (const entry of fsSync.readdirSync(targetPath)) {
			latest = Math.max(latest, latestMtimeMs(path.join(targetPath, entry)));
		}
		return latest;
	} catch {
		return 0;
	}
}

function sidecarBinaryNeedsBuild(): boolean {
	if (!fsSync.existsSync(SIDECAR_BINARY)) {
		return true;
	}
	const binaryMtime = latestMtimeMs(SIDECAR_BINARY);
	return SIDECAR_BUILD_INPUTS.some(
		(inputPath) => latestMtimeMs(inputPath) > binaryMtime,
	);
}

function ensureNativeSidecarBinary(): string {
	// A published install has no in-repo Cargo workspace to build from: resolve
	// the prebuilt platform binary (or the AGENTOS_SIDECAR_BIN override).
	if (
		process.env.AGENTOS_SIDECAR_BIN ||
		!fsSync.existsSync(path.join(REPO_ROOT, "Cargo.toml"))
	) {
		return resolvePublishedSidecarBinary();
	}
	if (
		ensuredSidecarBinary &&
		fsSync.existsSync(ensuredSidecarBinary) &&
		!sidecarBinaryNeedsBuild()
	) {
		return ensuredSidecarBinary;
	}
	if (sidecarBinaryNeedsBuild()) {
		const cargoBinary = findCargoBinary();
		if (cargoBinary) {
			execFileSync(cargoBinary, ["build", "-q", "-p", "agentos-sidecar"], {
				cwd: REPO_ROOT,
				stdio: "pipe",
			});
		} else if (!fsSync.existsSync(SIDECAR_BINARY)) {
			execFileSync(
				resolveCargoBinary(),
				["build", "-q", "-p", "agentos-sidecar"],
				{
					cwd: REPO_ROOT,
					stdio: "pipe",
				},
			);
		}
	}
	ensuredSidecarBinary = SIDECAR_BINARY;
	return ensuredSidecarBinary;
}

function rootEntryExecutable(
	kind: RootFilesystemEntry["kind"],
	mode: number,
): boolean {
	return kind === "file" && (mode & 0o111) !== 0;
}

function createBootstrapEntries(): RootFilesystemEntry[] {
	return [
		{
			path: "/",
			kind: "directory",
			mode: 0o755,
			uid: 0,
			gid: 0,
			executable: false,
		},
		...KERNEL_POSIX_BOOTSTRAP_DIRS.map((entryPath) => ({
			path: entryPath,
			kind: "directory" as const,
			mode:
				entryPath === "/tmp" || entryPath === "/var/tmp"
					? 0o1777
					: entryPath === "/home/agentos"
						? 0o2755
						: 0o755,
			uid:
				entryPath === "/home/agentos" || entryPath === "/workspace" ? 1000 : 0,
			gid:
				entryPath === "/home/agentos" || entryPath === "/workspace" ? 1000 : 0,
			executable: false,
		})),
		{
			path: "/usr/bin/env",
			kind: "file",
			mode: 0o644,
			uid: 0,
			gid: 0,
			content: "",
			encoding: "utf8",
			executable: false,
		},
	];
}

function mergeRootFilesystemEntries(
	baseEntries: RootFilesystemEntry[],
	overrideEntries: RootFilesystemEntry[],
): RootFilesystemEntry[] {
	const merged = new Map<string, RootFilesystemEntry>();
	for (const entry of baseEntries) {
		merged.set(entry.path, entry);
	}
	for (const entry of overrideEntries) {
		merged.set(entry.path, entry);
	}
	return [...merged.values()];
}

function rootFilesystemEntriesForConfig(
	entries: RootFilesystemEntry[],
): VmConfigRootFilesystemEntry[] {
	return entries.map((entry) => ({
		...entry,
		executable: entry.executable ?? false,
	}));
}

async function snapshotFilesystemEntries(
	filesystem: VirtualFileSystem,
	targetPath = "/",
	output: RootFilesystemEntry[] = [],
	options?: {
		passthroughDirectories?: ReadonlySet<string>;
	},
): Promise<RootFilesystemEntry[]> {
	const passthroughDirectories = options?.passthroughDirectories;
	const passthroughDirectory = passthroughDirectories?.has(targetPath) ?? false;
	const statInfo =
		targetPath === "/" || passthroughDirectory
			? await filesystem.stat(targetPath)
			: await filesystem.lstat(targetPath);
	if (statInfo.isSymbolicLink) {
		output.push({
			path: targetPath,
			kind: "symlink",
			mode: statInfo.mode,
			uid: statInfo.uid,
			gid: statInfo.gid,
			target: await filesystem.readlink(targetPath),
			executable: false,
		});
		return output;
	}
	if (statInfo.isDirectory) {
		output.push({
			path: targetPath,
			kind: "directory",
			mode: statInfo.mode,
			uid: statInfo.uid,
			gid: statInfo.gid,
			executable: false,
		});
		if (passthroughDirectory) {
			return output;
		}
		const children = (await filesystem.readDirWithTypes(targetPath))
			.map((entry) => entry.name)
			.filter((name) => name !== "." && name !== "..")
			.sort((left, right) => left.localeCompare(right));
		for (const child of children) {
			const childPath =
				targetPath === "/"
					? posixPath.join("/", child)
					: posixPath.join(targetPath, child);
			await snapshotFilesystemEntries(filesystem, childPath, output, options);
		}
		return output;
	}
	output.push({
		path: targetPath,
		kind: "file",
		mode: statInfo.mode,
		uid: statInfo.uid,
		gid: statInfo.gid,
		content: Buffer.from(await filesystem.readFile(targetPath)).toString(
			"base64",
		),
		encoding: "base64",
		executable: rootEntryExecutable("file", statInfo.mode),
	});
	return output;
}

async function materializeSnapshotEntriesIntoVm(
	client: SidecarProcess,
	session: AuthenticatedSession,
	vm: CreatedVm,
	entries: RootFilesystemEntry[],
): Promise<void> {
	for (const entry of entries) {
		if (entry.path === "/") {
			continue;
		}
		if (entry.kind === "directory") {
			await client.mkdir(session, vm, entry.path, { recursive: true });
		} else if (entry.kind === "file") {
			await client.writeFile(
				session,
				vm,
				entry.path,
				decodeRootFilesystemEntryContent(entry),
			);
		} else {
			await client.symlink(session, vm, entry.target ?? "", entry.path);
			continue;
		}

		if (typeof entry.mode === "number") {
			await client.chmod(session, vm, entry.path, entry.mode);
		}
		if (typeof entry.uid === "number" && typeof entry.gid === "number") {
			await client.chown(session, vm, entry.path, entry.uid, entry.gid);
		}
	}
}

function decodeRootFilesystemEntryContent(
	entry: RootFilesystemEntry,
): Uint8Array {
	const content = entry.content ?? "";
	if (entry.encoding === "base64") {
		return new Uint8Array(Buffer.from(content, "base64"));
	}
	return new TextEncoder().encode(content);
}

const NODE_FILESYSTEM_ROOT_PASSTHROUGH_DIRS = ["node_modules"] as const;

function planNodeFilesystemPassthroughMounts(
	filesystem: VirtualFileSystem,
	existingMounts: readonly LocalCompatMount[],
): {
	mounts: LocalCompatMount[];
	passthroughDirectories: ReadonlySet<string>;
} {
	if (!(filesystem instanceof NodeFileSystem)) {
		return {
			mounts: [],
			passthroughDirectories: new Set<string>(),
		};
	}

	const passthroughDirectories = new Set<string>();
	const existingGuestPaths = new Set(existingMounts.map((mount) => mount.path));
	const mounts: LocalCompatMount[] = [];
	for (const directoryName of NODE_FILESYSTEM_ROOT_PASSTHROUGH_DIRS) {
		const guestPath = normalizePath(`/${directoryName}`);
		const hostPath = path.join(filesystem.rootPath, directoryName);
		let statInfo: fsSync.Stats;
		try {
			statInfo = fsSync.statSync(hostPath);
		} catch {
			continue;
		}
		if (!statInfo.isDirectory()) {
			continue;
		}
		passthroughDirectories.add(guestPath);
		if (existingGuestPaths.has(guestPath)) {
			continue;
		}
		mounts.push({
			path: guestPath,
			fs: new NodeFileSystem({ root: hostPath }),
			readOnly: true,
		});
	}

	return {
		mounts,
		passthroughDirectories,
	};
}

class DeferredFileSystem implements VirtualFileSystem {
	constructor(private readonly getFilesystem: () => VirtualFileSystem | null) {}

	private filesystem(): VirtualFileSystem {
		const filesystem = this.getFilesystem();
		if (!filesystem) {
			throw new Error("kernel filesystem is not ready; mount a runtime first");
		}
		return filesystem;
	}

	readFile(path: string): Promise<Uint8Array> {
		return this.filesystem().readFile(path);
	}
	readTextFile(path: string): Promise<string> {
		return this.filesystem().readTextFile(path);
	}
	readDir(path: string): Promise<string[]> {
		return this.filesystem().readDir(path);
	}
	readDirWithTypes(path: string): Promise<VirtualDirEntry[]> {
		return this.filesystem().readDirWithTypes(path);
	}
	writeFile(path: string, content: string | Uint8Array): Promise<void> {
		return this.filesystem().writeFile(path, content);
	}
	createDir(path: string): Promise<void> {
		return this.filesystem().createDir(path);
	}
	mkdir(path: string, options?: { recursive?: boolean }): Promise<void> {
		return this.filesystem().mkdir(path, options);
	}
	exists(path: string): Promise<boolean> {
		return this.filesystem().exists(path);
	}
	stat(path: string): Promise<VirtualStat> {
		return this.filesystem().stat(path);
	}
	removeFile(path: string): Promise<void> {
		return this.filesystem().removeFile(path);
	}
	removeDir(path: string): Promise<void> {
		return this.filesystem().removeDir(path);
	}
	rename(oldPath: string, newPath: string): Promise<void> {
		return this.filesystem().rename(oldPath, newPath);
	}
	realpath(path: string): Promise<string> {
		return this.filesystem().realpath(path);
	}
	symlink(target: string, linkPath: string): Promise<void> {
		return this.filesystem().symlink(target, linkPath);
	}
	readlink(path: string): Promise<string> {
		return this.filesystem().readlink(path);
	}
	lstat(path: string): Promise<VirtualStat> {
		return this.filesystem().lstat(path);
	}
	link(oldPath: string, newPath: string): Promise<void> {
		return this.filesystem().link(oldPath, newPath);
	}
	chmod(path: string, mode: number): Promise<void> {
		return this.filesystem().chmod(path, mode);
	}
	chown(path: string, uid: number, gid: number): Promise<void> {
		return this.filesystem().chown(path, uid, gid);
	}
	utimes(path: string, atime: number, mtime: number): Promise<void> {
		return this.filesystem().utimes(path, atime, mtime);
	}
	truncate(path: string, length: number): Promise<void> {
		return this.filesystem().truncate(path, length);
	}
	pread(path: string, offset: number, length: number): Promise<Uint8Array> {
		return this.filesystem().pread(path, offset, length);
	}
	pwrite(path: string, offset: number, data: Uint8Array): Promise<void> {
		return this.filesystem().pwrite(path, offset, data);
	}
}

const VIRTUAL_FILESYSTEM_METHOD_NAMES = [
	"readFile",
	"readTextFile",
	"readDir",
	"readDirWithTypes",
	"writeFile",
	"createDir",
	"mkdir",
	"exists",
	"stat",
	"removeFile",
	"removeDir",
	"rename",
	"realpath",
	"symlink",
	"readlink",
	"lstat",
	"link",
	"chmod",
	"chown",
	"utimes",
	"truncate",
	"pread",
	"pwrite",
] as const;

type VirtualFileSystemMethodName =
	(typeof VIRTUAL_FILESYSTEM_METHOD_NAMES)[number];

type BoundVirtualFileSystemMethods = Partial<
	Record<VirtualFileSystemMethodName, (...args: unknown[]) => unknown>
>;

interface LiveFilesystemBinding {
	syncFromLive(paths: readonly string[]): Promise<void>;
	restore(): void;
}

const DEFAULT_LIVE_FILESYSTEM_SYNC_MAX_BYTES = 64 * 1024 * 1024;

type LiveFilesystemSnapshotEntry =
	| { kind: "directory"; path: string }
	| { kind: "symlink"; path: string; target: string }
	| { kind: "file"; path: string; content: Uint8Array };

class LiveFilesystemSyncLimitError extends Error {
	readonly code = "ERR_AGENTOS_LIVE_FILESYSTEM_SYNC_LIMIT";
}

function topLevelSyncRoot(targetPath: string): string {
	const normalized = normalizePath(targetPath);
	const [first] = normalized.split("/").filter(Boolean);
	return first ? `/${first}` : "/";
}

function collectLiveFilesystemSyncRoots(
	entries: readonly RootFilesystemEntry[],
): string[] {
	const roots = new Set<string>();
	for (const entry of entries) {
		if (entry.path === "/") {
			continue;
		}
		roots.add(topLevelSyncRoot(entry.path));
	}
	return [...roots].sort((left, right) => left.localeCompare(right));
}

async function callBoundFilesystemMethod<T>(
	methods: BoundVirtualFileSystemMethods,
	method: VirtualFileSystemMethodName,
	...args: unknown[]
): Promise<T> {
	const delegate = methods[method];
	if (!delegate) {
		throw new Error(`filesystem method ${method} is unavailable`);
	}
	return (await delegate(...args)) as T;
}

async function ensureBoundParentDirectory(
	methods: BoundVirtualFileSystemMethods,
	targetPath: string,
): Promise<void> {
	const parent = dirnameVirtual(targetPath);
	if (parent === targetPath) {
		return;
	}
	await callBoundFilesystemMethod(methods, "mkdir", parent, {
		recursive: true,
	});
}

async function syncLiveFilesystemToBoundMethods(
	live: VirtualFileSystem,
	methods: BoundVirtualFileSystemMethods,
	paths: readonly string[],
	maxBytes: number,
): Promise<void> {
	const snapshot: LiveFilesystemSnapshotEntry[] = [];
	const usage = { bytes: 0 };
	for (const targetPath of [...new Set(paths.map(normalizePath))].sort(
		(left, right) => left.localeCompare(right),
	)) {
		if (!(await live.exists(targetPath).catch(() => false))) {
			continue;
		}
		await snapshotLiveFilesystemPath(
			live,
			targetPath,
			snapshot,
			usage,
			maxBytes,
		);
	}
	if (usage.bytes >= maxBytes * 0.8) {
		process.emitWarning(
			`live filesystem sync retained ${usage.bytes} of ${maxBytes} bytes; ` +
				"raise createKernel({ maxFilesystemBytes }) before increasing VM filesystem limits",
			{ code: "AGENTOS_LIVE_FILESYSTEM_SYNC_NEAR_LIMIT" },
		);
	}
	for (const entry of snapshot) {
		await applyLiveFilesystemSnapshotEntry(methods, entry);
	}
}

async function snapshotLiveFilesystemPath(
	live: VirtualFileSystem,
	targetPath: string,
	snapshot: LiveFilesystemSnapshotEntry[],
	usage: { bytes: number },
	maxBytes: number,
	knownEntry?: Pick<VirtualDirEntry, "isDirectory" | "isSymbolicLink">,
): Promise<void> {
	const stat =
		knownEntry ??
		(targetPath === "/"
			? await live.stat(targetPath)
			: await live.lstat(targetPath));
	if (stat.isSymbolicLink) {
		snapshot.push({
			kind: "symlink",
			path: targetPath,
			target: await live.readlink(targetPath),
		});
		return;
	}
	if (stat.isDirectory) {
		snapshot.push({ kind: "directory", path: targetPath });
		const children = (await live.readDirWithTypes(targetPath))
			.filter((entry) => entry.name !== "." && entry.name !== "..")
			.sort((left, right) => left.name.localeCompare(right.name));
		for (const child of children) {
			await snapshotLiveFilesystemPath(
				live,
				targetPath === "/"
					? posixPath.join("/", child.name)
					: posixPath.join(targetPath, child.name),
				snapshot,
				usage,
				maxBytes,
				child,
			);
		}
		return;
	}

	const fileSize =
		"size" in stat && typeof stat.size === "number"
			? stat.size
			: (await live.lstat(targetPath)).size;
	if (fileSize > maxBytes - usage.bytes) {
		throw new LiveFilesystemSyncLimitError(
			`live filesystem sync for ${targetPath} exceeds ${maxBytes} bytes ` +
				"(createKernel maxFilesystemBytes); raise createKernel({ maxFilesystemBytes })",
		);
	}
	const content = await live.readFile(targetPath);
	if (content.byteLength > maxBytes - usage.bytes) {
		throw new LiveFilesystemSyncLimitError(
			`live filesystem sync for ${targetPath} exceeds ${maxBytes} bytes ` +
				"(createKernel maxFilesystemBytes); raise createKernel({ maxFilesystemBytes })",
		);
	}
	usage.bytes += content.byteLength;
	snapshot.push({ kind: "file", path: targetPath, content });
}

async function applyLiveFilesystemSnapshotEntry(
	methods: BoundVirtualFileSystemMethods,
	entry: LiveFilesystemSnapshotEntry,
): Promise<void> {
	if (entry.kind === "directory") {
		await callBoundFilesystemMethod(methods, "mkdir", entry.path, {
			recursive: true,
		});
		return;
	}
	await ensureBoundParentDirectory(methods, entry.path);
	if (entry.kind === "symlink") {
		await callBoundFilesystemMethod(methods, "removeFile", entry.path).catch(
			() => {},
		);
		await callBoundFilesystemMethod(
			methods,
			"symlink",
			entry.target,
			entry.path,
		);
		return;
	}
	await callBoundFilesystemMethod(
		methods,
		"writeFile",
		entry.path,
		entry.content,
	);
}

function bindLiveFilesystem(
	target: VirtualFileSystem,
	getFilesystem: () => VirtualFileSystem | null,
	maxBytes: number,
): LiveFilesystemBinding {
	const fallback: BoundVirtualFileSystemMethods = {};
	for (const method of VIRTUAL_FILESYSTEM_METHOD_NAMES) {
		const candidate = (target as unknown as Record<string, unknown>)[method];
		if (typeof candidate === "function") {
			fallback[method] = candidate.bind(target);
		}
	}

	for (const method of VIRTUAL_FILESYSTEM_METHOD_NAMES) {
		(target as unknown as Record<string, unknown>)[method] = (
			...args: unknown[]
		) => {
			const filesystem = getFilesystem();
			const delegate = filesystem
				? (filesystem[method] as (...args: unknown[]) => unknown).bind(
						filesystem,
					)
				: fallback[method];
			if (!delegate) {
				throw new Error(
					`kernel filesystem is not ready; mount a runtime before calling ${method}()`,
				);
			}
			return delegate(...args);
		};
	}

	return {
		async syncFromLive(paths: readonly string[]): Promise<void> {
			const filesystem = getFilesystem();
			if (!filesystem) {
				return;
			}
			await syncLiveFilesystemToBoundMethods(
				filesystem,
				fallback,
				paths,
				maxBytes,
			);
		},
		restore(): void {
			for (const [method, delegate] of Object.entries(fallback)) {
				(target as unknown as Record<string, unknown>)[method] = delegate;
			}
		},
	};
}

function serializeLocalCompatMountForSidecar(
	mount: LocalCompatMount,
): SidecarMountDescriptor {
	return (
		mount.sidecarMount ??
		(mount.fs instanceof NodeFileSystem
			? serializeMountConfigForSidecar({
					path: mount.path,
					readOnly: mount.readOnly,
					plugin: {
						id: "host_dir",
						config: {
							hostPath: mount.fs.rootPath,
							readOnly: mount.readOnly,
						},
					},
				})
			: serializeMountConfigForSidecar({
					path: mount.path,
					driver: mount.fs,
					readOnly: mount.readOnly,
				}))
	);
}

function makeLocalCompatMount(options: {
	path: string;
	fs: VirtualFileSystem;
	readOnly?: boolean;
}): LocalCompatMount {
	const localMount: LocalCompatMount = {
		path: normalizePath(options.path),
		fs: options.fs,
		readOnly: options.readOnly ?? false,
	};
	localMount.sidecarMount = serializeLocalCompatMountForSidecar(localMount);
	return localMount;
}

class NativeKernel implements Kernel {
	readonly env: Record<string, string>;
	readonly cwd: string;
	readonly commands = new Map<string, string>();
	readonly processes = new Map<number, ProcessInfo>();
	readonly socketTable;
	readonly processTable;
	readonly timerTable = {};
	readonly vfs: VirtualFileSystem;

	private client: SidecarProcess | null = null;
	private session: AuthenticatedSession | null = null;
	private vm: CreatedVm | null = null;
	private proxy: NativeSidecarKernelProxy | null = null;
	private rootFilesystem: VirtualFileSystem | null = null;
	private readyPromise: Promise<void> | null = null;
	private readonly liveFilesystemBinding: LiveFilesystemBinding;
	private liveFilesystemSyncRoots: string[] = [];
	private readonly pendingLocalMounts: LocalCompatMount[] = [];
	private mountedCommandDirs: string[] = [];
	private readonly mountedRuntimeDrivers: KernelRuntimeDriver[] = [];
	private readonly loopbackExemptPorts: number[];

	constructor(
		private readonly options: {
			filesystem: VirtualFileSystem;
			permissions?: Permissions;
			env?: Record<string, string>;
			cwd?: string;
			hostNetworkAdapter?: unknown;
			loopbackExemptPorts?: number[];
			mounts?: Array<{
				path: string;
				fs: VirtualFileSystem;
				readOnly?: boolean;
			}>;
			syncFilesystemOnDispose?: boolean;
			maxFilesystemBytes?: number;
		},
	) {
		this.env = { ...(options.env ?? {}) };
		this.cwd = options.cwd ?? "/workspace";
		this.socketTable = {
			hasHostNetworkAdapter: () => Boolean(options.hostNetworkAdapter),
			findListener: (request: {
				host?: string;
				port?: number;
				path?: string;
			}) => this.proxy?.findListener(request) ?? null,
			findBoundUdp: (request: { host?: string; port?: number }) =>
				this.proxy?.findBoundUdp(request) ?? null,
		};
		this.processTable = {
			getSignalState: (pid: number) =>
				this.proxy?.getSignalState(pid) ?? {
					handlers: new Map<number, unknown>(),
				},
		};
		this.loopbackExemptPorts = [...(options.loopbackExemptPorts ?? [])];
		for (const mount of options.mounts ?? []) {
			this.pendingLocalMounts.push(
				makeLocalCompatMount({
					path: mount.path,
					fs: mount.fs,
					readOnly: mount.readOnly,
				}),
			);
		}
		this.vfs = new DeferredFileSystem(() => this.rootFilesystem);
		this.liveFilesystemBinding = bindLiveFilesystem(
			this.options.filesystem,
			() => this.rootFilesystem,
			options.maxFilesystemBytes ?? DEFAULT_LIVE_FILESYSTEM_SYNC_MAX_BYTES,
		);
	}

	get zombieTimerCount(): number {
		return this.proxy?.zombieTimerCount ?? 0;
	}

	private async configureRuntimeCommandStubs(
		commands: Iterable<string>,
		commandDirs: string[] = this.mountedCommandDirs,
	): Promise<Map<string, string>> {
		if (!this.client || !this.session || !this.vm) {
			throw new Error("kernel is not ready");
		}
		const sidecarMounts = commandDirs.map((commandDir, index) =>
			serializeMountConfigForSidecar({
				path: `/__agentos/commands/${index}`,
				readOnly: true,
				plugin: {
					id: "host_dir",
					config: {
						hostPath: commandDir,
						readOnly: true,
					},
				},
			}),
		);
		const localMounts = this.pendingLocalMounts.map((mount) =>
			serializeLocalCompatMountForSidecar(mount),
		);
		const configuredVm = await this.client.configureVm(this.session, this.vm, {
			mounts: [...localMounts, ...sidecarMounts],
			loopbackExemptPorts: this.loopbackExemptPorts,
			bootstrapCommands: [...new Set(commands)].sort((left, right) =>
				left.localeCompare(right),
			),
		});
		return new Map(
			configuredVm.projectedCommands.map((command) => [
				command.name,
				command.guestPath,
			]),
		);
	}

	async mount(driver: KernelRuntimeDriver): Promise<void> {
		await this.ensureReady();
		if (!this.proxy || !this.client || !this.session || !this.vm) {
			throw new Error("kernel is not ready");
		}
		await driver.init?.(this);
		if (driver.kind === "node") {
			for (const command of driver.commands) {
				this.commands.set(command, "node");
			}
			this.mountedRuntimeDrivers.push(driver);
			await this.configureRuntimeCommandStubs(driver.commands);
			return;
		}

		const commandDirs = driver.commandDirs ?? [];
		if (commandDirs.length === 0) {
			for (const command of driver.commands) {
				this.commands.set(command, "wasmvm");
			}
			this.mountedRuntimeDrivers.push(driver);
			await this.configureRuntimeCommandStubs(driver.commands);
			return;
		}

		const allCommandDirs = [...this.mountedCommandDirs, ...commandDirs];
		const projectedCommands = await this.configureRuntimeCommandStubs(
			driver.commands,
			allCommandDirs,
		);
		this.proxy.registerCommandGuestPaths(projectedCommands);
		this.mountedCommandDirs.push(...commandDirs);
		this.mountedRuntimeDrivers.push(driver);
		for (const command of projectedCommands.keys()) {
			this.commands.set(command, "wasmvm");
		}
	}

	async dispose(): Promise<void> {
		await this.readyPromise?.catch(() => {});
		let syncError: unknown;
		if (
			this.options.syncFilesystemOnDispose !== false &&
			this.rootFilesystem &&
			!(this.options.filesystem instanceof NodeFileSystem)
		) {
			try {
				await this.liveFilesystemBinding.syncFromLive(
					this.liveFilesystemSyncRoots,
				);
			} catch (error) {
				syncError = error;
			}
		}
		try {
			await this.proxy?.dispose().catch(() => {});
		} finally {
			this.proxy = null;
			this.rootFilesystem = null;
			this.client = null;
			this.session = null;
			this.vm = null;
			this.liveFilesystemBinding.restore();
		}
		if (syncError) {
			throw syncError;
		}
	}

	async exec(
		command: string,
		options?: KernelExecOptions,
	): Promise<KernelExecResult> {
		await this.ensureReady();
		if (!this.proxy) {
			throw new Error("kernel is not ready");
		}
		return this.proxy.exec(command, options);
	}

	spawn(
		command: string,
		args: string[],
		options?: KernelSpawnOptions,
	): ManagedProcess {
		if (!this.proxy) {
			throw new Error("kernel is not ready; await kernel.mount(...) first");
		}
		const normalized = normalizeCommandLookup(command);
		const knownCommand =
			this.commands.has(command) || this.commands.has(normalized);
		if (!knownCommand && !this.tryResolveMountedCommand(command)) {
			throw new Error(`ENOENT: command not found: ${command}`);
		}
		const proc = this.proxy.spawn(command, args, options);
		const syncProcessSnapshot = () => {
			const snapshot = this.proxy?.processes.get(proc.pid);
			if (!snapshot) {
				return;
			}
			this.processes.set(proc.pid, {
				...snapshot,
				args: [...snapshot.args],
			});
		};
		syncProcessSnapshot();
		return {
			pid: proc.pid,
			writeStdin(data) {
				return proc.writeStdin(data);
			},
			closeStdin() {
				return proc.closeStdin();
			},
			kill(signal) {
				proc.kill(signal);
				syncProcessSnapshot();
			},
			async wait() {
				const exitCode = await proc.wait();
				syncProcessSnapshot();
				return exitCode;
			},
			get exitCode() {
				return proc.exitCode;
			},
		};
	}

	openShell(options?: OpenShellOptions): ShellHandle {
		if (!this.proxy) {
			throw new Error("kernel is not ready; await kernel.mount(...) first");
		}
		return this.proxy.openShell(options);
	}

	async connectTerminal(options?: ConnectTerminalOptions): Promise<number> {
		await this.ensureReady();
		if (!this.proxy) {
			throw new Error("kernel is not ready");
		}
		return this.proxy.connectTerminal(options);
	}

	mountFs(
		mountPath: string,
		filesystem: VirtualFileSystem,
		options?: { readOnly?: boolean },
	): void | Promise<void> {
		const localMount = makeLocalCompatMount({
			path: mountPath,
			fs: filesystem,
			readOnly: options?.readOnly,
		});
		this.pendingLocalMounts.unshift(localMount);
		this.pendingLocalMounts.sort(
			(left, right) => right.path.length - left.path.length,
		);
		if (!this.proxy) {
			// Pre-boot mounts apply during kernel initialization.
			return;
		}
		return this.proxy.mountFs(mountPath, filesystem, {
			readOnly: localMount.readOnly,
			sidecarMount: localMount.sidecarMount,
		});
	}

	unmountFs(mountPath: string): void | Promise<void> {
		const normalized = normalizePath(mountPath);
		const pendingIndex = this.pendingLocalMounts.findIndex(
			(mount) => mount.path === normalized,
		);
		if (pendingIndex >= 0) {
			this.pendingLocalMounts.splice(pendingIndex, 1);
		}
		return this.proxy?.unmountFs(mountPath);
	}

	async readFile(targetPath: string): Promise<Uint8Array> {
		await this.ensureReady();
		return this.proxy!.readFile(targetPath);
	}

	async writeFile(
		targetPath: string,
		content: string | Uint8Array,
	): Promise<void> {
		await this.ensureReady();
		return this.proxy!.writeFile(targetPath, content);
	}

	async mkdir(targetPath: string): Promise<void> {
		await this.ensureReady();
		return this.proxy!.mkdir(targetPath);
	}

	async readdir(targetPath: string): Promise<string[]> {
		await this.ensureReady();
		return this.proxy!.readdir(targetPath);
	}

	async readdirRecursive(
		targetPath: string,
		options?: { maxDepth?: number },
	): Promise<KernelRecursiveDirEntry[]> {
		await this.ensureReady();
		return this.proxy!.readdirRecursive(targetPath, options);
	}

	async stat(targetPath: string): Promise<VirtualStat> {
		await this.ensureReady();
		return this.proxy!.stat(targetPath);
	}

	async exists(targetPath: string): Promise<boolean> {
		await this.ensureReady();
		return this.proxy!.exists(targetPath);
	}

	async removeFile(targetPath: string): Promise<void> {
		await this.ensureReady();
		return this.proxy!.removeFile(targetPath);
	}

	async removeDir(targetPath: string): Promise<void> {
		await this.ensureReady();
		return this.proxy!.removeDir(targetPath);
	}

	async removePath(
		targetPath: string,
		options?: { recursive?: boolean },
	): Promise<void> {
		await this.ensureReady();
		return this.proxy!.removePath(targetPath, options);
	}

	async rename(oldPath: string, newPath: string): Promise<void> {
		await this.ensureReady();
		return this.proxy!.rename(oldPath, newPath);
	}

	async movePath(oldPath: string, newPath: string): Promise<void> {
		await this.ensureReady();
		return this.proxy!.movePath(oldPath, newPath);
	}

	private tryResolveMountedCommand(command: string): boolean {
		const normalized = normalizeCommandLookup(command);
		for (const driver of this.mountedRuntimeDrivers) {
			if (!driver.tryResolve?.(command)) {
				continue;
			}
			this.commands.set(normalized, driver.kind);
			return true;
		}
		return false;
	}

	private recordModuleExecution(command: string): void {
		for (const driver of this.mountedRuntimeDrivers) {
			driver.recordModuleExecution?.(command);
		}
	}

	private async ensureReady(): Promise<void> {
		if (!this.readyPromise) {
			this.readyPromise = this.initialize();
		}
		return this.readyPromise;
	}

	private async initialize(): Promise<void> {
		const createVmEnv = { ...this.env };
		const requestedPermissions = this.options.permissions;
		const bootstrapPermissions = requestedPermissions ? allowAll : undefined;
		if (this.loopbackExemptPorts.length > 0) {
			createVmEnv.AGENT_OS_LOOPBACK_EXEMPT_PORTS = JSON.stringify(
				this.loopbackExemptPorts,
			);
		}
		const rootPassthroughPlan = planNodeFilesystemPassthroughMounts(
			this.options.filesystem,
			this.pendingLocalMounts,
		);
		const snapshotEntries = await snapshotFilesystemEntries(
			this.options.filesystem,
			"/",
			[],
			{
				passthroughDirectories: rootPassthroughPlan.passthroughDirectories,
			},
		);
		this.liveFilesystemSyncRoots =
			collectLiveFilesystemSyncRoots(snapshotEntries);
		const rootFilesystem = {
			mode: "ephemeral" as const,
			disableDefaultBaseLayer: true,
			lowers: [
				{
					kind: "snapshot" as const,
					entries: rootFilesystemEntriesForConfig(
						mergeRootFilesystemEntries(
							createBootstrapEntries(),
							snapshotEntries,
						),
					),
				},
			],
			bootstrapEntries: [],
		};

		const client = SidecarProcess.spawn({
			cwd: REPO_ROOT,
			command: ensureNativeSidecarBinary(),
			args: [],
		});
		const session = await client.authenticateAndOpenSession();
		const createVmConfig: CreateVmConfig = {
			env: createVmEnv,
			rootFilesystem,
			permissions: bootstrapPermissions
				? serializePermissionsForSidecar(bootstrapPermissions)
				: undefined,
			loopbackExemptPorts: this.loopbackExemptPorts,
			bootstrapCommands: [...NODE_RUNTIME_BOOTSTRAP_COMMANDS],
		};
		const vm = await client.createVm(session, {
			runtime: "java_script",
			config: createVmConfig,
		});
		await client.waitForEvent(
			{
				type: "vm_lifecycle",
				ownership: {
					scope: "vm",
					connection_id: session.connectionId,
					session_id: session.sessionId,
					vm_id: vm.vmId,
				},
				state: "ready",
			},
			10_000,
		);
		if (requestedPermissions && snapshotEntries.length > 1) {
			await materializeSnapshotEntriesIntoVm(
				client,
				session,
				vm,
				snapshotEntries,
			);
		}
		if (rootPassthroughPlan.mounts.length > 0) {
			this.pendingLocalMounts.push(
				...rootPassthroughPlan.mounts.map((mount) =>
					makeLocalCompatMount({
						path: mount.path,
						fs: mount.fs,
						readOnly: mount.readOnly,
					}),
				),
			);
		}
		if (
			this.pendingLocalMounts.length > 0 ||
			this.loopbackExemptPorts.length > 0 ||
			requestedPermissions
		) {
			const sidecarMounts = this.pendingLocalMounts.map((mount) =>
				serializeLocalCompatMountForSidecar(mount),
			);
			await client.configureVm(session, vm, {
				mounts: sidecarMounts,
				permissions: requestedPermissions,
				loopbackExemptPorts: this.loopbackExemptPorts,
			});
		}
		const configuredSidecarMounts = this.pendingLocalMounts.map((mount) =>
			serializeLocalCompatMountForSidecar(mount),
		);

		const proxy = new NativeSidecarKernelProxy({
			client,
			session,
			vm,
			env: this.env,
			cwd: this.cwd,
			defaultExecCwd: this.options.cwd === undefined ? "/workspace" : this.cwd,
			localMounts: this.pendingLocalMounts,
			sidecarMounts: configuredSidecarMounts,
			permissions: requestedPermissions,
			loopbackExemptPorts: this.loopbackExemptPorts,
			commandGuestPaths: new Map<string, string>(),
			onWasmCommandResolved: (command) => {
				this.recordModuleExecution(command);
			},
		});

		this.client = client;
		this.session = session;
		this.vm = vm;
		this.proxy = proxy;
		this.rootFilesystem = proxy.createRootView();
	}
}

export function createKernel(options: {
	filesystem: VirtualFileSystem;
	permissions?: Permissions;
	env?: Record<string, string>;
	cwd?: string;
	maxProcesses?: number;
	hostNetworkAdapter?: unknown;
	loopbackExemptPorts?: number[];
	logger?: unknown;
	mounts?: Array<{ path: string; fs: VirtualFileSystem; readOnly?: boolean }>;
	syncFilesystemOnDispose?: boolean;
	maxFilesystemBytes?: number;
}): Kernel {
	return new NativeKernel(options);
}

function errnoError(code: string, message: string): KernelError {
	return new KernelError(code, `${code}: ${message}`);
}
