import { build } from "esbuild";
import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import stdLibBrowser from "node-stdlib-browser";

// Bundles the real `path` package (path-browserify, via node-stdlib-browser)
// into a guest polyfill so the converged browser presents node:path from a
// single upstream source instead of a hand-maintained copy. The VM is Linux, so
// path === path.posix. Mirrors build-browser-util-polyfill.mjs.

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const packageRoot = path.resolve(scriptDir, "..");
const workspaceRoot = path.resolve(packageRoot, "..", "..");

const outputPath = path.join(
	workspaceRoot,
	"packages",
	"browser",
	"src",
	"generated",
	"path-polyfill.ts",
);

const result = await build({
	stdin: {
		contents: [
			'const path = require("path");',
			"const resolved = path.default ?? path;",
			// Present Linux semantics: path === path.posix, and make that reflexive.
			"const posix = resolved.posix ?? resolved;",
			"posix.posix = posix;",
			"module.exports = posix;",
		].join("\n"),
		resolveDir: workspaceRoot,
		loader: "js",
	},
	bundle: true,
	write: false,
	format: "cjs",
	platform: "browser",
	alias: {
		path: stdLibBrowser.path,
		"node:path": stdLibBrowser.path,
	},
	banner: {
		js: [
			"var process = globalThis.process || {",
			"  env: {},",
			"  cwd: () => '/',",
			"};",
		].join("\n"),
	},
	target: "es2020",
});

let pathBundle = result.outputFiles[0].text;
pathBundle += [
	"",
	"if (module.exports && module.exports.default == null) module.exports.default = module.exports;",
	"",
].join("\n");

await mkdir(path.dirname(outputPath), { recursive: true });
await writeFile(
	outputPath,
	[
		"// @generated - run node packages/build-tools/scripts/build-browser-path-polyfill.mjs",
		"export const BROWSER_PATH_POLYFILL_CODE =",
		JSON.stringify(pathBundle),
		";",
		"",
	].join("\n"),
);

console.log("Built packages/browser/src/generated/path-polyfill.ts");
