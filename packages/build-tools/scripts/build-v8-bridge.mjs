import { build } from "esbuild";
import { createRequire } from "node:module";
import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import stdLibBrowser from "node-stdlib-browser";
import path from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const packageRoot = path.resolve(scriptDir, "..");
const workspaceRoot = path.resolve(packageRoot, "..", "..");
const require = createRequire(import.meta.url);

function parseArgs(argv) {
	const options = {};
	for (let index = 0; index < argv.length; index++) {
		const arg = argv[index];
		if (arg === "--out-dir" || arg === "--bridge-out" || arg === "--zlib-out") {
			const value = argv[++index];
			if (!value) {
				throw new Error(`${arg} requires a path`);
			}
			options[arg.slice(2)] = path.resolve(value);
			continue;
		}
		throw new Error(`Unknown argument: ${arg}`);
	}
	return options;
}

const options = parseArgs(process.argv.slice(2));
const bridgeEntry = path.join(packageRoot, "bridge-src", "index.ts");
const bridgeAssetsDir = path.join(workspaceRoot, "crates", "execution", "assets");
const bridgeSeamSourcefile = path.join(bridgeAssetsDir, "v8-bridge.generated-seam.js");
const bridgeContract = path.join(
	workspaceRoot,
	"crates",
	"bridge",
	"bridge-contract.json",
);
const defaultBridgeOutput = path.join(
	workspaceRoot,
	"crates",
	"execution",
	"assets",
	"v8-bridge.js",
);
const defaultZlibBridgeOutput = path.join(
	workspaceRoot,
	"crates",
	"execution",
	"assets",
	"v8-bridge-zlib.js",
);
const bridgeOutput =
	options["bridge-out"] ??
	(options["out-dir"]
		? path.join(options["out-dir"], "v8-bridge.js")
		: defaultBridgeOutput);
const zlibBridgeOutput =
	options["zlib-out"] ??
	(options["out-dir"]
		? path.join(options["out-dir"], "v8-bridge-zlib.js")
		: defaultZlibBridgeOutput);
const tempSuffix = `.tmp-${process.pid}-${Date.now()}`;
const bridgeTempOutput = `${bridgeOutput}${tempSuffix}`;
const zlibBridgeTempOutput = `${zlibBridgeOutput}${tempSuffix}`;
const undiciShimDir = path.join(
	workspaceRoot,
	"crates",
	"execution",
	"assets",
	"undici-shims",
);
const undiciRuntimeFeaturesShim = path.join(
	undiciShimDir,
	"runtime-features.js",
);
const undiciRuntimeFeaturesPath = require.resolve(
	"undici/lib/util/runtime-features.js",
);
const nodeStdlibUrlPackageEntry = createRequire(stdLibBrowser.url).resolve("url/");
const wsNodeEntry = require.resolve("ws");
const exodusBytesRoot = path.dirname(
	require.resolve("@exodus/bytes/encoding-lite.js"),
);
const fetchBlobEntry = require.resolve("fetch-blob");
const fetchBlobFileEntry = require.resolve("fetch-blob/file.js");

const alias = {};
const customAlias = {
	"@exodus/bytes/encoding-lite.js": require.resolve(
		"@exodus/bytes/encoding-lite.js",
	),
	"fetch-blob": fetchBlobEntry,
	"fetch-blob/file.js": fetchBlobFileEntry,
	crypto: path.join(undiciShimDir, "crypto.cjs"),
	"node:crypto": path.join(undiciShimDir, "crypto.cjs"),
	"agentos-legacy-crypto-polyfill": stdLibBrowser.crypto,
	url: path.join(undiciShimDir, "url.js"),
	"node:url": path.join(undiciShimDir, "url.js"),
	"agentos-legacy-url-polyfill": nodeStdlibUrlPackageEntry,
	"agentos-node-url-polyfill": stdLibBrowser.url,
	"agentos-text-encoding-polyfill": path.join(
		packageRoot,
		"bridge-src",
		"polyfills",
		"text-encoding.ts",
	),
	"agentos-blob-file-polyfill": path.join(
		packageRoot,
		"bridge-src",
		"polyfills",
		"blob-file.ts",
	),
	"agentos-whatwg-url-polyfill": path.join(
		packageRoot,
		"bridge-src",
		"polyfills",
		"whatwg-url.ts",
	),
	"agentos-dom-events-polyfill": path.join(
		packageRoot,
		"bridge-src",
		"polyfills",
		"dom-events.ts",
	),
	"agentos-abort-polyfill": path.join(
		packageRoot,
		"bridge-src",
		"polyfills",
		"abort.ts",
	),
	"whatwg-url": require.resolve("whatwg-url"),
	stream: path.join(undiciShimDir, "stream.js"),
	"node:stream": path.join(undiciShimDir, "stream.js"),
	"agentos-stream-stdlib": require.resolve("readable-stream"),
	net: path.join(undiciShimDir, "net.js"),
	"node:net": path.join(undiciShimDir, "net.js"),
	tls: path.join(undiciShimDir, "tls.js"),
	"node:tls": path.join(undiciShimDir, "tls.js"),
	dns: path.join(undiciShimDir, "dns.js"),
	"node:dns": path.join(undiciShimDir, "dns.js"),
	"dns/promises": path.join(undiciShimDir, "dns-promises.js"),
	"node:dns/promises": path.join(undiciShimDir, "dns-promises.js"),
	http: path.join(undiciShimDir, "http.js"),
	"node:http": path.join(undiciShimDir, "http.js"),
	https: path.join(undiciShimDir, "https.js"),
	"node:https": path.join(undiciShimDir, "https.js"),
	http2: path.join(undiciShimDir, "http2.js"),
	"node:http2": path.join(undiciShimDir, "http2.js"),
	"node:diagnostics_channel": path.join(
		undiciShimDir,
		"diagnostics_channel.js",
	),
	"diagnostics_channel": path.join(undiciShimDir, "diagnostics_channel.js"),
	"node:perf_hooks": path.join(undiciShimDir, "perf_hooks.js"),
	"perf_hooks": path.join(undiciShimDir, "perf_hooks.js"),
	"node:async_hooks": path.join(undiciShimDir, "async_hooks.js"),
	async_hooks: path.join(undiciShimDir, "async_hooks.js"),
	"node:util/types": path.join(undiciShimDir, "util-types.js"),
	"util/types": path.join(undiciShimDir, "util-types.js"),
	"node:worker_threads": path.join(undiciShimDir, "worker_threads.js"),
	worker_threads: path.join(undiciShimDir, "worker_threads.js"),
	"node:sqlite": path.join(undiciShimDir, "sqlite.js"),
	sqlite: path.join(undiciShimDir, "sqlite.js"),
	randombytes: path.join(undiciShimDir, "randombytes.js"),
	ws: require.resolve("ws"),
};
Object.assign(alias, customAlias);
for (const [name, modulePath] of Object.entries(stdLibBrowser)) {
	if (typeof modulePath === "string" && !(name in alias)) {
		alias[name] = modulePath;
		const nodeName = `node:${name}`;
		if (!(nodeName in alias)) {
			alias[nodeName] = modulePath;
		}
	}
}
const mainBundleAlias = {
	...alias,
	zlib: path.join(undiciShimDir, "zlib.js"),
	"node:zlib": path.join(undiciShimDir, "zlib.js"),
};
const zlibBundleAlias = {
	...alias,
	url: stdLibBrowser.url,
	"node:url": stdLibBrowser.url,
};

await mkdir(path.dirname(bridgeOutput), { recursive: true });
await mkdir(path.dirname(zlibBridgeOutput), { recursive: true });

let bridgeSourceText = await generateBridgeSeamText();
await validateBridgeContractGlobals(bridgeSourceText);
bridgeSourceText = bridgeSourceText.replace(/\n\s*rationale:\s*"[^"]*",?/g, "");
bridgeSourceText = bridgeSourceText
	.replace(/classification:\s*"hardened"/g, 'c:"h"')
	.replace(/classification:\s*"mutable-runtime-state"/g, 'c:"m"')
	.replace(/entry\.classification === "hardened"/g, 'entry.c==="h"')
	.replace(/entry\.classification === "mutable-runtime-state"/g, 'entry.c==="m"');

async function generateBridgeSeamText() {
	const result = await build({
		entryPoints: [bridgeEntry],
		outfile: bridgeSeamSourcefile,
		bundle: true,
		minify: false,
		format: "esm",
		platform: "neutral",
		target: "es2020",
		// Keep native class fields (do not lower to __publicField helpers) so the
		// seam preserves readable `static builtinModules = [...]` etc. that the
		// downstream text-consumers grep for.
		supported: {
			"class-field": true,
			"class-static-field": true,
		},
		loader: { ".ts": "ts" },
		write: false,
		treeShaking: false,
		// Externalize everything that is not a local bridge-src relative module.
		packages: "external",
		plugins: [
			{
				name: "agentos-bridge-source-externals",
				setup(pluginBuild) {
					// node: builtins stay as import specifiers.
					pluginBuild.onResolve({ filter: /^node:/ }, () => ({
						external: true,
					}));
					// ./undici-shims/* helpers are resolved by the downstream build
					// relative to the bridge assets directory.
					pluginBuild.onResolve({ filter: /^\.\/undici-shims\// }, () => ({
						external: true,
					}));
				},
			},
		],
	});
	if (result.errors.length > 0) {
		throw new Error(`Failed to build readable V8 bridge seam: ${result.errors[0].text}`);
	}
	return result.outputFiles[0].text;
}

async function validateBridgeContractGlobals(sourceText) {
	const contract = JSON.parse(await readFile(bridgeContract, "utf8"));
	const runtimeOnlyInventoryNames = new Set([
		"_processConfig",
		"__agentOsHrNowUs",
		"__agentOsRequireEsmSync",
		"_osConfig",
		"bridge",
		"_registerHandle",
		"_unregisterHandle",
		"_waitForActiveHandles",
		"_getActiveHandles",
		"_processExitRequested",
		"_childProcessDispatch",
		"_childProcessModule",
		"_osModule",
		"_moduleModule",
		"_httpModule",
		"_httpsModule",
		"_http2Module",
		"_dnsModule",
		"_dgramModule",
		"_netModule",
		"_tlsModule",
		"_netSocketDispatch",
		"_agentOSReadyDispatch",
		"_dgramSocketDispatch",
		"_http2RetainDispatch",
		"_httpServerDispatch",
		"_httpServerUpgradeDispatch",
		"_httpServerConnectDispatch",
		"_http2Dispatch",
		"_timerDispatch",
		"_drainImmediates",
		"_getPendingImmediateCount",
		"_upgradeSocketData",
		"_upgradeSocketEnd",
		"ProcessExitError",
		"_fs",
		"require",
		"_requireFrom",
		"__dynamicImport",
		"_moduleCache",
		"_pendingModules",
		"_currentModule",
		"_stdinData",
		"_stdinPosition",
		"_stdinEnded",
		"_stdinFlowMode",
		"module",
		"exports",
		"__filename",
		"__dirname",
		"fetch",
		"Headers",
		"Request",
		"Response",
		"WebSocket",
		"DOMException",
		"__importMetaResolve",
		"Blob",
		"File",
		"FormData",
	]);
	const contractNames = new Set();
	const duplicateContractNames = new Set();
	for (const group of contract.groups ?? []) {
		for (const name of group.names ?? []) {
			if (contractNames.has(name)) {
				duplicateContractNames.add(name);
			}
			contractNames.add(name);
		}
	}

	const inventoryMatch = sourceText.match(
		/var\s+NODE_CUSTOM_GLOBAL_INVENTORY\s*=\s*\[([\s\S]*?)\n\s*\];/,
	);
	if (!inventoryMatch) {
		throw new Error(
			"Failed to find NODE_CUSTOM_GLOBAL_INVENTORY in generated V8 bridge seam",
		);
	}

	const inventoryNames = new Set();
	const duplicateInventoryNames = new Set();
	for (const match of inventoryMatch[1].matchAll(/\bname:\s*"([^"]+)"/g)) {
		const name = match[1];
		if (inventoryNames.has(name)) {
			duplicateInventoryNames.add(name);
		}
		inventoryNames.add(name);
	}

	const missingInventoryNames = [...contractNames].filter(
		(name) => !inventoryNames.has(name),
	);
	const unexpectedInventoryNames = [...inventoryNames].filter(
		(name) => !contractNames.has(name) && !runtimeOnlyInventoryNames.has(name),
	);
	const staleRuntimeOnlyNames = [...runtimeOnlyInventoryNames].filter(
		(name) => !inventoryNames.has(name),
	);
	const errors = [];
	if (duplicateContractNames.size > 0) {
		errors.push(
			`duplicate names in bridge-contract.json: ${[...duplicateContractNames].sort().join(", ")}`,
		);
	}
	if (duplicateInventoryNames.size > 0) {
		errors.push(
			`duplicate names in NODE_CUSTOM_GLOBAL_INVENTORY: ${[...duplicateInventoryNames].sort().join(", ")}`,
		);
	}
	if (missingInventoryNames.length > 0) {
		errors.push(
			`contract names missing from NODE_CUSTOM_GLOBAL_INVENTORY: ${missingInventoryNames.sort().join(", ")}`,
		);
	}
	if (unexpectedInventoryNames.length > 0) {
		errors.push(
			`NODE_CUSTOM_GLOBAL_INVENTORY names missing from bridge-contract.json or the runtime-only allowlist: ${unexpectedInventoryNames.sort().join(", ")}`,
		);
	}
	if (staleRuntimeOnlyNames.length > 0) {
		errors.push(
			`runtime-only inventory allowlist names missing from NODE_CUSTOM_GLOBAL_INVENTORY: ${staleRuntimeOnlyNames.sort().join(", ")}`,
		);
	}
	if (errors.length > 0) {
		throw new Error(`V8 bridge contract drift detected:\n  - ${errors.join("\n  - ")}`);
	}
}

async function rewriteUndiciRuntimeFeaturesBundle(bundlePath, { required } = { required: false }) {
	const bundleText = await readFile(bundlePath, "utf8");
	const replacement = (moduleVar, commonJsHelperVar, cjsModuleVar) =>
		`${moduleVar}=${commonJsHelperVar}((_,${cjsModuleVar})=>{"use strict";var e=new Set(["crypto","sqlite","markAsUncloneable","zstd"]),t=new Map([["crypto",!0],["sqlite",!1],["markAsUncloneable",!1],["zstd",!1]]),r={clear(){t.clear()},has(n){if(!e.has(n))throw new TypeError(\`unknown feature: \${n}\`);return t.get(n)??!1},set(n,i){if(!e.has(n))throw new TypeError(\`unknown feature: \${n}\`);t.set(n,!!i)}};${cjsModuleVar}.exports.runtimeFeatures=r;${cjsModuleVar}.exports.default=r})`;
	const runtimeFeaturesModulePattern =
		/var ([A-Za-z_$][\w$]*)=([A-Za-z_$][\w$]*)\(\(([A-Za-z_$][\w$]*),([A-Za-z_$][\w$]*)\)=>\{"use strict";[\s\S]*?\4\.exports\.runtimeFeatures=([A-Za-z_$][\w$]*);\4\.exports\.default=\5\}\);/;
	let patched = bundleText.replace(
		runtimeFeaturesModulePattern,
		(_match, moduleVar, commonJsHelperVar, _exportsArgVar, cjsModuleVar) =>
			`var ${replacement(moduleVar, commonJsHelperVar, cjsModuleVar)};`,
	);
	if (patched === bundleText) {
		const runtimeFeaturesObjectModulePattern =
			/var ([A-Za-z_$][\w$]*)=([A-Za-z_$][\w$]*)\(\{"[^"]*runtime-features\.js"\(([A-Za-z_$][\w$]*),([A-Za-z_$][\w$]*)\)\{"use strict";[\s\S]*?\4\.exports\.runtimeFeatures=([A-Za-z_$][\w$]*),\4\.exports\.default=\5\}\}\);/;
		patched = bundleText.replace(
			runtimeFeaturesObjectModulePattern,
			(_match, moduleVar, commonJsHelperVar, _exportsArgVar, cjsModuleVar) =>
				`var ${replacement(moduleVar, commonJsHelperVar, cjsModuleVar)};`,
		);
	}
	if (patched === bundleText) {
		const runtimeFeaturesObjectAssignmentPattern =
			/([A-Za-z_$][\w$]*)=([A-Za-z_$][\w$]*)\(\{"[^"]*runtime-features\.js"\(([A-Za-z_$][\w$]*),([A-Za-z_$][\w$]*)\)\{"use strict";[\s\S]*?\4\.exports\.runtimeFeatures=([A-Za-z_$][\w$]*),\4\.exports\.default=\5\}\}\)(?=,|;)/;
		patched = bundleText.replace(
			runtimeFeaturesObjectAssignmentPattern,
			(_match, moduleVar, commonJsHelperVar, _exportsArgVar, cjsModuleVar) =>
				replacement(moduleVar, commonJsHelperVar, cjsModuleVar),
		);
	}
	if (patched === bundleText) {
		if (required) {
			throw new Error(`Failed to rewrite undici runtime-features in ${bundlePath}`);
		}
		return;
	}
	await writeFile(bundlePath, patched);
}

async function rewriteUnsupportedUtilTypesBundle(
	bundlePath,
	{ required } = { required: false },
) {
	const bundleText = await readFile(bundlePath, "utf8");
	const unsupportedUserlandTypesPattern =
		/\["isProxy","isExternal","isModuleNamespaceObject"\]\.forEach\(function\(([A-Za-z_$][\w$]*)\)\{Object\.defineProperty\(([A-Za-z_$][\w$]*),\1,\{enumerable:!1,value:function\(\)\{throw new Error\(\1\+" is not supported in userland"\)\}\}\)\}\)/;
	const patched = bundleText.replace(
		unsupportedUserlandTypesPattern,
		(_match, methodVar, exportsVar) =>
			`["isProxy","isExternal","isModuleNamespaceObject"].forEach(function(${methodVar}){Object.defineProperty(${exportsVar},${methodVar},{enumerable:!1,value:function(){return!1}})})`,
	);
	if (patched === bundleText) {
		if (required) {
			throw new Error(`Failed to rewrite util support/types in ${bundlePath}`);
		}
		return;
	}
	await writeFile(bundlePath, patched);
}

async function buildWebStreamsPrelude() {
	const preludeResult = await build({
		stdin: {
			contents: [
				'import {',
				'  ReadableStream,',
				'  WritableStream,',
				'  TransformStream,',
				'} from "web-streams-polyfill";',
				'import { TextDecoder as AgentOSTextDecoder, TextDecoderStream as AgentOSTextDecoderStream, TextEncoder2 as AgentOSTextEncoder, TextEncoderStream as AgentOSTextEncoderStream } from "agentos-text-encoding-polyfill";',
				'import { Blob as AgentOSBlob, File as AgentOSFile } from "agentos-blob-file-polyfill";',
				'if (typeof globalThis.TextEncoder === "undefined") {',
				"  globalThis.TextEncoder = AgentOSTextEncoder;",
				"}",
				'if (typeof globalThis.TextDecoder === "undefined") {',
				"  globalThis.TextDecoder = AgentOSTextDecoder;",
				"}",
				'if (typeof globalThis.ReadableStream === "undefined") {',
				"  globalThis.ReadableStream = ReadableStream;",
				"}",
				'if (typeof globalThis.WritableStream === "undefined") {',
				"  globalThis.WritableStream = WritableStream;",
				"}",
				'if (typeof globalThis.TransformStream === "undefined") {',
				"  globalThis.TransformStream = TransformStream;",
				"}",
				"globalThis.TextEncoderStream = AgentOSTextEncoderStream;",
				"globalThis.TextDecoderStream = AgentOSTextDecoderStream;",
				"globalThis.Blob = AgentOSBlob;",
				"globalThis.File = AgentOSFile;",
				'if (typeof globalThis.MessagePort === "undefined") {',
				"  globalThis.MessagePort = class MessagePortStub {",
				"    onmessage = null;",
				"    postMessage(_message) {}",
				"    start() {}",
				"    close() {}",
				"    addEventListener() {}",
				"    removeEventListener() {}",
				"  };",
				"}",
				'if (typeof globalThis.MessageChannel === "undefined") {',
				"  globalThis.MessageChannel = class MessageChannelStub {",
				"    constructor() {",
				"      this.port1 = new globalThis.MessagePort();",
				"      this.port2 = new globalThis.MessagePort();",
				"    }",
				"  };",
				"}",
				'if (typeof globalThis.performance === "undefined") {',
				"  const performanceStart = Date.now();",
				"  globalThis.performance = {",
				"    now() {",
				'      if (typeof globalThis.__agentOsHrNowUs === "function") {',
				"        return globalThis.__agentOsHrNowUs() / 1000;",
				"      }",
				"      return Date.now() - performanceStart;",
				"    },",
				"  };",
				"}",
				'if (typeof globalThis.performance.markResourceTiming !== "function") {',
				"  globalThis.performance.markResourceTiming = () => {};",
				"}",
			].join("\n"),
			resolveDir: bridgeAssetsDir,
			sourcefile: "v8-bridge-web-streams.entry.js",
			loader: "js",
		},
		bundle: true,
		write: false,
		format: "iife",
		platform: "browser",
		target: "es2020",
		minify: true,
		alias,
		plugins: createUndiciBuildPlugins(),
		define: {
			"process.env.NODE_ENV": '"production"',
			global: "globalThis",
		},
		banner: {
			js: [
				'if(typeof globalThis.global==="undefined"){globalThis.global=globalThis;}',
				'if(typeof globalThis.process==="undefined"){globalThis.process={env:{},argv:["node"],browser:false,version:"v22.0.0",versions:{node:"22.0.0"},nextTick(callback,...args){return Promise.resolve().then(()=>callback(...args));}};}',
				'if(typeof globalThis.SharedArrayBuffer==="undefined"){class SharedArrayBufferBootstrapStub{}Object.defineProperties(SharedArrayBufferBootstrapStub.prototype,{byteLength:{configurable:true,get(){if(!(this instanceof SharedArrayBufferBootstrapStub))throw new TypeError("incompatible receiver");return 0;}},growable:{configurable:true,get(){if(!(this instanceof SharedArrayBufferBootstrapStub))throw new TypeError("incompatible receiver");return false;}}});SharedArrayBufferBootstrapStub.__agentOSBootstrapStub=true;globalThis.SharedArrayBuffer=SharedArrayBufferBootstrapStub;}',
			].join(""),
		},
	});
	if (preludeResult.errors.length > 0) {
		throw new Error(`Failed to build web streams prelude: ${preludeResult.errors[0].text}`);
	}
	return `${preludeResult.outputFiles[0].text}\n`;
}

async function buildUrlPrelude() {
	const urlPreludeResult = await build({
		stdin: {
			contents: [
				'import { installWhatwgUrlGlobals } from "agentos-whatwg-url-polyfill";',
				"installWhatwgUrlGlobals(globalThis);",
				'if (globalThis.SharedArrayBuffer?.__agentOSBootstrapStub === true) {',
				"  delete globalThis.SharedArrayBuffer;",
				"}",
			].join("\n"),
			resolveDir: bridgeAssetsDir,
			sourcefile: "v8-bridge-url.entry.js",
			loader: "js",
		},
		bundle: true,
		write: false,
		format: "iife",
		platform: "browser",
		target: "es2020",
		minify: true,
		alias,
		plugins: createUndiciBuildPlugins(),
		define: {
			"process.env.NODE_ENV": '"production"',
			global: "globalThis",
		},
	});
	if (urlPreludeResult.errors.length > 0) {
		throw new Error(
			`Failed to build URL prelude: ${urlPreludeResult.errors[0].text}`,
		);
	}
	return `${urlPreludeResult.outputFiles[0].text}\n`;
}

async function buildEventAbortPrelude() {
	const eventAbortPreludeResult = await build({
		stdin: {
			contents: [
				'import { Event, CustomEvent, EventTarget } from "agentos-dom-events-polyfill";',
				'import { AbortController, AbortSignal } from "agentos-abort-polyfill";',
				"globalThis.Event = Event;",
				"globalThis.CustomEvent = CustomEvent;",
				"globalThis.EventTarget = EventTarget;",
				"globalThis.AbortSignal = AbortSignal;",
				"globalThis.AbortController = AbortController;",
			].join("\n"),
			resolveDir: bridgeAssetsDir,
			sourcefile: "v8-bridge-event-abort.entry.js",
			loader: "js",
		},
		bundle: true,
		write: false,
		format: "iife",
		platform: "browser",
		target: "es2020",
		minify: true,
		alias,
		plugins: createUndiciBuildPlugins(),
		define: {
			"process.env.NODE_ENV": '"production"',
			global: "globalThis",
		},
	});
	if (eventAbortPreludeResult.errors.length > 0) {
		throw new Error(
			`Failed to build event/abort prelude: ${eventAbortPreludeResult.errors[0].text}`,
		);
	}
	return `${eventAbortPreludeResult.outputFiles[0].text}\n`;
}

async function prependBundlePrelude(bundlePath, preludeSource) {
	const bundleText = await readFile(bundlePath, "utf8");
	if (bundleText.startsWith(preludeSource)) {
		return;
	}
	await writeFile(bundlePath, `${preludeSource}${bundleText}`);
}

function createUndiciBuildPlugins() {
	return [
		{
			name: "agentos-exodus-bytes-pure-js",
			setup(build) {
				build.onLoad(
					{
						filter:
							/[\\/]@exodus[\\/]bytes[\\/]fallback[\\/]platform\.browser\.js$/,
					},
					async () => ({
						contents: await readFile(
							path.join(
								exodusBytesRoot,
								"fallback",
								"platform.native.js",
							),
							"utf8",
						),
						loader: "js",
						resolveDir: path.join(exodusBytesRoot, "fallback"),
					}),
				);
				build.onLoad(
					{
						filter:
							/[\\/]@exodus[\\/]bytes[\\/]fallback[\\/]utf8\.auto\.browser\.js$/,
					},
					async () => ({
						contents: await readFile(
							path.join(
								exodusBytesRoot,
								"fallback",
								"utf8.auto.js",
							),
							"utf8",
						),
						loader: "js",
						resolveDir: path.join(exodusBytesRoot, "fallback"),
					}),
				);
				build.onLoad(
					{ filter: /[\\/]@exodus[\\/]bytes[\\/]utf16\.browser\.js$/ },
					async () => ({
						contents: await readFile(
							path.join(exodusBytesRoot, "utf16.native.js"),
							"utf8",
						),
						loader: "js",
						resolveDir: exodusBytesRoot,
					}),
				);
			},
		},
		{
			name: "agentos-fetch-blob-preinstalled-streams",
			setup(build) {
				build.onLoad(
					{ filter: /[\\/]fetch-blob[\\/]index\.js$/ },
					async (args) => {
						const source = await readFile(args.path, "utf8");
						const bootstrapStart = source.indexOf(
							"if (!globalThis.ReadableStream)",
						);
						const implementationStart = source.indexOf("// 64 KiB");
						if (bootstrapStart < 0 || implementationStart < bootstrapStart) {
							throw new Error(
								`fetch-blob bootstrap shape changed in ${args.path}`,
							);
						}
						return {
							contents:
								source.slice(0, bootstrapStart) +
								source.slice(implementationStart),
							loader: "js",
							resolveDir: path.dirname(args.path),
						};
					},
				);
			},
		},
		{
			name: "agentos-undici-runtime-features-shim",
			setup(build) {
				// readable-stream deliberately imports the trailing-slash package
				// specifier. esbuild aliases reject trailing-slash names, so resolve
				// this exact dependency through a plugin.
				build.onResolve({ filter: /^process\/$/ }, () => ({
					path: path.join(undiciShimDir, "process.cjs"),
				}));
				build.onResolve(
					{
						filter: /^(undici\/lib\/.+|web-streams-polyfill)$/,
					},
					(args) => {
						const resolvedPath = require.resolve(args.path, {
							paths: [packageRoot, workspaceRoot, args.resolveDir],
						});
						return { path: resolvedPath };
					},
				);
				build.onResolve({ filter: /^(?:node:)?worker_threads$/ }, () => ({
					path: path.join(undiciShimDir, "worker_threads.js"),
				}));
				build.onResolve({ filter: /runtime-features(?:\.js)?$/ }, (args) => {
					const resolved = path.resolve(args.resolveDir, args.path);
					if (resolved !== undiciRuntimeFeaturesPath) {
						return null;
					}
					return { path: undiciRuntimeFeaturesShim };
				});
			},
		},
	];
}

const result = await build({
	stdin: {
		contents: bridgeSourceText,
		resolveDir: bridgeAssetsDir,
		sourcefile: bridgeSeamSourcefile,
		loader: "js",
	},
	bundle: true,
	outfile: bridgeTempOutput,
	write: true,
	format: "iife",
	platform: "browser",
	target: "es2020",
	minify: true,
	metafile: true,
	alias: mainBundleAlias,
	define: {
		"process.env.NODE_ENV": '"production"',
		global: "globalThis",
	},
	plugins: createUndiciBuildPlugins(),
	banner: {
			js: [
				'if(typeof globalThis.global==="undefined"){globalThis.global=globalThis;}',
				'if(typeof globalThis.process==="undefined"){globalThis.process={env:{},argv:["node"],browser:false,version:"v22.0.0",versions:{node:"22.0.0"},nextTick(callback,...args){return Promise.resolve().then(()=>callback(...args));}};}',
				`if(typeof globalThis.TextEncoder==="undefined"){globalThis.TextEncoder=class{encode(value=""){const input=String(value??"");const encoded=unescape(encodeURIComponent(input));const out=new Uint8Array(encoded.length);for(let i=0;i<encoded.length;i++){out[i]=encoded.charCodeAt(i);}return out;}};}`,
				`if(typeof globalThis.TextDecoder==="undefined"){globalThis.TextDecoder=class{decode(value=new Uint8Array()){const view=value instanceof Uint8Array?value:ArrayBuffer.isView(value)?new Uint8Array(value.buffer,value.byteOffset,value.byteLength):value instanceof ArrayBuffer?new Uint8Array(value):new Uint8Array(0);let binary="";for(let i=0;i<view.length;i++){binary+=String.fromCharCode(view[i]);}return decodeURIComponent(escape(binary));}};}`,
				`if(typeof globalThis.Buffer==="undefined"){const __agentOsTe=typeof TextEncoder==="function"?new TextEncoder():null;const __agentOsTd=typeof TextDecoder==="function"?new TextDecoder():null;class __AgentOsEarlyBuffer extends Uint8Array{static from(value,encoding="utf8"){if(value instanceof ArrayBuffer){return new __AgentOsEarlyBuffer(value);}if(ArrayBuffer.isView(value)){return new __AgentOsEarlyBuffer(value.buffer.slice(value.byteOffset,value.byteOffset+value.byteLength));}if(Array.isArray(value)){return new __AgentOsEarlyBuffer(value);}const stringValue=String(value??"");if(encoding==="base64"&&typeof atob==="function"){const binary=atob(stringValue);const out=new __AgentOsEarlyBuffer(binary.length);for(let i=0;i<binary.length;i++){out[i]=binary.charCodeAt(i);}return out;}if(encoding==="binary"||encoding==="latin1"){const out=new __AgentOsEarlyBuffer(stringValue.length);for(let i=0;i<stringValue.length;i++){out[i]=stringValue.charCodeAt(i)&255;}return out;}if(__agentOsTe){return new __AgentOsEarlyBuffer(__agentOsTe.encode(stringValue));}const out=new __AgentOsEarlyBuffer(stringValue.length);for(let i=0;i<stringValue.length;i++){out[i]=stringValue.charCodeAt(i)&255;}return out;}static alloc(size){return new __AgentOsEarlyBuffer(Number(size)||0);}static concat(list,totalLength){const length=totalLength??list.reduce((sum,item)=>sum+(item?.length??0),0);const out=new __AgentOsEarlyBuffer(length);let offset=0;for(const item of list){const chunk=item instanceof Uint8Array?item:__AgentOsEarlyBuffer.from(item);out.set(chunk,offset);offset+=chunk.length;}return out;}static isBuffer(value){return value instanceof Uint8Array;}static byteLength(value,encoding="utf8"){return __AgentOsEarlyBuffer.from(value,encoding).byteLength;}toString(encoding="utf8"){if(encoding==="base64"&&typeof btoa==="function"){let binary="";for(const byte of this){binary+=String.fromCharCode(byte);}return btoa(binary);}if(encoding==="binary"||encoding==="latin1"){let binary="";for(const byte of this){binary+=String.fromCharCode(byte);}return binary;}if(__agentOsTd){return __agentOsTd.decode(this);}return Array.from(this,byte=>String.fromCharCode(byte)).join("");}}globalThis.Buffer=__AgentOsEarlyBuffer;}`,
				'if(typeof globalThis.Buffer.allocUnsafe!=="function"){globalThis.Buffer.allocUnsafe=globalThis.Buffer.alloc;}if(typeof globalThis.Buffer.allocUnsafeSlow!=="function"){globalThis.Buffer.allocUnsafeSlow=globalThis.Buffer.alloc;}',
				'if(typeof globalThis.performance==="undefined"){const __agentOsPerformanceStart=Date.now();globalThis.performance={now(){if(typeof globalThis.__agentOsHrNowUs==="function"){return globalThis.__agentOsHrNowUs()/1000;}return Date.now()-__agentOsPerformanceStart;}};}if(typeof globalThis.performance.markResourceTiming!=="function"){globalThis.performance.markResourceTiming=()=>{};}',
				'if(typeof TextEncoder==="undefined"&&typeof globalThis.TextEncoder!=="undefined"){var TextEncoder=globalThis.TextEncoder;}if(typeof TextDecoder==="undefined"&&typeof globalThis.TextDecoder!=="undefined"){var TextDecoder=globalThis.TextDecoder;}if(typeof Buffer==="undefined"&&typeof globalThis.Buffer!=="undefined"){var Buffer=globalThis.Buffer;}',
			].join(""),
	},
	external: ["process"],
});

const zlibResult = await build({
	stdin: {
		contents: [
			'import * as assertStdlibModuleNs from "node:assert";',
			'import { Buffer as zlibBuffer } from "node:buffer";',
			'import * as utilStdlibModuleNs from "node:util";',
			'import * as zlibStdlibModuleNs from "node:zlib";',
			`import wsModule from ${JSON.stringify(wsNodeEntry)};`,
			"const assertModule = assertStdlibModuleNs.default ?? assertStdlibModuleNs;",
			"const utilModule = utilStdlibModuleNs.default ?? utilStdlibModuleNs;",
			"const zlibModule = zlibStdlibModuleNs.default ?? zlibStdlibModuleNs;",
			'const zlibConstants = typeof zlibModule.constants === "object" && zlibModule.constants !== null ? zlibModule.constants : Object.fromEntries(Object.entries(zlibModule).filter(([key, value]) => /^[A-Z0-9_]+$/.test(key) && typeof value === "number"));',
			'if(typeof zlibModule.constants === "undefined"){zlibModule.constants = zlibConstants;}',
			'const normalizeZlibInput = (value) => { if (typeof value === "string" || zlibBuffer.isBuffer(value)) return value; if (value instanceof ArrayBuffer) return zlibBuffer.from(value); if (ArrayBuffer.isView(value)) return zlibBuffer.from(value.buffer, value.byteOffset, value.byteLength); return value; };',
			'for (const name of ["deflate", "deflateSync", "gzip", "gzipSync", "deflateRaw", "deflateRawSync", "unzip", "unzipSync", "inflate", "inflateSync", "gunzip", "gunzipSync", "inflateRaw", "inflateRawSync"]) { const original = zlibModule[name]; if (typeof original === "function") zlibModule[name] = function(input, ...args) { return original.call(this, normalizeZlibInput(input), ...args); }; }',
			'if(typeof utilModule.TextEncoder==="undefined"&&typeof globalThis.TextEncoder==="function"){utilModule.TextEncoder=globalThis.TextEncoder;}',
			'if(typeof utilModule.TextDecoder==="undefined"&&typeof globalThis.TextDecoder==="function"){utilModule.TextDecoder=globalThis.TextDecoder;}',
			"globalThis.__agentOsBuiltinAssertModule = assertModule;",
			"globalThis.__agentOsBuiltinUtilModule = utilModule;",
			"globalThis.__agentOsBuiltinZlibModule = zlibModule;",
			"const AgentOSWebSocket = wsModule?.WebSocket ?? wsModule?.default?.WebSocket ?? wsModule?.default ?? wsModule;",
			'if(typeof AgentOSWebSocket === "function"){Object.defineProperty(globalThis,"WebSocket",{value:AgentOSWebSocket,writable:true,configurable:true,enumerable:false});}',
		].join("\n"),
		resolveDir: bridgeAssetsDir,
		sourcefile: "v8-bridge-zlib.entry.js",
		loader: "js",
	},
	bundle: true,
	outfile: zlibBridgeTempOutput,
	write: true,
	format: "iife",
	platform: "browser",
	target: "es2020",
	minify: true,
	metafile: true,
	alias: zlibBundleAlias,
	define: {
		"process.env.NODE_ENV": '"production"',
		global: "globalThis",
	},
	plugins: createUndiciBuildPlugins(),
	banner: {
			js: [
				'if(typeof globalThis.global==="undefined"){globalThis.global=globalThis;}',
				'if(typeof globalThis.process==="undefined"){globalThis.process={env:{},argv:["node"],browser:false,version:"v22.0.0",versions:{node:"22.0.0"},nextTick(callback,...args){return Promise.resolve().then(()=>callback(...args));}};}',
				`if(typeof globalThis.TextEncoder==="undefined"){globalThis.TextEncoder=class{encode(value=""){const input=String(value??"");const encoded=unescape(encodeURIComponent(input));const out=new Uint8Array(encoded.length);for(let i=0;i<encoded.length;i++){out[i]=encoded.charCodeAt(i);}return out;}};}`,
				`if(typeof globalThis.TextDecoder==="undefined"){globalThis.TextDecoder=class{decode(value=new Uint8Array()){const view=value instanceof Uint8Array?value:ArrayBuffer.isView(value)?new Uint8Array(value.buffer,value.byteOffset,value.byteLength):value instanceof ArrayBuffer?new Uint8Array(value):new Uint8Array(0);let binary="";for(let i=0;i<view.length;i++){binary+=String.fromCharCode(view[i]);}return decodeURIComponent(escape(binary));}};}`,
				`if(typeof globalThis.Buffer==="undefined"){const __agentOsTe=typeof TextEncoder==="function"?new TextEncoder():null;const __agentOsTd=typeof TextDecoder==="function"?new TextDecoder():null;class __AgentOsEarlyBuffer extends Uint8Array{static from(value,encoding="utf8"){if(value instanceof ArrayBuffer){return new __AgentOsEarlyBuffer(value);}if(ArrayBuffer.isView(value)){return new __AgentOsEarlyBuffer(value.buffer.slice(value.byteOffset,value.byteOffset+value.byteLength));}if(Array.isArray(value)){return new __AgentOsEarlyBuffer(value);}const stringValue=String(value??"");if(encoding==="base64"&&typeof atob==="function"){const binary=atob(stringValue);const out=new __AgentOsEarlyBuffer(binary.length);for(let i=0;i<binary.length;i++){out[i]=binary.charCodeAt(i);}return out;}if(encoding==="binary"||encoding==="latin1"){const out=new __AgentOsEarlyBuffer(stringValue.length);for(let i=0;i<stringValue.length;i++){out[i]=stringValue.charCodeAt(i)&255;}return out;}if(__agentOsTe){return new __AgentOsEarlyBuffer(__agentOsTe.encode(stringValue));}const out=new __AgentOsEarlyBuffer(stringValue.length);for(let i=0;i<stringValue.length;i++){out[i]=stringValue.charCodeAt(i)&255;}return out;}static alloc(size){return new __AgentOsEarlyBuffer(Number(size)||0);}static concat(list,totalLength){const length=totalLength??list.reduce((sum,item)=>sum+(item?.length??0),0);const out=new __AgentOsEarlyBuffer(length);let offset=0;for(const item of list){const chunk=item instanceof Uint8Array?item:__AgentOsEarlyBuffer.from(item);out.set(chunk,offset);offset+=chunk.length;}return out;}static isBuffer(value){return value instanceof Uint8Array;}static byteLength(value,encoding="utf8"){return __AgentOsEarlyBuffer.from(value,encoding).byteLength;}toString(encoding="utf8"){if(encoding==="base64"&&typeof btoa==="function"){let binary="";for(const byte of this){binary+=String.fromCharCode(byte);}return btoa(binary);}if(encoding==="binary"||encoding==="latin1"){let binary="";for(const byte of this){binary+=String.fromCharCode(byte);}return binary;}if(__agentOsTd){return __agentOsTd.decode(this);}return Array.from(this,byte=>String.fromCharCode(byte)).join("");}}globalThis.Buffer=__AgentOsEarlyBuffer;}`,
				'if(typeof globalThis.Buffer.allocUnsafe!=="function"){globalThis.Buffer.allocUnsafe=globalThis.Buffer.alloc;}if(typeof globalThis.Buffer.allocUnsafeSlow!=="function"){globalThis.Buffer.allocUnsafeSlow=globalThis.Buffer.alloc;}',
				'if(typeof globalThis.performance==="undefined"){const __agentOsPerformanceStart=Date.now();globalThis.performance={now(){if(typeof globalThis.__agentOsHrNowUs==="function"){return globalThis.__agentOsHrNowUs()/1000;}return Date.now()-__agentOsPerformanceStart;}};}if(typeof globalThis.performance.markResourceTiming!=="function"){globalThis.performance.markResourceTiming=()=>{};}',
				'if(typeof TextEncoder==="undefined"&&typeof globalThis.TextEncoder!=="undefined"){var TextEncoder=globalThis.TextEncoder;}if(typeof TextDecoder==="undefined"&&typeof globalThis.TextDecoder!=="undefined"){var TextDecoder=globalThis.TextDecoder;}if(typeof Buffer==="undefined"&&typeof globalThis.Buffer!=="undefined"){var Buffer=globalThis.Buffer;}',
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

for (const [name, buildResult] of [
	["v8-bridge.js", result],
	["v8-bridge-zlib.js", zlibResult],
]) {
	const timerBackedProcessInputs = Object.keys(
		buildResult.metafile?.inputs ?? {},
	).filter((input) => /(?:^|\/)process\/browser\.js$/.test(input));
	if (timerBackedProcessInputs.length > 0) {
		throw new Error(
			`${name} bundled the timer-backed browser process shim: ${timerBackedProcessInputs.join(", ")}`,
		);
	}
}

const webStreamsPrelude = await buildWebStreamsPrelude();
const urlPrelude = await buildUrlPrelude();
const eventAbortPrelude = await buildEventAbortPrelude();
await prependBundlePrelude(
	bridgeTempOutput,
	`${webStreamsPrelude}${urlPrelude}${eventAbortPrelude}`,
);
await rewriteUndiciRuntimeFeaturesBundle(bridgeTempOutput, { required: true });
await rewriteUnsupportedUtilTypesBundle(bridgeTempOutput, { required: true });
await rewriteUndiciRuntimeFeaturesBundle(zlibBridgeTempOutput);
await rewriteUnsupportedUtilTypesBundle(zlibBridgeTempOutput);
await rename(bridgeTempOutput, bridgeOutput);
await rename(zlibBridgeTempOutput, zlibBridgeOutput);

console.log(
	`Built ${path.relative(workspaceRoot, bridgeOutput)} (${result.outputFiles?.[0]?.text?.length ?? "written"} bytes)`,
);
