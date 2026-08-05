#!/usr/bin/env node
// Convergence guard for the SINGLE shared WASI preview1 runner.
//
// The browser no longer maintains its own WASI runner: `src/wasi-polyfill.ts`
// is generated from the native runner (`crates/executor-v8-runtime/assets/runners/
// wasi-module.js`) by `generate-wasi-polyfill.mjs`. So the only drift that can
// happen now is (1) the generated browser file being stale vs the native source,
// or (2) the native runner's import surface drifting from the WASI manifest.
// This script checks both; the old native-vs-browser parity comparison is moot
// because they are literally the same source.
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import {
	BROWSER_WASI_POLYFILL_PATH,
	NATIVE_WASI_RUNNER_PATH,
	buildBrowserWasiPolyfill,
} from "./generate-wasi-polyfill.mjs";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const packageRoot = path.resolve(scriptDir, "..");
const repoRoot = path.resolve(packageRoot, "..", "..");

const errors = [];

// (1) The committed browser runner must equal the generator's current output.
const expected = buildBrowserWasiPolyfill();
const actual = readFileSync(BROWSER_WASI_POLYFILL_PATH, "utf8");
if (expected !== actual) {
	errors.push(
		"packages/browser/src/wasi-polyfill.ts is stale vs the shared native WASI runner; run `pnpm --dir packages/browser generate:wasi-polyfill`",
	);
}

// (2) The native runner's import surface must match the WASI preview1 manifest.
const nativeWasiSource = readFileSync(NATIVE_WASI_RUNNER_PATH, "utf8");
const wasiPreview1Imports = new Set(
	JSON.parse(
		readFileSync(
			path.join(
				repoRoot,
				"crates",
				"execution",
				"assets",
				"wasi-preview1-imports.json",
			),
			"utf8",
		),
	),
);
const nativeImports = parseWasiImports(nativeWasiSource, "native WASI runner");
for (const name of sorted(wasiPreview1Imports)) {
	if (!nativeImports.has(name)) {
		errors.push(`${name} is listed in the WASI manifest but missing in native`);
	}
}
for (const name of sorted(nativeImports)) {
	if (!wasiPreview1Imports.has(name)) {
		errors.push(`${name} is exported by native WASI but missing in the WASI manifest`);
	}
}

// (3) Both the native and browser WASI tests must keep exercising the shared
// escape/permission tokens that prove kernel-routed confinement (S3).
const nativeWasiTestSource = readFileSync(
	path.join(repoRoot, "crates", "execution", "tests", "wasm.rs"),
	"utf8",
);
const browserWasiTestSource = readFileSync(
	path.join(packageRoot, "tests", "browser", "runtime-driver.spec.ts"),
	"utf8",
);
assertSourceMentions(nativeWasiTestSource, "native WASI test", [
	"wasm_escape_preopen_module",
	"../../../../etc/passwd",
	"(i32.const 44)",
	"wasm_execution_rejects_path_open_escape_outside_preopen",
]);
assertSourceMentions(browserWasiTestSource, "browser WASI test", [
	"maps browser WASI preopen escapes to NOENT",
	"../../../../etc/passwd",
	'message: "escape:44\\n"',
	"routes browser WASI path_open through filesystem permissions",
	'message: "errno:2\\n"',
	"permissionDecisions.deniedFsReads",
]);

if (errors.length > 0) {
	console.error("Shared WASI runner drift detected:");
	for (const error of errors) {
		console.error(`  - ${error}`);
	}
	process.exit(1);
}

function parseWasiImports(source, label) {
	const match = source.match(/this\.wasiImport\s*=\s*\{\{?([\s\S]*?)\n\s*\}\}?;/);
	if (!match) {
		throw new Error(`Unable to find ${label} wasiImport table`);
	}
	return new Set(
		[...match[1].matchAll(/^\s*([a-z][a-z0-9_]*)\s*:/gm)].map(
			(entry) => entry[1],
		),
	);
}

function assertSourceMentions(source, label, tokens) {
	for (const token of tokens) {
		if (!source.includes(token)) {
			errors.push(`${label} must exercise shared WASI token ${token}`);
		}
	}
}

function sorted(values) {
	return [...values].sort((left, right) => left.localeCompare(right));
}
