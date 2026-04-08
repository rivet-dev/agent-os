/**
 * Module classification and resolution helpers.
 *
 * Node built-ins are split into three tiers:
 * - Bridge modules: fully polyfilled by the bridge (fs, process, http, etc.)
 * - Deferred core modules: known but not yet bridge-supported; surfaced via
 *   deferred stubs in require paths and polyfills/wrappers in ESM paths
 * - Unsupported core modules: known but intentionally unimplemented
 *
 * Everything else falls through to node-stdlib-browser polyfills or node_modules.
 */

/**
 * Static set of Node.js stdlib module names that have browser polyfills
 * available via node-stdlib-browser. Hardcoded to avoid importing
 * node-stdlib-browser at runtime (its ESM entry crashes on missing
 * mock/empty.js in published builds).
 */
const STDLIB_BROWSER_MODULES = new Set([
	"assert",
	"buffer",
	"child_process",
	"cluster",
	"console",
	"constants",
	"crypto",
	"dgram",
	"dns",
	"domain",
	"events",
	"fs",
	"http",
	"https",
	"http2",
	"module",
	"net",
	"os",
	"path",
	"punycode",
	"process",
	"querystring",
	"readline",
	"repl",
	"stream",
	"stream/promises",
	"_stream_duplex",
	"_stream_passthrough",
	"_stream_readable",
	"_stream_transform",
	"_stream_writable",
	"string_decoder",
	"sys",
	"timers/promises",
	"timers",
	"tls",
	"tty",
	"url",
	"util",
	"vm",
	"zlib",
]);

/** Check if a module has a polyfill available via node-stdlib-browser. */
function hasPolyfill(moduleName: string): boolean {
	const name = moduleName.replace(/^node:/, "");
	return STDLIB_BROWSER_MODULES.has(name);
}

/** Modules with full bridge implementations injected into the isolate. */
const BRIDGE_MODULES = [
	"fs",
	"fs/promises",
	"module",
	"os",
	"http",
	"https",
	"http2",
	"dns",
	"child_process",
	"process",
	"v8",
] as const;

/**
 * Recognized built-ins that lack bridge support.
 * Runtime handling differs by path (require stubs vs ESM/polyfill handling).
 */
const DEFERRED_CORE_MODULES = [
	"net",
	"tls",
	"readline",
	"perf_hooks",
	"async_hooks",
	"worker_threads",
	"diagnostics_channel",
] as const;

/** Built-ins that are intentionally unimplemented (throw on use). */
const UNSUPPORTED_CORE_MODULES = [
	"dgram",
	"cluster",
	"wasi",
	"inspector",
	"repl",
	"trace_events",
	"domain",
] as const;

const KNOWN_BUILTIN_MODULES = new Set([
	...BRIDGE_MODULES,
	...DEFERRED_CORE_MODULES,
	...UNSUPPORTED_CORE_MODULES,
	"assert",
	"buffer",
	"constants",
	"crypto",
	"events",
	"path",
	"path/posix",
	"path/win32",
	"querystring",
	"stream",
	"stream/consumers",
	"stream/promises",
	"stream/web",
	"string_decoder",
	"timers",
	"tty",
	"url",
	"util",
	"vm",
	"zlib",
]);

/**
 * Known named exports for each built-in module. Used by the ESM wrapper
 * generator to create `export const X = _builtin.X;` re-exports so that
 * `import { readFile } from 'fs'` works inside the isolate.
 */
export const BUILTIN_NAMED_EXPORTS: Record<string, string[]> = {
	readline: [
		"createInterface",
		"promises",
	],
	fs: [
		"promises",
		"readFileSync",
		"writeFileSync",
		"appendFileSync",
		"existsSync",
		"statSync",
		"mkdirSync",
		"readdirSync",
		"createReadStream",
		"createWriteStream",
	],
	"fs/promises": [
		"access",
		"readFile",
		"writeFile",
		"appendFile",
		"copyFile",
		"cp",
		"open",
		"opendir",
		"mkdir",
		"mkdtemp",
		"readdir",
		"rename",
		"stat",
		"lstat",
		"chmod",
		"chown",
		"utimes",
		"truncate",
		"unlink",
		"rm",
		"rmdir",
		"realpath",
		"readlink",
		"symlink",
		"link",
	],
	module: [
		"createRequire",
		"Module",
		"isBuiltin",
		"builtinModules",
		"SourceMap",
		"syncBuiltinESMExports",
	],
	os: [
		"arch",
		"platform",
		"tmpdir",
		"homedir",
		"hostname",
		"type",
		"release",
		"constants",
	],
	http: [
		"request",
		"get",
		"createServer",
		"Server",
		"IncomingMessage",
		"ServerResponse",
		"Agent",
		"validateHeaderName",
		"validateHeaderValue",
		"METHODS",
		"STATUS_CODES",
	],
	https: ["request", "get", "createServer", "Agent", "globalAgent"],
	dns: ["lookup", "resolve", "resolve4", "resolve6", "promises"],
	child_process: [
		"spawn",
		"spawnSync",
		"exec",
		"execSync",
		"execFile",
		"execFileSync",
		"fork",
	],
	process: [
		"argv",
		"env",
		"cwd",
		"chdir",
		"exit",
		"pid",
		"platform",
		"version",
		"versions",
		"stdout",
		"stderr",
		"stdin",
		"nextTick",
	],
	path: [
		"sep",
		"delimiter",
		"basename",
		"dirname",
		"extname",
		"format",
		"isAbsolute",
		"join",
		"normalize",
		"parse",
		"relative",
		"resolve",
	],
	async_hooks: [
		"AsyncLocalStorage",
		"AsyncResource",
		"createHook",
		"executionAsyncId",
		"triggerAsyncId",
	],
	perf_hooks: [
		"performance",
		"PerformanceObserver",
		"PerformanceEntry",
		"monitorEventLoopDelay",
		"createHistogram",
		"constants",
	],
	diagnostics_channel: [
		"channel",
		"hasSubscribers",
		"tracingChannel",
		"Channel",
	],
	stream: [
		"Readable",
		"Writable",
		"Duplex",
		"Transform",
		"PassThrough",
		"Stream",
		"pipeline",
		"finished",
		"promises",
		"addAbortSignal",
		"compose",
	],
	"stream/promises": [
		"finished",
		"pipeline",
	],
	"stream/web": [
		"ReadableStream",
		"ReadableStreamDefaultReader",
		"ReadableStreamBYOBReader",
		"ReadableStreamBYOBRequest",
		"ReadableByteStreamController",
		"ReadableStreamDefaultController",
		"TransformStream",
		"TransformStreamDefaultController",
		"WritableStream",
		"WritableStreamDefaultWriter",
		"WritableStreamDefaultController",
		"ByteLengthQueuingStrategy",
		"CountQueuingStrategy",
		"TextEncoderStream",
		"TextDecoderStream",
		"CompressionStream",
		"DecompressionStream",
	],
};

/**
 * Normalize a module specifier to its canonical form if it's a known built-in.
 * Returns null for non-builtin specifiers.
 * Preserves the `node:` prefix when present, strips it otherwise.
 */
export function normalizeBuiltinSpecifier(request: string): string | null {
	const moduleName = request.replace(/^node:/, "");
	if (KNOWN_BUILTIN_MODULES.has(moduleName) || hasPolyfill(moduleName)) {
		return request.startsWith("node:") ? `node:${moduleName}` : moduleName;
	}
	return null;
}

/** Extract the directory portion of a path (lightweight dirname without node:path). */
export function getPathDir(path: string): string {
	const normalizedPath = path.replace(/\\/g, "/");
	const lastSlash = normalizedPath.lastIndexOf("/");
	if (lastSlash <= 0) return "/";
	return normalizedPath.slice(0, lastSlash);
}
