import { execFileSync } from "node:child_process";
import * as fsSync from "node:fs";
import * as fs from "node:fs/promises";
import { tmpdir } from "node:os";
import * as path from "node:path";
import * as posixPath from "node:path/posix";
import { fileURLToPath } from "node:url";
import {
	type AuthenticatedSession,
	type CreatedVm,
	type LocalCompatMount,
	NativeSidecarKernelProxy,
	NativeSidecarProcessClient,
	type RootFilesystemEntry,
	serializeMountConfigForSidecar,
} from "./sidecar/rpc-client.js";

export const AF_INET = 2;
export const AF_UNIX = 1;
export const SOCK_STREAM = 1;
export const SOCK_DGRAM = 2;
export const SIGTERM = 15;

const S_IFREG = 0o100000;
const S_IFDIR = 0o040000;
const S_IFLNK = 0o120000;
const MAX_SYMLINK_DEPTH = 40;
const KERNEL_COMMAND_STUB = "#!/bin/sh\n# kernel command stub\n";
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
] as const;
const REPO_ROOT = fileURLToPath(new URL("../../..", import.meta.url));
const SIDECAR_BINARY = path.join(REPO_ROOT, "target/debug/agent-os-sidecar");
const SIDECAR_BUILD_INPUTS = [
	path.join(REPO_ROOT, "Cargo.toml"),
	path.join(REPO_ROOT, "Cargo.lock"),
	path.join(REPO_ROOT, "crates/bridge"),
	path.join(REPO_ROOT, "crates/execution"),
	path.join(REPO_ROOT, "crates/kernel"),
	path.join(REPO_ROOT, "crates/sidecar"),
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

interface FileEntry {
	type: "file";
	data: Uint8Array;
	mode: number;
	uid: number;
	gid: number;
	nlink: number;
	ino: number;
	atimeMs: number;
	mtimeMs: number;
	ctimeMs: number;
	birthtimeMs: number;
}

interface DirectoryEntry {
	type: "dir";
	mode: number;
	uid: number;
	gid: number;
	nlink: number;
	ino: number;
	atimeMs: number;
	mtimeMs: number;
	ctimeMs: number;
	birthtimeMs: number;
}

interface SymlinkEntry {
	type: "symlink";
	target: string;
	mode: number;
	uid: number;
	gid: number;
	nlink: number;
	ino: number;
	atimeMs: number;
	mtimeMs: number;
	ctimeMs: number;
	birthtimeMs: number;
}

type MemoryEntry = FileEntry | DirectoryEntry | SymlinkEntry;
let nextInode = 1;

export class InMemoryFileSystem implements VirtualFileSystem {
	private readonly entries = new Map<string, MemoryEntry>();

	constructor() {
		this.entries.set("/", this.newDirectory());
	}

	async readFile(targetPath: string): Promise<Uint8Array> {
		const entry = this.resolveEntry(targetPath);
		if (!entry || entry.type !== "file") {
			throw errnoError("ENOENT", `open '${targetPath}'`);
		}
		entry.atimeMs = Date.now();
		return entry.data;
	}

	async readTextFile(targetPath: string): Promise<string> {
		return new TextDecoder().decode(await this.readFile(targetPath));
	}

	async readDir(targetPath: string): Promise<string[]> {
		return (await this.readDirWithTypes(targetPath)).map((entry) => entry.name);
	}

	async readDirWithTypes(targetPath: string): Promise<VirtualDirEntry[]> {
		const resolved = this.resolvePath(targetPath);
		const entry = this.entries.get(resolved);
		if (!entry || entry.type !== "dir") {
			throw errnoError("ENOENT", `scandir '${targetPath}'`);
		}
		const prefix = resolved === "/" ? "/" : `${resolved}/`;
		const output = new Map<string, VirtualDirEntry>();
		for (const [entryPath, candidate] of this.entries) {
			if (!entryPath.startsWith(prefix)) continue;
			const rest = entryPath.slice(prefix.length);
			if (!rest || rest.includes("/")) continue;
			output.set(rest, {
				name: rest,
				isDirectory: candidate.type === "dir",
				isSymbolicLink: candidate.type === "symlink",
			});
		}
		return [...output.values()];
	}

	async writeFile(
		targetPath: string,
		content: string | Uint8Array,
	): Promise<void> {
		const normalized = normalizePath(targetPath);
		await this.mkdir(dirnameVirtual(normalized), { recursive: true });
		const data =
			typeof content === "string" ? new TextEncoder().encode(content) : content;
		const existing = this.entries.get(normalized);
		if (existing?.type === "file") {
			existing.data = data;
			existing.mtimeMs = Date.now();
			existing.ctimeMs = Date.now();
			return;
		}
		const now = Date.now();
		this.entries.set(normalized, {
			type: "file",
			data,
			mode: S_IFREG | 0o644,
			uid: 0,
			gid: 0,
			nlink: 1,
			ino: nextInode++,
			atimeMs: now,
			mtimeMs: now,
			ctimeMs: now,
			birthtimeMs: now,
		});
	}

	async createDir(targetPath: string): Promise<void> {
		const normalized = normalizePath(targetPath);
		if (!this.entries.has(dirnameVirtual(normalized))) {
			throw errnoError("ENOENT", `mkdir '${targetPath}'`);
		}
		if (!this.entries.has(normalized)) {
			this.entries.set(normalized, this.newDirectory());
		}
	}

	async mkdir(
		targetPath: string,
		options?: { recursive?: boolean },
	): Promise<void> {
		const normalized = normalizePath(targetPath);
		if (options?.recursive === false) {
			return this.createDir(normalized);
		}
		let current = "";
		for (const part of normalized.split("/").filter(Boolean)) {
			current += `/${part}`;
			if (!this.entries.has(current)) {
				this.entries.set(current, this.newDirectory());
			}
		}
	}

	async exists(targetPath: string): Promise<boolean> {
		try {
			return this.entries.has(this.resolvePath(targetPath));
		} catch {
			return false;
		}
	}

	async stat(targetPath: string): Promise<VirtualStat> {
		const entry = this.resolveEntry(targetPath);
		if (!entry) throw errnoError("ENOENT", `stat '${targetPath}'`);
		return this.toStat(entry);
	}

	async removeFile(targetPath: string): Promise<void> {
		const resolved = this.resolvePath(targetPath);
		const entry = this.entries.get(resolved);
		if (!entry || entry.type === "dir") {
			throw errnoError("ENOENT", `unlink '${targetPath}'`);
		}
		this.entries.delete(resolved);
	}

	async removeDir(targetPath: string): Promise<void> {
		const resolved = this.resolvePath(targetPath);
		if (resolved === "/") {
			throw errnoError("EPERM", "operation not permitted");
		}
		const entry = this.entries.get(resolved);
		if (!entry || entry.type !== "dir") {
			throw errnoError("ENOENT", `rmdir '${targetPath}'`);
		}
		const prefix = `${resolved}/`;
		for (const key of this.entries.keys()) {
			if (key.startsWith(prefix)) {
				throw errnoError("ENOTEMPTY", `directory not empty '${targetPath}'`);
			}
		}
		this.entries.delete(resolved);
	}

	async rename(oldPath: string, newPath: string): Promise<void> {
		const oldResolved = this.resolvePath(oldPath);
		const newResolved = normalizePath(newPath);
		const entry = this.entries.get(oldResolved);
		if (!entry) throw errnoError("ENOENT", `rename '${oldPath}'`);
		if (!this.entries.has(dirnameVirtual(newResolved))) {
			throw errnoError("ENOENT", `rename '${newPath}'`);
		}
		if (entry.type !== "dir") {
			this.entries.set(newResolved, entry);
			this.entries.delete(oldResolved);
			return;
		}
		const prefix = `${oldResolved}/`;
		const moved: Array<[string, MemoryEntry]> = [];
		for (const candidate of this.entries) {
			if (candidate[0] === oldResolved || candidate[0].startsWith(prefix)) {
				moved.push(candidate);
			}
		}
		for (const [candidatePath] of moved) {
			this.entries.delete(candidatePath);
		}
		for (const [candidatePath, candidate] of moved) {
			const nextPath =
				candidatePath === oldResolved
					? newResolved
					: `${newResolved}${candidatePath.slice(oldResolved.length)}`;
			this.entries.set(nextPath, candidate);
		}
	}

	async realpath(targetPath: string): Promise<string> {
		return this.resolvePath(targetPath);
	}

	async symlink(target: string, linkPath: string): Promise<void> {
		const normalized = normalizePath(linkPath);
		if (this.entries.has(normalized)) {
			throw errnoError("EEXIST", `symlink '${linkPath}'`);
		}
		const now = Date.now();
		this.entries.set(normalized, {
			type: "symlink",
			target,
			mode: S_IFLNK | 0o777,
			uid: 0,
			gid: 0,
			nlink: 1,
			ino: nextInode++,
			atimeMs: now,
			mtimeMs: now,
			ctimeMs: now,
			birthtimeMs: now,
		});
	}

	async readlink(targetPath: string): Promise<string> {
		const normalized = normalizePath(targetPath);
		const entry = this.entries.get(normalized);
		if (!entry || entry.type !== "symlink") {
			throw errnoError("ENOENT", `readlink '${targetPath}'`);
		}
		return entry.target;
	}

	async lstat(targetPath: string): Promise<VirtualStat> {
		const entry = this.entries.get(normalizePath(targetPath));
		if (!entry) throw errnoError("ENOENT", `lstat '${targetPath}'`);
		return this.toStat(entry);
	}

	async link(oldPath: string, newPath: string): Promise<void> {
		const entry = this.resolveEntry(oldPath);
		if (!entry || entry.type !== "file") {
			throw errnoError("ENOENT", `link '${oldPath}'`);
		}
		const normalized = normalizePath(newPath);
		if (this.entries.has(normalized)) {
			throw errnoError("EEXIST", `link '${newPath}'`);
		}
		entry.nlink += 1;
		this.entries.set(normalized, entry);
	}

	async chmod(targetPath: string, mode: number): Promise<void> {
		const entry = this.resolveEntry(targetPath);
		if (!entry) throw errnoError("ENOENT", `chmod '${targetPath}'`);
		const typeBits = mode & 0o170000;
		entry.mode =
			typeBits === 0 ? (entry.mode & 0o170000) | (mode & 0o7777) : mode;
		entry.ctimeMs = Date.now();
	}

	async chown(targetPath: string, uid: number, gid: number): Promise<void> {
		const entry = this.resolveEntry(targetPath);
		if (!entry) throw errnoError("ENOENT", `chown '${targetPath}'`);
		entry.uid = uid;
		entry.gid = gid;
		entry.ctimeMs = Date.now();
	}

	async utimes(
		targetPath: string,
		atime: number,
		mtime: number,
	): Promise<void> {
		const entry = this.resolveEntry(targetPath);
		if (!entry) throw errnoError("ENOENT", `utimes '${targetPath}'`);
		entry.atimeMs = atime;
		entry.mtimeMs = mtime;
		entry.ctimeMs = Date.now();
	}

	async truncate(targetPath: string, length: number): Promise<void> {
		const entry = this.resolveEntry(targetPath);
		if (!entry || entry.type !== "file") {
			throw errnoError("ENOENT", `truncate '${targetPath}'`);
		}
		if (length < entry.data.length) {
			entry.data = entry.data.slice(0, length);
		} else if (length > entry.data.length) {
			const expanded = new Uint8Array(length);
			expanded.set(entry.data);
			entry.data = expanded;
		}
		entry.mtimeMs = Date.now();
		entry.ctimeMs = Date.now();
	}

	async pread(
		targetPath: string,
		offset: number,
		length: number,
	): Promise<Uint8Array> {
		const entry = this.resolveEntry(targetPath);
		if (!entry || entry.type !== "file") {
			throw errnoError("ENOENT", `open '${targetPath}'`);
		}
		if (offset >= entry.data.length) return new Uint8Array(0);
		return entry.data.slice(
			offset,
			Math.min(offset + length, entry.data.length),
		);
	}

	async pwrite(
		targetPath: string,
		offset: number,
		data: Uint8Array,
	): Promise<void> {
		const entry = this.resolveEntry(targetPath);
		if (!entry || entry.type !== "file") {
			throw errnoError("ENOENT", `open '${targetPath}'`);
		}
		const nextSize = Math.max(entry.data.length, offset + data.length);
		const updated = new Uint8Array(nextSize);
		updated.set(entry.data);
		updated.set(data, offset);
		entry.data = updated;
		entry.mtimeMs = Date.now();
		entry.ctimeMs = Date.now();
	}

	private resolvePath(targetPath: string, depth = 0): string {
		if (depth > MAX_SYMLINK_DEPTH) {
			throw errnoError("ELOOP", `too many symbolic links '${targetPath}'`);
		}
		const normalized = normalizePath(targetPath);
		const entry = this.entries.get(normalized);
		if (!entry) return normalized;
		if (entry.type === "symlink") {
			const target = entry.target.startsWith("/")
				? entry.target
				: `${dirnameVirtual(normalized)}/${entry.target}`;
			return this.resolvePath(target, depth + 1);
		}
		return normalized;
	}

	private resolveEntry(targetPath: string): MemoryEntry | undefined {
		return this.entries.get(this.resolvePath(targetPath));
	}

	private newDirectory(): DirectoryEntry {
		const now = Date.now();
		return {
			type: "dir",
			mode: S_IFDIR | 0o755,
			uid: 0,
			gid: 0,
			nlink: 2,
			ino: nextInode++,
			atimeMs: now,
			mtimeMs: now,
			ctimeMs: now,
			birthtimeMs: now,
		};
	}

	private toStat(entry: MemoryEntry): VirtualStat {
		const size = entry.type === "file" ? entry.data.length : 4096;
		return {
			mode: entry.mode,
			size,
			blocks: size === 0 ? 0 : Math.ceil(size / 512),
			dev: 1,
			rdev: 0,
			isDirectory: entry.type === "dir",
			isSymbolicLink: entry.type === "symlink",
			atimeMs: entry.atimeMs,
			mtimeMs: entry.mtimeMs,
			ctimeMs: entry.ctimeMs,
			birthtimeMs: entry.birthtimeMs,
			ino: entry.ino,
			nlink: entry.nlink,
			uid: entry.uid,
			gid: entry.gid,
		};
	}
}

export function createInMemoryFileSystem(): InMemoryFileSystem {
	return new InMemoryFileSystem();
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
export const allowAllEnv: EnvPermissions = "allow";
export const allowAll: Permissions = {
	fs: allowAllFs,
	network: allowAllNetwork,
	childProcess: allowAllChildProcess,
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

export class ModuleAccessFileSystem extends NodeFileSystem {}

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
	"wget",
	"make",
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
	"spawn-test-host",
	"http-test",
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
	make: "full",
	codex: "full",
	"codex-exec": "full",
	"spawn-test-host": "full",
	"http-test": "full",
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
	wget: "full",
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

function isWasmBinaryFile(filePath: string): boolean {
	try {
		const header = fsSync.readFileSync(filePath);
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

function discoverCommands(commandDirs: string[]): string[] {
	const commands = new Set<string>();
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
			const fullPath = path.join(commandDir, entry);
			if (isWasmBinaryFile(fullPath)) {
				commands.add(entry);
				continue;
			}
			try {
				const realPath = fsSync.realpathSync(fullPath);
				if (isWasmBinaryFile(realPath)) {
					commands.add(entry);
				}
			} catch {}
		}
	}
	return [...commands];
}

export function createWasmVmRuntime(
	options: WasmVmRuntimeOptions = {},
): KernelRuntimeDriver {
	if (options.commandDirs && options.commandDirs.length > 0) {
		return new NativeRuntimeDescriptor(
			"wasmvm",
			"wasmvm",
			discoverCommands(options.commandDirs),
			options.commandDirs,
		);
	}
	return new NativeRuntimeDescriptor(
		"wasmvm",
		"wasmvm",
		[...WASMVM_COMMANDS],
		options.wasmBinaryPath ? [path.dirname(options.wasmBinaryPath)] : undefined,
	);
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
	if (
		ensuredSidecarBinary &&
		fsSync.existsSync(ensuredSidecarBinary) &&
		!sidecarBinaryNeedsBuild()
	) {
		return ensuredSidecarBinary;
	}
	if (sidecarBinaryNeedsBuild()) {
		execFileSync("cargo", ["build", "-q", "-p", "agent-os-sidecar"], {
			cwd: REPO_ROOT,
			stdio: "pipe",
		});
	}
	ensuredSidecarBinary = SIDECAR_BINARY;
	return ensuredSidecarBinary;
}

function createBootstrapEntries(commandNames: string[]): RootFilesystemEntry[] {
	const entries: RootFilesystemEntry[] = [
		{
			path: "/",
			kind: "directory",
			mode: 0o755,
			uid: 0,
			gid: 0,
		},
		...KERNEL_POSIX_BOOTSTRAP_DIRS.map((entryPath) => ({
			path: entryPath,
			kind: "directory" as const,
			mode: 0o755,
			uid: 0,
			gid: 0,
		})),
		{
			path: "/usr/bin/env",
			kind: "file",
			mode: 0o644,
			uid: 0,
			gid: 0,
			content: "",
			encoding: "utf8",
		},
	];
	for (const command of [...new Set(commandNames)].sort((left, right) =>
		left.localeCompare(right),
	)) {
		entries.push({
			path: `/bin/${command}`,
			kind: "file",
			mode: 0o755,
			uid: 0,
			gid: 0,
			content: KERNEL_COMMAND_STUB,
			encoding: "utf8",
		});
	}
	return entries;
}

async function snapshotFilesystemEntries(
	filesystem: VirtualFileSystem,
	targetPath = "/",
	output: RootFilesystemEntry[] = [],
): Promise<RootFilesystemEntry[]> {
	const statInfo =
		targetPath === "/"
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
		});
		const children = (await filesystem.readDirWithTypes(targetPath))
			.map((entry) => entry.name)
			.filter((name) => name !== "." && name !== "..")
			.sort((left, right) => left.localeCompare(right));
		for (const child of children) {
			const childPath =
				targetPath === "/"
					? posixPath.join("/", child)
					: posixPath.join(targetPath, child);
			await snapshotFilesystemEntries(filesystem, childPath, output);
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
	});
	return output;
}

function collectGuestCommandPaths(
	commandDirs: string[],
	startIndex = 0,
): Map<string, string> {
	const guestPaths = new Map<string, string>();
	commandDirs.forEach((commandDir, index) => {
		let entries: string[];
		try {
			entries = fsSync
				.readdirSync(commandDir)
				.sort((left, right) => left.localeCompare(right));
		} catch {
			return;
		}
		for (const entry of entries) {
			if (entry.startsWith(".")) continue;
			if (!isWasmBinaryFile(path.join(commandDir, entry))) continue;
			if (!guestPaths.has(entry)) {
				guestPaths.set(
					entry,
					`/__agentos/commands/${startIndex + index}/${entry}`,
				);
			}
		}
	});
	return guestPaths;
}

async function ensureCommandStubs(
	proxy: NativeSidecarKernelProxy,
	commands: Iterable<string>,
): Promise<void> {
	for (const command of commands) {
		const stubPath = `/bin/${command}`;
		if (await proxy.exists(stubPath)) continue;
		await proxy.writeFile(stubPath, KERNEL_COMMAND_STUB);
	}
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

class NativeKernel implements Kernel {
	readonly env: Record<string, string>;
	readonly cwd: string;
	readonly commands = new Map<string, string>();
	readonly processes = new Map<number, ProcessInfo>();
	readonly socketTable;
	readonly processTable;
	readonly timerTable = {};
	readonly vfs: VirtualFileSystem;

	private client: NativeSidecarProcessClient | null = null;
	private session: AuthenticatedSession | null = null;
	private vm: CreatedVm | null = null;
	private proxy: NativeSidecarKernelProxy | null = null;
	private rootFilesystem: VirtualFileSystem | null = null;
	private readyPromise: Promise<void> | null = null;
	private readonly pendingLocalMounts: LocalCompatMount[] = [];
	private mountedCommandDirs: string[] = [];
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
		},
	) {
		this.env = { ...(options.env ?? {}) };
		this.cwd = options.cwd ?? "/";
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
			this.pendingLocalMounts.push({
				path: normalizePath(mount.path),
				fs: mount.fs,
				readOnly: mount.readOnly ?? false,
			});
		}
		this.vfs = new DeferredFileSystem(() => this.rootFilesystem);
	}

	get zombieTimerCount(): number {
		return this.proxy?.zombieTimerCount ?? 0;
	}

	async mount(driver: KernelRuntimeDriver): Promise<void> {
		await this.ensureReady();
		if (!this.proxy || !this.client || !this.session || !this.vm) {
			throw new Error("kernel is not ready");
		}
		if (driver.kind === "node") {
			for (const command of driver.commands) {
				this.commands.set(command, "node");
			}
			await ensureCommandStubs(this.proxy, driver.commands);
			return;
		}

		const commandDirs = driver.commandDirs ?? [];
		if (commandDirs.length === 0) {
			for (const command of driver.commands) {
				this.commands.set(command, "wasmvm");
			}
			await ensureCommandStubs(this.proxy, driver.commands);
			return;
		}

		const startIndex = this.mountedCommandDirs.length;
		const newGuestPaths = collectGuestCommandPaths(commandDirs, startIndex);
		const commandMountMappings = commandDirs.map((commandDir, index) => ({
			guestPath: `/__agentos/commands/${startIndex + index}`,
			hostPath: commandDir,
		}));
		const sidecarMounts = commandDirs.map((commandDir, index) =>
			serializeMountConfigForSidecar({
				path: `/__agentos/commands/${startIndex + index}`,
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
		await this.client.configureVm(this.session, this.vm, {
			mounts: sidecarMounts,
			loopbackExemptPorts: this.loopbackExemptPorts,
		});
		this.proxy.registerCommandGuestPaths(newGuestPaths);
		this.mountedCommandDirs.push(...commandDirs);
		for (const command of newGuestPaths.keys()) {
			this.commands.set(command, "wasmvm");
		}
		await ensureCommandStubs(this.proxy, newGuestPaths.keys());
	}

	async dispose(): Promise<void> {
		await this.readyPromise?.catch(() => {});
		await this.proxy?.dispose().catch(() => {});
		this.proxy = null;
		this.rootFilesystem = null;
		this.client = null;
		this.session = null;
		this.vm = null;
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
		return this.proxy.spawn(command, args, options);
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
	): void {
		if (!this.proxy) {
			this.pendingLocalMounts.push({
				path: normalizePath(mountPath),
				fs: filesystem,
				readOnly: options?.readOnly ?? false,
			});
			return;
		}
		this.proxy.mountFs(mountPath, filesystem, options);
	}

	unmountFs(mountPath: string): void {
		this.proxy?.unmountFs(mountPath);
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

	async rename(oldPath: string, newPath: string): Promise<void> {
		await this.ensureReady();
		return this.proxy!.rename(oldPath, newPath);
	}

	private async ensureReady(): Promise<void> {
		if (!this.readyPromise) {
			this.readyPromise = this.initialize();
		}
		return this.readyPromise;
	}

	private async initialize(): Promise<void> {
		const createVmEnv = { ...this.env };
		if (this.loopbackExemptPorts.length > 0) {
			createVmEnv.AGENT_OS_LOOPBACK_EXEMPT_PORTS = JSON.stringify(
				this.loopbackExemptPorts,
			);
		}
		const rootFilesystem = {
			disableDefaultBaseLayer: true,
			lowers: [
				{
					kind: "snapshot" as const,
					entries: await snapshotFilesystemEntries(this.options.filesystem),
				},
			],
		};

		const client = NativeSidecarProcessClient.spawn({
			cwd: REPO_ROOT,
			command: ensureNativeSidecarBinary(),
			args: [],
			frameTimeoutMs: 60_000,
		});
		const session = await client.authenticateAndOpenSession();
		const vm = await client.createVm(session, {
			runtime: "java_script",
			metadata: {
				...Object.fromEntries(
					Object.entries(createVmEnv).map(([key, value]) => [`env.${key}`, value]),
				),
			},
			rootFilesystem,
		});
		await client.waitForEvent(
			(event) =>
				event.payload.type === "vm_lifecycle" &&
				event.payload.state === "ready",
			10_000,
		);
		if (
			this.pendingLocalMounts.length > 0 ||
			this.loopbackExemptPorts.length > 0
		) {
			await client.configureVm(session, vm, {
				mounts: this.pendingLocalMounts.map((mount) =>
					mount.fs instanceof NodeFileSystem
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
							}),
				),
				loopbackExemptPorts: this.loopbackExemptPorts,
			});
		}

		const proxy = new NativeSidecarKernelProxy({
			client,
			session,
			vm,
			env: this.env,
			cwd: this.cwd,
			localMounts: this.pendingLocalMounts,
			commandGuestPaths: new Map<string, string>(),
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
}): Kernel {
	return new NativeKernel(options);
}

function errnoError(code: string, message: string): KernelError {
	return new KernelError(code, `${code}: ${message}`);
}
