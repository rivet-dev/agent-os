#!/usr/bin/env node
// Static gates for the converged runtime Agent OS ships.
//
// Agent OS consumes @rivet-dev/agentos-runtime-browser's converged runtime (worker, sync-bridge,
// fs/net/dns/module servicers) rather than carrying its own copy, so the
// bridge-contract / signal-table / wasi-surface gates live in that package and are
// authoritative there. Re-implementing them here would be the dead-copy anti-pattern
// the spec forbids; instead we DELEGATE — run the consumed package's own gates so
// agent-os CI fails if the linked @rivet-dev/agentos-runtime-browser runtime drifts out of
// consistency with its Rust bridge contract / signal table / WASI surface.
//
// agent-os's own static gate is `tsc --noEmit` (the `check-types` script).

import { spawnSync } from "node:child_process";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";

const require = createRequire(import.meta.url);
const here = path.dirname(fileURLToPath(import.meta.url));

// Resolve the linked @rivet-dev/agentos-runtime-browser package root via an exported subpath
// (its package.json is not itself exported). dist/worker.js -> package root.
const workerEntry = require.resolve("@rivet-dev/agentos-runtime-browser/internal/worker");
const agentOsBrowserRoot = path.resolve(path.dirname(workerEntry), "..");
const gateScripts = [
	"check-bridge-contract.mjs",
	"check-signal-table.mjs",
	"check-wasi-surface.mjs",
];

let failed = false;
for (const script of gateScripts) {
	const scriptPath = path.join(agentOsBrowserRoot, "scripts", script);
	process.stdout.write(`▶ @rivet-dev/agentos-runtime-browser ${script}\n`);
	const result = spawnSync("node", [scriptPath], { stdio: "inherit" });
	if (result.status !== 0) {
		failed = true;
		process.stderr.write(`✗ converged gate failed: ${script}\n`);
	}
}

if (failed) {
	process.stderr.write(
		"\nConverged static gates failed against the linked @rivet-dev/agentos-runtime-browser.\n",
	);
	process.exit(1);
}
process.stdout.write("✓ converged static gates pass\n");
