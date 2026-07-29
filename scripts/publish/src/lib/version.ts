/**
 * Version management split across two surfaces:
 *
 * - `bumpPackageJsons` — rewrites every discovered publishable package.json
 *   `version` field and injects `optionalDependencies` on meta packages.
 *   Safe to call in CI on every run. Uses discovery as the source of truth.
 *   Does NOT touch Cargo.toml or non-discovered files.
 *
 * - `bumpCargoVersions` — rewrites the Rust `[workspace.package]` version,
 *   lock-step internal dependencies, and excluded AgentOS crate manifests so
 *   the crates.io publish chain stays consistent.
 *
 * - `resolveVersion` / `shouldTagAsLatest` — semver helpers for the local cut.
 *
 * All bump functions run ONLY in the CI publish checkout (via the `bump-versions`
 * subcommand). The committed tree stays pinned at product version `0.0.1`; the
 * local `cut-release.ts` cutter is a pure trigger and never calls them.
 */
import * as fs from "node:fs/promises";
import { join } from "node:path";
import { $ } from "execa";
import * as semver from "semver";
import { scoped } from "./logger.js";
import { buildMetaPlatformMap, discoverPackages } from "./packages.js";

const log = scoped("version");

interface PackageJson {
	name?: string;
	version?: string;
	dependencies?: Record<string, string>;
	devDependencies?: Record<string, string>;
	peerDependencies?: Record<string, string>;
	optionalDependencies?: Record<string, string>;
}

const DEP_FIELDS = [
	"dependencies",
	"devDependencies",
	"peerDependencies",
	"optionalDependencies",
] as const;

/**
 * Parse the default `catalog:` block from pnpm-workspace.yaml into a
 * name -> version map. Lightweight line parser (no YAML dep): reads entries
 * under a top-level `catalog:` key until the next non-indented key.
 */
async function readPnpmCatalog(repoRoot: string): Promise<Map<string, string>> {
	// Flattens the default `catalog:` block AND every named `catalogs:<name>:`
	// block into one dep -> version map. Specs are `catalog:` (default) or
	// `catalog:<name>` (named); dep names are unique across the catalogs this
	// repo defines, so a flat map resolves both forms.
	const map = new Map<string, string>();
	let text: string;
	try {
		text = await fs.readFile(join(repoRoot, "pnpm-workspace.yaml"), "utf8");
	} catch {
		return map;
	}
	const lines = text.split("\n");
	// depth 1 entries under `catalog:`; depth 2 under `catalogs:` -> `<name>:`.
	let mode: "none" | "catalog" | "catalogs" = "none";
	for (const line of lines) {
		if (/^catalog:\s*$/.test(line)) {
			mode = "catalog";
			continue;
		}
		if (/^catalogs:\s*$/.test(line)) {
			mode = "catalogs";
			continue;
		}
		if (mode === "none") continue;
		// A non-indented, non-comment, non-blank line ends the block.
		if (/^\S/.test(line) && !line.startsWith("#")) {
			mode = "none";
			continue;
		}
		const m = line.match(/^\s+'?([^':\s]+)'?\s*:\s*'?([^'\s#]+)'?\s*$/);
		if (m) map.set(m[1], m[2]);
	}
	return map;
}

export interface BumpOptions {
	/** If true, report actions but do not write. */
	dryRun?: boolean;
	/** Test hook for independently versioned registry package resolution. */
	resolveNpmLatestVersion?: (name: string) => Promise<string>;
	/**
	 * When true, only rewrite the `version` field. Does not touch dependency
	 * references or inject `optionalDependencies`. Safe to commit to git
	 * because it preserves `workspace:*` dep specs that the lockfile expects.
	 *
	 * When false (default), also rewrites `workspace:*` deps to the literal
	 * version and injects `optionalDependencies` on meta packages. This is
	 * the publish-time mode used by CI — never committed.
	 */
	versionOnly?: boolean;
}

/**
 * Rewrite every discovered package's `version` to the given string.
 *
 * In full mode (default, `versionOnly: false`): also injects
 * `optionalDependencies` on meta packages and rewrites `workspace:*`
 * dependency references to the literal version. This is the publish-time
 * mode used by CI and must NOT be committed — it breaks
 * `pnpm install --frozen-lockfile` because the lockfile expects
 * `workspace:*`, not literal versions.
 *
 * In version-only mode (`versionOnly: true`): only rewrites the `version`
 * field, preserving `workspace:*`/`catalog:` dep specs so the lockfile still
 * resolves. Used by the CI `bump-versions` build step before `turbo build` so
 * the built JS carries the real version; never committed.
 *
 * Returns the number of files written.
 */
/**
 * Resolve the published `latest` version of an independently-versioned
 * registry package from npm. Registry software packages (except `common`)
 * publish from local on their own release track; the main release track pins
 * `workspace:*` deps on them to whatever was last deliberately released.
 * Memoized per run. Throws when the package has never been released — a
 * published manifest carrying `workspace:` protocol is uninstallable by every
 * consumer, so this must fail the publish, loudly.
 */
const npmLatestCache = new Map<string, string>();
async function npmLatestVersion(dep: string): Promise<string> {
	const cached = npmLatestCache.get(dep);
	if (cached) return cached;
	const { execa } = await import("execa");
	const { stdout } = await execa("npm", [
		"view",
		dep,
		"dist-tags.latest",
	]).catch((error: Error & { shortMessage?: string }) => {
		throw new Error(
			`cannot resolve npm latest for workspace dep "${dep}": ${error.shortMessage ?? error.message}`,
		);
	});
	const version = stdout.trim();
	if (!version) {
		throw new Error(
			`workspace dep "${dep}" has no published latest on npm; release it from local first (agentos-toolchain publish ... --latest)`,
		);
	}
	npmLatestCache.set(dep, version);
	return version;
}

export async function bumpPackageJsons(
	repoRoot: string,
	version: string,
	opts: BumpOptions = {},
): Promise<number> {
	const packages = discoverPackages(repoRoot);
	const packageNames = new Set(packages.map((p) => p.name));
	const metaPlatformMap = buildMetaPlatformMap(packages);
	const versionOnly = opts.versionOnly ?? false;
	// pnpm `catalog:` specs are a workspace-only protocol; `npm publish` does not
	// resolve them, so rewrite them to the literal catalog version pre-publish.
	const catalog = await readPnpmCatalog(repoRoot);

	let updated = 0;
	for (const pkg of packages) {
		const pkgJsonPath = join(pkg.dir, "package.json");
		const raw = await fs.readFile(pkgJsonPath, "utf8");
		const pkgJson: PackageJson = JSON.parse(raw);

		pkgJson.version = version;

		if (!versionOnly) {
			// Inject optionalDependencies on meta packages so end users get the
			// correct platform-specific binary via npm's os/cpu/libc resolution.
			const platformPkgs = metaPlatformMap.get(pkg.name);
			if (platformPkgs && platformPkgs.length > 0) {
				pkgJson.optionalDependencies = pkgJson.optionalDependencies ?? {};
				for (const platPkg of platformPkgs) {
					pkgJson.optionalDependencies[platPkg] = version;
				}
			}

			for (const field of DEP_FIELDS) {
				const deps = pkgJson[field];
				if (!deps) continue;
				for (const [dep, spec] of Object.entries(deps)) {
					if (typeof spec !== "string") continue;
					if (spec === "catalog:" || spec.startsWith("catalog:")) {
						const resolved = catalog.get(dep);
						if (!resolved) {
							// A `catalog:` spec npm cannot install must never
							// reach a published manifest — fail the publish.
							throw new Error(
								`cannot resolve catalog spec "${spec}" for dep "${dep}" in ${pkg.name}: not found in pnpm-workspace.yaml catalog(s)`,
							);
						}
						deps[dep] = resolved;
						continue;
					}
					if (!spec.startsWith("workspace:")) continue;
					const isOurPkg =
						packageNames.has(dep) || dep.startsWith("@rivet-dev/agentos-");
					if (isOurPkg) {
						deps[dep] = version;
						continue;
					}
					// Independently-versioned registry software packages: pin to
					// the npm `latest` the owner last deliberately released from
					// local. Never leave `workspace:` in a published manifest.
					deps[dep] = await (
						opts.resolveNpmLatestVersion ?? npmLatestVersion
					)(dep);
				}
			}
		}

		// Tab-indented, trailing newline — matches the repo convention.
		const newContent = `${JSON.stringify(pkgJson, null, "\t")}\n`;
		if (opts.dryRun) {
			log.info(`[dry-run] would update ${pkg.name} -> ${version}`);
		} else {
			await fs.writeFile(pkgJsonPath, newContent);
			log.info(`updated ${pkg.name} -> ${version}`);
		}
		updated++;
	}

	log.info(`total: ${updated} package.json files updated to ${version}`);
	return updated;
}

/**
 * Rewrite the AgentOS Rust workspace version (`[workspace.package]`). AgentOS crates
 * inherit it via `version.workspace = true`.
 */
export async function bumpCargoVersions(
	repoRoot: string,
	version: string,
	opts: Pick<BumpOptions, "dryRun"> = {},
): Promise<void> {
	const cargoTomlPath = join(repoRoot, "Cargo.toml");
	const cargoToml = await fs.readFile(cargoTomlPath, "utf8");
	let next = cargoToml.replace(
		/(\[workspace\.package\]\n(?:[^\n]*\n)*?[ \t]*version = )"[^"]+"/,
		`$1"${version}"`,
	);
	// Bump AgentOS-owned crate dep requirements (path = "crates/...").
	next = next.replace(
		/((?:agentos|agent-os)-[a-z0-9-]+ = \{ path = "crates\/[^"]+", version = ")[^"]+(" \})/g,
		`$1${version}$2`,
	);
	// Also bump aliased AgentOS-owned crate deps declared as
	// `<alias> = { package = "agentos-...", path = "crates/...", version = "..." }`
	// (e.g. `vfs = { package = "agentos-vfs-core", ... }`), which the pattern
	// above misses because the line starts with the alias key, not `agentos-`.
	next = next.replace(
		/(\{ package = "(?:agentos|agent-os)-[a-z0-9-]+", path = "crates\/[^"]+", version = ")[^"]+(" \})/g,
		`$1${version}$2`,
	);

	const updates = new Map<string, string>();
	if (next !== cargoToml) updates.set(cargoTomlPath, next);

	// Excluded crates cannot inherit workspace values, but Cargo still resolves
	// their path dependency graph during a workspace publish dry-run. Rewrite
	// their explicit package and AgentOS path-dependency versions transiently.
	const cratesDir = join(repoRoot, "crates");
	for (const entry of await fs.readdir(cratesDir, { withFileTypes: true })) {
		if (!entry.isDirectory()) continue;
		const manifestPath = join(cratesDir, entry.name, "Cargo.toml");
		let manifest: string;
		try {
			manifest = await fs.readFile(manifestPath, "utf8");
		} catch (error) {
			if ((error as NodeJS.ErrnoException).code === "ENOENT") continue;
			throw error;
		}
		if (!/\[package\]\nname = "(?:agentos|agent-os)-[a-z0-9-]+"/.test(manifest)) {
			continue;
		}

		let updated = manifest.replace(
			/(\[package\]\n(?:[^\n]*\n)*?[ \t]*version = )"[^"]+"/,
			`$1"${version}"`,
		);
		updated = updated.replace(
			/^((?:agentos|agent-os)-[a-z0-9-]+ = \{[^\n]*\bpath = "\.\.\/[^\"]+"[^\n]*\bversion = ")[^"]+(")/gm,
			`$1${version}$2`,
		);
		updated = updated.replace(
			/^(\w[\w-]* = \{[^\n]*\bpackage = "(?:agentos|agent-os)-[a-z0-9-]+"[^\n]*\bpath = "\.\.\/[^\"]+"[^\n]*\bversion = ")[^"]+(")/gm,
			`$1${version}$2`,
		);
		if (updated !== manifest) updates.set(manifestPath, updated);
	}

	if (updates.size === 0) {
		log.info(`Cargo Rust versions already set to ${version}`);
		return;
	}
	if (opts.dryRun) {
		log.info(`[dry-run] would update ${updates.size} Cargo manifests -> ${version}`);
		return;
	}
	for (const [path, contents] of updates) await fs.writeFile(path, contents);
	log.info(`updated ${updates.size} Cargo manifests -> ${version}`);
}

// -----------------------------------------------------------------------------
// Local semver helpers — used only by `cut-release.ts`.
// -----------------------------------------------------------------------------

async function getAllGitVersions(): Promise<string[]> {
	try {
		await $`git fetch --tags --force --quiet`;
	} catch {
		throw new Error(
			"could not fetch git tags — refusing to compute latest flag from stale local tags",
		);
	}
	const result = await $`git tag -l v*`;
	const tags = result.stdout.trim().split("\n").filter(Boolean);
	if (tags.length === 0) return [];
	return tags
		.map((tag) => tag.replace(/^v/, ""))
		.filter((v) => semver.valid(v))
		.sort((a, b) => semver.rcompare(a, b));
}

export async function getLatestGitVersion(): Promise<string | null> {
	const versions = await getAllGitVersions();
	const stable = versions.filter((v) => {
		const p = semver.parse(v);
		return p && p.prerelease.length === 0;
	});
	return stable[0] ?? null;
}

export async function listRecentVersions(limit = 10): Promise<string[]> {
	const all = await getAllGitVersions();
	return all.slice(0, limit);
}

/**
 * Auto-detect whether a version should be tagged as `latest`. A version is
 * `latest` only if it has no prerelease identifier AND is greater than any
 * existing stable git tag.
 */
export async function shouldTagAsLatest(version: string): Promise<boolean> {
	const parsed = semver.parse(version);
	if (!parsed) throw new Error(`invalid semantic version: ${version}`);
	if (parsed.prerelease.length > 0) return false;
	const latest = await getLatestGitVersion();
	if (!latest) return true;
	return semver.gt(version, latest);
}

export interface ResolveVersionOpts {
	version?: string;
	major?: boolean;
	minor?: boolean;
	patch?: boolean;
}

export async function resolveVersion(
	opts: ResolveVersionOpts,
): Promise<string> {
	if (opts.version) {
		if (!semver.valid(opts.version)) {
			throw new Error(`invalid semantic version: ${opts.version}`);
		}
		return opts.version;
	}
	if (!opts.major && !opts.minor && !opts.patch) {
		throw new Error("must provide --version, --major, --minor, or --patch");
	}
	const latest = await getLatestGitVersion();
	if (!latest) {
		throw new Error(
			"no existing version tags found — use --version to set an explicit version",
		);
	}
	let next: string | null = null;
	if (opts.major) next = semver.inc(latest, "major");
	else if (opts.minor) next = semver.inc(latest, "minor");
	else if (opts.patch) next = semver.inc(latest, "patch");
	if (!next) throw new Error("failed to compute next version");
	return next;
}
