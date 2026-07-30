import { build } from "esbuild";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import stdLibBrowser from "node-stdlib-browser";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const packageRoot = path.resolve(scriptDir, "..");
const workspaceRoot = path.resolve(packageRoot, "..", "..");

const consoleBuiltinSourcePath = path.join(
	workspaceRoot,
	"packages",
	"build-tools",
	"bridge-src",
	"builtins",
	"console.ts",
);
const outputPath = path.join(
	workspaceRoot,
	"packages",
	"browser",
	"src",
	"generated",
	"util-polyfill.ts",
);

const consoleBuiltinSource = await readFile(consoleBuiltinSourcePath, "utf8");
const formatWithOptionsHelper = extractFunction(
	consoleBuiltinSource,
	"installBuiltinUtilFormatWithOptions",
);

const result = await build({
	stdin: {
		contents: [
			'const util = require("node:util");',
			"module.exports = util.default ?? util;",
		].join("\n"),
		resolveDir: workspaceRoot,
		loader: "js",
	},
	bundle: true,
	write: false,
	format: "cjs",
	platform: "browser",
	alias: {
		util: stdLibBrowser.util,
		"node:util": stdLibBrowser.util,
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

let utilBundle = result.outputFiles[0].text;
utilBundle += [
	"",
	formatWithOptionsHelper,
	"module.exports = installBuiltinUtilFormatWithOptions(module.exports);",
	"if (module.exports && module.exports.default == null) module.exports.default = module.exports;",
	"",
].join("\n");

await mkdir(path.dirname(outputPath), { recursive: true });
await writeFile(
	outputPath,
	[
		"// @generated - run node packages/build-tools/scripts/build-browser-util-polyfill.mjs",
		"export const BROWSER_UTIL_POLYFILL_CODE =",
		JSON.stringify(utilBundle),
		";",
		"",
	].join("\n"),
);

function extractFunction(source, name) {
	const start = source.indexOf(`function ${name}(`);
	if (start < 0) {
		throw new Error(`Failed to find ${name} in bridge-src/builtins/console.ts`);
	}
	const openBrace = source.indexOf("{", start);
	if (openBrace < 0) {
		throw new Error(`Failed to find ${name} body`);
	}
	let depth = 0;
	for (let index = openBrace; index < source.length; index += 1) {
		const char = source[index];
		if (char === "{") {
			depth += 1;
		} else if (char === "}") {
			depth -= 1;
			if (depth === 0) {
				return source.slice(start, index + 1);
			}
		}
	}
	throw new Error(`Failed to extract ${name}`);
}
