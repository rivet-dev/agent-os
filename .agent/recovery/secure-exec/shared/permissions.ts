/**
 * Permission enforcement layer.
 *
 * Wraps filesystem, network, and command-executor adapters with permission
 * checks that throw EACCES on denial. When no permission callback is provided
 * for a category, guarded operations in that category are denied by default.
 */

import { createEaccesError, createEnosysError } from "./errors.js";
import type {
	EnvAccessRequest,
	FsAccessRequest,
	Permissions,
} from "../kernel/types.js";
import type {
	VirtualFileSystem,
} from "../kernel/vfs.js";
import type {
	CommandExecutor,
	NetworkAdapter,
} from "../types.js";

/** Normalize a filesystem path: collapse //, resolve . and .., strip trailing /. */
function normalizeFsPath(path: string): string {
	// Collapse repeated slashes
	let p = path.replace(/\/+/g, "/");
	// Resolve . and .. segments
	const parts = p.split("/");
	const resolved: string[] = [];
	for (const seg of parts) {
		if (seg === ".") continue;
		if (seg === "..") {
			// Don't pop past root
			if (resolved.length > 1) resolved.pop();
		} else {
			resolved.push(seg);
		}
	}
	p = resolved.join("/") || "/";
	// Strip trailing slash (except root)
	if (p.length > 1 && p.endsWith("/")) {
		p = p.slice(0, -1);
	}
	return p;
}

/** Run the permission check; throw the deny error if no checker exists or it denies. */
function checkPermission<T>(
	check: ((request: T) => { allow: boolean; reason?: string }) | undefined,
	request: T,
	onDenied: (request: T, reason?: string) => Error,
): void {
	if (!check) {
		throw onDenied(request);
	}
	const decision = check(request);
	if (!decision?.allow) {
		throw onDenied(request, decision?.reason);
	}
}

// Permission callbacks must be self-contained (no closures) because they are
// serialized via `.toString()` for transfer to the browser Web Worker.
export const allowAllFs: Pick<Permissions, "fs"> = {
	fs: () => ({ allow: true }),
};

export const allowAllNetwork: Pick<Permissions, "network"> = {
	network: () => ({ allow: true }),
};

export const allowAllChildProcess: Pick<Permissions, "childProcess"> = {
	childProcess: () => ({ allow: true }),
};

export const allowAllEnv: Pick<Permissions, "env"> = {
	env: () => ({ allow: true }),
};

export const allowAll: Permissions = {
	...allowAllFs,
	...allowAllNetwork,
	...allowAllChildProcess,
	...allowAllEnv,
};

function fsOpToSyscall(op: FsAccessRequest["op"]): string {
	switch (op) {
		case "read":
			return "open";
		case "write":
			return "write";
		case "mkdir":
		case "createDir":
			return "mkdir";
		case "readdir":
			return "scandir";
		case "stat":
			return "stat";
		case "rm":
			return "unlink";
		case "rename":
			return "rename";
		case "exists":
			return "access";
		case "chmod":
			return "chmod";
		case "chown":
			return "chown";
		case "link":
			return "link";
		case "symlink":
			return "symlink";
		case "readlink":
			return "readlink";
		case "truncate":
			return "open";
		case "utimes":
			return "utimes";
		default:
			return "open";
	}
}

/**
 * Wrap a VirtualFileSystem so every operation passes through the fs permission check.
 * Throws EACCES if the permission callback denies or is absent.
 */
export function wrapFileSystem(
	fs: VirtualFileSystem,
	permissions?: Permissions,
): VirtualFileSystem {
	/** Check fs permission with normalized path to prevent traversal bypasses. */
	function checkFs(op: FsAccessRequest["op"], path: string, reason?: string): void {
		checkPermission(
			permissions?.fs,
			{ op, path: normalizeFsPath(path) },
			(req, r) => createEaccesError(fsOpToSyscall(req.op), req.path, r),
		);
	}

	return {
		readFile: async (path) => {
			checkFs("read", path);
			return fs.readFile(path);
		},
		readTextFile: async (path) => {
			checkFs("read", path);
			return fs.readTextFile(path);
		},
		readDir: async (path) => {
			checkFs("readdir", path);
			return fs.readDir(path);
		},
		readDirWithTypes: async (path) => {
			checkFs("readdir", path);
			return fs.readDirWithTypes(path);
		},
		writeFile: async (path, content) => {
			checkFs("write", path);
			return fs.writeFile(path, content);
		},
		createDir: async (path) => {
			checkFs("createDir", path);
			return fs.createDir(path);
		},
		mkdir: async (path, options?) => {
			checkFs("mkdir", path);
			return fs.mkdir(path, options);
		},
		exists: async (path) => {
			checkFs("exists", path);
			return fs.exists(path);
		},
		stat: async (path) => {
			checkFs("stat", path);
			return fs.stat(path);
		},
		removeFile: async (path) => {
			checkFs("rm", path);
			return fs.removeFile(path);
		},
		removeDir: async (path) => {
			checkFs("rm", path);
			return fs.removeDir(path);
		},
		rename: async (oldPath, newPath) => {
			checkFs("rename", oldPath);
			checkFs("rename", newPath);
			return fs.rename(oldPath, newPath);
		},
		symlink: async (target, linkPath) => {
			checkFs("symlink", linkPath);
			return fs.symlink(target, linkPath);
		},
		readlink: async (path) => {
			checkFs("readlink", path);
			return fs.readlink(path);
		},
		lstat: async (path) => {
			checkFs("stat", path);
			return fs.lstat(path);
		},
		link: async (oldPath, newPath) => {
			checkFs("link", newPath);
			return fs.link(oldPath, newPath);
		},
		chmod: async (path, mode) => {
			checkFs("chmod", path);
			return fs.chmod(path, mode);
		},
		chown: async (path, uid, gid) => {
			checkFs("chown", path);
			return fs.chown(path, uid, gid);
		},
		utimes: async (path, atime, mtime) => {
			checkFs("utimes", path);
			return fs.utimes(path, atime, mtime);
		},
		truncate: async (path, length) => {
			checkFs("truncate", path);
			return fs.truncate(path, length);
		},
		realpath: async (path) => {
			checkFs("read", path);
			return fs.realpath(path);
		},
		pread: async (path, offset, length) => {
			checkFs("read", path);
			return fs.pread(path, offset, length);
		},
		pwrite: async (path, offset, data) => {
			checkFs("write", path);
			return fs.pwrite(path, offset, data);
		},
	};
}

/** Wrap a NetworkAdapter so external client operations pass through the network permission check. */
export function wrapNetworkAdapter(
	adapter: NetworkAdapter,
	permissions?: Permissions,
): NetworkAdapter {
	const loopbackAwareAdapter = adapter as NetworkAdapter & {
		__setLoopbackPortChecker?: (checker: (hostname: string, port: number) => boolean) => void;
	};
	const wrapped: NetworkAdapter & {
		__setLoopbackPortChecker?: (checker: (hostname: string, port: number) => boolean) => void;
	} = {
		fetch: async (url, options) => {
			checkPermission(
				permissions?.network,
				{ op: "fetch", url, method: options?.method },
				(req, reason) => createEaccesError("connect", req.url, reason),
			);
			return adapter.fetch(url, options);
		},
		dnsLookup: async (hostname) => {
			checkPermission(
				permissions?.network,
				{ op: "dns", hostname },
				(req, reason) => createEaccesError("connect", req.hostname, reason),
			);
			return adapter.dnsLookup(hostname);
		},
		httpRequest: async (url, options) => {
			checkPermission(
				permissions?.network,
				{ op: "http", url, method: options?.method },
				(req, reason) => createEaccesError("connect", req.url, reason),
			);
			return adapter.httpRequest(url, options);
		},
		// Forward upgrade socket methods for bidirectional WebSocket relay
		upgradeSocketWrite: adapter.upgradeSocketWrite?.bind(adapter),
		upgradeSocketEnd: adapter.upgradeSocketEnd?.bind(adapter),
		upgradeSocketDestroy: adapter.upgradeSocketDestroy?.bind(adapter),
		setUpgradeSocketCallbacks: adapter.setUpgradeSocketCallbacks?.bind(adapter),
	};
	if (typeof loopbackAwareAdapter.__setLoopbackPortChecker === "function") {
		wrapped.__setLoopbackPortChecker = (checker) =>
			loopbackAwareAdapter.__setLoopbackPortChecker!(checker);
	}
	return wrapped;
}

/** Wrap a CommandExecutor so spawn passes through the childProcess permission check. */
export function wrapCommandExecutor(
	executor: CommandExecutor,
	permissions?: Permissions,
): CommandExecutor {
	return {
		spawn: (command, args, options) => {
			checkPermission(
				permissions?.childProcess,
				{ command, args, cwd: options.cwd, env: options.env },
				(req, reason) => createEaccesError("spawn", req.command, reason),
			);
			return executor.spawn(command, args, options);
		},
	};
}

export function envAccessAllowed(
	permissions: Permissions | undefined,
	request: EnvAccessRequest,
): void {
	checkPermission(permissions?.env, request, (req, reason) =>
		createEaccesError("access", req.key, reason),
	);
}

/** Create a stub VFS where every operation throws ENOSYS (no filesystem configured). */
export function createFsStub(): VirtualFileSystem {
	const stub = (op: string, path?: string) => {
		throw createEnosysError(op, path);
	};
	return {
		readFile: async (path) => stub("open", path),
		readTextFile: async (path) => stub("open", path),
		readDir: async (path) => stub("scandir", path),
		readDirWithTypes: async (path) => stub("scandir", path),
		writeFile: async (path) => stub("write", path),
		createDir: async (path) => stub("mkdir", path),
		mkdir: async (path) => stub("mkdir", path),
		exists: async (path) => stub("access", path),
		stat: async (path) => stub("stat", path),
		removeFile: async (path) => stub("unlink", path),
		removeDir: async (path) => stub("rmdir", path),
		rename: async (oldPath, newPath) => stub("rename", `${oldPath}->${newPath}`),
		symlink: async (_target, linkPath) => stub("symlink", linkPath),
		readlink: async (path) => stub("readlink", path),
		lstat: async (path) => stub("stat", path),
		link: async (_oldPath, newPath) => stub("link", newPath),
		chmod: async (path) => stub("chmod", path),
		chown: async (path) => stub("chown", path),
		utimes: async (path) => stub("utimes", path),
		truncate: async (path) => stub("open", path),
		realpath: async (path) => stub("realpath", path),
		pread: async (path) => stub("open", path),
		pwrite: async (path) => stub("open", path),
	};
}

/** Create a stub network adapter where every operation throws ENOSYS. */
export function createNetworkStub(): NetworkAdapter {
	const stub = (op: string, path?: string) => {
		throw createEnosysError(op, path);
	};
	return {
		fetch: async (url) => stub("connect", url),
		dnsLookup: async (hostname) => stub("connect", hostname),
		httpRequest: async (url) => stub("connect", url),
	};
}

/** Create a stub executor where spawn throws ENOSYS. */
export function createCommandExecutorStub(): CommandExecutor {
	return {
		spawn: () => {
			throw createEnosysError("spawn");
		},
	};
}

/**
 * Filter an env record through the env permission check, returning only
 * allowed key-value pairs. Returns empty object if no permissions configured.
 */
export function filterEnv(
	env: Record<string, string> | undefined,
	permissions?: Permissions,
): Record<string, string> {
	if (!env) return {};
	if (!permissions?.env) return {};
	const result: Record<string, string> = {};
	for (const [key, value] of Object.entries(env)) {
		const request: EnvAccessRequest = { op: "read", key, value };
		const decision = permissions.env(request);
		if (decision?.allow) {
			result[key] = value;
		}
	}
	return result;
}
