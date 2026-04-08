/**
 * ESM wrapper generator for built-in modules inside the isolate.
 *
 * The V8 isolate's ESM `import` can only resolve modules we explicitly provide.
 * For Node built-ins (fs, path, etc.) we generate thin ESM wrappers that
 * re-export the bridge-provided globalThis objects as proper ESM modules
 * with both default and named exports.
 */

import { BUILTIN_NAMED_EXPORTS } from "./builtin-modules.js";

function isValidIdentifier(value: string): boolean {
	return /^[$A-Z_][0-9A-Z_$]*$/i.test(value);
}

/** Generate `export const X = _builtin.X;` lines for each known named export. */
function buildNamedExportLines(namedExports: string[]): string[] {
	return Array.from(new Set(namedExports))
		.filter(isValidIdentifier)
		.map(
			(name) =>
				"export const " +
				name +
				" = _builtin == null ? undefined : _builtin[" +
				JSON.stringify(name) +
				"];",
		);
}

/**
 * Build a complete ESM wrapper that reads a bridge global via `bindingExpression`
 * and re-exports it as `default` plus individual named exports.
 */
function buildWrapperSource(bindingExpression: string, namedExports: string[]): string {
	const lines = [
		"const _builtin = " + bindingExpression + ";",
		"export default _builtin;",
		...buildNamedExportLines(namedExports),
	];
	return lines.join("\n");
}

const MODULE_FALLBACK_BINDING =
	"globalThis.bridge?.module || {" +
	"createRequire: globalThis._createRequire || function(f) {" +
	"const dir = f.replace(/\\\\[^\\\\]*$/, '') || '/';" +
	"return function(m) { return globalThis._requireFrom(m, dir); };" +
	"}," +
	"Module: { builtinModules: [] }," +
	"isBuiltin: () => false," +
	"builtinModules: []" +
	"}";

const STATIC_BUILTIN_BINDINGS: Readonly<Record<string, string>> = {
	fs: "globalThis.bridge?.fs || globalThis.bridge?.default || {}",
	"fs/promises": "(globalThis.bridge?.fs || globalThis.bridge?.default || {}).promises || {}",
	"stream/promises": 'globalThis._requireFrom("stream/promises", "/")',
	module: MODULE_FALLBACK_BINDING,
	os: "globalThis.bridge?.os || {}",
	http: "globalThis._httpModule || globalThis.bridge?.network?.http || {}",
	https: "globalThis._httpsModule || globalThis.bridge?.network?.https || {}",
	http2: "globalThis._http2Module || {}",
	dns: "globalThis._dnsModule || globalThis.bridge?.network?.dns || {}",
	child_process: "globalThis._childProcessModule || globalThis.bridge?.childProcess || {}",
	process: "globalThis.process || {}",
	v8: "globalThis._moduleCache?.v8 || {}",
	async_hooks: 'globalThis._requireFrom("async_hooks", "/")',
	perf_hooks: 'globalThis._requireFrom("perf_hooks", "/")',
	worker_threads: 'globalThis._requireFrom("worker_threads", "/")',
	diagnostics_channel: 'globalThis._requireFrom("diagnostics_channel", "/")',
	net: 'globalThis._requireFrom("net", "/")',
	tls: 'globalThis._requireFrom("tls", "/")',
	readline: 'globalThis._requireFrom("readline", "/")',
	"path/win32": 'globalThis._requireFrom("path/win32", "/")',
	"path/posix": 'globalThis._requireFrom("path/posix", "/")',
};

const STATIC_BUILTIN_WRAPPER_SOURCES: Readonly<Record<string, string>> = {
	fs: buildWrapperSource(STATIC_BUILTIN_BINDINGS.fs, BUILTIN_NAMED_EXPORTS.fs),
	"fs/promises": buildWrapperSource(
		STATIC_BUILTIN_BINDINGS["fs/promises"],
		BUILTIN_NAMED_EXPORTS["fs/promises"],
	),
	"stream/promises": buildWrapperSource(
		STATIC_BUILTIN_BINDINGS["stream/promises"],
		BUILTIN_NAMED_EXPORTS["stream/promises"],
	),
	module: buildWrapperSource(STATIC_BUILTIN_BINDINGS.module, BUILTIN_NAMED_EXPORTS.module),
	os: buildWrapperSource(STATIC_BUILTIN_BINDINGS.os, BUILTIN_NAMED_EXPORTS.os),
	http: buildWrapperSource(STATIC_BUILTIN_BINDINGS.http, BUILTIN_NAMED_EXPORTS.http),
	https: buildWrapperSource(STATIC_BUILTIN_BINDINGS.https, BUILTIN_NAMED_EXPORTS.https),
	http2: buildWrapperSource(STATIC_BUILTIN_BINDINGS.http2, []),
	dns: buildWrapperSource(STATIC_BUILTIN_BINDINGS.dns, BUILTIN_NAMED_EXPORTS.dns),
	child_process: buildWrapperSource(
		STATIC_BUILTIN_BINDINGS.child_process,
		BUILTIN_NAMED_EXPORTS.child_process,
	),
	process: buildWrapperSource(STATIC_BUILTIN_BINDINGS.process, BUILTIN_NAMED_EXPORTS.process),
	v8: buildWrapperSource(STATIC_BUILTIN_BINDINGS.v8, []),
	async_hooks: buildWrapperSource(
		STATIC_BUILTIN_BINDINGS.async_hooks,
		BUILTIN_NAMED_EXPORTS.async_hooks,
	),
	perf_hooks: buildWrapperSource(
		STATIC_BUILTIN_BINDINGS.perf_hooks,
		BUILTIN_NAMED_EXPORTS.perf_hooks,
	),
	worker_threads: buildWrapperSource(
		STATIC_BUILTIN_BINDINGS.worker_threads,
		BUILTIN_NAMED_EXPORTS.worker_threads ?? [],
	),
	diagnostics_channel: buildWrapperSource(
		STATIC_BUILTIN_BINDINGS.diagnostics_channel,
		BUILTIN_NAMED_EXPORTS.diagnostics_channel ?? [],
	),
	net: buildWrapperSource(STATIC_BUILTIN_BINDINGS.net, BUILTIN_NAMED_EXPORTS.net ?? []),
	tls: buildWrapperSource(STATIC_BUILTIN_BINDINGS.tls, BUILTIN_NAMED_EXPORTS.tls ?? []),
	readline: buildWrapperSource(
		STATIC_BUILTIN_BINDINGS.readline,
		BUILTIN_NAMED_EXPORTS.readline ?? [],
	),
	"path/win32": buildWrapperSource(
		STATIC_BUILTIN_BINDINGS["path/win32"],
		BUILTIN_NAMED_EXPORTS.path ?? [],
	),
	"path/posix": buildWrapperSource(
		STATIC_BUILTIN_BINDINGS["path/posix"],
		BUILTIN_NAMED_EXPORTS.path ?? [],
	),
};

export function getBuiltinBindingExpression(moduleName: string): string | null {
	return STATIC_BUILTIN_BINDINGS[moduleName] ?? null;
}

/** Get a pre-built ESM wrapper for a bridge-backed built-in, or null if not bridge-handled. */
export function getStaticBuiltinWrapperSource(moduleName: string): string | null {
	return STATIC_BUILTIN_WRAPPER_SOURCES[moduleName] ?? null;
}

/** Build a custom ESM wrapper for a dynamically-resolved module (e.g. polyfills). */
export function createBuiltinESMWrapper(
	bindingExpression: string,
	namedExports: string[],
): string {
	return buildWrapperSource(bindingExpression, namedExports);
}

/** Wrapper for unsupported built-ins: exports an empty object as default. */
export function getEmptyBuiltinESMWrapper(): string {
	return buildWrapperSource("{}", []);
}
