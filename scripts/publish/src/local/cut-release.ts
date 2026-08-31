#!/usr/bin/env tsx
/**
 * Linear release cutter — called by humans, never by CI.
 *
 * Steps:
 *   1. Resolve target version (flags → semver bump → error)
 *   2. Auto-detect or confirm `latest` flag
 *   3. Validate git working tree is clean
 *   4. Print release plan and confirm
 *   5. Optional local type-check fail-fast
 *   6. Trigger the publish.yaml workflow
 *
 * Debugging: comment out any step. No `--only-steps`, no phases.
 */
import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import * as readline from "node:readline";
import { fileURLToPath } from "node:url";
import { Command } from "commander";
import { $ } from "execa";
import { validateClean } from "../lib/git.js";
import { scoped } from "../lib/logger.js";
import {
	getLatestGitVersion,
	listRecentVersions,
	resolveVersion,
	shouldTagAsLatest,
} from "../lib/version.js";

const log = scoped("release");

function findRepoRoot(): string {
	let dir = dirname(fileURLToPath(import.meta.url));
	for (let i = 0; i < 10; i++) {
		if (existsSync(join(dir, "pnpm-workspace.yaml"))) return dir;
		dir = dirname(dir);
	}
	throw new Error("could not locate repo root");
}

async function confirmPrompt(question: string): Promise<boolean> {
	const rl = readline.createInterface({
		input: process.stdin,
		output: process.stdout,
	});
	const answer = await new Promise<string>((resolve) => {
		rl.question(question, resolve);
	});
	rl.close();
	const a = answer.trim().toLowerCase();
	return a === "yes" || a === "y";
}

interface CliOpts {
	version?: string;
	major?: boolean;
	minor?: boolean;
	patch?: boolean;
	latest?: boolean;
	noLatest?: boolean;
	dryRun?: boolean;
	yes?: boolean;
	skipChecks?: boolean;
	profile?: string;
}

async function main() {
	const program = new Command();
	program
		.name("cut-release")
		.description("Cut a new Agent OS release (local orchestrator)")
		.option("--version <version>", "Explicit version (e.g. 0.2.0 or 0.2.0-rc.1)")
		.option("--major", "Bump major")
		.option("--minor", "Bump minor")
		.option("--patch", "Bump patch")
		.option("--latest", "Mark as latest dist-tag")
		.option("--no-latest", "Do not mark as latest")
		.option("--dry-run", "Resolve and print the release plan without triggering the workflow")
		.option("-y, --yes", "Skip interactive confirmation")
		.option("--skip-checks", "Skip local type-check fail-fast")
		.option("--profile <profile>", "Native binary build profile (release or debug)", "release")
		.parse();

	const opts = program.opts<CliOpts>();
	const repoRoot = findRepoRoot();
	const profile = opts.profile ?? "release";
	if (profile !== "release" && profile !== "debug") {
		throw new Error(`invalid --profile ${profile}; expected release or debug`);
	}

	// 1. Resolve version.
	const version = await resolveVersion({
		version: opts.version,
		major: opts.major,
		minor: opts.minor,
		patch: opts.patch,
	});

	// 2. Latest flag: explicit > auto > false.
	let latest: boolean;
	if (opts.latest === true) latest = true;
	else if (opts.noLatest === true || opts.latest === false) latest = false;
	else latest = await shouldTagAsLatest(version);

	// 3. Validate git clean.
	await validateClean();

	// 4. Print plan.
	const { stdout: branch } = await $`git rev-parse --abbrev-ref HEAD`;
	const latestGit = await getLatestGitVersion();
	const recent = await listRecentVersions(10);
	console.log("");
	console.log("Release plan");
	console.log(`  Version:  ${version}`);
	console.log(`  Latest:   ${latest}`);
	console.log(`  Profile:  ${profile}`);
	console.log(`  Branch:   ${branch.trim()}`);
	console.log(`  Previous: ${latestGit ?? "(none)"}`);
	if (opts.dryRun) console.log("  Dry run:  no workflow trigger");
	console.log("");
	if (recent.length > 0) {
		console.log("Recent versions:");
		for (const v of recent) {
			const marker = v === latestGit ? " (latest)" : "";
			console.log(`  - ${v}${marker}`);
		}
		console.log("");
	}

	if (opts.dryRun) {
		log.info("dry run complete — workflow not triggered");
		return;
	}

	if (!opts.yes) {
		const ok = await confirmPrompt("Proceed with release? (yes/no): ");
		if (!ok) {
			log.info("release cancelled");
			process.exit(0);
		}
	}

	// 5. Local type-check fail-fast.
	if (!opts.skipChecks) {
		log.info("running local core build + type-check (fail-fast)");
		await $({ stdio: "inherit", cwd: repoRoot })`pnpm --dir packages/core build`;
		await $({
			stdio: "inherit",
			cwd: repoRoot,
		})`pnpm --dir packages/core check-types`;
	}

	// 6. Trigger the workflow.
	log.info("triggering publish.yaml workflow");
	const latestFlag = latest ? "true" : "false";
	const currentBranch = branch.trim();
	await $({
		stdio: "inherit",
		cwd: repoRoot,
	})`gh workflow run .github/workflows/publish.yaml -f version=${version} -f latest=${latestFlag} -f profile=${profile} --ref ${currentBranch}`;

	const { stdout: repo } =
		await $`gh repo view --json nameWithOwner -q .nameWithOwner`;
	console.log("");
	console.log(
		`Workflow triggered: https://github.com/${repo.trim()}/actions/workflows/publish.yaml`,
	);
}

main().catch((err) => {
	log.error(String(err?.stack ?? err));
	process.exit(1);
});
