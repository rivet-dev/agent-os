import * as esbuild from "esbuild";
import stdLibBrowser from "node-stdlib-browser";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

// Resolve @secure-exec/core package root (generated files live in core).
const __dirname = path.dirname(fileURLToPath(import.meta.url));
const coreRoot = path.resolve(__dirname, "..", "..", "core");

const alias = {};
for (const [name, modulePath] of Object.entries(stdLibBrowser)) {
	if (modulePath !== null) {
		alias[name] = modulePath;
		alias[`node:${name}`] = modulePath;
	}
}

async function bundlePolyfill(moduleName) {
	const entryPoint = stdLibBrowser[moduleName];
	if (!entryPoint) return null;

	const result = await esbuild.build({
		entryPoints: [entryPoint],
		bundle: true,
		write: false,
		format: "cjs",
		platform: "browser",
		target: "es2020",
		minify: false,
		alias,
		define: {
			"process.env.NODE_ENV": '"production"',
			global: "globalThis",
		},
		external: ["process"],
	});

	const code = result.outputFiles[0].text;
	const defaultExportMatch = code.match(/var\s+(\w+_default)\s*=\s*\{/);
	if (defaultExportMatch && !code.includes("module.exports")) {
		const defaultVar = defaultExportMatch[1];
		return `(function() {\n${code}\nreturn ${defaultVar};\n})()`;
	}

	return `(function() {\nvar module = { exports: {} };\nvar exports = module.exports;\n${code}\nreturn module.exports;\n})()`;
}

const polyfills = {};
for (const name of Object.keys(stdLibBrowser)) {
	if (stdLibBrowser[name] === null) continue;
	const code = await bundlePolyfill(name);
	if (code) polyfills[name] = code;
}

const output = `export const POLYFILL_CODE_MAP = ${JSON.stringify(polyfills, null, 2)};\n`;
const outPath = path.join(coreRoot, "src", "generated", "polyfills.ts");
await fs.mkdir(path.dirname(outPath), { recursive: true });
await fs.writeFile(outPath, output, "utf8");
console.log(`Wrote ${Object.keys(polyfills).length} polyfills to ${outPath}`);
