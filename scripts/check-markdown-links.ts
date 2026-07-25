#!/usr/bin/env tsx

import { execFileSync, spawnSync } from "node:child_process";
import { existsSync, readFileSync, readdirSync } from "node:fs";
import { dirname, relative, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");

function usage(): never {
	console.error(
		"Usage: pnpm exec tsx scripts/check-markdown-links.ts [--no-external] [--built-site path] [path ...]",
	);
	process.exit(2);
}

function sourceMarkdownFiles(requestedPaths: string[]): string[] {
	const output = execFileSync(
		"git",
		["ls-files", "--cached", "--others", "--exclude-standard", "--", "*.md", "*.mdx"],
		{ cwd: repoRoot, encoding: "utf8" },
	);
	const requested = requestedPaths.map((path) => resolve(repoRoot, path));
	const generatedDocs = resolve(repoRoot, "website/public/docs");
	return output
		.split("\n")
		.filter(Boolean)
		.map((path) => resolve(repoRoot, path))
		.filter((path) => path !== generatedDocs && !path.startsWith(`${generatedDocs}/`))
		.filter(
			(path) =>
				requested.length === 0 ||
				requested.some((root) => path === root || path.startsWith(`${root}/`)),
		)
		.map((path) => relative(repoRoot, path))
		.sort();
}

function fileUrl(path: string): string {
	return pathToFileURL(path).href.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function checkBuiltMarkdownAssets(siteDir: string): boolean {
	const failures: string[] = [];
	const visit = (directory: string) => {
		for (const entry of readdirSync(directory, { withFileTypes: true })) {
			const path = resolve(directory, entry.name);
			if (entry.isDirectory()) {
				visit(path);
				continue;
			}
			if (entry.name !== "index.html") continue;
			const html = readFileSync(path, "utf8");
			const island = html.match(
				/component-export="DocsPageDropdown"[^>]*\sprops="([^"]+)"/,
			);
			if (!island) continue;
			const props = JSON.parse(
				island[1].replace(/&quot;/g, '"').replace(/&amp;/g, "&"),
			) as { markdownPath?: [number, string] };
			const advertisedPath = props.markdownPath?.[1];
			if (!advertisedPath) {
				failures.push(`${relative(repoRoot, path)}: missing markdownPath prop`);
				continue;
			}
			const markdownAsset = resolve(siteDir, `${advertisedPath}.md`);
			if (
				!markdownAsset.startsWith(`${siteDir}/`) ||
				!existsSync(markdownAsset)
			) {
				failures.push(`/${advertisedPath}.md`);
			}
		}
	};
	visit(siteDir);
	for (const path of failures.sort()) {
		console.error(`Built docs page points to missing Markdown asset: ${path}`);
	}
	if (failures.length > 0) {
		console.error(`\nFound ${failures.length} missing built Markdown assets.\n`);
	}
	return failures.length === 0;
}

function main() {
	let offline = false;
	let builtSite: string | undefined;
	const requestedPaths: string[] = [];
	const argv = process.argv.slice(2);
	for (let index = 0; index < argv.length; index++) {
		const arg = argv[index];
		if (arg === "--no-external") offline = true;
		else if (arg === "--built-site") {
			const path = argv[++index];
			if (!path) usage();
			builtSite = resolve(repoRoot, path);
		}
		else if (arg === "--help" || arg === "-h" || arg.startsWith("-")) usage();
		else requestedPaths.push(arg);
	}

	if (builtSite && !existsSync(builtSite)) {
		throw new Error(`built site does not exist: ${relative(repoRoot, builtSite)}`);
	}
	const builtSitePassed = builtSite ? checkBuiltMarkdownAssets(builtSite) : true;

	const files = sourceMarkdownFiles(requestedPaths);
	if (files.length === 0) throw new Error("no Markdown source files found");

	const rootUrl = fileUrl(repoRoot);
	const args = [
		"--config",
		resolve(repoRoot, "lychee.toml"),
		"--root-dir",
		repoRoot,
		"--remap",
		`^${rootUrl}/docs/(.*)$ ${pathToFileURL(resolve(repoRoot, "website/src/content/docs/docs")).href}/$1`,
		"--remap",
		`^${rootUrl}/images/(.*)$ ${pathToFileURL(resolve(repoRoot, "website/public/images")).href}/$1`,
		"--remap",
		`^${rootUrl}/registry/?$ ${pathToFileURL(resolve(repoRoot, "website/src/pages/registry/index.astro")).href}`,
		"--remap",
		`^${rootUrl}/use-cases/?$ ${pathToFileURL(resolve(repoRoot, "website/src/pages/use-cases.astro")).href}`,
	];
	if (offline) args.push("--offline");
	args.push("--files-from", "-");

	const result = spawnSync("lychee", args, {
		cwd: repoRoot,
		input: `${files.join("\n")}\n`,
		stdio: ["pipe", "inherit", "inherit"],
	});
	if (result.error) {
		if ((result.error as NodeJS.ErrnoException).code === "ENOENT") {
			throw new Error(
				"lychee is not installed; see https://github.com/lycheeverse/lychee#installation",
			);
		}
		throw result.error;
	}
	process.exitCode = builtSitePassed ? (result.status ?? 1) : 1;
}

main();
