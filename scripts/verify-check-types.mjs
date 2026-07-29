#!/usr/bin/env node
/**
 * Verify every workspace package defines a `check-types` script.
 *
 * Runs BEFORE `turbo check-types` so a package can never silently skip type
 * checking just because it forgot the script (turbo only runs the task for
 * packages that define it). Embedded docs code lives in example packages, so a
 * missing `check-types` means shipped docs code that nothing type-checks.
 *
 * A package with no TypeScript should still declare a (possibly trivial)
 * `check-types` script so coverage is explicit rather than accidental.
 */
import { execSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { dirname, relative } from "node:path";

const root = process.cwd();

// All package.json in the repo except the monorepo root and anything vendored
// or generated.
const found = execSync(
	[
		"find .",
		"\\(",
		"-type d",
		"\\(",
		'-name node_modules -o -name dist -o -name .output -o -name .astro -o -name .cache -o -name .turbo -o -name .codex-build -o -name vendor -o -name target -o -name .git -o -name .jj -o -name .claude',
		'-o -path "./packages/runtime-core/tests/integration/projects"',
		'-o -path "./crates/execution/assets/undici-shims"',
		"\\)",
		"\\)",
		"-prune -o -name package.json -print",
	].join(" "),
	{ encoding: "utf8", cwd: root },
)
	.trim()
	.split("\n")
	.filter((f) => f && f !== "./package.json");

const missing = [];
for (const file of found) {
	let pkg;
	try {
		pkg = JSON.parse(readFileSync(file, "utf8"));
	} catch {
		console.error(`✗ ${file}: invalid JSON`);
		process.exit(1);
	}
	// Skip packages explicitly marked private-with-no-build, but still require
	// the script otherwise.
	if (!pkg.scripts || !pkg.scripts["check-types"]) {
		missing.push(`${relative(root, dirname(file)) || "."}  (${pkg.name ?? "no name"})`);
	}
}

if (missing.length > 0) {
	console.error(
		`✗ ${missing.length} package(s) are missing a "check-types" script:\n` +
			missing.map((m) => `  - ${m}`).join("\n") +
			'\n\nEvery package must define "check-types" (e.g. "check-types": "tsc --noEmit")' +
			" so `turbo check-types` covers it. Add one, or a no-op if the package has no TypeScript.",
	);
	process.exit(1);
}

console.log(`✓ all ${found.length} packages define a "check-types" script`);
