import { builtinModules, loadBuiltinModule, rejectRestrictedBuiltinRequest } from "./builtin-modules.js";
import { exposeCustomGlobal, exposeMutableRuntimeStateGlobal } from "../global-exposure.js";
import { defineGlobal } from "../polyfills/index.js";

var initialModuleCache = {};
var initialPendingModules = {};
var initialCurrentModule = {
  id: "/<entry>.js",
  filename: "/<entry>.js",
  dirname: "/",
  exports: {},
  loaded: false
};
exposeMutableRuntimeStateGlobal("_moduleCache", initialModuleCache);
exposeMutableRuntimeStateGlobal("_pendingModules", initialPendingModules);
exposeMutableRuntimeStateGlobal("_currentModule", initialCurrentModule);
function _pathDirname(p) {
  const lastSlash = p.lastIndexOf("/");
  if (lastSlash === -1) return ".";
  if (lastSlash === 0) return "/";
  return p.slice(0, lastSlash);
}
function _parseFileUrl(url) {
  if (url.startsWith("file://")) {
    let path = url.slice(7);
    if (path.startsWith("/")) {
      return path;
    }
    return "/" + path;
  }
  return url;
}
function computeRequireResolvePaths(dirname, request) {
  if (Module.isBuiltin(request)) {
    return null;
  }
  if (request.startsWith("./") || request.startsWith("../") || request.startsWith("/")) {
    return [dirname];
  }
  return Module._nodeModulePaths(dirname);
}
function createRequireResolve(dirname) {
  const resolve = function(request, _options) {
    const resolved = _resolveModule.applySyncPromise(void 0, [
      request,
      dirname,
      "require"
    ]);
    if (resolved === null) {
      const err = new Error("Cannot find module '" + request + "'");
      err.code = "MODULE_NOT_FOUND";
      throw err;
    }
    return resolved;
  };
  resolve.paths = function(request) {
    return computeRequireResolvePaths(dirname, request);
  };
  return resolve;
}
const defaultRequireExtensions = {
  ".js": function(_module, _filename) {
  },
  ".json": function(_module, _filename) {
  },
  ".node": function(_module, _filename) {
    throw new Error(".node extensions are not supported in sandbox");
  }
};
function attachRequireMetadata(requireFn) {
  requireFn.cache = _moduleCache;
  requireFn.main = globalThis.process?.mainModule;
  requireFn.extensions = defaultRequireExtensions;
  return requireFn;
}
function createRequireEsmError(filename) {
  const error = new Error(`require() of ES Module ${filename} is not supported.`);
  error.code = "ERR_REQUIRE_ESM";
  return error;
}
function createModuleFormatBridgeMissingError(filename) {
  const error = new Error(
    `agentos module format bridge is not registered; cannot require ${filename}.`
  );
  error.code = "ERR_AGENTOS_MODULE_FORMAT_BRIDGE_MISSING";
  return error;
}
function moduleFormat(filename) {
  if (
    typeof _moduleFormat === "undefined" ||
    typeof _moduleFormat.applySyncPromise !== "function"
  ) {
    throw createModuleFormatBridgeMissingError(filename);
  }
  return _moduleFormat.applySyncPromise(void 0, [filename]);
}
function assertCommonjsLoadable(filename) {
  if (moduleFormat(filename) === "module") throw createRequireEsmError(filename);
}
function requireEsmSync(filename, parentPath) {
  if (typeof globalThis.__agentOsRequireEsmSync !== "function") {
    throw createRequireEsmError(filename);
  }
  const namespace = globalThis.__agentOsRequireEsmSync(filename, parentPath);
  if (
    namespace != null &&
    (typeof namespace === "object" || typeof namespace === "function") &&
    Object.prototype.hasOwnProperty.call(namespace, "default") &&
    namespace.__esModule !== true
  ) {
    const compatibleNamespace = Object.create(null);
    for (const key of Reflect.ownKeys(namespace)) {
      Object.defineProperty(compatibleNamespace, key, {
        configurable: false,
        enumerable: Object.prototype.propertyIsEnumerable.call(namespace, key),
        get: () => namespace[key],
      });
    }
    Object.defineProperty(compatibleNamespace, "__esModule", {
      configurable: false,
      enumerable: true,
      value: true,
    });
    return compatibleNamespace;
  }
  return namespace;
}
function createRequire(filename) {
  if (typeof filename !== "string" && !(filename instanceof URL)) {
    throw new TypeError("filename must be a string or URL");
  }
  const filepath = _parseFileUrl(String(filename));
  const dirname = _pathDirname(filepath);
  const resolve = createRequireResolve(dirname);
  const rejectRestrictedBuiltin = function(_request) {
  };
  const requireFn = function(request) {
    rejectRestrictedBuiltin(request);
    return _requireFrom(request, dirname);
  };
  requireFn.resolve = resolve;
  return attachRequireMetadata(requireFn);
}
// Expose createRequire under a namespaced global so an agent-SDK snapshot bundle
// (which runs as a raw IIFE Script, not a wrapped CJS module, when evaluated into
// the V8 startup snapshot) can bind a `require` for its node-builtin imports. This
// grants no new capability — guest CJS modules already receive `require` via the
// module wrapper, and resolution still flows through _requireFrom with the same
// permission checks — and the namespaced name avoids changing `typeof require` for
// guest code that branches on CJS-vs-ESM.
defineGlobal("__agentOsGuestCreateRequire", createRequire);
var Module = class _Module {
  id;
  path;
  exports;
  filename;
  loaded;
  children;
  paths;
  parent;
  isPreloading;
  constructor(id, parent) {
    this.id = id;
    this.path = _pathDirname(id);
    this.exports = {};
    this.filename = id;
    this.loaded = false;
    this.children = [];
    this.paths = [];
    this.parent = parent;
    this.isPreloading = false;
    let current = this.path;
    while (current !== "/") {
      this.paths.push(current + "/node_modules");
      current = _pathDirname(current);
    }
    this.paths.push("/node_modules");
  }
  require(request) {
    return _requireFrom(request, this.path);
  }
  _compile(content, filename) {
    let source = String(content);
    if (source.startsWith("#!")) {
      const newline = source.indexOf("\n");
      source = newline === -1 ? "" : "\n" + source.slice(newline + 1);
    }
    const contentWithSourceUrl = source + "\n//# sourceURL=" + String(filename);
    const wrapper = new Function(
      "exports",
      "require",
      "module",
      "__filename",
      "__dirname",
      contentWithSourceUrl
    );
    const moduleRequire = (request) => {
      rejectRestrictedBuiltinRequest(request);
      return _requireFrom(request, this.path);
    };
    moduleRequire.resolve = createRequireResolve(this.path);
    attachRequireMetadata(moduleRequire);
    const previousModule = globalThis._currentModule;
    globalThis._currentModule = this;
    try {
      wrapper(this.exports, moduleRequire, this, filename, this.path);
      this.loaded = true;
      return this.exports;
    } finally {
      globalThis._currentModule = previousModule;
    }
  }
  static _extensions = {
    ...defaultRequireExtensions,
    ".js": function(module, filename) {
      assertCommonjsLoadable(filename);
      const content = typeof _loadFile !== "undefined" ? _loadFile.applySyncPromise(void 0, [
        filename
      ]) : _requireFrom("fs", "/").readFileSync(filename, "utf8");
      module._compile(content, filename);
    },
    ".json": function(module, filename) {
      const content = typeof _loadFile !== "undefined" ? _loadFile.applySyncPromise(void 0, [
        filename
      ]) : _requireFrom("fs", "/").readFileSync(filename, "utf8");
      module.exports = JSON.parse(content);
    }
  };
  static _cache = typeof _moduleCache !== "undefined" ? _moduleCache : {};
  static _resolveFilename(request, parent, _isMain, _options) {
    const parentDir = parent && parent.path ? parent.path : "/";
    const resolved = _resolveModule.applySyncPromise(void 0, [
      request,
      parentDir,
      "require"
    ]);
    if (resolved === null) {
      const err = new Error("Cannot find module '" + request + "'");
      err.code = "MODULE_NOT_FOUND";
      throw err;
    }
    return resolved;
  }
  static wrap(content) {
    return "(function (exports, require, module, __filename, __dirname) { " + content + "\n});";
  }
  static builtinModules = builtinModules;
  static isBuiltin(moduleName) {
    const name = moduleName.replace(/^node:/, "");
    return _Module.builtinModules.includes(name);
  }
  static createRequire = createRequire;
  static syncBuiltinESMExports() {
  }
  static findSourceMap(_path) {
    return void 0;
  }
  static _nodeModulePaths(from) {
    const paths = [];
    let current = from;
    while (current !== "/") {
      paths.push(current + "/node_modules");
      current = _pathDirname(current);
      if (current === ".") break;
    }
    paths.push("/node_modules");
    return paths;
  }
  static _load(request, parent, _isMain) {
    const parentDir = parent && parent.path ? parent.path : "/";
    return _requireFrom(request, parentDir, _isMain === true);
  }
  static runMain() {
  }
};
var SourceMap = class {
  constructor(_payload) {
    throw new Error("SourceMap is not implemented in sandbox");
  }
  get payload() {
    throw new Error("SourceMap is not implemented in sandbox");
  }
  set payload(_value) {
    throw new Error("SourceMap is not implemented in sandbox");
  }
  findEntry(_line, _column) {
    throw new Error("SourceMap is not implemented in sandbox");
  }
};
var moduleModule = Object.assign(Module, {
  Module,
  createRequire,
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
  SourceMap
});
exposeCustomGlobal("_moduleModule", moduleModule);
function markMainModule(module) {
  module.id = ".";
  module.parent = null;
  if (globalThis.process) {
    globalThis.process.mainModule = module;
  }
}
function requireFrom(request, parentDir, isMain = false) {
  const parentPath = typeof parentDir === "string" ? parentDir : "/";
  if (Module.isBuiltin(request)) {
    try {
      return loadBuiltinModule(request);
    } catch (error) {
      if (error?.code !== "MODULE_NOT_FOUND") {
        throw error;
      }
    }
  }
  const resolved = _resolveModule.applySyncPromise(void 0, [
    request,
    parentPath,
    "require"
  ]);
  if (resolved === null) {
    const error = new Error(`Cannot find module '${request}'`);
    error.code = "MODULE_NOT_FOUND";
    throw error;
  }
  if (Object.prototype.hasOwnProperty.call(_moduleCache, resolved)) {
    if (isMain) markMainModule(_moduleCache[resolved]);
    return _moduleCache[resolved].exports;
  }
  if (moduleFormat(resolved) === "module") {
    const module = new Module(resolved, isMain ? null : { path: parentPath });
    if (isMain) markMainModule(module);
    _moduleCache[resolved] = module;
    try {
      module.exports = requireEsmSync(resolved, parentPath);
      module.loaded = true;
      return module.exports;
    } catch (error) {
      delete _moduleCache[resolved];
      throw error;
    }
  }
  const module = new Module(resolved, isMain ? null : { path: parentPath });
  if (isMain) markMainModule(module);
  _moduleCache[resolved] = module;
  try {
    const extension = resolved.endsWith(".json") ? ".json" : resolved.endsWith(".node") ? ".node" : ".js";
    const loader = Module._extensions[extension] ?? Module._extensions[".js"];
    loader(module, resolved);
    module.loaded = true;
    return module.exports;
  } catch (error) {
    delete _moduleCache[resolved];
    throw error;
  }
}
exposeCustomGlobal("_requireFrom", requireFrom);
var module_default = moduleModule;
export { initialModuleCache, initialPendingModules, initialCurrentModule, _pathDirname, _parseFileUrl, computeRequireResolvePaths, createRequireResolve, defaultRequireExtensions, attachRequireMetadata, createRequireEsmError, createModuleFormatBridgeMissingError, moduleFormat, assertCommonjsLoadable, requireEsmSync, createRequire, Module, SourceMap, moduleModule, requireFrom, module_default };
