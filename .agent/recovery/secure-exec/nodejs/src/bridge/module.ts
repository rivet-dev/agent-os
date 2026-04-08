import {
	exposeCustomGlobal,
	exposeMutableRuntimeStateGlobal,
} from "@secure-exec/core/internal/shared/global-exposure";
import type {
	ModuleCacheBridgeRecord,
	RequireFromBridgeFn,
	ResolveModuleBridgeRef,
} from "../bridge-contract.js";

// Module polyfill for the sandbox
// Provides module.createRequire and other module utilities for npm compatibility

// Seed the mutable CommonJS loader state before requireSetup runs.
const initialModuleCache: ModuleCacheBridgeRecord = {};
const initialPendingModules: Record<string, { exports: unknown }> = {};
const initialCurrentModule = {
	id: "/<entry>.js",
	filename: "/<entry>.js",
	dirname: "/",
	exports: {},
	loaded: false,
};

exposeMutableRuntimeStateGlobal("_moduleCache", initialModuleCache);
exposeMutableRuntimeStateGlobal("_pendingModules", initialPendingModules);
exposeMutableRuntimeStateGlobal("_currentModule", initialCurrentModule);

// Declare host bridge globals that are set up by setupRequire()
declare const _requireFrom: RequireFromBridgeFn;
declare const _resolveModule: ResolveModuleBridgeRef;
declare const _moduleCache: ModuleCacheBridgeRecord;

// Path utilities for module resolution
function _pathDirname(p: string): string {
  const lastSlash = p.lastIndexOf("/");
  if (lastSlash === -1) return ".";
  if (lastSlash === 0) return "/";
  return p.slice(0, lastSlash);
}

function _parseFileUrl(url: string): string {
  // Handle file:// URLs
  if (url.startsWith("file://")) {
    // Remove file:// prefix
    let path = url.slice(7);
    // Handle file:///path on Unix (3 slashes = absolute path)
    if (path.startsWith("/")) {
      return path;
    }
    // Handle file://host/path (rare, treat host as empty)
    return "/" + path;
  }
  return url;
}

// Require function interface
interface RequireFunction {
  (request: string): unknown;
  resolve: RequireResolve;
  cache: Record<string, { exports: unknown }>;
  main: Module | undefined;
  extensions: Record<string, (module: Module, filename: string) => void>;
}

interface RequireResolve {
  (request: string, options?: { paths?: string[] }): string;
  paths: (request: string) => string[] | null;
}

/**
 * Create a require function that resolves relative to the given filename.
 * This mimics Node.js's module.createRequire(filename).
 */
export function createRequire(filename: string | URL): RequireFunction {
  if (typeof filename !== "string" && !(filename instanceof URL)) {
    throw new TypeError("filename must be a string or URL");
  }

  // Parse file:// URLs
  const filepath = _parseFileUrl(String(filename));
  const dirname = _pathDirname(filepath);

  const builtins = [
    "fs",
    "path",
    "os",
    "events",
    "util",
    "http",
    "_http_common",
    "https",
    "dns",
    "dgram",
    "child_process",
    "stream",
    "buffer",
    "url",
    "querystring",
    "crypto",
    "zlib",
    "assert",
    "tty",
    "net",
    "tls",
  ];

  // Create resolve.paths function
  const resolvePaths = function (request: string): string[] | null {
    // For built-in modules, return null
    if (builtins.includes(request) || request.startsWith("node:")) {
      return null;
    }
    // For relative paths, return array starting from dirname
    if (
      request.startsWith("./") ||
      request.startsWith("../") ||
      request.startsWith("/")
    ) {
      return [dirname];
    }
    // For bare specifiers, return node_modules search paths
    const paths: string[] = [];
    let current = dirname;
    while (current !== "/") {
      paths.push(current + "/node_modules");
      current = _pathDirname(current);
    }
    paths.push("/node_modules");
    return paths;
  };

  // Create resolve function
  const resolve = function (
    request: string,
    _options?: { paths?: string[] }
  ): string {
    const resolved = _resolveModule.applySyncPromise(undefined, [
      request,
      dirname,
    ]);
    if (resolved === null) {
      const err = new Error("Cannot find module '" + request + "'") as NodeJS.ErrnoException;
      err.code = "MODULE_NOT_FOUND";
      throw err;
    }
    return resolved;
  } as RequireResolve;

  resolve.paths = resolvePaths;

  // Create a require function bound to this directory
  const requireFn = function (request: string): unknown {
    return _requireFrom(request, dirname);
  } as RequireFunction;

  // Add require.resolve
  requireFn.resolve = resolve;

  // Add require.cache reference to global module cache
  requireFn.cache = _moduleCache as Record<string, { exports: unknown }>;

  // Add require.main (null for dynamically created require)
  requireFn.main = undefined;

  // Add require.extensions (deprecated but still used by some tools)
  requireFn.extensions = {
    ".js": function (_module: Module, _filename: string): void {
      // This is a stub - actual loading is handled by our require implementation
    },
    ".json": function (_module: Module, _filename: string): void {
      // JSON loading stub
    },
    ".node": function (_module: Module, _filename: string): void {
      throw new Error(".node extensions are not supported in sandbox");
    },
  };

  return requireFn;
}

/**
 * Polyfill of Node.js `Module` class for sandbox compatibility. Provides
 * `_compile`, `_resolveFilename`, `_load`, `_extensions`, and `_cache` statics
 * that npm tooling (promzard, resolve, etc.) relies on.
 */
export class Module {
  id: string;
  path: string;
  exports: unknown;
  filename: string;
  loaded: boolean;
  children: Module[];
  paths: string[];
  parent: Module | null | undefined;
  isPreloading: boolean;

  constructor(id: string, parent?: Module | null) {
    this.id = id;
    this.path = _pathDirname(id);
    this.exports = {};
    this.filename = id;
    this.loaded = false;
    this.children = [];
    this.paths = [];
    this.parent = parent;
    this.isPreloading = false;

    // Build module paths
    let current = this.path;
    while (current !== "/") {
      this.paths.push(current + "/node_modules");
      current = _pathDirname(current);
    }
    this.paths.push("/node_modules");
  }

  require(request: string): unknown {
    return _requireFrom(request, this.path);
  }

  _compile(content: string, filename: string): unknown {
    // Create wrapper function and execute
    const wrapper = new Function(
      "exports",
      "require",
      "module",
      "__filename",
      "__dirname",
      content
    );
    const moduleRequire = (request: string): unknown =>
      _requireFrom(request, this.path);
    (moduleRequire as { resolve?: (request: string) => string }).resolve = (
      request: string
    ): string => {
      const resolved = _resolveModule.applySyncPromise(undefined, [
        request,
        this.path,
      ]);
      if (resolved === null) {
        const err = new Error("Cannot find module '" + request + "'") as NodeJS.ErrnoException;
        err.code = "MODULE_NOT_FOUND";
        throw err;
      }
      return resolved;
    };
    wrapper(this.exports, moduleRequire, this, filename, this.path);
    this.loaded = true;
    return this.exports;
  }

  static _extensions: Record<string, (module: Module, filename: string) => void> = {
    ".js": function (module: Module, filename: string): void {
      const fs = _requireFrom("fs", "/") as { readFileSync: (path: string, encoding: string) => string };
      const content = fs.readFileSync(filename, "utf8");
      module._compile(content, filename);
    },
    ".json": function (module: Module, filename: string): void {
      const fs = _requireFrom("fs", "/") as { readFileSync: (path: string, encoding: string) => string };
      const content = fs.readFileSync(filename, "utf8");
      module.exports = JSON.parse(content);
    },
    ".node": function (): void {
      throw new Error(".node extensions are not supported in sandbox");
    },
  };

  static _cache =
    typeof _moduleCache !== "undefined"
      ? (_moduleCache as Record<string, { exports: unknown }>)
      : {};

  static _resolveFilename(
    request: string,
    parent: Module | null | undefined,
    _isMain?: boolean,
    _options?: unknown
  ): string {
    const parentDir = parent && parent.path ? parent.path : "/";
    const resolved = _resolveModule.applySyncPromise(undefined, [
      request,
      parentDir,
    ]);
    if (resolved === null) {
      const err = new Error("Cannot find module '" + request + "'") as NodeJS.ErrnoException;
      err.code = "MODULE_NOT_FOUND";
      throw err;
    }
    return resolved;
  }

  static wrap(content: string): string {
    return (
      "(function (exports, require, module, __filename, __dirname) { " +
      content +
      "\n});"
    );
  }

  static builtinModules = [
    "assert",
    "buffer",
    "child_process",
    "crypto",
    "dns",
    "events",
    "fs",
    "http",
    "https",
    "net",
    "os",
    "path",
    "querystring",
    "stream",
    "string_decoder",
    "timers",
    "tls",
    "tty",
    "url",
    "util",
    "zlib",
    "vm",
    "module",
  ];

  static isBuiltin(moduleName: string): boolean {
    const name = moduleName.replace(/^node:/, "");
    return Module.builtinModules.includes(name);
  }

  static createRequire = createRequire;

  static syncBuiltinESMExports(): void {
    // No-op in our environment
  }

  static findSourceMap(_path: string): undefined {
    return undefined;
  }

  static _nodeModulePaths(from: string): string[] {
    // Return array of node_modules paths from the given directory up to root
    const paths: string[] = [];
    let current = from;
    while (current !== "/") {
      paths.push(current + "/node_modules");
      current = _pathDirname(current);
      if (current === ".") break;
    }
    paths.push("/node_modules");
    return paths;
  }

  static _load(
    request: string,
    parent: Module | null | undefined,
    _isMain?: boolean
  ): unknown {
    // Use our require system
    const parentDir = parent && parent.path ? parent.path : "/";
    return _requireFrom(request, parentDir);
  }

  static runMain(): void {
    // No-op - we don't have a main module in this context
  }
}

// SourceMap class - not implemented
export class SourceMap {
  constructor(_payload: unknown) {
    throw new Error("SourceMap is not implemented in sandbox");
  }

  get payload(): never {
    throw new Error("SourceMap is not implemented in sandbox");
  }

  set payload(_value: unknown) {
    throw new Error("SourceMap is not implemented in sandbox");
  }

  findEntry(_line: number, _column: number): never {
    throw new Error("SourceMap is not implemented in sandbox");
  }
}

// Module namespace export matching Node.js 'module' module
// Note: We don't strictly satisfy typeof nodeModule due to complex intersection types
const moduleModule = {
  Module: Module,
  createRequire: createRequire,

  // Module._extensions (deprecated alias)
  _extensions: Module._extensions,

  // Module._cache reference
  _cache: Module._cache,

  // Built-in module list
  builtinModules: Module.builtinModules,

  // isBuiltin check
  isBuiltin: Module.isBuiltin,

  // Module._resolveFilename (internal but sometimes used)
  _resolveFilename: Module._resolveFilename,

  // wrap function
  wrap: Module.wrap,

  // syncBuiltinESMExports (stub for ESM interop)
  syncBuiltinESMExports: Module.syncBuiltinESMExports,

  // findSourceMap (stub)
  findSourceMap: Module.findSourceMap,

  // SourceMap class (stub)
  SourceMap: SourceMap,
};

// Expose to global for require() to use
exposeCustomGlobal("_moduleModule", moduleModule);

export default moduleModule;
