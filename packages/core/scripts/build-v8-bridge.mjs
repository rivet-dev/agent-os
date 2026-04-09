import { build } from "esbuild";
import { createRequire } from "node:module";
import { readFile, writeFile } from "node:fs/promises";
import stdLibBrowser from "node-stdlib-browser";
import path from "node:path";
import { fileURLToPath } from "node:url";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const packageRoot = path.resolve(scriptDir, "..");
const workspaceRoot = path.resolve(packageRoot, "..", "..");
const require = createRequire(import.meta.url);
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

const alias = {};
const customAlias = {
	stream: path.join(undiciShimDir, "stream.js"),
	"node:stream": path.join(undiciShimDir, "stream.js"),
	"agent-os-stream-stdlib": stdLibBrowser.stream,
	net: path.join(undiciShimDir, "net.js"),
	"node:net": path.join(undiciShimDir, "net.js"),
	tls: path.join(undiciShimDir, "tls.js"),
	"node:tls": path.join(undiciShimDir, "tls.js"),
	dns: path.join(undiciShimDir, "dns.js"),
	"node:dns": path.join(undiciShimDir, "dns.js"),
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
	"node:util/types": path.join(undiciShimDir, "util-types.js"),
	"util/types": path.join(undiciShimDir, "util-types.js"),
	"node:worker_threads": path.join(undiciShimDir, "worker_threads.js"),
	worker_threads: path.join(undiciShimDir, "worker_threads.js"),
	"node:sqlite": path.join(undiciShimDir, "sqlite.js"),
	sqlite: path.join(undiciShimDir, "sqlite.js"),
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

let bridgeSourceText = await readFile(bridgeSource, "utf8");
bridgeSourceText = bridgeSourceText.replace(/\n\s*rationale:\s*"[^"]*",?/g, "");
bridgeSourceText = bridgeSourceText
	.replace(/classification:\s*"hardened"/g, 'c:"h"')
	.replace(/classification:\s*"mutable-runtime-state"/g, 'c:"m"')
	.replace(/entry\.classification === "hardened"/g, 'entry.c==="h"')
	.replace(/entry\.classification === "mutable-runtime-state"/g, 'entry.c==="m"');

async function rewriteUndiciRuntimeFeaturesBundle(bundlePath, { required } = { required: false }) {
	const bundleText = await readFile(bundlePath, "utf8");
	const runtimeFeaturesModulePattern =
		/var ([A-Za-z_$][\w$]*)=([A-Za-z_$][\w$]*)\(\(\s*[A-Za-z_$][\w$]*,([A-Za-z_$][\w$]*)\)=>\{"use strict";var [A-Za-z_$][\w$]*=\{__proto__:null,"node:crypto":\(\)=>[A-Za-z_$][\w$]*\(\),"node:sqlite":\(\)=>[A-Za-z_$][\w$]*\(\),"node:worker_threads":\(\)=>[A-Za-z_$][\w$]*\(\),"node:zlib":\(\)=>[A-Za-z_$][\w$]*\(\)\};[\s\S]*?\3\.exports\.runtimeFeatures=([A-Za-z_$][\w$]*);\3\.exports\.default=\4\}\);/;
	const patched = bundleText.replace(
		runtimeFeaturesModulePattern,
		(_match, moduleVar, commonJsHelperVar, cjsModuleVar) =>
			`var ${moduleVar}=${commonJsHelperVar}((_,${cjsModuleVar})=>{"use strict";var e=new Set(["crypto","sqlite","markAsUncloneable","zstd"]),t=new Map([["crypto",!0],["sqlite",!1],["markAsUncloneable",!1],["zstd",!1]]),r={clear(){t.clear()},has(n){if(!e.has(n))throw new TypeError(\`unknown feature: \${n}\`);return t.get(n)??!1},set(n,i){if(!e.has(n))throw new TypeError(\`unknown feature: \${n}\`);t.set(n,!!i)}};${cjsModuleVar}.exports.runtimeFeatures=r;${cjsModuleVar}.exports.default=r});`,
	);
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
				'} from "web-streams-polyfill/ponyfill/es2018";',
				'if (typeof globalThis.ReadableStream === "undefined") {',
				"  globalThis.ReadableStream = ReadableStream;",
				"}",
				'if (typeof globalThis.WritableStream === "undefined") {',
				"  globalThis.WritableStream = WritableStream;",
				"}",
				'if (typeof globalThis.TransformStream === "undefined") {',
				"  globalThis.TransformStream = TransformStream;",
				"}",
				'if (typeof globalThis.URLSearchParams === "undefined") {',
				"  globalThis.URLSearchParams = class URLSearchParamsStub {",
				"    _entries = [];",
				"    constructor(init = undefined) {",
				'      if (typeof init === "string") {',
				'        const source = init.startsWith("?") ? init.slice(1) : init;',
				'        if (source.length > 0) {',
				'          for (const pair of source.split("&")) {',
				'            if (!pair) continue;',
				'            const [rawKey, rawValue = ""] = pair.split("=");',
				"            this.append(decodeURIComponent(rawKey), decodeURIComponent(rawValue));",
				"          }",
				"        }",
				"      } else if (Array.isArray(init)) {",
				"        for (const [key, value] of init) {",
				"          this.append(key, value);",
				"        }",
				'      } else if (init && typeof init === "object") {',
				"        for (const [key, value] of Object.entries(init)) {",
				"          this.append(key, value);",
				"        }",
				"      }",
				"    }",
				"    append(name, value) {",
				"      this._entries.push([String(name), String(value)]);",
				"    }",
				"    delete(name) {",
				"      const key = String(name);",
				"      this._entries = this._entries.filter(([entryKey]) => entryKey !== key);",
				"    }",
				"    get(name) {",
				"      const key = String(name);",
				"      const entry = this._entries.find(([entryKey]) => entryKey === key);",
				"      return entry ? entry[1] : null;",
				"    }",
				"    getAll(name) {",
				"      const key = String(name);",
				"      return this._entries.filter(([entryKey]) => entryKey === key).map(([, value]) => value);",
				"    }",
				"    has(name) {",
				"      const key = String(name);",
				"      return this._entries.some(([entryKey]) => entryKey === key);",
				"    }",
				"    set(name, value) {",
				"      this.delete(name);",
				"      this.append(name, value);",
				"    }",
				"    entries() {",
				"      return this._entries[Symbol.iterator]();",
				"    }",
				"    keys() {",
				"      return this._entries.map(([key]) => key)[Symbol.iterator]();",
				"    }",
				"    values() {",
				"      return this._entries.map(([, value]) => value)[Symbol.iterator]();",
				"    }",
				"    forEach(callback, thisArg = undefined) {",
				"      for (const [key, value] of this._entries) {",
				"        callback.call(thisArg, value, key, this);",
				"      }",
				"    }",
				"    toString() {",
				'      return this._entries.map(([key, value]) => `${encodeURIComponent(key)}=${encodeURIComponent(value)}`).join("&");',
				"    }",
				"    [Symbol.iterator]() {",
				"      return this.entries();",
				"    }",
				"  };",
				"  globalThis.URLSearchParams.__agentOsBootstrapStub = true;",
				"}",
				'if (typeof globalThis.URL === "undefined") {',
				"  globalThis.URL = class URLStub {",
				"    constructor(url, base = undefined) {",
				"      const raw = String(url ?? \"\");",
				"      const hasScheme = /^[a-zA-Z][a-zA-Z\\d+\\-.]*:/.test(raw);",
				"      const baseHref = hasScheme || typeof base === \"undefined\"",
				"        ? \"\"",
				"        : String(new globalThis.URL(base).href);",
				"      const resolved = hasScheme",
				"        ? raw",
				"        : baseHref.replace(/\\/[^/]*$/, \"/\") + raw;",
				"      const match = resolved.match(/^(\\w+:)\\/\\/([^/:?#]+)(:\\d+)?(.*)$/);",
				"      if (!match) {",
				"        throw new TypeError(`Invalid URL: ${raw}`);",
				"      }",
				"      this.protocol = match[1];",
				"      this.hostname = match[2];",
				"      this.port = (match[3] || \"\").slice(1);",
				"      const remainder = match[4] || \"/\";",
				"      const searchIndex = remainder.indexOf(\"?\");",
				"      const hashIndex = remainder.indexOf(\"#\");",
				"      const pathEnd = [searchIndex, hashIndex].filter((index) => index >= 0).sort((a, b) => a - b)[0] ?? remainder.length;",
				"      this.pathname = remainder.slice(0, pathEnd) || \"/\";",
				"      this.search = searchIndex >= 0",
				"        ? remainder.slice(searchIndex, hashIndex >= 0 && hashIndex > searchIndex ? hashIndex : remainder.length)",
				"        : \"\";",
				"      this.hash = hashIndex >= 0 ? remainder.slice(hashIndex) : \"\";",
				"      this.host = this.hostname + (this.port ? `:${this.port}` : \"\");",
				"      this.origin = `${this.protocol}//${this.host}`;",
				"      this.href = `${this.origin}${this.pathname}${this.search}${this.hash}`;",
				"      this.searchParams = new globalThis.URLSearchParams(this.search);",
				"    }",
				"    toString() {",
				"      return this.href;",
				"    }",
				"    toJSON() {",
				"      return this.href;",
				"    }",
				"  };",
				"  globalThis.URL.__agentOsBootstrapStub = true;",
				"}",
				'if (typeof globalThis.Blob === "undefined") {',
				"  globalThis.Blob = class BlobStub {};",
				"}",
				'if (typeof globalThis.AbortSignal === "undefined") {',
				"  globalThis.AbortSignal = class AbortSignalStub {",
				"    aborted = false;",
				"    reason = undefined;",
				"    _listeners = new Set();",
				"    addEventListener(type, listener) {",
				'      if (type !== "abort" || typeof listener !== "function") return;',
				"      this._listeners.add(listener);",
				"    }",
				"    removeEventListener(type, listener) {",
				'      if (type !== "abort") return;',
				"      this._listeners.delete(listener);",
				"    }",
				"    dispatchEvent(event) {",
				"      for (const listener of this._listeners) {",
				"        listener.call(this, event);",
				"      }",
				"      return true;",
				"    }",
				"    throwIfAborted() {",
				"      if (this.aborted) {",
				'        throw this.reason instanceof Error ? this.reason : new Error(String(this.reason ?? "AbortError"));',
				"      }",
				"    }",
				"  };",
				"}",
				'if (typeof globalThis.AbortController === "undefined") {',
				"  globalThis.AbortController = class AbortControllerStub {",
				"    constructor() {",
				"      this.signal = new globalThis.AbortSignal();",
				"    }",
				"    abort(reason = undefined) {",
				"      if (this.signal.aborted) return;",
				"      this.signal.aborted = true;",
				"      this.signal.reason = reason;",
				'      this.signal.dispatchEvent({ type: "abort" });',
				"    }",
				"  };",
				"}",
				'if (typeof globalThis.File === "undefined") {',
				"  globalThis.File = class FileStub extends Blob {",
				"    name;",
				"    lastModified;",
				"    webkitRelativePath;",
				'    constructor(parts = [], name = "", options = {}) {',
				"      super(parts, options);",
				"      this.name = String(name);",
				'      this.lastModified = typeof options.lastModified === "number" ? options.lastModified : Date.now();',
				'      this.webkitRelativePath = "";',
				"    }",
				"  };",
				"}",
				'if (typeof globalThis.FormData === "undefined") {',
				"  globalThis.FormData = class FormDataStub {",
				"    _entries = [];",
				"    append(name, value) {",
				"      this._entries.push([name, value]);",
				"    }",
				"    get(name) {",
				"      const entry = this._entries.find(([key]) => key === name);",
				"      return entry ? entry[1] : null;",
				"    }",
				"    getAll(name) {",
				"      return this._entries.filter(([key]) => key === name).map(([, value]) => value);",
				"    }",
				"    has(name) {",
				"      return this._entries.some(([key]) => key === name);",
				"    }",
				"    delete(name) {",
				"      this._entries = this._entries.filter(([key]) => key !== name);",
				"    }",
				"    entries() {",
				"      return this._entries[Symbol.iterator]();",
				"    }",
				"    [Symbol.iterator]() {",
				"      return this.entries();",
				"    }",
				"  };",
				"}",
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
				"      return Date.now() - performanceStart;",
				"    },",
				"  };",
				"}",
				'if (typeof globalThis.performance.markResourceTiming !== "function") {',
				"  globalThis.performance.markResourceTiming = () => {};",
				"}",
			].join("\n"),
			resolveDir: path.dirname(bridgeSource),
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
		define: {
			"process.env.NODE_ENV": '"production"',
			global: "globalThis",
		},
		banner: {
			js: [
				'if(typeof globalThis.global==="undefined"){globalThis.global=globalThis;}',
				'if(typeof globalThis.process==="undefined"){globalThis.process={env:{},argv:["node"],browser:false,version:"v22.0.0",versions:{node:"22.0.0"},nextTick(callback,...args){return Promise.resolve().then(()=>callback(...args));}};}',
			].join(""),
		},
	});
	if (preludeResult.errors.length > 0) {
		throw new Error(`Failed to build web streams prelude: ${preludeResult.errors[0].text}`);
	}
	return `${preludeResult.outputFiles[0].text}\n`;
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
			name: "agent-os-undici-runtime-features-shim",
			setup(build) {
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
	plugins: createUndiciBuildPlugins(),
	banner: {
			js: [
				'if(typeof globalThis.global==="undefined"){globalThis.global=globalThis;}',
				'if(typeof globalThis.process==="undefined"){globalThis.process={env:{},argv:["node"],browser:false,version:"v22.0.0",versions:{node:"22.0.0"},nextTick(callback,...args){return Promise.resolve().then(()=>callback(...args));}};}',
				`if(typeof globalThis.TextEncoder==="undefined"){globalThis.TextEncoder=class{encode(value=""){const input=String(value??"");const encoded=unescape(encodeURIComponent(input));const out=new Uint8Array(encoded.length);for(let i=0;i<encoded.length;i++){out[i]=encoded.charCodeAt(i);}return out;}};}`,
				`if(typeof globalThis.TextDecoder==="undefined"){globalThis.TextDecoder=class{decode(value=new Uint8Array()){const view=value instanceof Uint8Array?value:ArrayBuffer.isView(value)?new Uint8Array(value.buffer,value.byteOffset,value.byteLength):value instanceof ArrayBuffer?new Uint8Array(value):new Uint8Array(0);let binary="";for(let i=0;i<view.length;i++){binary+=String.fromCharCode(view[i]);}return decodeURIComponent(escape(binary));}};}`,
				`if(typeof globalThis.Buffer==="undefined"){const __agentOsTe=typeof TextEncoder==="function"?new TextEncoder():null;const __agentOsTd=typeof TextDecoder==="function"?new TextDecoder():null;class __AgentOsEarlyBuffer extends Uint8Array{static from(value,encoding="utf8"){if(value instanceof ArrayBuffer){return new __AgentOsEarlyBuffer(value);}if(ArrayBuffer.isView(value)){return new __AgentOsEarlyBuffer(value.buffer.slice(value.byteOffset,value.byteOffset+value.byteLength));}if(Array.isArray(value)){return new __AgentOsEarlyBuffer(value);}const stringValue=String(value??"");if(encoding==="base64"&&typeof atob==="function"){const binary=atob(stringValue);const out=new __AgentOsEarlyBuffer(binary.length);for(let i=0;i<binary.length;i++){out[i]=binary.charCodeAt(i);}return out;}if(encoding==="binary"||encoding==="latin1"){const out=new __AgentOsEarlyBuffer(stringValue.length);for(let i=0;i<stringValue.length;i++){out[i]=stringValue.charCodeAt(i)&255;}return out;}if(__agentOsTe){return new __AgentOsEarlyBuffer(__agentOsTe.encode(stringValue));}const out=new __AgentOsEarlyBuffer(stringValue.length);for(let i=0;i<stringValue.length;i++){out[i]=stringValue.charCodeAt(i)&255;}return out;}static alloc(size){return new __AgentOsEarlyBuffer(Number(size)||0);}static concat(list,totalLength){const length=totalLength??list.reduce((sum,item)=>sum+(item?.length??0),0);const out=new __AgentOsEarlyBuffer(length);let offset=0;for(const item of list){const chunk=item instanceof Uint8Array?item:__AgentOsEarlyBuffer.from(item);out.set(chunk,offset);offset+=chunk.length;}return out;}static isBuffer(value){return value instanceof Uint8Array;}static byteLength(value,encoding="utf8"){return __AgentOsEarlyBuffer.from(value,encoding).byteLength;}toString(encoding="utf8"){if(encoding==="base64"&&typeof btoa==="function"){let binary="";for(const byte of this){binary+=String.fromCharCode(byte);}return btoa(binary);}if(encoding==="binary"||encoding==="latin1"){let binary="";for(const byte of this){binary+=String.fromCharCode(byte);}return binary;}if(__agentOsTd){return __agentOsTd.decode(this);}return Array.from(this,byte=>String.fromCharCode(byte)).join("");}}globalThis.Buffer=__AgentOsEarlyBuffer;}`,
				'if(typeof globalThis.performance==="undefined"){const __agentOsPerformanceStart=Date.now();globalThis.performance={now(){return Date.now()-__agentOsPerformanceStart;}};}if(typeof globalThis.performance.markResourceTiming!=="function"){globalThis.performance.markResourceTiming=()=>{};}',
				'if(typeof TextEncoder==="undefined"&&typeof globalThis.TextEncoder!=="undefined"){var TextEncoder=globalThis.TextEncoder;}if(typeof TextDecoder==="undefined"&&typeof globalThis.TextDecoder!=="undefined"){var TextDecoder=globalThis.TextDecoder;}if(typeof Buffer==="undefined"&&typeof globalThis.Buffer!=="undefined"){var Buffer=globalThis.Buffer;}',
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
			'const zlibConstants = typeof zlibModule.constants === "object" && zlibModule.constants !== null ? zlibModule.constants : Object.fromEntries(Object.entries(zlibModule).filter(([key, value]) => /^[A-Z0-9_]+$/.test(key) && typeof value === "number"));',
			'if(typeof zlibModule.constants === "undefined"){zlibModule.constants = zlibConstants;}',
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
	plugins: createUndiciBuildPlugins(),
	banner: {
			js: [
				'if(typeof globalThis.global==="undefined"){globalThis.global=globalThis;}',
				'if(typeof globalThis.process==="undefined"){globalThis.process={env:{},argv:["node"],browser:false,version:"v22.0.0",versions:{node:"22.0.0"},nextTick(callback,...args){return Promise.resolve().then(()=>callback(...args));}};}',
				`if(typeof globalThis.TextEncoder==="undefined"){globalThis.TextEncoder=class{encode(value=""){const input=String(value??"");const encoded=unescape(encodeURIComponent(input));const out=new Uint8Array(encoded.length);for(let i=0;i<encoded.length;i++){out[i]=encoded.charCodeAt(i);}return out;}};}`,
				`if(typeof globalThis.TextDecoder==="undefined"){globalThis.TextDecoder=class{decode(value=new Uint8Array()){const view=value instanceof Uint8Array?value:ArrayBuffer.isView(value)?new Uint8Array(value.buffer,value.byteOffset,value.byteLength):value instanceof ArrayBuffer?new Uint8Array(value):new Uint8Array(0);let binary="";for(let i=0;i<view.length;i++){binary+=String.fromCharCode(view[i]);}return decodeURIComponent(escape(binary));}};}`,
				`if(typeof globalThis.Buffer==="undefined"){const __agentOsTe=typeof TextEncoder==="function"?new TextEncoder():null;const __agentOsTd=typeof TextDecoder==="function"?new TextDecoder():null;class __AgentOsEarlyBuffer extends Uint8Array{static from(value,encoding="utf8"){if(value instanceof ArrayBuffer){return new __AgentOsEarlyBuffer(value);}if(ArrayBuffer.isView(value)){return new __AgentOsEarlyBuffer(value.buffer.slice(value.byteOffset,value.byteOffset+value.byteLength));}if(Array.isArray(value)){return new __AgentOsEarlyBuffer(value);}const stringValue=String(value??"");if(encoding==="base64"&&typeof atob==="function"){const binary=atob(stringValue);const out=new __AgentOsEarlyBuffer(binary.length);for(let i=0;i<binary.length;i++){out[i]=binary.charCodeAt(i);}return out;}if(encoding==="binary"||encoding==="latin1"){const out=new __AgentOsEarlyBuffer(stringValue.length);for(let i=0;i<stringValue.length;i++){out[i]=stringValue.charCodeAt(i)&255;}return out;}if(__agentOsTe){return new __AgentOsEarlyBuffer(__agentOsTe.encode(stringValue));}const out=new __AgentOsEarlyBuffer(stringValue.length);for(let i=0;i<stringValue.length;i++){out[i]=stringValue.charCodeAt(i)&255;}return out;}static alloc(size){return new __AgentOsEarlyBuffer(Number(size)||0);}static concat(list,totalLength){const length=totalLength??list.reduce((sum,item)=>sum+(item?.length??0),0);const out=new __AgentOsEarlyBuffer(length);let offset=0;for(const item of list){const chunk=item instanceof Uint8Array?item:__AgentOsEarlyBuffer.from(item);out.set(chunk,offset);offset+=chunk.length;}return out;}static isBuffer(value){return value instanceof Uint8Array;}static byteLength(value,encoding="utf8"){return __AgentOsEarlyBuffer.from(value,encoding).byteLength;}toString(encoding="utf8"){if(encoding==="base64"&&typeof btoa==="function"){let binary="";for(const byte of this){binary+=String.fromCharCode(byte);}return btoa(binary);}if(encoding==="binary"||encoding==="latin1"){let binary="";for(const byte of this){binary+=String.fromCharCode(byte);}return binary;}if(__agentOsTd){return __agentOsTd.decode(this);}return Array.from(this,byte=>String.fromCharCode(byte)).join("");}}globalThis.Buffer=__AgentOsEarlyBuffer;}`,
				'if(typeof globalThis.performance==="undefined"){const __agentOsPerformanceStart=Date.now();globalThis.performance={now(){return Date.now()-__agentOsPerformanceStart;}};}if(typeof globalThis.performance.markResourceTiming!=="function"){globalThis.performance.markResourceTiming=()=>{};}',
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

const webStreamsPrelude = await buildWebStreamsPrelude();
await prependBundlePrelude(bridgeOutput, webStreamsPrelude);
await rewriteUndiciRuntimeFeaturesBundle(bridgeOutput, { required: true });
await rewriteUnsupportedUtilTypesBundle(bridgeOutput, { required: true });
await rewriteUndiciRuntimeFeaturesBundle(zlibBridgeOutput);
await rewriteUnsupportedUtilTypesBundle(zlibBridgeOutput);

console.log(
	`Built ${path.relative(workspaceRoot, bridgeOutput)} (${result.outputFiles?.[0]?.text?.length ?? "written"} bytes)`,
);
