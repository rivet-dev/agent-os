import * as esbuild from "esbuild";
import { createRequire } from "node:module";
import stdLibBrowser from "node-stdlib-browser";
import { fileURLToPath } from "node:url";

// Cache bundled polyfills
const polyfillCache: Map<string, string> = new Map();

function resolveCustomPolyfillSource(fileName: string): string {
	return fileURLToPath(new URL(`../src/polyfills/${fileName}`, import.meta.url));
}

const require = createRequire(import.meta.url);
const WEB_STREAMS_PONYFILL_PATH = require.resolve(
	"web-streams-polyfill/dist/ponyfill.js",
);

const CUSTOM_POLYFILL_ENTRY_POINTS = new Map([
	["crypto", resolveCustomPolyfillSource("crypto.js")],
	["stream/web", resolveCustomPolyfillSource("stream-web.js")],
	["util/types", resolveCustomPolyfillSource("util-types.js")],
	["internal/webstreams/util", resolveCustomPolyfillSource("internal-webstreams-util.js")],
	["internal/webstreams/adapters", resolveCustomPolyfillSource("internal-webstreams-adapters.js")],
	["internal/webstreams/readablestream", resolveCustomPolyfillSource("internal-webstreams-readablestream.js")],
	["internal/webstreams/writablestream", resolveCustomPolyfillSource("internal-webstreams-writablestream.js")],
	["internal/webstreams/transformstream", resolveCustomPolyfillSource("internal-webstreams-transformstream.js")],
	["internal/worker/js_transferable", resolveCustomPolyfillSource("internal-worker-js-transferable.js")],
	["internal/test/binding", resolveCustomPolyfillSource("internal-test-binding.js")],
	["internal/mime", resolveCustomPolyfillSource("internal-mime.js")],
]);

// node-stdlib-browser provides the mapping from Node.js stdlib to polyfill paths
// e.g., { path: "/path/to/path-browserify/index.js", fs: null, ... }
// We use this mapping instead of maintaining our own

/**
 * Bundle a stdlib polyfill module using esbuild
 */
export async function bundlePolyfill(moduleName: string): Promise<string> {
	const cached = polyfillCache.get(moduleName);
	if (cached) return cached;

	// Get the polyfill entry point from node-stdlib-browser
	const entryPoint =
		CUSTOM_POLYFILL_ENTRY_POINTS.get(moduleName) ??
		stdLibBrowser[moduleName as keyof typeof stdLibBrowser];
	if (!entryPoint) {
		throw new Error(`No polyfill available for module: ${moduleName}`);
	}

	// Build alias mappings for all Node.js builtins
	// This ensures nested dependencies (like crypto -> stream) are resolved correctly
	const alias: Record<string, string> = {};
	for (const [name, path] of Object.entries(stdLibBrowser)) {
		if (path !== null) {
			alias[name] = path;
			alias[`node:${name}`] = path;
		}
	}
	if (typeof stdLibBrowser.crypto === "string") {
		alias.__secure_exec_crypto_browserify__ = stdLibBrowser.crypto;
	}
	alias["web-streams-polyfill/dist/ponyfill.js"] = WEB_STREAMS_PONYFILL_PATH;

	// Bundle using esbuild with CommonJS format
	// This ensures proper module.exports handling for all module types including JSON
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
		// Externalize 'process' - we provide our own process polyfill in the bridge.
		// Without this, node-stdlib-browser's process polyfill gets bundled and
		// overwrites globalThis.process, breaking process.argv modifications.
		external: ["process"],
	});

	const code = result.outputFiles[0].text;

	// Check if this is a JSON module (esbuild creates *_default but doesn't export it)
	// For JSON modules, look for the default export pattern and extract it
	const defaultExportMatch = code.match(/var\s+(\w+_default)\s*=\s*\{/);

	let wrappedCode: string;
	if (defaultExportMatch && !code.includes("module.exports")) {
		// JSON module: wrap and return the default export object
		const defaultVar = defaultExportMatch[1];
		wrappedCode = `(function() {
    ${code}
    return ${defaultVar};
  })()`;
	} else {
		// Regular CommonJS module: wrap and return module.exports
		wrappedCode = `(function() {
    var module = { exports: {} };
    var exports = module.exports;
    ${code}
    return module.exports;
  })()`;
	}

	polyfillCache.set(moduleName, wrappedCode);
	return wrappedCode;
}

/**
 * Get all available stdlib modules (those with non-null polyfills)
 */
export function getAvailableStdlib(): string[] {
	return Object.keys(stdLibBrowser).filter(
		(key) => stdLibBrowser[key as keyof typeof stdLibBrowser] !== null,
	);
}

/**
 * Check if a module has a polyfill available
 * Note: fs returns null from node-stdlib-browser since we provide our own implementation
 */
export function hasPolyfill(moduleName: string): boolean {
	// Strip node: prefix
	const name = moduleName.replace(/^node:/, "");
	if (CUSTOM_POLYFILL_ENTRY_POINTS.has(name)) {
		return true;
	}
	const polyfill = stdLibBrowser[name as keyof typeof stdLibBrowser];
	return polyfill !== undefined && polyfill !== null;
}

/**
 * Pre-bundle all polyfills (for faster startup)
 */
export async function prebundleAllPolyfills(): Promise<Map<string, string>> {
	const modules = getAvailableStdlib();
	await Promise.all(modules.map((m) => bundlePolyfill(m)));
	return new Map(polyfillCache);
}
