#!/usr/bin/env node
// Builds the Agent OS browser wasm sidecar (web target) into dist/sidecar-wasm-web/
// so the shipped package's `createAgentOsConvergedSidecar` loader can resolve the
// wasm glue + binary relative to dist/converged-sidecar.js. Mirrors agentos's
// build-dist-wasm.mjs (which ships @rivet-dev/agentos-runtime-browser's dist/sidecar-wasm-web).
//
// Requires `wasm-pack` on PATH. Run as part of the package build (`pnpm build`).

import { spawnSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const packageRoot = path.resolve(here, "..");
const repoRoot = path.resolve(packageRoot, "..", "..");
const cratePath = path.join(repoRoot, "crates", "agentos-sidecar-browser");
const outDir = path.join(packageRoot, "dist", "sidecar-wasm-web");

const result = spawnSync(
	"wasm-pack",
	["build", cratePath, "--release", "--target", "web", "--out-dir", outDir],
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
