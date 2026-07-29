#!/usr/bin/env node
// Builds the Agent OS browser sidecar (crates/agentos-sidecar-browser) to a
// wasm-bindgen package under .cache/agentos-sidecar-wasm. This is the converged
// wasm kernel (from agentos) plus the Agent OS ACP BrowserExtension, driven by
// the browser harness / integration tests over pushFrame/pollEvent.
//
// Requires `wasm-pack` on PATH. Targets nodejs by default so the bindings load
// directly in vitest/Node; pass `--target web` for the browser harness build.
//
// The agentos-sidecar-browser crate is host-free on purpose (no tokio/mio/native
// agentos-native-sidecar) so it compiles to wasm32; see the crate's Cargo.toml.

import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const packageRoot = path.resolve(here, "..");
const repoRoot = path.resolve(packageRoot, "..", "..");
const cratePath = path.join(repoRoot, "crates", "agentos-sidecar-browser");

const targetArg = process.argv.indexOf("--target");
const target = targetArg !== -1 ? process.argv[targetArg + 1] : "nodejs";
const outDir = path.join(
	packageRoot,
	".cache",
	target === "web" ? "agentos-sidecar-wasm-web" : "agentos-sidecar-wasm",
);

const cachedJs = path.join(outDir, "agentos_sidecar_browser.js");
const cachedWasm = path.join(outDir, "agentos_sidecar_browser_bg.wasm");
const hasCachedBuild = () => existsSync(cachedJs) && existsSync(cachedWasm);

const result = spawnSync(
	"wasm-pack",
	["build", cratePath, "--dev", "--target", target, "--out-dir", outDir],
	{ stdio: "inherit" },
);

if (result.error) {
	if (result.error.code === "ENOENT" && hasCachedBuild()) {
		console.warn(
			`wasm-pack is not on PATH; using cached Agent OS sidecar wasm in ${outDir}`,
		);
		process.exit(0);
	}
	console.error(
		"Failed to run wasm-pack. Install it from https://rustwasm.github.io/wasm-pack/",
	);
	console.error(result.error.message);
	process.exit(1);
}

process.exit(result.status ?? 1);
