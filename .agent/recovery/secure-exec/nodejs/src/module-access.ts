import * as fs from "node:fs/promises";
import * as fsSync from "node:fs";
import path from "node:path";
import { createEaccesError } from "@secure-exec/core/internal/shared/errors";
import { O_CREAT, O_EXCL, O_TRUNC } from "@secure-exec/core";
import type { VirtualDirEntry, VirtualFileSystem, VirtualStat } from "@secure-exec/core";

/** Host-to-VM path mapping for package-provided module roots. */
export interface PackageRootMapping {
	/** Absolute host path to the package directory. */
	hostPath: string;
	/** VM path where this package should appear (e.g. /root/node_modules/pi-acp). */
	vmPath: string;
}

/**
 * Options controlling which host node_modules are projected into the sandbox.
 * The overlay exposes `<cwd>/node_modules` read-only by default.
 */
export interface ModuleAccessOptions {
	cwd?: string;
	/**
	 * Deprecated: retained for API compatibility only.
	 * The overlay now exposes scoped <cwd>/node_modules read-only by default.
	 */
	allowPackages?: string[];
	/**
	 * Explicit host-to-VM path mappings from packages. These are checked
	 * before the CWD-based node_modules fallback, using longest-prefix match
	 * on the VM path. Each root is added to the symlink-safety allowlist.
	 */
	packageRoots?: PackageRootMapping[];
}

const MODULE_ACCESS_INVALID_CONFIG = "ERR_MODULE_ACCESS_INVALID_CONFIG";
const MODULE_ACCESS_OUT_OF_SCOPE = "ERR_MODULE_ACCESS_OUT_OF_SCOPE";
const MODULE_ACCESS_NATIVE_ADDON = "ERR_MODULE_ACCESS_NATIVE_ADDON";

const SANDBOX_APP_ROOT = "/root";
const SANDBOX_NODE_MODULES_ROOT = `${SANDBOX_APP_ROOT}/node_modules`;

const VIRTUAL_DIR_MODE = 0o040755;

function toVirtualPath(value: string): string {
	if (!value || value === ".") return "/";
	const normalized = path.posix.normalize(value.startsWith("/") ? value : `/${value}`);
	if (normalized.length > 1 && normalized.endsWith("/")) {
		return normalized.slice(0, -1);
	}
	return normalized;
}

function isWithinPath(candidate: string, parent: string): boolean {
	const relative = path.relative(parent, candidate);
	return (
		relative === "" ||
		(!relative.startsWith("..") && !path.isAbsolute(relative))
	);
}

function startsWithPath(value: string, prefix: string): boolean {
	return value === prefix || value.startsWith(`${prefix}/`);
}

function createEnoentError(syscall: string, targetPath: string): Error {
	const error = new Error(
		`ENOENT: no such file or directory, ${syscall} '${targetPath}'`,
	) as NodeJS.ErrnoException;
	error.code = "ENOENT";
	error.path = targetPath;
	error.syscall = syscall;
	return error;
}

function createModuleAccessError(code: string, message: string): Error {
	const error = new Error(`${code}: ${message}`) as NodeJS.ErrnoException;
	error.code = code;
	return error;
}

function createVirtualDirStat(): VirtualStat {
	const now = Date.now();
	return {
		mode: VIRTUAL_DIR_MODE,
		size: 4096,
		isDirectory: true,
		isSymbolicLink: false,
		atimeMs: now,
		mtimeMs: now,
		ctimeMs: now,
		birthtimeMs: now,
		ino: 0,
		nlink: 2,
		uid: 0,
		gid: 0,
	};
}

function normalizeOverlayPath(pathValue: string): string {
	return toVirtualPath(pathValue);
}

function isNativeAddonPath(pathValue: string): boolean {
	return pathValue.endsWith(".node");
}

/**
 * Walk the host node_modules directory and its pnpm virtual-store, resolving
 * symlink targets to build the full set of allowed host paths. This prevents
 * symlink-based escapes from the overlay projection.
 */
function collectOverlayAllowedRoots(hostNodeModulesRoot: string): string[] {
	const roots = new Set<string>([hostNodeModulesRoot]);
	const symlinkScanRoots = [hostNodeModulesRoot, path.join(hostNodeModulesRoot, ".pnpm", "node_modules")];
	const scannedSymlinkDirs = new Set<string>();

	const findNearestNodeModulesAncestor = (targetPath: string): string | null => {
		let current = path.resolve(targetPath);
		while (true) {
			if (path.basename(current) === "node_modules") {
				return current;
			}
			const parent = path.dirname(current);
			if (parent === current) {
				return null;
			}
			current = parent;
		}
	};

	const addSymlinkTarget = (entryPath: string): void => {
		try {
			const target = fsSync.realpathSync(entryPath);
			roots.add(target);
			const packageNodeModulesRoot = findNearestNodeModulesAncestor(target);
			if (packageNodeModulesRoot) {
				roots.add(packageNodeModulesRoot);
				scanDirForSymlinks(packageNodeModulesRoot);
			}
		} catch {
			// Ignore broken symlinks.
		}
	};

	const scanDirForSymlinks = (scanRoot: string): void => {
		if (scannedSymlinkDirs.has(scanRoot)) {
			return;
		}
		scannedSymlinkDirs.add(scanRoot);

		let entries: fsSync.Dirent[] = [];
		try {
			entries = fsSync.readdirSync(scanRoot, { withFileTypes: true });
		} catch {
			return;
		}

		for (const entry of entries) {
			const entryPath = path.join(scanRoot, entry.name);
			if (entry.isSymbolicLink()) {
				addSymlinkTarget(entryPath);
				continue;
			}
			if (entry.isDirectory() && entry.name.startsWith("@")) {
				let scopedEntries: fsSync.Dirent[] = [];
				try {
					scopedEntries = fsSync.readdirSync(entryPath, { withFileTypes: true });
				} catch {
					continue;
				}
				for (const scopedEntry of scopedEntries) {
					if (!scopedEntry.isSymbolicLink()) continue;
					addSymlinkTarget(path.join(entryPath, scopedEntry.name));
				}
			}
		}
	};

	for (const scanRoot of symlinkScanRoots) {
		scanDirForSymlinks(scanRoot);
	}

	return [...roots];
}

/**
 * Union filesystem that overlays host `node_modules` (read-only) onto a base
 * VFS. Sandbox code sees `/root/node_modules/...` which maps to the host's
 * real `<cwd>/node_modules/...`. Write operations to the overlay throw EACCES.
 * Symlinks are resolved and validated against the allowed-roots allowlist to
 * prevent path-traversal escapes. Native `.node` addons are rejected.
 */
export class ModuleAccessFileSystem implements VirtualFileSystem {
	private readonly baseFileSystem?: VirtualFileSystem;
	private readonly configuredNodeModulesRoot: string;
	private readonly hostNodeModulesRoot: string | null;
	private readonly overlayAllowedRoots: string[];
	/** Package roots sorted by vmPath length descending for longest-prefix match. */
	private readonly packageRoots: PackageRootMapping[];

	constructor(baseFileSystem: VirtualFileSystem | undefined, options: ModuleAccessOptions) {
		this.baseFileSystem = baseFileSystem;

		const cwdInput = options.cwd ?? process.cwd();
		if (options.cwd !== undefined && !path.isAbsolute(options.cwd)) {
			throw createModuleAccessError(
				MODULE_ACCESS_INVALID_CONFIG,
				`moduleAccess.cwd must be an absolute path, got '${options.cwd}'`,
			);
		}

		const cwd = path.resolve(cwdInput);
		const nodeModulesPath = path.join(cwd, "node_modules");
		this.configuredNodeModulesRoot = nodeModulesPath;
		try {
			this.hostNodeModulesRoot = fsSync.realpathSync(nodeModulesPath);
			this.overlayAllowedRoots = collectOverlayAllowedRoots(this.hostNodeModulesRoot);
		} catch {
			this.hostNodeModulesRoot = null;
			this.overlayAllowedRoots = [];
		}

		// Sort package roots by vmPath length (longest first) for prefix matching.
		this.packageRoots = [...(options.packageRoots ?? [])].sort(
			(a, b) => b.vmPath.length - a.vmPath.length,
		);

		// Expand allowed roots to include package root host paths and their
		// sibling node_modules (for transitive dep resolution in pnpm).
		for (const root of this.packageRoots) {
			try {
				const canonical = fsSync.realpathSync(root.hostPath);
				if (!this.overlayAllowedRoots.includes(canonical)) {
					this.overlayAllowedRoots.push(canonical);
				}
				// Also add the symlink-resolved roots from the package's own node_modules
				const pkgNodeModules = path.join(canonical, "node_modules");
				if (fsSync.existsSync(pkgNodeModules)) {
					const additionalRoots = collectOverlayAllowedRoots(pkgNodeModules);
					for (const additionalRoot of additionalRoots) {
						if (!this.overlayAllowedRoots.includes(additionalRoot)) {
							this.overlayAllowedRoots.push(additionalRoot);
						}
					}
				}
				// Add the pnpm store root if the package is inside a .pnpm directory.
				// This makes all transitive deps in the store accessible.
				const canonicalParts = canonical.split(path.sep);
				const pnpmIdx = canonicalParts.indexOf(".pnpm");
				if (pnpmIdx >= 0) {
					const pnpmStoreRoot = canonicalParts.slice(0, pnpmIdx + 1).join(path.sep);
					if (!this.overlayAllowedRoots.includes(pnpmStoreRoot)) {
						this.overlayAllowedRoots.push(pnpmStoreRoot);
					}
				}
				// Also add the parent node_modules directory for non-pnpm setups.
				const parentNm = path.dirname(root.hostPath);
				try {
					const canonicalParent = fsSync.realpathSync(parentNm);
					if (!this.overlayAllowedRoots.includes(canonicalParent)) {
						this.overlayAllowedRoots.push(canonicalParent);
					}
				} catch { /* skip */ }
			} catch {
				// Package root doesn't exist on host; skip.
			}
		}
	}

	private isWithinAllowedOverlayRoots(canonicalPath: string): boolean {
		return this.overlayAllowedRoots.some((root) => isWithinPath(canonicalPath, root));
	}

	private isSyntheticPath(virtualPath: string): boolean {
		if (virtualPath === "/" || virtualPath === SANDBOX_APP_ROOT) {
			return true;
		}
		if (virtualPath === SANDBOX_NODE_MODULES_ROOT) {
			return this.hostNodeModulesRoot !== null || this.packageRoots.length > 0;
		}
		return false;
	}

	private syntheticChildren(pathValue: string): Map<string, boolean> {
		const entries = new Map<string, boolean>();
		if (pathValue === "/") {
			entries.set("app", true);
		}
		if (pathValue === SANDBOX_APP_ROOT && (this.hostNodeModulesRoot !== null || this.packageRoots.length > 0)) {
			entries.set("node_modules", true);
		}
		return entries;
	}

	private isReadOnlyProjectionPath(virtualPath: string): boolean {
		return (
			startsWithPath(virtualPath, SANDBOX_NODE_MODULES_ROOT) ||
			this.isProjectedHostPath(virtualPath)
		);
	}

	private shouldMergeBase(pathValue: string): boolean {
		return (
			pathValue === "/" ||
			pathValue === SANDBOX_APP_ROOT ||
			!startsWithPath(pathValue, SANDBOX_NODE_MODULES_ROOT)
		);
	}

	private overlayHostPathFor(virtualPath: string): string | null {
		if (!startsWithPath(virtualPath, SANDBOX_NODE_MODULES_ROOT)) {
			return null;
		}

		// Check package roots first (longest-prefix match, already sorted).
		for (const root of this.packageRoots) {
			if (virtualPath === root.vmPath || startsWithPath(virtualPath, root.vmPath)) {
				if (virtualPath === root.vmPath) {
					return root.hostPath;
				}
				const relative = path.posix
					.relative(root.vmPath, virtualPath)
					.replace(/^\/+/, "");
				if (!relative) {
					return root.hostPath;
				}
				return path.join(root.hostPath, ...relative.split("/"));
			}
		}

		// Fall back to CWD-based node_modules.
		if (this.hostNodeModulesRoot) {
			if (virtualPath === SANDBOX_NODE_MODULES_ROOT) {
				return this.hostNodeModulesRoot;
			}
			const relative = path.posix
				.relative(SANDBOX_NODE_MODULES_ROOT, virtualPath)
				.replace(/^\/+/, "");
			if (!relative) {
				return this.hostNodeModulesRoot;
			}
			const candidate = path.join(this.hostNodeModulesRoot, ...relative.split("/"));
			if (fsSync.existsSync(candidate)) {
				return candidate;
			}
		}

		// Fall back: resolve transitive dependencies from package root siblings.
		// In pnpm, each package root sits in a node_modules/ dir alongside
		// its transitive deps (e.g., .pnpm/pi-agent@.../node_modules/chalk).
		// Check each package root's sibling node_modules for the requested package.
		const nmRelative = path.posix
			.relative(SANDBOX_NODE_MODULES_ROOT, virtualPath)
			.replace(/^\/+/, "");
		if (nmRelative) {
			const relParts = nmRelative.split("/");
			const pkgName = relParts[0].startsWith("@")
				? relParts.slice(0, 2).join("/")
				: relParts[0];
			const subPath = relParts[0].startsWith("@")
				? relParts.slice(2).join("/")
				: relParts.slice(1).join("/");
			for (const root of this.packageRoots) {
				// root.hostPath is e.g. .../node_modules/@mariozechner/pi-coding-agent
				// Its parent node_modules dir may contain chalk, undici, etc.
				const siblingPkg = path.join(path.dirname(root.hostPath), pkgName);
				try {
					if (fsSync.existsSync(siblingPkg)) {
						const realPkg = fsSync.realpathSync(siblingPkg);
						return subPath ? path.join(realPkg, ...subPath.split("/")) : realPkg;
					}
				} catch { /* skip */ }
				// Also try the parent's parent for scoped packages
				const parentNm = path.dirname(path.dirname(root.hostPath));
				if (parentNm !== path.dirname(root.hostPath)) {
					const parentSibling = path.join(parentNm, pkgName);
					try {
						if (fsSync.existsSync(parentSibling)) {
							const realPkg = fsSync.realpathSync(parentSibling);
							return subPath ? path.join(realPkg, ...subPath.split("/")) : realPkg;
						}
					} catch { /* skip */ }
				}
			}
		}

		return null;
	}

	private isProjectedHostPath(pathValue: string): boolean {
		if (!path.isAbsolute(pathValue)) {
			return false;
		}

		const resolved = path.resolve(pathValue);
		if (isWithinPath(resolved, this.configuredNodeModulesRoot)) {
			return true;
		}
		if (
			this.hostNodeModulesRoot &&
			isWithinPath(resolved, this.hostNodeModulesRoot)
		) {
			return true;
		}
		return this.overlayAllowedRoots.some((root) => isWithinPath(resolved, root));
	}

	private getOverlayHostPathCandidate(pathValue: string): string | null {
		const overlayPath = this.overlayHostPathFor(pathValue);
		if (overlayPath) {
			return overlayPath;
		}
		if (!this.isProjectedHostPath(pathValue)) {
			return null;
		}
		return path.resolve(pathValue);
	}

	prepareOpenSync(pathValue: string, flags: number): boolean {
		const virtualPath = normalizeOverlayPath(pathValue);
		if (this.isReadOnlyProjectionPath(virtualPath)) {
			throw createEaccesError(
				(flags & O_TRUNC) !== 0 ? "truncate" : "write",
				virtualPath,
			);
		}

		const syncBase = this.baseFileSystem as (VirtualFileSystem & {
			prepareOpenSync?: (targetPath: string, openFlags: number) => boolean;
		}) | undefined;
		return syncBase?.prepareOpenSync?.(virtualPath, flags) ?? false;
	}

	/** Translate a sandbox path to the corresponding host path (for sync module resolution). */
	toHostPath(sandboxPath: string): string | null {
		return this.overlayHostPathFor(normalizeOverlayPath(sandboxPath));
	}

	/** Translate a host path back to the sandbox path (reverse of toHostPath). */
	toSandboxPath(hostPath: string): string {
		// Check package roots first (longest host path match).
		for (const root of this.packageRoots) {
			if (isWithinPath(hostPath, root.hostPath)) {
				const relative = path.relative(root.hostPath, hostPath);
				return relative
					? path.posix.join(root.vmPath, ...relative.split(path.sep))
					: root.vmPath;
			}
		}
		if (this.hostNodeModulesRoot && isWithinPath(hostPath, this.hostNodeModulesRoot)) {
			const relative = path.relative(this.hostNodeModulesRoot, hostPath);
			return path.posix.join(SANDBOX_NODE_MODULES_ROOT, ...relative.split(path.sep));
		}
		return hostPath;
	}

	private async resolveOverlayHostPath(
		virtualPath: string,
		syscall: string,
	): Promise<string | null> {
		if (isNativeAddonPath(virtualPath)) {
			throw createModuleAccessError(
				MODULE_ACCESS_NATIVE_ADDON,
				`native addon '${virtualPath}' is not supported for module overlay`,
			);
		}

		const hostPath = this.getOverlayHostPathCandidate(virtualPath);
		if (!hostPath) {
			return null;
		}

		try {
			const canonical = await fs.realpath(hostPath);
			if (
				!this.hostNodeModulesRoot ||
				!this.isWithinAllowedOverlayRoots(canonical)
			) {
				console.error(`[module-access] OUT_OF_SCOPE: virtualPath=${virtualPath} canonical=${canonical} allowedRoots=${this.overlayAllowedRoots.length} first3=${this.overlayAllowedRoots.slice(0,3).join(', ')}`);
				throw createModuleAccessError(
					MODULE_ACCESS_OUT_OF_SCOPE,
					`resolved path for '${virtualPath}' escapes allowed overlay roots`,
				);
			}
			if (isNativeAddonPath(canonical)) {
				throw createModuleAccessError(
					MODULE_ACCESS_NATIVE_ADDON,
					`native addon '${virtualPath}' is not supported for module overlay`,
				);
			}
			return canonical;
		} catch (error) {
			const err = error as NodeJS.ErrnoException;
			if (err?.code === "ENOENT") {
				return null;
			}
			if (err?.code === MODULE_ACCESS_OUT_OF_SCOPE) {
				throw err;
			}
			if (err?.code === MODULE_ACCESS_NATIVE_ADDON) {
				throw err;
			}
			if (err?.code === "EACCES") {
				throw createEaccesError(syscall, virtualPath);
			}
			throw err;
		}
	}

	private async readMergedDir(pathValue: string): Promise<Map<string, boolean>> {
		const entries = this.syntheticChildren(pathValue);

		const overlayHostPath = await this.resolveOverlayHostPath(pathValue, "scandir");
		if (overlayHostPath) {
			const hostEntries = await fs.readdir(overlayHostPath, { withFileTypes: true });
			for (const entry of hostEntries) {
				entries.set(entry.name, entry.isDirectory());
			}
		}

		if (this.baseFileSystem && this.shouldMergeBase(pathValue)) {
			try {
				const baseEntries = await this.baseFileSystem.readDirWithTypes(pathValue);
				for (const entry of baseEntries) {
					if (!entries.has(entry.name)) {
						entries.set(entry.name, entry.isDirectory);
					}
				}
			} catch {
				// Ignore base fs misses for synthetic and overlay-facing reads.
			}
		}

		if (entries.size === 0 && !this.isSyntheticPath(pathValue)) {
			throw createEnoentError("scandir", pathValue);
		}

		return entries;
	}

	private async fallbackReadFile(pathValue: string): Promise<Uint8Array> {
		if (!this.baseFileSystem) {
			throw createEnoentError("open", pathValue);
		}
		return this.baseFileSystem.readFile(pathValue);
	}

	private async fallbackReadTextFile(pathValue: string): Promise<string> {
		if (!this.baseFileSystem) {
			throw createEnoentError("open", pathValue);
		}
		return this.baseFileSystem.readTextFile(pathValue);
	}

	private async fallbackReadDir(pathValue: string): Promise<string[]> {
		if (!this.baseFileSystem) {
			throw createEnoentError("scandir", pathValue);
		}
		return this.baseFileSystem.readDir(pathValue);
	}

	private async fallbackReadDirWithTypes(pathValue: string): Promise<VirtualDirEntry[]> {
		if (!this.baseFileSystem) {
			throw createEnoentError("scandir", pathValue);
		}
		return this.baseFileSystem.readDirWithTypes(pathValue);
	}

	private async fallbackWriteFile(
		pathValue: string,
		content: string | Uint8Array,
	): Promise<void> {
		if (!this.baseFileSystem) {
			throw createEnoentError("write", pathValue);
		}
		return this.baseFileSystem.writeFile(pathValue, content);
	}

	private async fallbackCreateDir(pathValue: string): Promise<void> {
		if (!this.baseFileSystem) {
			throw createEnoentError("mkdir", pathValue);
		}
		return this.baseFileSystem.createDir(pathValue);
	}

	private async fallbackMkdir(pathValue: string): Promise<void> {
		if (!this.baseFileSystem) {
			throw createEnoentError("mkdir", pathValue);
		}
		return this.baseFileSystem.mkdir(pathValue);
	}

	private async fallbackExists(pathValue: string): Promise<boolean> {
		if (!this.baseFileSystem) {
			return false;
		}
		return this.baseFileSystem.exists(pathValue);
	}

	private async fallbackStat(pathValue: string): Promise<VirtualStat> {
		if (!this.baseFileSystem) {
			throw createEnoentError("stat", pathValue);
		}
		return this.baseFileSystem.stat(pathValue);
	}

	private async fallbackRemoveFile(pathValue: string): Promise<void> {
		if (!this.baseFileSystem) {
			throw createEnoentError("unlink", pathValue);
		}
		return this.baseFileSystem.removeFile(pathValue);
	}

	private async fallbackRemoveDir(pathValue: string): Promise<void> {
		if (!this.baseFileSystem) {
			throw createEnoentError("rmdir", pathValue);
		}
		return this.baseFileSystem.removeDir(pathValue);
	}

	private async fallbackRename(oldPath: string, newPath: string): Promise<void> {
		if (!this.baseFileSystem) {
			throw createEnoentError("rename", `${oldPath} -> ${newPath}`);
		}
		return this.baseFileSystem.rename(oldPath, newPath);
	}

	async readFile(pathValue: string): Promise<Uint8Array> {
		const virtualPath = normalizeOverlayPath(pathValue);
		const hostPath = await this.resolveOverlayHostPath(virtualPath, "open");
		if (hostPath) {
			return fs.readFile(hostPath);
		}
		if (startsWithPath(virtualPath, SANDBOX_NODE_MODULES_ROOT)) {
			throw createEnoentError("open", virtualPath);
		}
		return this.fallbackReadFile(virtualPath);
	}

	async readTextFile(pathValue: string): Promise<string> {
		const virtualPath = normalizeOverlayPath(pathValue);
		const hostPath = await this.resolveOverlayHostPath(virtualPath, "open");
		if (hostPath) {
			return fs.readFile(hostPath, "utf8");
		}
		if (startsWithPath(virtualPath, SANDBOX_NODE_MODULES_ROOT)) {
			throw createEnoentError("open", virtualPath);
		}
		return this.fallbackReadTextFile(virtualPath);
	}

	async readDir(pathValue: string): Promise<string[]> {
		const virtualPath = normalizeOverlayPath(pathValue);
		if (
			this.isSyntheticPath(virtualPath) ||
			startsWithPath(virtualPath, SANDBOX_NODE_MODULES_ROOT)
		) {
			const entries = await this.readMergedDir(virtualPath);
			return Array.from(entries.keys()).sort((left, right) =>
				left.localeCompare(right),
			);
		}
		return this.fallbackReadDir(virtualPath);
	}

	async readDirWithTypes(pathValue: string): Promise<VirtualDirEntry[]> {
		const virtualPath = normalizeOverlayPath(pathValue);
		if (
			this.isSyntheticPath(virtualPath) ||
			startsWithPath(virtualPath, SANDBOX_NODE_MODULES_ROOT)
		) {
			const entries = await this.readMergedDir(virtualPath);
			return Array.from(entries.entries())
				.map(([name, isDirectory]) => ({ name, isDirectory }))
				.sort((left, right) => left.name.localeCompare(right.name));
		}
		return this.fallbackReadDirWithTypes(virtualPath);
	}

	async writeFile(pathValue: string, content: string | Uint8Array): Promise<void> {
		const virtualPath = normalizeOverlayPath(pathValue);
		if (this.isReadOnlyProjectionPath(virtualPath)) {
			throw createEaccesError("write", virtualPath);
		}
		return this.fallbackWriteFile(virtualPath, content);
	}

	async createDir(pathValue: string): Promise<void> {
		const virtualPath = normalizeOverlayPath(pathValue);
		if (this.isReadOnlyProjectionPath(virtualPath)) {
			throw createEaccesError("mkdir", virtualPath);
		}
		return this.fallbackCreateDir(virtualPath);
	}

	async mkdir(pathValue: string, _options?: { recursive?: boolean }): Promise<void> {
		const virtualPath = normalizeOverlayPath(pathValue);
		if (this.isReadOnlyProjectionPath(virtualPath)) {
			throw createEaccesError("mkdir", virtualPath);
		}
		return this.fallbackMkdir(virtualPath);
	}

	async exists(pathValue: string): Promise<boolean> {
		const virtualPath = normalizeOverlayPath(pathValue);
		if (this.isSyntheticPath(virtualPath)) {
			return true;
		}

		const hostPath = await this.resolveOverlayHostPath(virtualPath, "access");
		if (hostPath) {
			return true;
		}
		if (startsWithPath(virtualPath, SANDBOX_NODE_MODULES_ROOT)) {
			return false;
		}
		return this.fallbackExists(virtualPath);
	}

	async stat(pathValue: string): Promise<VirtualStat> {
		const virtualPath = normalizeOverlayPath(pathValue);
		if (this.isSyntheticPath(virtualPath)) {
			const hostPath = await this.resolveOverlayHostPath(virtualPath, "stat");
			if (!hostPath) {
				return createVirtualDirStat();
			}
		}

		const hostPath = await this.resolveOverlayHostPath(virtualPath, "stat");
		if (hostPath) {
			const info = await fs.stat(hostPath);
			return {
				mode: info.mode,
				size: info.size,
				isDirectory: info.isDirectory(),
				isSymbolicLink: false,
				atimeMs: info.atimeMs,
				mtimeMs: info.mtimeMs,
				ctimeMs: info.ctimeMs,
				birthtimeMs: info.birthtimeMs,
				ino: info.ino,
				nlink: info.nlink,
				uid: info.uid,
				gid: info.gid,
			};
		}
		if (startsWithPath(virtualPath, SANDBOX_NODE_MODULES_ROOT)) {
			throw createEnoentError("stat", virtualPath);
		}
		return this.fallbackStat(virtualPath);
	}

	async removeFile(pathValue: string): Promise<void> {
		const virtualPath = normalizeOverlayPath(pathValue);
		if (this.isReadOnlyProjectionPath(virtualPath)) {
			throw createEaccesError("unlink", virtualPath);
		}
		return this.fallbackRemoveFile(virtualPath);
	}

	async removeDir(pathValue: string): Promise<void> {
		const virtualPath = normalizeOverlayPath(pathValue);
		if (this.isReadOnlyProjectionPath(virtualPath)) {
			throw createEaccesError("rmdir", virtualPath);
		}
		return this.fallbackRemoveDir(virtualPath);
	}

	async rename(oldPath: string, newPath: string): Promise<void> {
		const oldVirtualPath = normalizeOverlayPath(oldPath);
		const newVirtualPath = normalizeOverlayPath(newPath);
		if (
			this.isReadOnlyProjectionPath(oldVirtualPath) ||
			this.isReadOnlyProjectionPath(newVirtualPath)
		) {
			throw createEaccesError("rename", `${oldVirtualPath} -> ${newVirtualPath}`);
		}
		return this.fallbackRename(oldVirtualPath, newVirtualPath);
	}

	async symlink(target: string, linkPath: string): Promise<void> {
		const virtualPath = normalizeOverlayPath(linkPath);
		if (this.isReadOnlyProjectionPath(virtualPath)) {
			throw createEaccesError("symlink", virtualPath);
		}
		if (!this.baseFileSystem) throw createEnoentError("symlink", virtualPath);
		return this.baseFileSystem.symlink(target, virtualPath);
	}

	async readlink(path: string): Promise<string> {
		const virtualPath = normalizeOverlayPath(path);
		if (!this.baseFileSystem) throw createEnoentError("readlink", virtualPath);
		return this.baseFileSystem.readlink(virtualPath);
	}

	async lstat(path: string): Promise<VirtualStat> {
		const virtualPath = normalizeOverlayPath(path);
		if (this.isSyntheticPath(virtualPath)) {
			return createVirtualDirStat();
		}
		const hostPath = await this.resolveOverlayHostPath(virtualPath, "lstat");
		if (hostPath) {
			const info = await fs.lstat(hostPath);
			return {
				mode: info.mode,
				size: info.size,
				isDirectory: info.isDirectory(),
				isSymbolicLink: info.isSymbolicLink(),
				atimeMs: info.atimeMs,
				mtimeMs: info.mtimeMs,
				ctimeMs: info.ctimeMs,
				birthtimeMs: info.birthtimeMs,
				ino: info.ino,
				nlink: info.nlink,
				uid: info.uid,
				gid: info.gid,
			};
		}
		if (startsWithPath(virtualPath, SANDBOX_NODE_MODULES_ROOT)) {
			throw createEnoentError("lstat", virtualPath);
		}
		if (!this.baseFileSystem) throw createEnoentError("lstat", virtualPath);
		return this.baseFileSystem.lstat(virtualPath);
	}

	async link(oldPath: string, newPath: string): Promise<void> {
		const oldVirtualPath = normalizeOverlayPath(oldPath);
		const newVirtualPath = normalizeOverlayPath(newPath);
		if (this.isReadOnlyProjectionPath(newVirtualPath)) {
			throw createEaccesError("link", newVirtualPath);
		}
		if (!this.baseFileSystem) throw createEnoentError("link", oldVirtualPath);
		return this.baseFileSystem.link(oldVirtualPath, newVirtualPath);
	}

	async chmod(path: string, mode: number): Promise<void> {
		const virtualPath = normalizeOverlayPath(path);
		if (this.isReadOnlyProjectionPath(virtualPath)) {
			throw createEaccesError("chmod", virtualPath);
		}
		if (!this.baseFileSystem) throw createEnoentError("chmod", virtualPath);
		return this.baseFileSystem.chmod(virtualPath, mode);
	}

	async chown(path: string, uid: number, gid: number): Promise<void> {
		const virtualPath = normalizeOverlayPath(path);
		if (this.isReadOnlyProjectionPath(virtualPath)) {
			throw createEaccesError("chown", virtualPath);
		}
		if (!this.baseFileSystem) throw createEnoentError("chown", virtualPath);
		return this.baseFileSystem.chown(virtualPath, uid, gid);
	}

	async utimes(path: string, atime: number, mtime: number): Promise<void> {
		const virtualPath = normalizeOverlayPath(path);
		if (this.isReadOnlyProjectionPath(virtualPath)) {
			throw createEaccesError("utimes", virtualPath);
		}
		if (!this.baseFileSystem) throw createEnoentError("utimes", virtualPath);
		return this.baseFileSystem.utimes(virtualPath, atime, mtime);
	}

	async truncate(path: string, length: number): Promise<void> {
		const virtualPath = normalizeOverlayPath(path);
		if (this.isReadOnlyProjectionPath(virtualPath)) {
			throw createEaccesError("truncate", virtualPath);
		}
		if (!this.baseFileSystem) throw createEnoentError("truncate", virtualPath);
		return this.baseFileSystem.truncate(virtualPath, length);
	}

	async realpath(pathValue: string): Promise<string> {
		const virtualPath = normalizeOverlayPath(pathValue);
		if (this.isSyntheticPath(virtualPath)) {
			return virtualPath;
		}
		const hostPath = await this.resolveOverlayHostPath(virtualPath, "realpath");
		if (hostPath) {
			return virtualPath;
		}
		if (startsWithPath(virtualPath, SANDBOX_NODE_MODULES_ROOT)) {
			throw createEnoentError("realpath", virtualPath);
		}
		if (!this.baseFileSystem) throw createEnoentError("realpath", virtualPath);
		return this.baseFileSystem.realpath(virtualPath);
	}

	async pread(pathValue: string, offset: number, length: number): Promise<Uint8Array> {
		const virtualPath = normalizeOverlayPath(pathValue);
		const hostPath = await this.resolveOverlayHostPath(virtualPath, "open");
		if (hostPath) {
			const handle = await fs.open(hostPath, "r");
			try {
				const buf = new Uint8Array(length);
				const { bytesRead } = await handle.read(buf, 0, length, offset);
				return buf.slice(0, bytesRead);
			} finally {
				await handle.close();
			}
		}
		if (startsWithPath(virtualPath, SANDBOX_NODE_MODULES_ROOT)) {
			throw createEnoentError("open", virtualPath);
		}
		if (!this.baseFileSystem) throw createEnoentError("open", virtualPath);
		return this.baseFileSystem.pread(virtualPath, offset, length);
	}

	async pwrite(pathValue: string, offset: number, data: Uint8Array): Promise<void> {
		const virtualPath = normalizeOverlayPath(pathValue);
		const hostPath = await this.resolveOverlayHostPath(virtualPath, "write");
		if (hostPath) {
			const handle = await fs.open(hostPath, "r+");
			try {
				await handle.write(data, 0, data.length, offset);
			} finally {
				await handle.close();
			}
			return;
		}
		if (startsWithPath(virtualPath, SANDBOX_NODE_MODULES_ROOT)) {
			throw createEnoentError("open", virtualPath);
		}
		if (!this.baseFileSystem) throw createEnoentError("open", virtualPath);
		return this.baseFileSystem.pwrite(virtualPath, offset, data);
	}
}
