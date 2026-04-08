import { build } from "esbuild";
import { readFile } from "node:fs/promises";
import stdLibBrowser from "node-stdlib-browser";
import path from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const packageRoot = path.resolve(scriptDir, "..");
const workspaceRoot = path.resolve(packageRoot, "..", "..");
const bridgeSource = path.join(
	workspaceRoot,
	"crates",
	"execution",
	"assets",
	"v8-bridge.source.js",
);
const bridgeOutput = path.join(
	workspaceRoot,
	"crates",
	"execution",
	"assets",
	"v8-bridge.js",
);
const zlibBridgeOutput = path.join(
	workspaceRoot,
	"crates",
	"execution",
	"assets",
	"v8-bridge-zlib.js",
);

const alias = {};
for (const [name, modulePath] of Object.entries(stdLibBrowser)) {
	if (typeof modulePath === "string") {
		alias[name] = modulePath;
		alias[`node:${name}`] = modulePath;
	}
}

let bridgeSourceText = await readFile(bridgeSource, "utf8");
bridgeSourceText = bridgeSourceText.replace(/\n\s*rationale:\s*"[^"]*",?/g, "");
bridgeSourceText = bridgeSourceText
	.replace(/classification:\s*"hardened"/g, 'c:"h"')
	.replace(/classification:\s*"mutable-runtime-state"/g, 'c:"m"')
	.replace(/entry\.classification === "hardened"/g, 'entry.c==="h"')
	.replace(/entry\.classification === "mutable-runtime-state"/g, 'entry.c==="m"');

const result = await build({
	stdin: {
		contents: bridgeSourceText,
		resolveDir: path.dirname(bridgeSource),
		sourcefile: bridgeSource,
		loader: "js",
	},
	bundle: true,
	outfile: bridgeOutput,
	write: true,
	format: "iife",
	platform: "browser",
	target: "es2020",
	minify: true,
	alias,
	define: {
		"process.env.NODE_ENV": '"production"',
		global: "globalThis",
	},
	banner: {
		js: [
			'if(typeof globalThis.global==="undefined"){globalThis.global=globalThis;}',
			'if(typeof globalThis.process==="undefined"){globalThis.process={env:{},argv:["node"],browser:false,nextTick(callback,...args){return Promise.resolve().then(()=>callback(...args));}};}',
		].join(""),
	},
	external: ["process"],
});

const zlibResult = await build({
	stdin: {
		contents: [
			'import * as assertStdlibModuleNs from "node:assert";',
			'import * as utilStdlibModuleNs from "node:util";',
			'import * as zlibStdlibModuleNs from "node:zlib";',
			"const assertModule = assertStdlibModuleNs.default ?? assertStdlibModuleNs;",
			"const utilModule = utilStdlibModuleNs.default ?? utilStdlibModuleNs;",
			"const zlibModule = zlibStdlibModuleNs.default ?? zlibStdlibModuleNs;",
			'if(typeof utilModule.TextEncoder==="undefined"&&typeof globalThis.TextEncoder==="function"){utilModule.TextEncoder=globalThis.TextEncoder;}',
			'if(typeof utilModule.TextDecoder==="undefined"&&typeof globalThis.TextDecoder==="function"){utilModule.TextDecoder=globalThis.TextDecoder;}',
			"globalThis.__agentOsBuiltinAssertModule = assertModule;",
			"globalThis.__agentOsBuiltinUtilModule = utilModule;",
			"globalThis.__agentOsBuiltinZlibModule = zlibModule;",
		].join("\n"),
		resolveDir: path.dirname(bridgeSource),
		sourcefile: "v8-bridge-zlib.entry.js",
		loader: "js",
	},
	bundle: true,
	outfile: zlibBridgeOutput,
	write: true,
	format: "iife",
	platform: "browser",
	target: "es2020",
	minify: true,
	alias,
	define: {
		"process.env.NODE_ENV": '"production"',
		global: "globalThis",
	},
	banner: {
		js: [
			'if(typeof globalThis.global==="undefined"){globalThis.global=globalThis;}',
			'if(typeof globalThis.process==="undefined"){globalThis.process={env:{},argv:["node"],browser:false,nextTick(callback,...args){return Promise.resolve().then(()=>callback(...args));}};}',
		].join(""),
	},
	external: ["process"],
});

if (result.errors.length > 0) {
	throw new Error(`Failed to build v8-bridge.js: ${result.errors[0].text}`);
}
if (zlibResult.errors.length > 0) {
	throw new Error(`Failed to build v8-bridge-zlib.js: ${zlibResult.errors[0].text}`);
}

console.log(
	`Built ${path.relative(workspaceRoot, bridgeOutput)} (${result.outputFiles?.[0]?.text?.length ?? "written"} bytes)`,
);
