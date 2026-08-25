#!/usr/bin/env tsx
/**
 * CI entrypoint. Every workflow step calls exactly one subcommand.
 *
 * Each subcommand is a pure function of its flags — nothing orchestrates
 * other subcommands. The GitHub Actions workflow is the orchestrator.
 *
 * Subcommands accept inputs via flags AND will fall back to re-resolving
 * the `PublishContext` from env vars when flags aren't passed. The workflow
 * uses the `context-output` subcommand to resolve once and pin values as
 * job outputs, then passes those outputs to each subsequent step as flags.
 */
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { setTimeout as sleep } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { Command } from "commander";
import { $ } from "execa";
import {
	releaseArtifactPrefix,
	releaseUserAgent,
} from "../lib/artifacts.js";
import {
	resolveContext,
	writeContextToGithubOutput,
	type Trigger,
} from "../lib/context.js";
import { createGhRelease, tagAndPush } from "../lib/git.js";
import { scoped } from "../lib/logger.js";
import { publishAll } from "../lib/npm.js";
import { copyPrefix, uploadDir } from "../lib/r2.js";
import { discoverRustCrates } from "../lib/rust-crates.js";
import { bumpCargoVersions, bumpPackageJsons } from "../lib/version.js";

const log = scoped("ci");

function findRepoRoot(): string {
	if (process.env.GITHUB_WORKSPACE) return process.env.GITHUB_WORKSPACE;
	let dir = dirname(fileURLToPath(import.meta.url));
	for (let i = 0; i < 10; i++) {
		try {
			readFileSync(join(dir, "pnpm-workspace.yaml"), "utf-8");
			return dir;
		} catch {
			dir = dirname(dir);
		}
	}
	throw new Error("could not locate repo root");
}

async function crateVersionExists(name: string, version: string): Promise<boolean> {
	const response = await fetch(
		`https://crates.io/api/v1/crates/${encodeURIComponent(name)}/${encodeURIComponent(version)}`,
		{
			headers: {
				"User-Agent": releaseUserAgent(),
			},
		},
	);
	if (response.status === 200) return true;
	if (response.status === 404) return false;
	throw new Error(
		`crates.io lookup failed for ${name}@${version}: ${response.status} ${response.statusText}`,
	);
}

async function waitForCrateVersion(
	name: string,
	version: string,
	timeoutSeconds: number,
): Promise<void> {
	const deadline = Date.now() + timeoutSeconds * 1000;
	while (Date.now() < deadline) {
		if (await crateVersionExists(name, version)) return;
		await sleep(10_000);
	}
	throw new Error(`timed out waiting for crates.io to index ${name}@${version}`);
}

async function cargoPublishWithRateLimitRetry(
	repoRoot: string,
	args: string[],
): Promise<void> {
	for (;;) {
		const result = await $({
			all: true,
			cwd: repoRoot,
			reject: false,
		})`cargo ${args}`;

		if (result.all) process.stdout.write(result.all);
		if (result.exitCode === 0) return;

		const retryAt = parseCratesIoRateLimitRetry(result.all ?? "");
		if (retryAt === undefined) {
			throw new Error(`cargo ${args.join(" ")} failed with exit code ${result.exitCode}`);
		}

		const waitMs = Math.max(retryAt.getTime() - Date.now() + 5_000, 10_000);
		log.info(`crates.io rate limited publish; retrying at ${retryAt.toISOString()}`);
		await sleep(waitMs);
	}
}

function parseCratesIoRateLimitRetry(output: string): Date | undefined {
	const match = output.match(/try again after (.+? GMT)/);
	if (!match) return undefined;
	const retryAt = new Date(match[1]);
	if (Number.isNaN(retryAt.getTime())) return undefined;
	return retryAt;
}

const program = new Command();
program.name("ci").description("CI subcommands for the publish flow");

// ---------------------------------------------------------------------------
// context-output — resolve once, write to $GITHUB_OUTPUT for downstream steps
// ---------------------------------------------------------------------------
program
	.command("context-output")
	.description("Resolve publish context and write to $GITHUB_OUTPUT")
	.option("--trigger <trigger>", "Override trigger (branch|release)")
	.option("--version <version>", "Override version")
	.option("--latest <bool>", "Override latest")
	.option("--branch <name>", "Override branch name")
	.action(async (opts) => {
		const overrides: Parameters<typeof resolveContext>[0] = {};
		if (opts.trigger) overrides.trigger = opts.trigger as Trigger;
		if (opts.version) overrides.version = opts.version;
		if (opts.latest !== undefined) overrides.latest = opts.latest === "true";
		if (opts.branch) overrides.branch = opts.branch;
		const ctx = await resolveContext(overrides);
		log.info(
			`resolved: trigger=${ctx.trigger} version=${ctx.version} npm_tag=${ctx.npmTag} sha=${ctx.sha} latest=${ctx.latest}${ctx.branch !== undefined ? ` branch=${ctx.branch}` : ""}`,
		);
		writeContextToGithubOutput(ctx);
	});

// ---------------------------------------------------------------------------
// bump-versions — rewrite package.jsons + Cargo.toml to a version
// ---------------------------------------------------------------------------
program
	.command("bump-versions")
	.description("Rewrite every publishable package.json and Cargo.toml to the given version")
	.option("--version <version>", "Version to write (defaults to resolved context)")
	.option(
		"--version-only",
		"Only rewrite version fields without publish-time dependency injection",
	)
	.option(
		"--repository <owner/repo>",
		"GitHub repository recorded in publish-time package metadata (defaults to GITHUB_REPOSITORY)",
	)
	.option("--dry-run", "Do not write, only report")
	.action(async (opts) => {
		const repoRoot = findRepoRoot();
		const ctx = await resolveContext();
		const version = opts.version ?? ctx.version;
		await bumpPackageJsons(repoRoot, version, {
			dryRun: !!opts.dryRun,
			versionOnly: !!opts.versionOnly,
			repository: opts.repository ?? process.env.GITHUB_REPOSITORY,
		});
		await bumpCargoVersions(repoRoot, version, { dryRun: !!opts.dryRun });
	});

// ---------------------------------------------------------------------------
// publish-npm — parallel npm publish with retries
// ---------------------------------------------------------------------------
program
	.command("publish-npm")
	.description("Publish all discovered packages to npm")
	.option("--tag <tag>", "npm dist-tag (defaults to resolved context)")
	.option("--parallel <n>", "Max simultaneous publishes", "16")
	.option("--retries <n>", "Retries per package", "3")
	.option("--release-mode", "Fail if every package is already published")
	.option("--dry-run", "Pass --dry-run to npm publish (publishes nothing)")
	.action(async (opts) => {
		const repoRoot = findRepoRoot();
		let tag: string = opts.tag;
		let releaseMode: boolean | undefined = opts.releaseMode;
		if (!tag || releaseMode === undefined) {
			const ctx = await resolveContext();
			tag = tag ?? ctx.npmTag;
			if (opts.releaseMode === undefined) {
				releaseMode = ctx.trigger === "release";
			}
		}
		await publishAll(repoRoot, {
			tag,
			parallel: Number(opts.parallel),
			retries: Number(opts.retries),
			releaseMode,
			dryRun: !!opts.dryRun,
		});
	});

// ---------------------------------------------------------------------------
// publish-crates — ordered, idempotent crates.io publish
// ---------------------------------------------------------------------------
program
	.command("publish-crates")
	.description("Publish Rust crates to crates.io in dependency order")
	.option("--version <version>", "Version to publish (defaults to resolved context)")
	.option("--wait-seconds <n>", "Max wait for crates.io indexing per crate", "600")
	.option("--dry-run", "Run cargo publish --dry-run for the first crate only")
	.option("--allow-dirty", "Pass --allow-dirty to cargo publish")
	.action(async (opts) => {
		const repoRoot = findRepoRoot();
		const version = opts.version ?? (await resolveContext()).version;
		const crates = discoverRustCrates(repoRoot);
		const waitSeconds = Number(opts.waitSeconds);
		if (!Number.isFinite(waitSeconds) || waitSeconds <= 0) {
			throw new Error("--wait-seconds must be a positive number");
		}
		if (crates.length === 0) {
			throw new Error("no publishable Rust crates discovered");
		}

		if (opts.dryRun) {
			const crate = crates[0];
			log.info(
				`dry-running ${crate}; later crates require earlier versions to exist in the crates.io index`,
			);
			const args = ["publish", "-p", crate, "--dry-run"];
			if (opts.allowDirty) args.push("--allow-dirty");
			await $({ stdio: "inherit", cwd: repoRoot })`cargo ${args}`;
			return;
		}

		if (!process.env.CARGO_REGISTRY_TOKEN) {
			throw new Error("CARGO_REGISTRY_TOKEN must be set to publish crates");
		}

		for (const crate of crates) {
			if (await crateVersionExists(crate, version)) {
				log.info(`skipping ${crate}@${version}; already published`);
				continue;
			}

			log.info(`publishing ${crate}@${version}`);
			const args = ["publish", "-p", crate];
			if (opts.allowDirty) args.push("--allow-dirty");
			await cargoPublishWithRateLimitRetry(repoRoot, args);
			log.info(`waiting for crates.io to index ${crate}@${version}`);
			await waitForCrateVersion(crate, version, waitSeconds);
		}
	});

// ---------------------------------------------------------------------------
// upload-r2 — upload the sidecar artifact dir to {namespace}/{sha}/sidecar/
// ---------------------------------------------------------------------------
program
	.command("upload-r2")
	.description("Upload an artifact directory to R2")
	.requiredOption("--source <dir>", "Local directory to upload")
	.option("--sha <sha>", "Short sha (defaults to resolved context)")
	.option("--name <name>", "R2 sub-path name", "sidecar")
	.action(async (opts) => {
		const sha = opts.sha ?? (await resolveContext()).sha;
		const prefix = releaseArtifactPrefix({ ref: sha, name: opts.name });
		await uploadDir(opts.source, prefix);
	});

// ---------------------------------------------------------------------------
// copy-r2 — copy {namespace}/{sha}/sidecar/ to {namespace}/{version}/sidecar/
// ---------------------------------------------------------------------------
program
	.command("copy-r2")
	.description("Copy R2 artifacts from {sha} to {version} (+latest)")
	.option("--sha <sha>", "Source sha (defaults to resolved context)")
	.option("--version <version>", "Target version (defaults to resolved context)")
	.option("--latest <bool>", "Also copy to /latest/ (defaults to resolved context)")
	.option("--name <name>", "R2 sub-path name", "sidecar")
	.action(async (opts) => {
		const ctx = await resolveContext();
		const sha: string = opts.sha ?? ctx.sha;
		const version: string = opts.version ?? ctx.version;
		const latest = opts.latest !== undefined ? opts.latest === "true" : ctx.latest;
		const source = releaseArtifactPrefix({ ref: sha, name: opts.name });
		await copyPrefix(
			source,
			releaseArtifactPrefix({ ref: version, name: opts.name }),
		);
		if (latest) {
			await copyPrefix(
				source,
				releaseArtifactPrefix({ ref: "latest", name: opts.name }),
			);
		}
	});

// ---------------------------------------------------------------------------
// git-tag — force-create and push v{version}
// ---------------------------------------------------------------------------
program
	.command("git-tag")
	.description("Create and force-push v{version} tag")
	.option("--version <version>", "Version (defaults to resolved context)")
	.action(async (opts) => {
		const version = opts.version ?? (await resolveContext()).version;
		await tagAndPush(version);
	});

// ---------------------------------------------------------------------------
// gh-release — create or update GitHub release for the version
// ---------------------------------------------------------------------------
program
	.command("gh-release")
	.description("Create or update GitHub release")
	.option("--version <version>", "Version (defaults to resolved context)")
	.action(async (opts) => {
		const version = opts.version ?? (await resolveContext()).version;
		await createGhRelease(version);
	});

program.parseAsync(process.argv).catch((err) => {
	log.error(String(err?.stack ?? err));
	process.exit(1);
});
