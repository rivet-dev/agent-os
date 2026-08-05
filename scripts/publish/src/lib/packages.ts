/**
 * Single source of truth for the set of packages we publish.
 *
 * Discovery order matters: platform-specific packages are returned first so
 * they land on npm before the meta package that lists them as
 * `optionalDependencies`. The sidecar meta packages users install resolve the
 * platform-specific binary package for the current host at install time via npm
 * `os`/`cpu`/`libc`, so those platform packages must exist on the registry
 * before anyone installs the meta.
 */
import { execSync } from "node:child_process";
import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative, resolve } from "node:path";

export interface Package {
	name: string;
	/** Directory containing the package.json (absolute). */
	dir: string;
	/** Directory relative to repo root. */
	relDir: string;
}

export interface DiscoverPackagesOptions {
	/** Reserved for parity with the rivetkit discovery API. */
	includeReleaseOnly?: boolean;
}

/**
 * Packages excluded from discovery (private, built separately, or otherwise
 * not publishable). The `private: true` flag in package.json already excludes
 * most of these; the explicit list is a belt-and-suspenders guard for names
 * that must never be published even if their `private` flag is dropped.
 */
export const EXCLUDED = new Set<string>([
	"@rivet-dev/agentos-workspace",
	"@rivet-dev/agentos-dev-shell",
	"@rivet-dev/agentos-shell",
	"publish",
]);

/**
 * Meta packages that need `optionalDependencies` injected at publish time.
 * The meta package's runtime resolver requires the platform-specific package
 * for the current host. The committed `package.json` deliberately does NOT
 * include these — they would pollute non-CI installs with version pins that do
 * not exist yet — so `bumpPackageJsons` injects them in full (publish-time)
 * mode.
 */
export interface MetaPackageSpec {
	/** Name of the meta package. */
	meta: string;
	/** Prefix of the platform-specific packages to inject. */
	platformPrefix: string;
}

export const META_PACKAGES: readonly MetaPackageSpec[] = [
	{
		meta: "@rivet-dev/agentos-sidecar",
		platformPrefix: "@rivet-dev/agentos-sidecar-",
	},
];

const SIDECAR_BINARY_PACKAGE_DIRS = ["packages/sidecar/npm"] as const;

/**
 * Runtime packages consumed directly by lockstep AgentOS packages. Ordinary
 * registry software keeps its independent release flow.
 */
export const LOCKSTEP_SOFTWARE_PACKAGES = new Set([
	"@agentos-software/common",
	"@agentos-software/apps-builder",
	"@agentos-software/sh",
]);

/**
 * Platforms whose sidecar binary package is built and published. Kept in sync
 * with the build matrix in `.github/workflows/publish.yaml`. Override via the
 * `SIDECAR_PLATFORMS` env var (space-separated) to publish a different set.
 */
export const DEFAULT_SIDECAR_PLATFORMS = [
	"linux-x64-gnu",
	"linux-arm64-gnu",
	"darwin-x64",
	"darwin-arm64",
] as const;

export function sidecarPlatforms(): string[] {
	const env = process.env.SIDECAR_PLATFORMS?.trim();
	if (env) return env.split(/\s+/).filter(Boolean);
	return [...DEFAULT_SIDECAR_PLATFORMS];
}

function isPublishable(pkg: { name?: string; private?: boolean }): boolean {
	if (!pkg.name) return false;
	if (pkg.private) return false;
	if (EXCLUDED.has(pkg.name)) return false;
	return true;
}

function readPackageJson(
	dir: string,
): { name?: string; private?: boolean } | null {
	const pkgPath = join(dir, "package.json");
	if (!existsSync(pkgPath)) return null;
	try {
		return JSON.parse(readFileSync(pkgPath, "utf8"));
	} catch {
		return null;
	}
}

export function discoverPackages(
	repoRoot: string,
	_opts: DiscoverPackagesOptions = {},
): Package[] {
	const packages: Package[] = [];
	const seen = new Set<string>();

	const add = (dir: string) => {
		const absDir = resolve(dir);
		const pkg = readPackageJson(absDir);
		if (!pkg) return;
		if (!pkg.name) return;
		if (!isPublishable(pkg)) return;
		if (seen.has(pkg.name)) return;
		seen.add(pkg.name);
		packages.push({
			name: pkg.name,
			dir: absDir,
			relDir: relative(repoRoot, absDir),
		});
	};

	// 1. Platform-specific sidecar binary packages first. These are
	//    `optionalDependencies` of the meta package and must exist on npm before
	//    the meta package resolves at install time. Only the allowlisted
	//    platforms are included so unbuilt platform dirs are never published.
	const platformAllowlist = new Set(sidecarPlatforms());
	for (const packageDir of SIDECAR_BINARY_PACKAGE_DIRS) {
		const npmDir = join(repoRoot, packageDir);
		if (existsSync(npmDir)) {
			for (const entry of readdirSync(npmDir).sort()) {
				if (!platformAllowlist.has(entry)) continue;
				const platDir = join(npmDir, entry);
				if (!statSync(platDir).isDirectory()) continue;
				add(platDir);
			}
		}
	}

	// 2. pnpm workspace packages. Skip independently-versioned software/* WASM
	//    packages, but include the small runtime packages consumed directly by
	//    lockstep AgentOS packages.
	const pnpmList = execSync("pnpm -r list --json --depth -1", {
		cwd: repoRoot,
		encoding: "utf8",
		maxBuffer: 16 * 1024 * 1024,
	});
	const workspacePkgs: Array<{
		name: string;
		path: string;
		private?: boolean;
	}> = JSON.parse(pnpmList);
	for (const p of workspacePkgs) {
		if (!p.name) continue;
		if (
			!p.name.startsWith("@rivet-dev/agentos-") &&
			p.name !== "@rivet-dev/agentos" &&
			!p.name.startsWith("@agentos-software/")
		) {
			continue;
		}
		if (
			p.path.includes("/software/") &&
			!LOCKSTEP_SOFTWARE_PACKAGES.has(p.name)
		) {
			continue;
		}
		add(p.path);
	}

	return packages;
}

/**
 * Returns a map of meta package name → list of platform package names that
 * should be injected as its `optionalDependencies`.
 */
export function buildMetaPlatformMap(
	packages: Package[],
): Map<string, string[]> {
	return new Map(
		META_PACKAGES.map(({ meta, platformPrefix }) => [
			meta,
			packages
				.filter((p) => p.name.startsWith(platformPrefix))
				.map((p) => p.name)
				.sort(),
		]),
	);
}

/**
 * Sanity check — asserts the expected root packages are present. Fail loud in
 * CI if discovery silently regressed. Called at the top of subcommands that
 * touch the full set.
 */
export function assertDiscoverySanity(packages: Package[]): void {
	const byName = new Set(packages.map((p) => p.name));
	const hasAgentOsPackages = packages.some((p) =>
		p.name.startsWith("@rivet-dev/agentos-"),
	);
	const required: string[] = [];
	if (hasAgentOsPackages) {
		required.push(
			"@rivet-dev/agentos",
			"@rivet-dev/agentos-core",
			"@rivet-dev/agentos-sidecar",
		);
	}
	if (byName.has("@rivet-dev/agentos-apps")) {
		required.push(
			"@agentos-software/apps-builder",
			"@agentos-software/sh",
		);
	}
	const missing = required.filter((r) => !byName.has(r));
	if (missing.length > 0) {
		throw new Error(
			`package discovery missing required packages: ${missing.join(", ")}`,
		);
	}
	// Each discovered meta package must have at least one platform package.
	const metaMap = buildMetaPlatformMap(packages);
	for (const { meta } of META_PACKAGES) {
		if (!byName.has(meta)) continue;
		const plats = metaMap.get(meta) ?? [];
		if (plats.length === 0) {
			throw new Error(
				`meta package ${meta} has zero platform packages discovered`,
			);
		}
	}
}
