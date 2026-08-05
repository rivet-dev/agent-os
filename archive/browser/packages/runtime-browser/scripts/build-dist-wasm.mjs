#!/usr/bin/env node
// Builds the converged browser sidecar (archive/browser/crates/sidecar-browser) to a web-target
// wasm-bindgen package emitted INTO the published output (dist/sidecar-wasm-web/),
// so `@rivet-dev/agentos-runtime-browser` ships the converged kernel + can be loaded with the
// zero-config default loader (src/default-sidecar.ts). Run after `tsc` (build)
// and before publishing. Requires `wasm-pack` on PATH.

import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import path from "node:path";
import { mkdirSync, rmSync } from "node:fs";

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
const outDir = path.join(packageRoot, "dist", "sidecar-wasm-web");

mkdirSync(path.join(packageRoot, "dist"), { recursive: true });

const result = spawnSync(
	"wasm-pack",
	[
		"build",
		cratePath,
		"--release",
		"--target",
		"web",
		"--out-dir",
		outDir,
		"--out-name",
		"agentos_vm_browser",
	],
	{ stdio: "inherit" },
);

if (result.error) {
	console.error(
		"Failed to run wasm-pack. Install it from https://rustwasm.github.io/wasm-pack/",
	);
	console.error(result.error.message);
	process.exit(1);
}

if (result.status !== 0) {
	process.exit(result.status ?? 1);
}

// wasm-pack drops a `.gitignore` ("*") and its own package.json in the out dir;
// the `.gitignore` would exclude the wasm from the published npm tarball, so
// remove it (the assets are shipped via this package's `files: ["dist"]`).
rmSync(path.join(outDir, ".gitignore"), { force: true });

console.log("Built dist/sidecar-wasm-web/ (web-target converged kernel)");
