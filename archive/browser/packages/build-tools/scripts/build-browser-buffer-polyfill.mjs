import { build } from "esbuild";
import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import stdLibBrowser from "node-stdlib-browser";

// Bundles the real `buffer` package (via node-stdlib-browser, same source the
// rest of the pure-JS builtins use) into a guest polyfill, so the converged
// browser exposes a faithful node:buffer / Buffer instead of the WASI runner's
// internal Uint8Array shim. Mirrors build-browser-util-polyfill.mjs.

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const packageRoot = path.resolve(scriptDir, "..");
const workspaceRoot = path.resolve(packageRoot, "..", "..");

const outputPath = path.join(
	workspaceRoot,
	"packages",
	"browser",
	"src",
	"generated",
	"buffer-polyfill.ts",
);

const result = await build({
	stdin: {
		contents: [
			'const buffer = require("buffer");',
			"module.exports = buffer.default ?? buffer;",
		].join("\n"),
		resolveDir: workspaceRoot,
		loader: "js",
	},
	bundle: true,
	write: false,
	format: "cjs",
	platform: "browser",
	alias: {
		buffer: stdLibBrowser.buffer,
		"node:buffer": stdLibBrowser.buffer,
	},
	banner: {
		js: [
			"var process = globalThis.process || {",
			"  env: {},",
			"  nextTick: (fn, ...args) => queueMicrotask(() => fn(...args)),",
			"};",
		].join("\n"),
	},
	target: "es2020",
});

let bufferBundle = result.outputFiles[0].text;
bufferBundle += [
	"",
	"if (module.exports && module.exports.default == null) module.exports.default = module.exports;",
	"",
].join("\n");

await mkdir(path.dirname(outputPath), { recursive: true });
await writeFile(
	outputPath,
	[
		"// @generated - run node packages/build-tools/scripts/build-browser-buffer-polyfill.mjs",
		"export const BROWSER_BUFFER_POLYFILL_CODE =",
		JSON.stringify(bufferBundle),
		";",
		"",
	].join("\n"),
);

console.log("Built packages/browser/src/generated/buffer-polyfill.ts");
