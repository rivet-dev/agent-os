import { spawnSync } from "node:child_process";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { verifyAospkgCommands } from "./aospkg.js";
import { declaredCommandNames, readManifest } from "./manifest.js";

export interface PublishOptions {
	packageDir: string;
	/** npm dist-tag. Defaults to `dev` so a publish NEVER moves `latest`. */
	tag?: string;
	/** Explicit opt-in to the `latest` dist-tag. */
	latest?: boolean;
	dryRun?: boolean;
	/** Rewrite package.json `version` before publishing. */
	setVersion?: string;
}

export interface PublishResult {
	name: string;
	version: string;
	tag: string;
}

/**
 * `latest` is opt-in only: consumers installing `@agentos-software/<x>` with no
 * tag must keep resolving a deliberate release, never whatever was published
 * last from a dev machine or CI branch.
 */
export function resolveTag(
	options: Pick<PublishOptions, "tag" | "latest">,
): string {
	if (options.latest) {
		if (options.tag !== undefined && options.tag !== "latest") {
			throw new Error(
				`--latest conflicts with --tag ${options.tag}: pass one or the other`,
			);
		}
		return "latest";
	}
	if (options.tag === "latest") {
		throw new Error(
			"refusing implicit `--tag latest` — pass --latest to move the latest pointer",
		);
	}
	return options.tag ?? "dev";
}

function findUp(startDir: string, fileName: string): string | undefined {
	let dir = startDir;
	for (;;) {
		if (existsSync(join(dir, fileName))) return join(dir, fileName);
		const parent = dirname(dir);
		if (parent === dir) return undefined;
		dir = parent;
	}
}

/**
 * Publish a built agentOS package to npm.
 *
 * Uses `pnpm publish` when the package lives in a pnpm workspace (so any
 * `workspace:*` deps are rewritten to real versions), plain `npm publish`
 * otherwise — 3rd-party repos need no pnpm.
 */
export function publish(options: PublishOptions): PublishResult {
	const packageDir = resolve(options.packageDir);
	const tag = resolveTag(options);

	const pkgPath = join(packageDir, "package.json");
	if (!existsSync(pkgPath)) {
		throw new Error(`no package.json in ${packageDir}`);
	}
	const pkg = JSON.parse(readFileSync(pkgPath, "utf8")) as {
		name?: string;
		version?: string;
	};
	if (typeof pkg.name !== "string" || pkg.name.length === 0) {
		throw new Error(`package.json in ${packageDir} is missing a valid "name"`);
	}

	if (options.setVersion !== undefined) {
		const raw = JSON.parse(readFileSync(pkgPath, "utf8"));
		raw.version = options.setVersion;
		writeFileSync(pkgPath, `${JSON.stringify(raw, null, "\t")}\n`);
		pkg.version = options.setVersion;
	}
	if (typeof pkg.version !== "string" || pkg.version.length === 0) {
		throw new Error(
			`package.json in ${packageDir} is missing a valid "version"`,
		);
	}

	// A package that was never built would publish an empty shell — refuse.
	if (!existsSync(join(packageDir, "dist", "index.js"))) {
		throw new Error(
			`${pkg.name} is not built (no dist/index.js in ${packageDir}) — build it first`,
		);
	}
	const declaredCommands = declaredCommandNames(readManifest(packageDir));
	if (declaredCommands.length > 0) {
		const packagePath = join(packageDir, "dist", "package.aospkg");
		if (!existsSync(packagePath)) {
			throw new Error(
				`${pkg.name} declares commands but has no built dist/package.aospkg`,
			);
		}
		try {
			verifyAospkgCommands(readFileSync(packagePath), declaredCommands);
		} catch (error) {
			throw new Error(
				`refusing to publish invalid command artifact for ${pkg.name}: ${String(error)}`,
			);
		}
	}

	const inPnpmWorkspace =
		findUp(packageDir, "pnpm-workspace.yaml") !== undefined;
	const pm = inPnpmWorkspace ? "pnpm" : "npm";
	const args = ["publish", "--access", "public", "--tag", tag];
	if (pm === "pnpm") args.push("--no-git-checks");
	if (options.dryRun) args.push("--dry-run");

	process.stdout.write(
		`publishing ${pkg.name}@${pkg.version} (dist-tag: ${tag}${options.dryRun ? ", dry-run" : ""}) via ${pm}\n`,
	);
	const result = spawnSync(pm, args, { cwd: packageDir, stdio: "inherit" });
	if (result.error) throw result.error;
	if (result.status !== 0) {
		throw new Error(
			`${pm} publish failed for ${pkg.name} (exit ${result.status})`,
		);
	}
	return { name: pkg.name, version: pkg.version, tag };
}
