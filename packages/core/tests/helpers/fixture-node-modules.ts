import { execFileSync } from "node:child_process";
import {
	cpSync,
	existsSync,
	lstatSync,
	mkdirSync,
	readdirSync,
	readFileSync,
	realpathSync,
	renameSync,
	rmSync,
	statSync,
	unlinkSync,
	writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve, sep } from "node:path";

/**
 * Build (and cache) a flat, self-contained `node_modules` tree for the workspace
 * package rooted at `cwd`, suitable for mounting into the VM at
 * `/root/node_modules`.
 *
 * Why this exists: pnpm's default (`node-linker=symlinked`) install fills
 * `<pkg>/node_modules/<dep>` with symlinks into the workspace-root `.pnpm`
 * store. The agentos `host_dir` mount resolves strictly beneath the mount
 * root (`openat2(RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS)`), so those
 * store-escaping symlinks are refused ("escapes mapped host root") and
 * transitive deps (e.g. `undici`) are not even present in the slim tree.
 *
 * `pnpm deploy --node-linker=hoisted` produces an npm-style flat tree where
 * every (incl. transitive) dependency is a real directory at the top level, so
 * nothing escapes the mount. We additionally strip any remaining
 * store-escaping symlinks (workspace package links), since the deployed tree
 * already hoists their real npm deps to the top level and those links are not
 * resolved inside the VM by these tests.
 */

// In-process cache (per vitest worker): packageName -> flat node_modules path.
const cache = new Map<string, string>();

function findRepoRoot(start: string): string {
	let dir = resolve(start);
	for (;;) {
		if (existsSync(join(dir, "pnpm-workspace.yaml"))) return dir;
		const parent = dirname(dir);
		if (parent === dir) {
			throw new Error(
				`flat node_modules fixture: no pnpm-workspace.yaml above ${start}`,
			);
		}
		dir = parent;
	}
}

function readPackageName(cwd: string): string {
	const manifestPath = join(cwd, "package.json");
	const { name } = JSON.parse(readFileSync(manifestPath, "utf8")) as {
		name?: string;
	};
	if (!name) {
		throw new Error(`flat node_modules fixture: ${manifestPath} has no "name"`);
	}
	return name;
}

function isInside(root: string, candidate: string): boolean {
	const r = resolve(root);
	const c = resolve(candidate);
	return c === r || c.startsWith(r + sep);
}

/**
 * Remove symlinks whose resolved target escapes `root`, except `.bin` shims
 * (which stay within the tree and are never resolved as modules by these
 * tests). Returns the paths that were stripped.
 */
function stripEscapingSymlinks(root: string): string[] {
	const stripped: string[] = [];
	const walk = (dir: string): void => {
		for (const entry of readdirSync(dir)) {
			if (entry === ".bin") continue;
			const full = join(dir, entry);
			const link = lstatSync(full);
			if (link.isSymbolicLink()) {
				let target: string | null = null;
				try {
					target = realpathSync(full);
				} catch {
					target = null; // dangling
				}
				if (target === null || !isInside(root, target)) {
					unlinkSync(full);
					// A hoisted deploy has no `.pnpm` store-escape symlinks, so an
					// escaping link here is a workspace `link:` dep pnpm didn't copy
					// — e.g. an agent package that now lives in the sibling
					// agentos repo (software/*). Materialize a dereferenced
					// copy so it's still present in the flat tree the VM mounts; a
					// published install would have it as a real dir. Dangling or
					// non-package escapes are dropped as before.
					if (target !== null && existsSync(join(target, "package.json"))) {
						cpSync(target, full, { recursive: true, dereference: true });
					} else {
						stripped.push(full);
					}
				}
			} else if (link.isDirectory()) {
				walk(full);
			}
		}
	};
	walk(root);
	return stripped;
}

/** Synchronous sleep (the mount helper is synchronous). */
function sleepSync(ms: number): void {
	Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
}

/**
 * Ensure a flat `node_modules` tree exists for the workspace package at `cwd`
 * and return its absolute path. Built once per (package, lockfile) and shared
 * across vitest workers via an on-disk cache guarded by an atomic mkdir lock.
 */
export function ensureFlatNodeModules(cwd: string): string {
	const packageName = readPackageName(cwd);
	const cached = cache.get(packageName);
	if (cached) return cached;

	const repoRoot = findRepoRoot(cwd);
	const safe = packageName.replace(/[^a-z0-9]+/gi, "_");
	const repoSafe = repoRoot.replace(/[^a-z0-9]+/gi, "_");
	const cacheBase =
		process.env.AGENTOS_FLAT_FIXTURE_CACHE_ROOT ??
		join(tmpdir(), "agentos-flat-fixtures");
	const cacheRoot = join(cacheBase, repoSafe);
	mkdirSync(cacheRoot, { recursive: true });
	const target = join(cacheRoot, safe);
	const readyMarker = join(target, ".ready");
	const lockfile = join(repoRoot, "pnpm-lock.yaml");

	const isFresh = (): boolean => {
		if (!existsSync(readyMarker)) return false;
		try {
			return statSync(readyMarker).mtimeMs >= statSync(lockfile).mtimeMs;
		} catch {
			return false;
		}
	};

	const resolveResult = (): string => {
		const path = join(target, "node_modules");
		cache.set(packageName, path);
		return path;
	};

	if (isFresh()) return resolveResult();

	const lockDir = `${target}.lock`;
	const deadline = Date.now() + 5 * 60_000;
	for (;;) {
		try {
			mkdirSync(lockDir); // atomic: throws EEXIST while another worker builds
			break;
		} catch {
			if (isFresh()) return resolveResult();
			if (Date.now() > deadline) {
				throw new Error(
					`flat node_modules fixture: timed out waiting for ${packageName}`,
				);
			}
			sleepSync(250);
		}
	}

	try {
		if (!isFresh()) buildInto(repoRoot, packageName, target);
	} finally {
		rmSync(lockDir, { recursive: true, force: true });
	}
	return resolveResult();
}

function buildInto(
	repoRoot: string,
	packageName: string,
	target: string,
): void {
	// Build into a sibling staging dir, then atomically swap into place so
	// readers never observe a half-built tree.
	const staging = `${target}.building`;
	rmSync(staging, { recursive: true, force: true });
	rmSync(target, { recursive: true, force: true });

	// `--node-linker=hoisted` => flat npm-style tree (no escaping store symlinks).
	// `--prod=false`          => include devDependencies (the agent SDKs).
	// `--ignore-scripts`      => skip native rebuilds: the VM cannot run
	//                            host-native binaries, and this keeps fixture
	//                            setup to a few seconds.
	execFileSync(
		"pnpm",
		[
			"--filter",
			packageName,
			"deploy",
			"--legacy",
			"--prod=false",
			"--node-linker=hoisted",
			"--ignore-scripts",
			staging,
		],
		{ cwd: repoRoot, stdio: "pipe" },
	);

	const stagedModules = join(staging, "node_modules");
	if (!existsSync(stagedModules)) {
		throw new Error(
			`flat node_modules fixture: pnpm deploy produced no node_modules for ${packageName}`,
		);
	}
	stripEscapingSymlinks(stagedModules);

	renameSync(staging, target);
	writeFileSync(join(target, ".ready"), `${new Date().toISOString()}\n`);
}
