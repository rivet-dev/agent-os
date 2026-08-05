#!/usr/bin/env node
// Builds the converged browser sidecar (archive/browser/crates/sidecar-browser) to a
// wasm-bindgen Node package under .cache/sidecar-wasm, used by the converged
// integration test to drive the REAL wasm kernel end to end.
//
// Requires `wasm-pack` on PATH. Targets nodejs so the bindings load directly in
// vitest/Node; the browser harness uses the same crate built for the web.

import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import path from "node:path";

const here = path.dirname(fileURLToPath(import.meta.url));
const packageRoot = path.resolve(here, "..");
const repoRoot = path.resolve(packageRoot, "..", "..");
const cratePath = path.join(
	repoRoot,
	"archive",
	"browser",
	"crates",
	"sidecar-browser",
);
const outDir = path.join(packageRoot, ".cache", "sidecar-wasm");

const result = spawnSync(
	"wasm-pack",
	["build", cratePath, "--dev", "--target", "nodejs", "--out-dir", outDir],
	{ stdio: "inherit" },
);

if (result.error) {
	console.error(
		"Failed to run wasm-pack. Install it from https://rustwasm.github.io/wasm-pack/",
	);
	console.error(result.error.message);
	process.exit(1);
}

process.exit(result.status ?? 1);
