import { InMemoryFileSystem, createInMemoryFileSystem } from "./os-filesystem.js";

export type StdioChannel = "stdout" | "stderr";
export type TimingMitigation = "off" | "freeze";
type BodyLike = unknown;

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

export type PermissionDecision =
	| boolean
	| { allowed: boolean; reason?: string }
	| { allow: boolean; reason?: string };
export type PermissionCheck<T = unknown> = (request: T) => PermissionDecision;

export interface Permissions {
	fs?: PermissionCheck<{ path: string; operation: string }>;
	network?: PermissionCheck<{ url?: string; host?: string; port?: number }>;
	childProcess?: PermissionCheck<{ command: string; args: string[] }>;
	env?: PermissionCheck<{ name: string; value: string }>;
}

export const allowAllFs: PermissionCheck = () => true;
export const allowAllNetwork: PermissionCheck = () => true;
export const allowAllChildProcess: PermissionCheck = () => true;
export const allowAllEnv: PermissionCheck = () => true;
export const allowAll: Permissions = {
	fs: allowAllFs,
	network: allowAllNetwork,
	childProcess: allowAllChildProcess,
	env: allowAllEnv,
};

export interface ExecOptions {
	filePath?: string;
	env?: Record<string, string>;
	cwd?: string;
	stdin?: string;
	cpuTimeLimitMs?: number;
	timingMitigation?: TimingMitigation;
	onStdio?: StdioHook;
}

export interface ExecResult {
	code: number;
	exitCode?: number;
	stdout?: string;
	stderr?: string;
	errorMessage?: string;
}

export interface RunResult<T = unknown> {
	value?: T;
	code: number;
	errorMessage?: string;
	exports?: T;
}

export interface OSConfig {
	homedir?: string;
	tmpdir?: string;
}

export interface ProcessConfig {
	cwd?: string;
	env?: Record<string, string>;
	argv?: string[];
	timingMitigation?: TimingMitigation;
	frozenTimeMs?: number;
}

export type StdioEvent = { channel: StdioChannel; message: string };
export type StdioHook = (event: StdioEvent) => void;

export interface CommandExecutor {
	spawn(
		command: string,
		args: string[],
		options?: {
			cwd?: string;
			env?: Record<string, string>;
			onStdout?: (data: Uint8Array) => void;
			onStderr?: (data: Uint8Array) => void;
		},
	): {
		wait(): Promise<number>;
		writeStdin(data: Uint8Array | string): void;
		closeStdin(): void;
		kill(signal?: number): void;
	};
}

export interface NetworkAdapter {
	fetch(
		url: string,
		options?: { method?: string; headers?: Record<string, string>; body?: BodyLike | null },
	): Promise<{
		ok: boolean;
		status: number;
		statusText: string;
		headers: Record<string, string>;
		body: string;
		url: string;
		redirected: boolean;
	}>;
	dnsLookup(
		hostname: string,
	): Promise<{ address?: string; family?: number; error?: string; code?: string }>;
	httpRequest(
		url: string,
		options?: { method?: string; headers?: Record<string, string>; body?: BodyLike | null },
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
}

export interface NodeRuntimeDriver {
	exec(code: string, options?: ExecOptions): Promise<ExecResult>;
	run<T = unknown>(code: string, filePath?: string): Promise<RunResult<T>>;
	dispose(): void;
	terminate?(): Promise<void>;
}

export interface NodeRuntimeDriverFactory {
	createRuntimeDriver(options: RuntimeDriverOptions): NodeRuntimeDriver;
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
		if (!part || part === ".") continue;
		if (part === "..") {
			resolved.pop();
			continue;
		}
		resolved.push(part);
	}
	return resolved.length === 0 ? "/" : `/${resolved.join("/")}`;
}

function dirname(inputPath: string): string {
	const normalized = normalizePath(inputPath);
	if (normalized === "/") return "/";
	const parts = normalized.split("/").filter(Boolean);
	return parts.length <= 1 ? "/" : `/${parts.slice(0, -1).join("/")}`;
}

function permissionAllowed(decision: PermissionDecision | undefined): boolean {
	if (decision === undefined) return true;
	if (typeof decision === "boolean") return decision;
	return "allowed" in decision ? decision.allowed : decision.allow;
}

export function filterEnv(
	env: Record<string, string> | undefined,
	permissions?: Permissions,
): Record<string, string> {
	const source = env ?? {};
	if (!permissions?.env) return { ...source };
	const output: Record<string, string> = {};
	for (const [name, value] of Object.entries(source)) {
		if (permissionAllowed(permissions.env({ name, value }))) {
			output[name] = value;
		}
	}
	return output;
}

export function createEnosysError(operation: string): Error {
	const error = new Error(`ENOSYS: ${operation} is not supported`);
	(error as { code?: string }).code = "ENOSYS";
	return error;
}

export function createFsStub(): VirtualFileSystem {
	return createInMemoryFileSystem();
}

export function createNetworkStub(): NetworkAdapter {
	return {
		async fetch() {
			throw createEnosysError("network.fetch");
		},
		async dnsLookup() {
			return { error: "DNS not supported", code: "ENOSYS" };
		},
		async httpRequest() {
			throw createEnosysError("network.httpRequest");
		},
	};
}

export function createCommandExecutorStub(): CommandExecutor {
	return {
		spawn() {
			throw createEnosysError("child_process.spawn");
		},
	};
}

export function wrapFileSystem(
	filesystem: VirtualFileSystem,
	permissions?: Permissions,
): VirtualFileSystem {
	if (!permissions?.fs) return filesystem;
	const check = (path: string, operation: string): void => {
		if (!permissionAllowed(permissions.fs?.({ path, operation }))) {
			throw new Error(`EACCES: blocked ${operation} on '${path}'`);
		}
	};
	return {
		readFile(path) {
			check(path, "readFile");
			return filesystem.readFile(path);
		},
		readTextFile(path) {
			check(path, "readTextFile");
			return filesystem.readTextFile(path);
		},
		readDir(path) {
			check(path, "readDir");
			return filesystem.readDir(path);
		},
		readDirWithTypes(path) {
			check(path, "readDirWithTypes");
			return filesystem.readDirWithTypes(path);
		},
		writeFile(path, content) {
			check(path, "writeFile");
			return filesystem.writeFile(path, content);
		},
		createDir(path) {
			check(path, "createDir");
			return filesystem.createDir(path);
		},
		mkdir(path, options) {
			check(path, "mkdir");
			return filesystem.mkdir(path, options);
		},
		exists(path) {
			check(path, "exists");
			return filesystem.exists(path);
		},
		stat(path) {
			check(path, "stat");
			return filesystem.stat(path);
		},
		removeFile(path) {
			check(path, "removeFile");
			return filesystem.removeFile(path);
		},
		removeDir(path) {
			check(path, "removeDir");
			return filesystem.removeDir(path);
		},
		rename(oldPath, newPath) {
			check(oldPath, "rename");
			check(newPath, "rename");
			return filesystem.rename(oldPath, newPath);
		},
		realpath(path) {
			check(path, "realpath");
			return filesystem.realpath(path);
		},
		symlink(target, linkPath) {
			check(linkPath, "symlink");
			return filesystem.symlink(target, linkPath);
		},
		readlink(path) {
			check(path, "readlink");
			return filesystem.readlink(path);
		},
		lstat(path) {
			check(path, "lstat");
			return filesystem.lstat(path);
		},
		link(oldPath, newPath) {
			check(oldPath, "link");
			check(newPath, "link");
			return filesystem.link(oldPath, newPath);
		},
		chmod(path, mode) {
			check(path, "chmod");
			return filesystem.chmod(path, mode);
		},
		chown(path, uid, gid) {
			check(path, "chown");
			return filesystem.chown(path, uid, gid);
		},
		utimes(path, atime, mtime) {
			check(path, "utimes");
			return filesystem.utimes(path, atime, mtime);
		},
		truncate(path, length) {
			check(path, "truncate");
			return filesystem.truncate(path, length);
		},
		pread(path, offset, length) {
			check(path, "pread");
			return filesystem.pread(path, offset, length);
		},
		pwrite(path, offset, data) {
			check(path, "pwrite");
			return filesystem.pwrite(path, offset, data);
		},
	};
}

export function wrapNetworkAdapter(
	adapter: NetworkAdapter,
	permissions?: Permissions,
): NetworkAdapter {
	if (!permissions?.network) return adapter;
	const check = (request: { url?: string; host?: string; port?: number }): void => {
		if (!permissionAllowed(permissions.network?.(request))) {
			throw new Error(`EACCES: blocked network access to '${request.url ?? request.host ?? ""}'`);
		}
	};
	return {
		async fetch(url, options) {
			check({ url });
			return adapter.fetch(url, options);
		},
		async dnsLookup(hostname) {
			check({ host: hostname });
			return adapter.dnsLookup(hostname);
		},
		async httpRequest(url, options) {
			check({ url });
			return adapter.httpRequest(url, options);
		},
	};
}

export async function mkdir(
	filesystem: VirtualFileSystem,
	path: string,
	options?: { recursive?: boolean } | boolean,
): Promise<void> {
	if (typeof options === "boolean") {
		return filesystem.mkdir(path, { recursive: options });
	}
	return filesystem.mkdir(path, options);
}

export async function loadFile(
	path: string,
	filesystem: VirtualFileSystem,
): Promise<string | null> {
	try {
		return await filesystem.readTextFile(path);
	} catch {
		return null;
	}
}

export async function resolveModule(
	specifier: string,
	fromPath: string,
	filesystem: VirtualFileSystem,
	_mode: "require" | "import" = "require",
): Promise<string | null> {
	if (!specifier.startsWith(".") && !specifier.startsWith("/") && !specifier.startsWith("node:")) {
		return specifier;
	}
	if (specifier.startsWith("node:")) {
		return specifier;
	}
	const base = specifier.startsWith("/") ? specifier : `${dirname(fromPath)}/${specifier}`;
	const candidates = [
		normalizePath(base),
		`${normalizePath(base)}.js`,
		`${normalizePath(base)}.mjs`,
		`${normalizePath(base)}/index.js`,
	];
	for (const candidate of candidates) {
		if (await filesystem.exists(candidate)) {
			return candidate;
		}
	}
	return null;
}

export function isESM(code: string, filePath?: string): boolean {
	if (filePath?.endsWith(".mjs")) return true;
	return /\b(import|export)\b/.test(code);
}

export function transformDynamicImport(code: string): string {
	return code;
}

export const POLYFILL_CODE_MAP: Record<string, string> = {
	fs: "module.exports = globalThis._fsModule;",
	"node:fs": "module.exports = globalThis._fsModule;",
};

export function exposeCustomGlobal(name: string, value: unknown): void {
	(globalThis as Record<string, unknown>)[name] = value;
}

export function exposeMutableRuntimeStateGlobal(name: string, value: unknown): void {
	(globalThis as Record<string, unknown>)[name] = value;
}

export function getIsolateRuntimeSource(id: string): string {
	if (id === "overrideProcessCwd") {
		return `
			if (globalThis.process && globalThis.__runtimeProcessCwdOverride) {
				globalThis.process.cwd = () => String(globalThis.__runtimeProcessCwdOverride);
			}
		`;
	}
	return "";
}

export function getRequireSetupCode(): string {
	return `
		(function () {
			const pathDirname = (value) => {
				const normalized = String(value || "/").replace(/\\\\/g, "/");
				if (normalized === "/") return "/";
				const parts = normalized.split("/").filter(Boolean);
				return parts.length <= 1 ? "/" : "/" + parts.slice(0, -1).join("/");
			};

			globalThis.require = function require(specifier) {
				const polyfillSource = globalThis._loadPolyfill?.(specifier.replace(/^node:/, ""));
				if (polyfillSource) {
					const module = { exports: {} };
					const fn = new Function("module", "exports", polyfillSource);
					fn(module, module.exports);
					return module.exports;
				}

				const currentModule = globalThis._currentModule || { dirname: "/" };
				const resolved = globalThis._resolveModuleSync?.(
					specifier,
					currentModule.dirname || "/",
					"require",
				);
				if (!resolved) {
					throw new Error("Cannot resolve module '" + specifier + "'");
				}

				const cache = globalThis._moduleCache || (globalThis._moduleCache = {});
				if (cache[resolved]) {
					return cache[resolved].exports;
				}

				const source = globalThis._loadFileSync?.(resolved, "require");
				if (source == null) {
					throw new Error("Cannot load module '" + resolved + "'");
				}

				const module = { exports: {} };
				cache[resolved] = module;
				const previous = globalThis._currentModule;
				globalThis._currentModule = { filename: resolved, dirname: pathDirname(resolved) };
				try {
					const fn = new Function(
						"require",
						"module",
						"exports",
						"__filename",
						"__dirname",
						source,
					);
					fn(globalThis.require, module, module.exports, resolved, pathDirname(resolved));
				} finally {
					globalThis._currentModule = previous;
				}
				return module.exports;
			};
		})();
	`;
}

export { InMemoryFileSystem, createInMemoryFileSystem };
