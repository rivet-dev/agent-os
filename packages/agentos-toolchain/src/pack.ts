import { execFileSync } from "node:child_process";
import {
	chmodSync,
	cpSync,
	existsSync,
	mkdirSync,
	mkdtempSync,
	lstatSync,
	openSync,
	readFileSync,
	readlinkSync,
	readSync,
	closeSync,
	readdirSync,
	rmSync,
	statSync,
	symlinkSync,
	writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, isAbsolute, join, relative, resolve } from "node:path";
import { packAospkgFromTar } from "./aospkg.js";
import { detectExecutableKind, isNativeKind } from "./header.js";

export interface PackOptions {
	/** npm package spec (`name`, `name@version`) or a local directory path. */
	source: string;
	/**
	 * Output tar for the package itself. The tar contains
	 * `{bin/, node_modules/, agentos-package.json}` at its root. The versioned
	 * `/opt/agentos/<name>/<version>` + `current` layout is the sidecar
	 * projection's job (name/version from `agentos-package.json`).
	 */
	out: string;
	/** Mark a bin command as the package's ACP entrypoint (validated against bin/). */
	agent?: string;
	/**
	 * Delete native `.node` addons from the flat closure instead of failing.
	 * Use when a package's dependency tree contains optional/platform native
	 * addons (e.g. `koffi`, `clipboard-*`) that are never loaded on the code path
	 * that runs in V8. The JS closure (including dynamically-required modules) is
	 * kept; only the V8-incompatible `.node` files are removed. If a pruned addon
	 * IS reached at runtime it fails then — so this is opt-in.
	 */
	pruneNative?: boolean;
	/**
	 * Do not install optional npm dependencies into the runtime closure.
	 *
	 * Use this when an upstream package exposes a documented executable override
	 * and its optional dependencies are only platform-native fallback binaries.
	 * The selected replacement must be packaged as a normal command and covered
	 * by an entrypoint smoke test.
	 */
	omitOptional?: boolean;
}

export interface PackResult {
	name: string;
	version: string;
	packageTar: string;
	/** The packed `.aospkg` runtime artifact (empty when `out` was a directory). */
	packageAospkg?: string;
	commands: string[];
}

const HEAD_BYTES = 256; // BINPRM_BUF_SIZE-sized header read

function npmBinary(): string {
	const candidates = [
		process.env.AGENTOS_TOOLCHAIN_NPM,
		join(dirname(process.execPath), "npm"),
		"npm",
	].filter((value): value is string => typeof value === "string" && value.length > 0);
	return candidates.find((candidate) => candidate === "npm" || existsSync(candidate)) ?? "npm";
}

function readHead(file: string): Buffer {
	const fd = openSync(file, "r");
	try {
		const buf = Buffer.alloc(HEAD_BYTES);
		const n = readSync(fd, buf, 0, HEAD_BYTES, 0);
		return buf.subarray(0, n);
	} finally {
		closeSync(fd);
	}
}

function npmInstallFlat(
	source: string,
	into: string,
	omitOptional: boolean,
): void {
	mkdirSync(join(into, "node_modules"), { recursive: true });
	// `source` here is already a copy-forcing install spec (a tarball for local
	// dirs, via `resolveInstallSpec`) — never a bare directory, which npm would
	// symlink as a depless `file:` link. A flat, production-only install: full
	// closure, no symlinked store, no scripts.
	execFileSync(
		npmBinary(),
		[
			"install",
			source,
			"--omit=dev",
			...(omitOptional ? ["--omit=optional"] : []),
			"--ignore-scripts",
			"--no-audit",
			"--no-fund",
			"--no-package-lock",
			"--install-strategy=hoisted",
			"--prefix",
			into,
		],
		{ stdio: "pipe" },
	);
}

/**
 * Resolve a pack source into an install spec npm will COPY (not symlink).
 *
 * `npm install <local-dir>` adds the directory as a `file:` link — it symlinks
 * the package into `node_modules` and does NOT install its dependency closure,
 * which yields an empty, source-symlinked "package" instead of a self-contained
 * one. Packing the directory into a tarball first (honoring its `files` field)
 * and installing THAT makes npm extract a real copy and resolve the full
 * dependency tree. npm specs (`name`, `name@version`) are returned unchanged.
 */
function resolveInstallSpec(source: string, scratch: string): string {
	if (!(existsSync(source) && statSync(source).isDirectory())) return source;
	const dest = join(scratch, "tarball");
	mkdirSync(dest, { recursive: true });
	const out = execFileSync(
		npmBinary(),
		["pack", resolve(source), "--pack-destination", dest, "--ignore-scripts", "--silent"],
		{ encoding: "utf8" },
	);
	const file = out.trim().split("\n").pop()?.trim();
	if (!file) throw new Error(`npm pack produced no tarball for ${source}`);
	return join(dest, file);
}

/** Resolve the installed package's name from a source spec or local dir. */
function installedPackageName(source: string): string {
	if (existsSync(source) && statSync(source).isDirectory()) {
		const pkg = JSON.parse(readFileSync(join(source, "package.json"), "utf8"));
		return pkg.name as string;
	}
	// `name`, `name@version`, `@scope/name@version`
	const at = source.lastIndexOf("@");
	return at > 0 ? source.slice(0, at) : source;
}

function readAgentosPackageManifest(source: string): Record<string, unknown> | undefined {
	if (!(existsSync(source) && statSync(source).isDirectory())) return undefined;
	const manifestPath = join(source, "agentos-package.json");
	if (!existsSync(manifestPath)) return undefined;
	return JSON.parse(readFileSync(manifestPath, "utf8")) as Record<string, unknown>;
}

function unscopedName(name: string): string {
	return name.replace(/^@[^/]+\//, "");
}

/** Normalize a package.json `bin` field to `{ commandName: relativeEntryPath }`. */
function binEntries(pkgDir: string): Record<string, string> {
	const pkg = JSON.parse(readFileSync(join(pkgDir, "package.json"), "utf8"));
	const bin = pkg.bin;
	if (!bin) return {};
	if (typeof bin === "string") {
		return { [pkg.name.replace(/^@[^/]+\//, "")]: bin };
	}
	return bin as Record<string, string>;
}

export function findNativeAddons(root: string): string[] {
	const hits: string[] = [];
	const walk = (dir: string) => {
		for (const entry of readdirSync(dir, { withFileTypes: true })) {
			const p = join(dir, entry.name);
			if (entry.isDirectory()) walk(p);
			else if (entry.isFile() && entry.name.endsWith(".node")) hits.push(p);
		}
	};
	if (existsSync(root)) walk(root);
	return hits;
}

/**
 * Verify a finished package directory satisfies the agentOS package format:
 * every `bin/` entry has a recognized, non-native header, and the tree has no
 * native `.node` addons. Throws with a clear message on the first violation.
 */
export function verifyPackageDir(packageDir: string): void {
	const manifestPath = join(packageDir, "agentos-package.json");
	if (!existsSync(manifestPath)) {
		throw new Error(`missing required agentos-package.json in ${packageDir}`);
	}
	let manifest: { name?: unknown; version?: unknown };
	try {
		manifest = JSON.parse(readFileSync(manifestPath, "utf8")) as {
			name?: unknown;
			version?: unknown;
		};
	} catch (error) {
		throw new Error(
			`agentos-package.json in ${packageDir} is not valid JSON: ${String(error)}`,
		);
	}
	if (typeof manifest.name !== "string" || manifest.name.length === 0) {
		throw new Error(
			`agentos-package.json in ${packageDir} is missing a valid "name"`,
		);
	}
	if (typeof manifest.version !== "string" || manifest.version.length === 0) {
		throw new Error(
			`agentos-package.json in ${packageDir} is missing a valid "version"`,
		);
	}
	const binDir = join(packageDir, "bin");
	if (!existsSync(binDir)) {
		throw new Error(`package has no bin/ directory: ${packageDir}`);
	}
	for (const entry of readdirSync(binDir)) {
		const target = resolve(binDir, entry); // follows the symlink
		const kind = detectExecutableKind(readHead(target));
		if (isNativeKind(kind)) {
			throw new Error(
				`bin/${entry} is a native ${kind} binary, which cannot run in agentOS`,
			);
		}
		if (kind === "unknown") {
			throw new Error(
				`bin/${entry} has no recognized header — JS/script commands need a '#!' shebang`,
			);
		}
	}
	const addons = findNativeAddons(join(packageDir, "node_modules"));
	if (addons.length > 0) {
		throw new Error(
			`package contains native .node addon(s) that won't load in V8: ${addons
				.map((a) => relative(packageDir, a))
				.join(", ")}; re-run with --prune-native to drop them if they are unreachable on the V8 code path`,
		);
	}
}

function isInside(root: string, target: string): boolean {
	const rel = relative(root, target);
	return rel === "" || (!rel.startsWith("..") && !isAbsolute(rel));
}

function dereferenceEscapingSymlinks(root: string): void {
	const walk = (dir: string) => {
		for (const entry of readdirSync(dir, { withFileTypes: true })) {
			const path = join(dir, entry.name);
			const stat = lstatSync(path);
			if (stat.isSymbolicLink()) {
				const target = resolve(dirname(path), readlinkSync(path));
				if (!isInside(root, target)) {
					rmSync(path, { recursive: true, force: true });
					cpSync(target, path, {
						recursive: true,
						dereference: true,
						verbatimSymlinks: false,
					});
				}
				continue;
			}
			if (stat.isDirectory()) walk(path);
		}
	};
	walk(root);
}

function createPackageTar(packageDir: string, packageTar: string): void {
	rmSync(packageTar, { recursive: true, force: true });
	mkdirSync(dirname(packageTar), { recursive: true });
	execFileSync("tar", ["-cf", packageTar, "-C", packageDir, "."], {
		stdio: "pipe",
	});
}

/**
 * Resolve a package's `bin` entry to its real on-disk location in the packed
 * closure. A bin path like `./node_modules/<dep>/<sub>` is written RELATIVE to the
 * declaring package's root, but a flat (`npm install`) install HOISTS shared deps
 * to the top-level `node_modules`, so the literal nested path may not exist. A
	 * wrapper package whose `bin` points into its dependencies is the
 * common case. Try the nested path first, then the hoisted top-level copy.
 */
function resolveBinTarget(
	closureModules: string,
	name: string,
	entryRel: string,
): string {
	const nested = join(closureModules, name, entryRel);
	if (existsSync(nested)) return nested;
	// `./node_modules/<dep>/<sub>` → hoisted `<closure>/node_modules/<dep>/<sub>`.
	const hoistedMatch = entryRel.match(/^\.?\/?node_modules\/(.+)$/);
	if (hoistedMatch) {
		const hoisted = join(closureModules, hoistedMatch[1]);
		if (existsSync(hoisted)) return hoisted;
	}
	throw new Error(
		`pack: cannot resolve bin target "${entryRel}" for ${name} — not found nested ` +
			`(${nested}) or hoisted. The declaring package's bin path does not match the ` +
			`installed (hoisted) layout.`,
	);
}

export function pack(options: PackOptions): PackResult {
	const { source, out, agent, pruneNative } = options;
	const tmp = mkdtempSync(join(tmpdir(), "agentos-pack-"));
	try {
		npmInstallFlat(resolveInstallSpec(source, tmp), tmp, options.omitOptional ?? false);

		const name = installedPackageName(source);
		const installedDir = join(tmp, "node_modules", name);
		// Read the source `agentos-package.json` from the INSTALLED package dir, not
		// the source spec: for npm specs (`@scope/name`) the spec is not a directory,
		// so reading it yields nothing and the packed name falls back to the unscoped
		// npm name — which is only coincidentally correct (e.g. `@agentos-software/pi`
		// -> `pi`) and wrong when they differ (`@agentos-software/claude-code` ->
		// `claude-code`, but the agent id is `claude`). The installed manifest carries
		// the canonical unscoped agent `name` the sidecar projects/resolves on.
		const sourceManifest = readAgentosPackageManifest(installedDir);
		const pkg = JSON.parse(readFileSync(join(installedDir, "package.json"), "utf8"));
		const version: string = pkg.version;
		const bins = binEntries(installedDir);
		const commands = Object.keys(bins);
		if (commands.length === 0) {
			throw new Error(`package "${name}" declares no bin commands`);
		}
		if (agent && !commands.includes(agent)) {
			throw new Error(
				`--agent "${agent}" is not one of the package's commands: ${commands.join(", ")}`,
			);
		}

		// Flat temporary package dir. The emitted artifact is a tar; the versioned
		// `/opt/agentos/<name>/<version>` + `current` layout is the sidecar
		// projection's job.
		const explicitTarOut = out.endsWith(".tar");
		const packageDir = explicitTarOut ? join(tmp, "package") : out;
		rmSync(packageDir, { recursive: true, force: true });
		mkdirSync(packageDir, { recursive: true });

		const binDir = join(packageDir, "bin");
		mkdirSync(binDir, { recursive: true });

		// Flat closure (includes the package itself under node_modules/<name>).
		cpSync(join(tmp, "node_modules"), join(packageDir, "node_modules"), {
			recursive: true,
		});
		// The sidecar resolves each command from the root package.json "bin" map to
		// its REAL entry file in the packed closure (see package_projection
		// binEntries) — it must NOT rely on `bin/<cmd>` symlinks: npm/pnpm publish
		// DROP symlinks, so a `bin/<cmd>` symlink vanishes from the published tarball
		// and the sidecar can't project the entrypoint (the adapter then resolves to
		// `/unknown/<cmd>`). So point `bin` at the real relative entry path; also keep
		// a `bin/<cmd>` symlink for local `$PATH`/dev use (harmless if published away).
		const closureModules = join(packageDir, "node_modules");
		const binMap: Record<string, string> = {};
		for (const [cmd, entryRel] of Object.entries(bins)) {
			const targetAbs = resolveBinTarget(closureModules, name, entryRel);
			binMap[cmd] = relative(packageDir, targetAbs).split(/[\\/]/).join("/");
			chmodSync(targetAbs, 0o755);
			symlinkSync(relative(binDir, targetAbs), join(binDir, cmd));
		}
		if (pruneNative) {
			// Remove V8-incompatible native addons from the kept JS closure. The
			// rest of node_modules (including dynamically-required modules) stays.
			const addons = findNativeAddons(join(packageDir, "node_modules"));
			for (const addon of addons) {
				rmSync(addon, { force: true });
			}
			if (addons.length > 0) {
				console.warn(
					`[agentos-toolchain] pruned ${addons.length} native .node addon(s) from ${name} (--prune-native); they must be unreachable on the V8 code path`,
				);
			}
		}

		// Write a normal root package.json {name, version, bin}. The sidecar reads
		// `version` and commands from here (bin map -> real entry file, built above);
		// JSON package metadata lives in agentos-package.json next to it.
		writeFileSync(
			join(packageDir, "package.json"),
			`${JSON.stringify({ name, version, bin: binMap }, null, 2)}\n`,
		);
		const manifest = {
			name:
				typeof sourceManifest?.name === "string" &&
				sourceManifest.name.length > 0
					? sourceManifest.name
					: unscopedName(name),
			...(sourceManifest?.agent !== undefined
				? { agent: sourceManifest.agent }
				: agent
					? { agent: { acpEntrypoint: agent } }
					: {}),
			version,
			...(sourceManifest?.provides !== undefined
				? { provides: sourceManifest.provides }
				: {}),
		};
		writeFileSync(
			join(packageDir, "agentos-package.json"),
			`${JSON.stringify(manifest, null, 2)}\n`,
		);

		verifyPackageDir(packageDir);
		dereferenceEscapingSymlinks(packageDir);
		const packageTar = out.endsWith(".tar") ? out : `${out}.tar`;
		createPackageTar(packageDir, packageTar);
		// Pack the runtime artifact next to the tar. agentos-package.json is
		// consumed at pack time and stripped from the packed mount tar; the
		// vbare chunk1 manifest is the single runtime manifest.
		const packageAospkg = packageTar.replace(/\.tar$/, ".aospkg");
		packAospkgFromTar(packageTar, packageAospkg);
		return { name, version, packageTar, packageAospkg, commands };
	} finally {
		rmSync(tmp, { recursive: true, force: true });
	}
}
