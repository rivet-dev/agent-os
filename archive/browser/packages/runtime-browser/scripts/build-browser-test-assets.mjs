import { spawnSync } from "node:child_process";
import { mkdir } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const packageDir = path.resolve(__dirname, "..");
const outDir = path.join(packageDir, ".cache", "browser-tests");

await mkdir(outDir, { recursive: true });

function buildWorker(entrypoint, outputFile) {
	const result = spawnSync("pnpm", [
		"--dir",
		path.join(packageDir, "..", "build-tools"),
		"exec",
		"esbuild",
		path.join(packageDir, "dist", entrypoint),
		"--bundle",
		"--format=esm",
		"--platform=browser",
		"--target=es2022",
		"--sourcemap=inline",
		`--outfile=${path.join(outDir, outputFile)}`,
	], {
		stdio: "inherit",
		encoding: "utf8",
	});

	if (result.status !== 0) {
		throw new Error(`Failed to build ${entrypoint} with esbuild (${result.status})`);
	}
}

// Bundle a TypeScript entry (esbuild handles TS) with some imports left
// external (e.g. the runtime-loaded wasm module URL).
function bundleEntry(entryPath, outputFile, external = []) {
	const result = spawnSync(
		"pnpm",
		[
			"--dir",
			path.join(packageDir, "..", "build-tools"),
			"exec",
			"esbuild",
			entryPath,
			"--bundle",
			"--format=esm",
			"--platform=browser",
			"--target=es2022",
			"--sourcemap=inline",
			...external.map((pattern) => `--external:${pattern}`),
			`--outfile=${path.join(outDir, outputFile)}`,
		],
		{ stdio: "inherit", encoding: "utf8" },
	);
	if (result.status !== 0) {
		throw new Error(`Failed to bundle ${entryPath} with esbuild (${result.status})`);
	}
}

buildWorker("worker.js", "agentos-worker.js");
bundleEntry(
	path.join(
		packageDir,
		"tests/browser/fixtures/frontend/converged-harness.entry.ts",
	),
	"agentos-converged-harness.js",
	["/sidecar-wasm-web/*"],
);
bundleEntry(
	path.join(
		packageDir,
		"tests/browser/fixtures/frontend/converged-runtime-harness.entry.ts",
	),
	"agentos-converged-runtime-harness.js",
	["/sidecar-wasm-web/*"],
);
bundleEntry(
	path.join(
		packageDir,
		"tests/browser/fixtures/frontend/converged-conformance-harness.entry.ts",
	),
	"agentos-converged-conformance-harness.js",
	["/sidecar-wasm-web/*"],
);
console.log("Built .cache/browser-tests/agentos-worker.js");
console.log("Built .cache/browser-tests/agentos-converged-harness.js");

// Build the converged sidecar kernel to a web-target wasm package, served
// alongside the worker bundles so the converged in-browser test can load the
// real kernel on the main thread.
function buildSidecarWasm() {
	const result = spawnSync(
		"wasm-pack",
		[
			"build",
			path.join(packageDir, "..", "..", "crates", "sidecar-browser"),
			"--dev",
			"--target",
			"web",
			"--out-dir",
			path.join(outDir, "sidecar-wasm-web"),
		],
		{ stdio: "inherit", encoding: "utf8" },
	);
	if (result.error || result.status !== 0) {
		throw new Error(
			`Failed to build the web sidecar wasm with wasm-pack (${
				result.status ?? result.error?.message
			})`,
		);
	}
}

buildSidecarWasm();
console.log("Built .cache/browser-tests/sidecar-wasm-web/");
